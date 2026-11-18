//! Disposable read caches are not canonical storage and never acquire a writer.
use super::*;
use serde::{Deserialize, Serialize};
use std::{fs::OpenOptions, io::Write};

const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cache {
    source: StorageSnapshot,
    value: serde_json::Value,
}
fn path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!(
        ".storage-cache-{}.json",
        crate::io::sha256_hex(key.as_bytes())
    ))
}
fn same_prefix(a: &StorageSnapshot, b: &StorageSnapshot) -> bool {
    a.session == b.session && a.revision == b.revision && a.content_sha256 == b.content_sha256
}
pub(super) fn read_cache(
    session: SessionLocator,
    key: String,
    source: Option<StorageSnapshot>,
) -> Result<SessionStorageResponse> {
    let dir = checked_path(&session)?;
    let bytes = match crate::io::bounded_read(&path(&dir, &key), MAX_CACHE_BYTES + 1024 * 1024) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(SessionStorageResponse::Metadata { document: None }),
    };
    let cache: Cache = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return Ok(SessionStorageResponse::Metadata { document: None }),
    };
    if cache.source.session != session
        || source
            .as_ref()
            .is_some_and(|source| !same_prefix(source, &cache.source))
    {
        return Ok(SessionStorageResponse::Metadata { document: None });
    }
    let etag = storage_value_etag(&cache.value)?;
    // Cache values are published by atomic rename. Recheck the locator after
    // reading so a retired incarnation cannot cross a concurrent replacement.
    checked_path(&session)?;
    Ok(SessionStorageResponse::Metadata {
        document: Some(StorageMetadataDocument {
            etag,
            value: cache.value,
        }),
    })
}

impl State {
    pub(super) fn write_cache(
        &mut self,
        session: SessionLocator,
        key: String,
        source: StorageSnapshot,
        value: serde_json::Value,
    ) -> Result<SessionStorageResponse> {
        ensure!(
            source.session == session,
            "cache source belongs to another session"
        );
        let dir = match self.checked_path(&session) {
            Ok(dir) if dir.is_dir() => dir,
            _ => return Ok(SessionStorageResponse::CacheWritten { etag: None }),
        };
        let current = self.snapshot(session.clone())?;
        let matches = same_prefix(&current, &source);
        self.snapshots.remove(&current.snapshot_id);
        self.snapshot_order.retain(|id| id != &current.snapshot_id);
        if !matches {
            return Ok(SessionStorageResponse::CacheWritten { etag: None });
        }
        let cache = Cache { source, value };
        let bytes = serde_json::to_vec(&cache)?;
        ensure!(
            bytes.len() <= MAX_CACHE_BYTES + 1024 * 1024,
            "cache value exceeds maximum"
        );
        // No create_dir_all: a cache write may never resurrect a deleted session.
        let temp = dir.join(format!(".storage-cache-{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp)?;
            file.write_all(&bytes)?;
            file.sync_data()?;
            ensure!(
                identity::resolve_session_identity(&dir)?.session_id == session.session_id,
                "cache target identity changed"
            );
            std::fs::rename(&temp, path(&dir, &key))?;
            Ok::<(), anyhow::Error>(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
            return Ok(SessionStorageResponse::CacheWritten { etag: None });
        }
        evict(&dir, &path(&dir, &key))?;
        Ok(SessionStorageResponse::CacheWritten {
            etag: Some(storage_value_etag(&cache.value)?),
        })
    }
}

/// Quota applies across all content-addressed generations and page keys.
fn evict(dir: &Path, keep: &Path) -> Result<()> {
    let mut files = Vec::new();
    let mut total = 0_u64;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(".storage-cache-") || !name.ends_with(".json") {
            continue;
        }
        let meta = entry.metadata()?;
        if !meta.is_file() {
            continue;
        }
        total = total.saturating_add(meta.len());
        if entry.path() != keep {
            files.push((meta.modified()?, entry.path(), meta.len()));
        }
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    for (_, path, bytes) in files {
        if total <= 128 * 1024 * 1024 {
            break;
        }
        match std::fs::remove_file(path) {
            Ok(()) => total = total.saturating_sub(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
