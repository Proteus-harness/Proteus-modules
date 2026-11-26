//! Lossless content-defined chunks shared by large journal payloads.
use crate::io::{MAX_PAYLOAD_BYTES, atomic_write, bounded_file_eq, bounded_read_into, sha256_hex};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::{HashMap, hash_map::Entry},
    path::{Path, PathBuf},
};

const MAGIC: &[u8] = b"PROTEUS-SHARED-BLOB-1\n";
const MIN_CHUNK: usize = 16 * 1024;
const MAX_CHUNK: usize = 64 * 1024;
const MAX_CHUNKS: usize = MAX_PAYLOAD_BYTES / MIN_CHUNK + 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    bytes: usize,
    sha256: String,
    chunks: Vec<Chunk>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Chunk {
    sha256: String,
    bytes: usize,
}

const fn gear_table() -> [u64; 256] {
    let mut table = [0; 256];
    let mut index = 0;
    while index < table.len() {
        let mut value = (index as u64).wrapping_add(0x9e3779b97f4a7c15);
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        table[index] = value ^ (value >> 31);
        index += 1;
    }
    table
}
const GEAR: [u64; 256] = gear_table();

fn chunk_path(session: &Path, hash: &str) -> Result<PathBuf> {
    Ok(session.join("blobs").join(chunk_file(hash)?))
}

fn chunk_file(hash: &str) -> Result<PathBuf> {
    ensure!(
        hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid shared blob hash"
    );
    Ok(Path::new("shared").join(format!("{hash}.bin")))
}

/// Gear shifts the previous state left one bit for every byte. After 64
/// updates none of the earlier state survives. Seed only that suffix before
/// the first eligible cut; boundaries and persisted chunk hashes stay identical.
fn chunk_len(bytes: &[u8]) -> usize {
    let end = bytes.len().min(MAX_CHUNK);
    if end <= MIN_CHUNK {
        return end;
    }
    let start = MIN_CHUNK - u64::BITS as usize;
    let mut rolling = 0_u64;
    for (offset, byte) in bytes[start..end].iter().enumerate() {
        rolling = rolling.wrapping_shl(1).wrapping_add(GEAR[*byte as usize]);
        let length = start + offset + 1;
        if length >= MIN_CHUNK && rolling & 0x7fff == 0 {
            return length;
        }
    }
    end
}

/// A digest that belongs to this exact borrowed payload. Callers cannot
/// attach a digest copied from a different buffer or mutate these bytes.
pub(crate) struct BlobIdentity<'a> {
    bytes: &'a [u8],
    sha256: String,
}

impl<'a> BlobIdentity<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Result<Self> {
        ensure!(bytes.len() <= MAX_PAYLOAD_BYTES, "blob exceeds maximum");
        Ok(Self {
            bytes,
            sha256: sha256_hex(bytes),
        })
    }

    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    pub(crate) fn into_sha256(self) -> String {
        self.sha256
    }

    pub(crate) fn pack(&self, session: &Path) -> Result<PackedBlob> {
        pack_inner(session, self.bytes, Some(&self.sha256))
    }
}

/// Serialized output and, only for a manifest produced here, its own typed
/// descriptors. Other callers cannot manufacture file lists for stored bytes.
pub(crate) struct PackedBlob {
    stored: Vec<u8>,
    chunks: Option<Vec<Chunk>>,
}

impl PackedBlob {
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.stored
    }

    pub(crate) fn chunk_files(&self) -> Result<Vec<PathBuf>> {
        match &self.chunks {
            Some(chunks) => files_for_chunks(chunks),
            // Plain bytes keep the existing detection/parse path.
            None => chunk_files(&self.stored),
        }
    }
}

/// Write all shared chunks before publishing the returned manifest. Journal
/// references continue to identify the hash and size of the original bytes.
pub fn pack(session: &Path, bytes: &[u8]) -> Result<Vec<u8>> {
    Ok(pack_inner(session, bytes, None)?.stored)
}

fn pack_inner(session: &Path, bytes: &[u8], known_sha256: Option<&str>) -> Result<PackedBlob> {
    ensure!(bytes.len() <= MAX_PAYLOAD_BYTES, "blob exceeds maximum");
    if bytes.len() < MIN_CHUNK {
        return Ok(PackedBlob {
            stored: bytes.to_vec(),
            chunks: None,
        });
    }
    let mut chunks = Vec::new();
    let mut remaining = bytes;
    while !remaining.is_empty() {
        let (chunk, rest) = remaining.split_at(chunk_len(remaining));
        chunks.push(store_chunk(session, chunk)?);
        remaining = rest;
    }
    let manifest = Manifest {
        bytes: bytes.len(),
        sha256: known_sha256.map_or_else(|| sha256_hex(bytes), str::to_owned),
        chunks,
    };
    let mut packed = MAGIC.to_vec();
    serde_json::to_writer(&mut packed, &manifest)?;
    ensure!(
        packed.len() <= MAX_PAYLOAD_BYTES,
        "blob manifest exceeds maximum"
    );
    Ok(PackedBlob {
        stored: packed,
        chunks: Some(manifest.chunks),
    })
}

fn store_chunk(session: &Path, bytes: &[u8]) -> Result<Chunk> {
    let sha256 = sha256_hex(bytes);
    let path = chunk_path(session, &sha256)?;
    if path.try_exists()? {
        ensure!(
            bounded_file_eq(&path, bytes, MAX_CHUNK)?,
            "shared blob collision"
        );
    } else {
        atomic_write(&path, bytes)?;
    }
    Ok(Chunk {
        sha256,
        bytes: bytes.len(),
    })
}

/// Ordinary JSON blobs remain readable. Packed bytes are verified both at each
/// chunk and as a complete payload before a caller parses canonical records.
pub fn unpack<'a>(session: &Path, stored: &'a [u8]) -> Result<Cow<'a, [u8]>> {
    unpack_checked(session, stored, None)
}

/// The caller has already packed or verified these bytes.
pub(crate) fn chunk_files(stored: &[u8]) -> Result<Vec<PathBuf>> {
    let Some(body) = stored.strip_prefix(MAGIC) else {
        return Ok(Vec::new());
    };
    let manifest: Manifest = serde_json::from_slice(body)?;
    files_for_chunks(&manifest.chunks)
}

fn files_for_chunks(chunks: &[Chunk]) -> Result<Vec<PathBuf>> {
    chunks.iter().map(|chunk| chunk_file(&chunk.sha256)).collect()
}

pub(crate) fn unpack_expected<'a>(
    session: &Path,
    stored: &'a [u8],
    sha256: &str,
    bytes: u64,
) -> Result<Cow<'a, [u8]>> {
    unpack_checked(session, stored, Some((sha256, bytes)))
}

fn unpack_checked<'a>(
    session: &Path,
    stored: &'a [u8],
    expected: Option<(&str, u64)>,
) -> Result<Cow<'a, [u8]>> {
    ensure!(
        stored.len() <= MAX_PAYLOAD_BYTES,
        "stored blob exceeds maximum"
    );
    let Some(body) = stored.strip_prefix(MAGIC) else {
        if let Some((hash, length)) = expected {
            ensure!(stored.len() as u64 == length, "journal blob size mismatch");
            ensure!(sha256_hex(stored) == hash, "journal blob hash mismatch");
        }
        return Ok(Cow::Borrowed(stored));
    };
    let manifest: Manifest = serde_json::from_slice(body)?;
    ensure!(
        manifest.bytes <= MAX_PAYLOAD_BYTES,
        "expanded blob exceeds maximum"
    );
    ensure!(
        !manifest.chunks.is_empty() && manifest.chunks.len() <= MAX_CHUNKS,
        "invalid shared blob chunk count"
    );
    if let Some((hash, length)) = expected {
        ensure!(
            manifest.bytes as u64 == length && manifest.sha256 == hash,
            "shared manifest differs from journal blob identity"
        );
    }
    let mut bytes = Vec::with_capacity(manifest.bytes);
    // A chunk can occur many times in one payload. Index only ranges in our
    // output; a later unpack verifies its files independently.
    let mut verified = HashMap::new();
    for chunk in &manifest.chunks {
        let start = bytes.len();
        ensure!(
            chunk.bytes > 0
                && chunk.bytes <= MAX_CHUNK
                && chunk.bytes <= manifest.bytes.saturating_sub(start),
            "shared blob chunk exceeds declared payload"
        );
        let vacant = match verified.entry(chunk.sha256.as_str()) {
            Entry::Occupied(entry) => {
                let (offset, len): (usize, usize) = *entry.get();
                ensure!(len == chunk.bytes, "shared blob chunk size mismatch");
                bytes.extend_from_within(offset..offset + len);
                continue;
            }
            Entry::Vacant(entry) => entry,
        };
        let path = chunk_path(session, &chunk.sha256)?;
        bytes.resize(start + chunk.bytes, 0);
        let content = &mut bytes[start..];
        let count = bounded_read_into(&path, MAX_CHUNK, content)?;
        ensure!(count == chunk.bytes, "shared blob chunk size mismatch");
        ensure!(
            sha256_hex(content) == chunk.sha256,
            "shared blob chunk hash mismatch"
        );
        vacant.insert((start, chunk.bytes));
    }
    ensure!(bytes.len() == manifest.bytes, "shared blob size mismatch");
    ensure!(
        sha256_hex(&bytes) == manifest.sha256,
        "shared blob hash mismatch"
    );
    Ok(Cow::Owned(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> Vec<u8> {
        let mut state = 7_u64;
        (0..1024 * 1024)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                b'a' + (state % 26) as u8
            })
            .collect()
    }

    #[test]
    fn chunk_boundaries_match_the_persisted_layout() {
        // All zeroes have no natural cut. The two final bytes below make
        // the original Gear accumulator reach the cut mask exactly at MIN.
        let mut fixed = vec![0; MAX_CHUNK + 1];
        for len in [1, MIN_CHUNK - 1, MIN_CHUNK, MIN_CHUNK + 1, MAX_CHUNK + 1] {
            assert_eq!(chunk_len(&fixed[..len]), len.min(MAX_CHUNK));
        }
        fixed[MIN_CHUNK - 2..MIN_CHUNK].copy_from_slice(&[227, 180]);
        assert_eq!(chunk_len(&fixed), MIN_CHUNK);
        // Golden lengths from the original full scan, including a forced
        // maximum, natural cuts after resets, and the final short remainder.
        let mut state = 7_u64;
        let bytes: Vec<_> = (0..4 * MAX_CHUNK + 73)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let mut remaining = &bytes[..];
        for expected in [33240, 65536, 19642, 26104, 25984, 50094, 41617] {
            assert_eq!(chunk_len(remaining), expected);
            remaining = &remaining[expected..];
        }
        assert!(remaining.is_empty());
    }

    #[test]
    fn repeated_chunk_ranges_validate_lengths_and_are_local_to_one_unpack() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = [vec![0; MAX_CHUNK], vec![1; MAX_CHUNK], vec![0; MAX_CHUNK]].concat();
        let packed = pack(dir.path(), &bytes).unwrap();
        let mut manifest: Manifest = serde_json::from_slice(&packed[MAGIC.len()..]).unwrap();
        assert_eq!(manifest.chunks.len(), 3);
        assert_eq!(manifest.chunks[0].sha256, manifest.chunks[2].sha256);
        assert_eq!(unpack(dir.path(), &packed).unwrap(), bytes);
        manifest.chunks[2].bytes -= 1;
        let mut invalid = MAGIC.to_vec();
        serde_json::to_writer(&mut invalid, &manifest).unwrap();
        assert!(
            unpack(dir.path(), &invalid)
                .unwrap_err()
                .to_string()
                .contains("chunk size")
        );
        let path = chunk_path(dir.path(), &manifest.chunks[0].sha256).unwrap();
        std::fs::write(path, vec![2; MAX_CHUNK]).unwrap();
        assert!(unpack(dir.path(), &packed).is_err());
    }

    #[test]
    fn fresh_pack_file_list_matches_parsed_order_and_keeps_repeated_paths() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = [vec![0; MAX_CHUNK], vec![1; MAX_CHUNK], vec![0; MAX_CHUNK]].concat();
        let packed = BlobIdentity::new(&bytes).unwrap().pack(dir.path()).unwrap();
        let paths = packed.chunk_files().unwrap();
        assert_eq!(paths, chunk_files(packed.as_bytes()).unwrap());
        assert_eq!(paths.len(), 3);
        assert_eq!(paths[0], paths[2]);
        assert_ne!(paths[0], paths[1]);
        assert_eq!(unpack(dir.path(), packed.as_bytes()).unwrap(), bytes);

        let legacy = br#"{"stock":true}"#;
        let raw = BlobIdentity::new(legacy).unwrap().pack(dir.path()).unwrap();
        assert_eq!(raw.as_bytes(), legacy);
        assert!(raw.chunk_files().unwrap().is_empty());
        assert_eq!(raw.chunk_files().unwrap(), chunk_files(legacy).unwrap());
    }

    #[test]
    fn shifted_payloads_share_chunks_and_restore_exact_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let original = payload();
        let mut changed = b"new prefix".to_vec();
        changed.extend_from_slice(&original);
        for bytes in [&original, &changed] {
            let packed = pack(dir.path(), bytes).unwrap();
            assert_eq!(unpack(dir.path(), &packed).unwrap(), *bytes);
        }
        let total: u64 = std::fs::read_dir(dir.path().join("blobs/shared"))
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum();
        assert!(
            total < original.len() as u64 * 3 / 2,
            "shared bytes {total}"
        );
        assert_eq!(
            unpack(dir.path(), br#"{"old":true}"#).unwrap().as_ref(),
            br#"{"old":true}"#
        );
    }

    #[test]
    fn changed_or_missing_chunks_and_expansion_limits_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let packed = pack(dir.path(), &payload()).unwrap();
        let mut manifest: Manifest = serde_json::from_slice(&packed[MAGIC.len()..]).unwrap();
        let chunk = chunk_path(dir.path(), &manifest.chunks[0].sha256).unwrap();
        let original = std::fs::read(&chunk).unwrap();
        std::fs::write(&chunk, vec![b'!'; original.len()]).unwrap();
        assert!(unpack(dir.path(), &packed).is_err());
        std::fs::remove_file(chunk).unwrap();
        assert!(unpack(dir.path(), &packed).is_err());
        manifest.bytes = MAX_PAYLOAD_BYTES + 1;
        let mut oversized = MAGIC.to_vec();
        serde_json::to_writer(&mut oversized, &manifest).unwrap();
        assert!(unpack(dir.path(), &oversized).is_err());
    }
}
