//! Disposable read caches are not canonical storage and never acquire a writer.
use super::*;
use serde::{Deserialize, Serialize};
use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
};

const CACHE_ENVELOPE_ALLOWANCE: usize = 1024 * 1024;
const MAX_CACHE_FILE_BYTES: usize = SESSION_STORAGE_MAX_CACHE_BYTES + CACHE_ENVELOPE_ALLOWANCE;
// Hint only ordinary small cache documents; other object readers keep no hint.
const CACHE_READ_HINT_MAX_BYTES: usize = 64 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cache<S = StorageSnapshot, V = serde_json::Value> {
    source: S,
    value: V,
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
    let bytes = match crate::io::bounded_read_with_size_hint(
        &path(&dir, &key),
        MAX_CACHE_FILE_BYTES,
        CACHE_READ_HINT_MAX_BYTES,
    ) {
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
        let current = self.prepare_snapshot(session.clone())?.identity;
        if !same_prefix(&current, &source) {
            return Ok(SessionStorageResponse::CacheWritten { etag: None });
        }
        let cache = Cache { source, value };
        // execute() has already checked the compact value against the contract limit.
        // A small header therefore proves the whole file fits. For unusual snapshot
        // IDs count the exact envelope, still before opening the temporary file.
        let header = Cache {
            source: &cache.source,
            value: (),
        };
        if crate::codec::encoded_bytes(&header)? > CACHE_ENVELOPE_ALLOWANCE {
            ensure!(
                crate::codec::encoded_bytes(&cache)? <= MAX_CACHE_FILE_BYTES,
                "cache value exceeds maximum"
            );
        }
        // No create_dir_all: a cache write may never resurrect a deleted session.
        let temp = dir.join(format!(".storage-cache-{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp)?;
            {
                // Compact headers stay cheap; long string runs bypass the buffer.
                let mut writer = BufWriter::with_capacity(1024, &file);
                serde_json::to_writer(&mut writer, &cache)?;
                writer.flush()?;
            }
            file.sync_data()?;
            ensure!(
                identity::resolve_session_identity(&dir)?.session_id == session.session_id,
                "cache target identity changed"
            );
            let destination = path(&dir, &key);
            std::fs::rename(&temp, &destination)?;
            Ok::<_, anyhow::Error>(destination)
        })();
        let destination = match result {
            Ok(destination) => destination,
            Err(_) => {
                let _ = std::fs::remove_file(&temp);
                return Ok(SessionStorageResponse::CacheWritten { etag: None });
            }
        };
        evict(&dir, &destination)?;
        Ok(SessionStorageResponse::CacheWritten {
            etag: Some(storage_value_etag(&cache.value)?),
        })
    }
}

const CACHE_QUOTA_BYTES: u64 = 128 * 1024 * 1024;

fn is_cache_name(name: &std::ffi::OsStr) -> bool {
    // These are ASCII boundaries on the native string; no Unicode display
    // conversion is needed to classify a cache document.
    let bytes = name.as_encoded_bytes();
    bytes.starts_with(b".storage-cache-") && bytes.ends_with(b".json")
}

/// Quota applies across all content-addressed generations and page keys.
fn evict(dir: &Path, keep: &Path) -> Result<()> {
    let keep_name = keep.file_name();
    let mut files = Vec::new();
    let mut total = 0_u64;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if !is_cache_name(&name) {
            continue;
        }
        let meta = entry.metadata()?;
        if !meta.is_file() {
            continue;
        }
        total = total.saturating_add(meta.len());
        if Some(name.as_os_str()) != keep_name {
            files.push((meta.modified()?, name, meta.len()));
        }
    }
    if total <= CACHE_QUOTA_BYTES {
        return Ok(());
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    for (_, name, bytes) in files {
        if total <= CACHE_QUOTA_BYTES {
            break;
        }
        match std::fs::remove_file(dir.join(name)) {
            Ok(()) => total = total.saturating_sub(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
