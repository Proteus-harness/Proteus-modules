//! A disposable catalog index over a previously validated journal and blobs.
use super::*;
use std::{collections::BTreeMap, fs::OpenOptions, io::Write};

const FILE: &str = ".storage-catalog-v1.json";
const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_BLOBS: usize = 16_384;
type BlobStamps = BTreeMap<PathBuf, FileStamp>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    summary: StorageSessionSummary,
    stamps: SourceStamp,
    blobs: BlobStamps,
    revision: StorageRevision,
    extent: u64,
    fingerprint: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    checksum: String,
    data: Index,
}

pub(super) fn blob_stamps(dir: &Path) -> Option<BlobStamps> {
    let root = dir.join("blobs");
    let mut stamps = BTreeMap::new();
    let mut pending = vec![(PathBuf::new(), 0)];
    while let Some((relative, depth)) = pending.pop() {
        let entries = match std::fs::read_dir(root.join(&relative)) {
            Ok(entries) => entries,
            Err(error) if depth == 0 && error.kind() == std::io::ErrorKind::NotFound => {
                return Some(BTreeMap::new());
            }
            Err(_) => return None,
        };
        for entry in entries {
            let entry = entry.ok()?;
            if stamps.len() >= MAX_BLOBS {
                return None;
            }
            let meta = std::fs::symlink_metadata(entry.path()).ok()?;
            let name = relative.join(entry.file_name());
            if meta.is_dir() && depth < 16 {
                pending.push((name.clone(), depth + 1));
            } else if !meta.is_file() {
                return None;
            }
            stamps.insert(name, file_stamp(&meta).ok()?);
        }
    }
    Some(stamps)
}

pub(super) fn load(
    dir: &Path,
    descriptor: &StorageSessionDescriptor,
    stamps: &SourceStamp,
) -> Option<(StorageSessionSummary, super::super::views::PrefixCache)> {
    let bytes = crate::io::bounded_read(&dir.join(FILE), MAX_BYTES).ok()?;
    let envelope: Envelope = serde_json::from_slice(&bytes).ok()?;
    let index = envelope.data;
    if envelope.checksum != crate::io::sha256_hex(&serde_json::to_vec(&index).ok()?)
        || index.summary.descriptor.session != descriptor.session
        || &index.stamps != stamps
        || blob_stamps(dir)? != index.blobs
        || source_stamp(dir, journal_kind(descriptor)?)
            .ok()?
            .as_slice()
            != stamps.as_slice()
        || index.fingerprint.len() != 64
        || !index
            .fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let prefix = super::super::views::PrefixCache {
        stamps: index.stamps,
        revision: index.revision,
        hint: crate::backend::PrefixHint {
            extent: index.extent,
            fingerprint: index.fingerprint,
        },
    };
    let mut summary = index.summary;
    summary.descriptor = descriptor.clone();
    Some((summary, prefix))
}

fn journal_kind(descriptor: &StorageSessionDescriptor) -> Option<BackendKind> {
    match descriptor.record_location.as_ref()?.file_name()?.to_str()? {
        "journal.jsonl" => Some(BackendKind::Jsonl),
        "journal.sqlite" => Some(BackendKind::Sqlite),
        _ => None,
    }
}

pub(super) fn save(
    dir: &Path,
    summary: &StorageSessionSummary,
    prefix: &super::super::views::PrefixCache,
    blobs: BlobStamps,
) {
    let result = (|| -> Result<()> {
        let kind = journal_kind(&summary.descriptor).context("catalog journal kind")?;
        // A concurrent append or payload replacement makes this generation unusable.
        ensure!(
            source_stamp(dir, kind)? == prefix.stamps,
            "catalog source changed"
        );
        ensure!(
            blob_stamps(dir).as_ref() == Some(&blobs),
            "catalog blobs changed"
        );
        let data = Index {
            summary: summary.clone(),
            stamps: prefix.stamps.clone(),
            blobs,
            revision: prefix.revision,
            extent: prefix.hint.extent,
            fingerprint: prefix.hint.fingerprint.clone(),
        };
        let checksum = crate::io::sha256_hex(&serde_json::to_vec(&data)?);
        let bytes = serde_json::to_vec(&Envelope { checksum, data })?;
        ensure!(bytes.len() <= MAX_BYTES, "catalog index exceeds maximum");
        let temporary = dir.join(format!(".storage-catalog-{}.tmp", Uuid::new_v4()));
        let write = (|| -> Result<()> {
            // Never create a session directory from a cache write.
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_data()?;
            std::fs::rename(&temporary, dir.join(FILE))?;
            Ok(())
        })();
        if write.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        write
    })();
    let _ = result;
}
