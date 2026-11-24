//! A disposable catalog index over a previously validated journal and blobs.
use super::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    io::Write,
};

const FILE: &str = ".storage-catalog-v1.json";
const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_BLOBS: usize = 16_384;
pub(in crate::service) type BlobStamps = BTreeMap<PathBuf, FileStamp>;
pub(in crate::service) type BlobStampDigest = [u8; 32];

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

pub(in crate::service) fn blob_stamps(dir: &Path) -> Option<BlobStamps> {
    let mut stamps = BTreeMap::new();
    walk_blob_stamps(dir, |name, stamp| {
        stamps.insert(name.to_owned(), stamp);
        stamps.len()
    })?;
    Some(stamps)
}

// The observer returns the number of distinct paths seen, not the number of
// directory entries. A changing directory can report a name more than once.
fn walk_blob_stamps(dir: &Path, mut observe: impl FnMut(&Path, FileStamp) -> usize) -> Option<()> {
    let root = dir.join("blobs");
    let mut observed = 0;
    let mut pending = vec![(PathBuf::new(), 0)];
    while let Some((mut relative, depth)) = pending.pop() {
        let mut path = root.join(&relative);
        let entries = match std::fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) if depth == 0 && error.kind() == std::io::ErrorKind::NotFound => {
                return Some(());
            }
            Err(_) => return None,
        };
        for entry in entries {
            let entry = entry.ok()?;
            if observed >= MAX_BLOBS {
                return None;
            }
            let leaf = entry.file_name();
            // Keep the pathname-based check, but reuse its directory buffer.
            path.push(&leaf);
            let meta = std::fs::symlink_metadata(&path).ok()?;
            path.pop();
            // The comparator borrows this reusable relative name; only the
            // collecting observer needs a separate allocation for each path.
            relative.push(leaf);
            if meta.is_dir() && depth < 16 {
                pending.push((relative.clone(), depth + 1));
            } else if !meta.is_file() {
                return None;
            }
            observed = observe(&relative, file_stamp(&meta).ok()?);
            relative.pop();
        }
    }
    Some(())
}

struct BlobStampMatch<'a> {
    // Index keys are already sorted under the same Path ordering. Borrow
    // their stamps and keep only the outcome of the last observation.
    paths: Vec<(&'a PathBuf, &'a FileStamp, Option<bool>)>,
    seen_expected: usize,
    unexpected: BTreeSet<PathBuf>,
}

impl<'a> BlobStampMatch<'a> {
    fn new(expected: &'a BlobStamps) -> Self {
        Self {
            paths: expected.iter().map(|(p, s)| (p, s, None)).collect(),
            seen_expected: 0,
            unexpected: BTreeSet::new(),
        }
    }

    fn observe(&mut self, name: &Path, stamp: FileStamp) -> usize {
        match self
            .paths
            .binary_search_by(|(p, _, _)| p.as_path().cmp(name))
        {
            Ok(index) => {
                let (_, expected, last) = &mut self.paths[index];
                self.seen_expected += usize::from(last.is_none());
                *last = Some(**expected == stamp);
            }
            Err(_) => {
                // Keep distinct unexpected paths too, so the walk hits the
                // same cap at the same entry even for an invalid catalog.
                self.unexpected.insert(name.to_owned());
            }
        }
        self.seen_expected + self.unexpected.len()
    }

    fn matches(&self) -> bool {
        self.unexpected.is_empty() && self.paths.iter().all(|(_, _, last)| *last == Some(true))
    }
}

fn matches_blob_stamps(dir: &Path, expected: &BlobStamps) -> Option<bool> {
    let mut seen = BlobStampMatch::new(expected);
    walk_blob_stamps(dir, |name, stamp| seen.observe(name, stamp))?;
    Some(seen.matches())
}

/// Keep only a fixed-size proof in the in-process catalog, including for
/// archives with thousands of payload files.
pub(in crate::service) fn blob_stamp_digest(stamps: &BlobStamps) -> BlobStampDigest {
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    for (path, stamp) in stamps {
        let path = path.as_os_str().as_encoded_bytes();
        hash.update(&(path.len() as u64).to_le_bytes());
        hash.update(path);
        hash.update(&stamp.len.to_le_bytes());
        hash.update(&stamp.modified.to_le_bytes());
        hash.update(&stamp.device.to_le_bytes());
        hash.update(&stamp.inode.to_le_bytes());
        hash.update(&stamp.changed.0.to_le_bytes());
        hash.update(&stamp.changed.1.to_le_bytes());
    }
    hash.finish()
        .as_ref()
        .try_into()
        .expect("SHA-256 digest length")
}

pub(super) fn update_blobs(
    dir: &Path,
    stamps: &mut BlobStamps,
    touched: Vec<PathBuf>,
) -> Option<()> {
    let root = dir.join("blobs");
    for file in touched {
        for path in file.ancestors().filter(|p| !p.as_os_str().is_empty()) {
            let meta = std::fs::symlink_metadata(root.join(path)).ok()?;
            let expected_type = if path == file {
                meta.is_file()
            } else {
                meta.is_dir()
            };
            if !expected_type {
                return None;
            }
            stamps.insert(path.to_owned(), file_stamp(&meta).ok()?);
        }
        if stamps.len() > MAX_BLOBS {
            return None;
        }
    }
    Some(())
}

pub(super) fn load(
    dir: &Path,
    descriptor: &StorageSessionDescriptor,
    stamps: &SourceStamp,
) -> Option<(StorageSessionSummary, super::super::views::PrefixCache)> {
    let mut bytes = crate::io::bounded_read(&dir.join(FILE), MAX_BYTES).ok()?;
    let Envelope {
        checksum,
        data: index,
    } = serde_json::from_slice(&bytes).ok()?;
    // The envelope owns its data. Reuse the read buffer for the canonical JSON,
    // which can differ from the JSON spelling accepted by the parser.
    bytes.clear();
    serde_json::to_writer(&mut bytes, &index).ok()?;
    let digest = crate::io::sha256_hex(&bytes);
    drop(bytes);
    if checksum != digest
        || index.summary.descriptor.session != descriptor.session
        || &index.stamps != stamps
        || !matches_blob_stamps(dir, &index.blobs)?
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
        blob_digest: Some(blob_stamp_digest(&index.blobs)),
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

/// Write the compact v1 envelope in field order, leaving room to backfill
/// the lowercase SHA-256 of exactly the serialized `data` value.
fn encode(data: &Index) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(128);
    bytes.extend_from_slice(br#"{"checksum":""#);
    let checksum = bytes.len()..bytes.len() + 64;
    bytes.resize(checksum.end, b'0');
    bytes.extend_from_slice(br#"","data":"#);
    let data_start = bytes.len();
    serde_json::to_writer(&mut bytes, data)?;
    let hash = crate::io::sha256_hex(&bytes[data_start..]);
    bytes[checksum].copy_from_slice(hash.as_bytes());
    bytes.push(b'}');
    ensure!(bytes.len() <= MAX_BYTES, "catalog index exceeds maximum");
    Ok(bytes)
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
        let bytes = encode(&Index {
            summary: summary.clone(),
            stamps: prefix.stamps.clone(),
            blobs,
            revision: prefix.revision,
            extent: prefix.hint.extent,
            fingerprint: prefix.hint.fingerprint.clone(),
        })?;
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
            crate::io::sync_directory(dir)?;
            Ok(())
        })();
        if write.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        write
    })();
    let _ = result;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (crate::tests::Fixture, Index) {
        let mut f = crate::tests::Fixture::new(BackendKind::Jsonl);
        f.append(vec![crate::tests::text(
            "quote: \" / slash: \\ / line: \n / Ёж 🦀",
        )]);
        let dir = f.descriptor.local_path.as_ref().unwrap();
        f.service.catalog_summary(dir, false).unwrap();
        let bytes = std::fs::read(dir.join(FILE)).unwrap();
        let envelope: Envelope = serde_json::from_slice(&bytes).unwrap();
        (f, envelope.data)
    }

    #[test]
    fn encoded_v1_matches_the_typed_envelope_byte_for_byte() {
        let (_f, mut data) = fixture();
        let stamp = data.stamps.iter().flatten().next().unwrap().clone();
        data.blobs.insert(PathBuf::from("blobs/Ёж \"abc\""), stamp);
        let actual = encode(&data).unwrap();
        let checksum = crate::io::sha256_hex(&serde_json::to_vec(&data).unwrap());
        let expected = serde_json::to_vec(&Envelope { checksum, data }).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn limit_includes_the_entire_catalog_envelope() {
        let (_f, mut data) = fixture();
        data.summary.preview = Some(String::new());
        let padding = MAX_BYTES - encode(&data).unwrap().len();
        data.summary.preview = Some("x".repeat(padding));
        assert_eq!(encode(&data).unwrap().len(), MAX_BYTES);
        data.summary.preview.as_mut().unwrap().push('x');
        assert_eq!(
            encode(&data).unwrap_err().to_string(),
            "catalog index exceeds maximum"
        );
    }
}
