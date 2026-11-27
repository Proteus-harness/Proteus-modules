//! A disposable catalog index over a previously validated journal and blobs.
use super::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    io::{self, BufWriter, Write},
};

const FILE: &str = ".storage-catalog-v1.json";
const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_BLOBS: usize = 16_384;
const STREAM_CHECKSUM_AT_CAPACITY: usize = 64 * 1024;
const CHECKSUM_BUFFER_BYTES: usize = 4 * 1024;
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
    saw_mismatch: bool,
    unexpected: BTreeSet<PathBuf>,
}

impl<'a> BlobStampMatch<'a> {
    fn new(expected: &'a BlobStamps) -> Self {
        Self {
            paths: expected.iter().map(|(p, s)| (p, s, None)).collect(),
            seen_expected: 0,
            saw_mismatch: false,
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
                let matches = **expected == stamp;
                self.saw_mismatch |= !matches;
                *last = Some(matches);
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
        // If all paths were observed and none ever differed, the last stamp
        // matched for every key. A changing directory can repeat a path, so
        // after any disagreement use the same last-observation rule as before.
        self.unexpected.is_empty()
            && self.seen_expected == self.paths.len()
            && (!self.saw_mismatch || self.paths.iter().all(|(_, _, last)| *last == Some(true)))
    }
}

pub(in crate::service) fn matches_blob_stamps(dir: &Path, expected: &BlobStamps) -> Option<bool> {
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
        let mut metadata = [0; 56];
        metadata[..8].copy_from_slice(&stamp.len.to_le_bytes());
        metadata[8..24].copy_from_slice(&stamp.modified.to_le_bytes());
        metadata[24..32].copy_from_slice(&stamp.device.to_le_bytes());
        metadata[32..40].copy_from_slice(&stamp.inode.to_le_bytes());
        metadata[40..48].copy_from_slice(&stamp.changed.0.to_le_bytes());
        metadata[48..].copy_from_slice(&stamp.changed.1.to_le_bytes());
        hash.update(&metadata);
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

struct DigestWriter(ring::digest::Context);

impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn checksum(mut bytes: Vec<u8>, index: &Index) -> Option<String> {
    if bytes.capacity() < STREAM_CHECKSUM_AT_CAPACITY {
        bytes.clear();
        serde_json::to_writer(&mut bytes, index).ok()?;
        return Some(crate::io::sha256_hex(&bytes));
    }

    // The parsed index owns its data. Hash the same compact encoding without
    // retaining the large read buffer or growing a whole canonical byte copy.
    drop(bytes);
    let mut writer = BufWriter::with_capacity(
        CHECKSUM_BUFFER_BYTES,
        DigestWriter(ring::digest::Context::new(&ring::digest::SHA256)),
    );
    serde_json::to_writer(&mut writer, index).ok()?;
    let writer = writer.into_inner().ok()?;
    Some(crate::io::digest_hex(writer.0.finish()))
}

pub(super) fn load(
    dir: &Path,
    descriptor: &StorageSessionDescriptor,
    stamps: &SourceStamp,
) -> Option<(StorageSessionSummary, super::super::views::PrefixCache)> {
    let bytes = crate::io::bounded_read(&dir.join(FILE), MAX_BYTES).ok()?;
    let Envelope {
        checksum: expected,
        data: index,
    } = serde_json::from_slice(&bytes).ok()?;
    if expected != checksum(bytes, &index)?
        || index.summary.descriptor.session != descriptor.session
        || &index.stamps != stamps
        || !matches_blob_stamps(dir, &index.blobs)?
        || source_stamp_items(dir, journal_kind(descriptor)?)
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
            source_stamp_items(dir, kind)?.as_slice() == prefix.stamps.as_slice(),
            "catalog source changed"
        );
        ensure!(
            matches_blob_stamps(dir, &blobs) == Some(true),
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
        fixture_for(BackendKind::Jsonl)
    }

    fn fixture_for(kind: BackendKind) -> (crate::tests::Fixture, Index) {
        let mut f = crate::tests::Fixture::new(kind);
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
    fn catalog_checksum_matches_compact_index_across_scratch_threshold() {
        let (_f, mut data) = fixture();
        let stamp = data.stamps.iter().flatten().next().unwrap().clone();
        for item in 0..512 {
            data.blobs.insert(
                PathBuf::from(format!("parts/{item:064x}.json")),
                stamp.clone(),
            );
        }
        data.blobs.insert(PathBuf::from("parts/Ω \"abc\""), stamp);
        let canonical = serde_json::to_vec(&data).unwrap();
        assert!(canonical.len() >= STREAM_CHECKSUM_AT_CAPACITY);
        let expected = crate::io::sha256_hex(&canonical);
        assert_eq!(
            checksum(Vec::new(), &data).as_deref(),
            Some(expected.as_str())
        );
        let retained = Vec::with_capacity(STREAM_CHECKSUM_AT_CAPACITY);
        assert_eq!(checksum(retained, &data).as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn larger_noncompact_catalog_keeps_the_same_typed_checksum_and_read_only_load() {
        for kind in crate::tests::backends() {
            let (f, data) = fixture_for(kind);
            let dir = f.descriptor.local_path.as_ref().unwrap();
            let descriptor = &data.summary.descriptor;
            let bound = &data.stamps;
            let (stock, prefix) = load(dir, descriptor, bound).unwrap();
            let path = dir.join(FILE);
            let compact = std::fs::read(&path).unwrap();
            assert!(compact.len() < STREAM_CHECKSUM_AT_CAPACITY);
            assert_eq!(encode(&data).unwrap(), compact);

            // Larger input spelling of the same envelope, not a different checksum.
            let mut spaced = compact;
            spaced.resize(STREAM_CHECKSUM_AT_CAPACITY + 1, b' ');
            std::fs::write(&path, &spaced).unwrap();
            let (summary, observed) = load(dir, descriptor, bound).unwrap();
            assert_eq!(summary, stock);
            assert!(observed.stamps == prefix.stamps);
            assert_eq!(observed.revision, prefix.revision);
            assert_eq!(observed.hint.extent, prefix.hint.extent);
            assert_eq!(observed.hint.fingerprint, prefix.hint.fingerprint);
            assert_eq!(observed.blob_digest, prefix.blob_digest);
            assert_eq!(std::fs::read(&path).unwrap(), spaced);

            let mut wrong: Envelope = serde_json::from_slice(&spaced).unwrap();
            let replacement = if wrong.checksum.starts_with('0') { "1" } else { "0" };
            wrong.checksum.replace_range(..1, replacement);
            let mut bytes = serde_json::to_vec(&wrong).unwrap();
            bytes.resize(STREAM_CHECKSUM_AT_CAPACITY + 1, b' ');
            std::fs::write(&path, &bytes).unwrap();
            assert!(load(dir, descriptor, bound).is_none());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
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

    #[test]
    fn catalog_save_rechecks_inventory_without_replacing_a_rejected_sidecar() {
        let mut f = crate::tests::Fixture::new(BackendKind::Jsonl);
        f.append(vec![crate::tests::text(
            &"x".repeat(crate::codec::DEFAULT_BLOB_THRESHOLD + 1024),
        )]);
        let dir = f.descriptor.local_path.as_ref().unwrap();
        let summary = f.service.catalog_summary(dir, false).unwrap();
        let sidecar = dir.join(FILE);
        let expected = std::fs::read(&sidecar).unwrap();
        let envelope: Envelope = serde_json::from_slice(&expected).unwrap();
        let (_, prefix) = load(dir, &summary.descriptor, &envelope.data.stamps).unwrap();
        let inventory = envelope.data.blobs;
        assert!(!inventory.is_empty());
        let root = dir.join("blobs");
        let expected_leaf = inventory
            .keys()
            .map(|path| root.join(path))
            .find(|path| path.is_file())
            .unwrap();
        let journal = summary.descriptor.record_location.as_ref().unwrap();
        let original_journal = std::fs::read(journal).unwrap();

        assert_eq!(matches_blob_stamps(dir, &inventory), Some(true));
        std::fs::remove_file(&sidecar).unwrap();
        save(dir, &summary, &prefix, inventory.clone());
        assert_eq!(std::fs::read(&sidecar).unwrap(), expected);

        let check_rejected = |outcome| {
            assert_eq!(matches_blob_stamps(dir, &inventory), outcome);
            assert_eq!(
                blob_stamps(dir).map(|observed| observed == inventory),
                outcome
            );
            assert!(source_stamp(dir, BackendKind::Jsonl).unwrap() == prefix.stamps);
            save(dir, &summary, &prefix, inventory.clone());
            assert_eq!(std::fs::read(&sidecar).unwrap(), expected);
            assert_eq!(std::fs::read(journal).unwrap(), original_journal);
        };

        // A well-formed observation disagrees with the prior full stamp.
        std::fs::write(&expected_leaf, b"changed owned blob fixture").unwrap();
        check_rejected(Some(false));

        // No observed paths cannot satisfy this nonempty expected inventory.
        std::fs::rename(&root, dir.join("parked-fixture-blobs")).unwrap();
        check_rejected(Some(false));

        // A blob root that is no longer a directory fails the walk, not merely
        // the comparison; save declines with the old file still present.
        std::fs::write(&root, b"not a directory").unwrap();
        check_rejected(None);
    }
}
