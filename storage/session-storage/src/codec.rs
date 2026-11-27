//! Schema-21 JSONL encoding. Kept byte-format compatible with existing sessions.
use crate::{
    io::{MAX_PAYLOAD_BYTES, atomic_write, bounded_read, bounded_read_with_size_hint},
    types::*,
};
use anyhow::{Context, Result, bail, ensure};
use proteus_contracts::domain::{ExecutionId, RecordId, SessionId, ThreadId, TurnId};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

mod payload;

pub(crate) const JOURNAL_FILE: &str = "journal.jsonl";
pub(crate) const DEFAULT_BLOB_THRESHOLD: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredRecord {
    schema_version: u32,
    record_id: RecordId,
    session_seq: u64,
    timestamp_ms: i64,
    session_id: SessionId,
    execution_id: Option<ExecutionId>,
    thread_id: Option<ThreadId>,
    turn_id: Option<TurnId>,
    kind: JournalKind,
    #[serde(deserialize_with = "payload::read")]
    payload: StoredPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "storage", rename_all = "snake_case", deny_unknown_fields)]
enum StoredPayload {
    Inline {
        value: serde_json::Value,
    },
    Blob {
        sha256: String,
        bytes: u64,
        relative_path: PathBuf,
    },
}

pub(crate) fn canonicalize_entry(entry: JournalEntry) -> Result<JournalEntry> {
    let kind = entry.kind();
    let mut payload = entry.payload_value()?;
    drop(entry);
    proteus_session_semantics::redact_sensitive_values(&mut payload);
    ensure!(
        encoded_bytes(&payload)? <= MAX_PAYLOAD_BYTES,
        "journal payload exceeds maximum"
    );
    JournalEntry::from_kind_and_payload(kind, payload).map_err(Into::into)
}

pub(crate) fn encode(
    session: &Path,
    record: &JournalRecord,
    blob_threshold: usize,
) -> Result<(Vec<u8>, Vec<PathBuf>)> {
    let kind = record.entry.kind();
    let value = record.entry.stored_payload_value()?;
    let bytes = serde_json::to_vec(&value)?;
    ensure!(
        bytes.len() <= MAX_PAYLOAD_BYTES,
        "journal payload exceeds maximum"
    );
    let mut blobs = Vec::new();
    let payload = if bytes.len() < blob_threshold {
        StoredPayload::Inline { value }
    } else {
        let identity = crate::blob_codec::BlobIdentity::new(&bytes)?;
        let name = PathBuf::from(format!("{}.json", identity.sha256()));
        let relative_path = Path::new("blobs").join(&name);
        let path = session.join(&relative_path);
        blobs = if path.try_exists()? {
            let stored = bounded_read(&path, MAX_PAYLOAD_BYTES)?;
            crate::blob_codec::verified_chunk_files(session, &stored, &bytes)?
        } else {
            let packed = identity.pack(session)?;
            atomic_write(&path, packed.as_bytes())?;
            packed.chunk_files()?
        };
        blobs.push(name);
        StoredPayload::Blob {
            sha256: identity.into_sha256(),
            bytes: bytes.len() as u64,
            relative_path,
        }
    };
    let stored = StoredRecord {
        schema_version: record.schema_version,
        record_id: record.record_id,
        session_seq: record.session_seq,
        timestamp_ms: record.timestamp_ms,
        session_id: record.session_id,
        execution_id: record.execution_id,
        thread_id: record.thread_id,
        turn_id: record.turn_id,
        kind,
        payload,
    };
    let mut line = serde_json::to_vec(&stored)?;
    line.push(b'\n');
    Ok((line, blobs))
}

pub(crate) fn decode(
    session: &Path,
    session_id: SessionId,
    line: &[u8],
) -> Result<(JournalRecord, bool)> {
    decode_with_witness(session, session_id, line, None)
}

/// A small exact journal line and the descriptor from its successful decode.
/// Reuse still has to reopen and authenticate the external body.
pub(crate) struct ExternalWitness {
    line: Vec<u8>,
    sha256: String,
    bytes: u64,
    relative_path: PathBuf,
}

impl ExternalWitness {
    pub(crate) fn reauthenticate(&self, session: &Path, line: &[u8]) -> Result<bool> {
        if line != self.line {
            return Ok(false);
        }
        let stored = read_blob(session, &self.sha256, self.bytes, &self.relative_path)?;
        crate::blob_codec::verify_expected(session, &stored, &self.sha256, self.bytes)?;
        Ok(true)
    }
}

pub(crate) fn decode_with_witness(
    session: &Path,
    session_id: SessionId,
    line: &[u8],
    witness: Option<&mut Option<ExternalWitness>>,
) -> Result<(JournalRecord, bool)> {
    crate::diagnostics::decoded();
    let stored: StoredRecord = serde_json::from_slice(line).context("invalid journal record")?;
    ensure!(
        stored.schema_version == JOURNAL_SCHEMA_VERSION,
        "unsupported journal schema_version {}",
        stored.schema_version
    );
    ensure!(
        stored.session_id == session_id,
        "journal record belongs to another session"
    );
    let external = matches!(stored.payload, StoredPayload::Blob { .. });
    let entry = match stored.payload {
        StoredPayload::Inline { value } => {
            // The compact Value cannot exceed 32 times its source JSON:
            // string escapes cost at most 6 bytes per decoded byte, numbers
            // need at most 24 bytes in the current serializer, and literals
            // and structural tokens cannot grow. The whole line also bounds
            // the payload source. Larger lines keep the exact size check.
            ensure!(
                line.len() <= MAX_PAYLOAD_BYTES / 32 || encoded_bytes(&value)? <= MAX_PAYLOAD_BYTES,
                "inline payload exceeds maximum"
            );
            JournalEntry::from_stored_kind_and_payload(stored.kind, value)?
        }
        StoredPayload::Blob {
            sha256,
            bytes,
            relative_path,
        } => {
            let stored_content = read_blob(session, &sha256, bytes, &relative_path)?;
            let content =
                crate::blob_codec::unpack_expected(session, &stored_content, &sha256, bytes)?;
            let entry = JournalEntry::from_stored_kind_and_bytes(stored.kind, &content)?;
            if let Some(witness) = witness.filter(|_| line.len() <= 1024) {
                *witness = Some(ExternalWitness {
                    line: line.to_vec(),
                    sha256,
                    bytes,
                    relative_path,
                });
            }
            entry
        }
    };
    Ok((
        JournalRecord {
            schema_version: stored.schema_version,
            record_id: stored.record_id,
            session_seq: stored.session_seq,
            timestamp_ms: stored.timestamp_ms,
            session_id: stored.session_id,
            execution_id: stored.execution_id,
            thread_id: stored.thread_id,
            turn_id: stored.turn_id,
            entry,
        },
        external,
    ))
}

fn read_blob(session: &Path, sha256: &str, bytes: u64, path: &Path) -> Result<Vec<u8>> {
    validate_blob(sha256, path)?;
    ensure!(
        bytes <= MAX_PAYLOAD_BYTES as u64,
        "declared blob exceeds maximum"
    );
    bounded_read_with_size_hint(&session.join(path), MAX_PAYLOAD_BYTES, 256 * 1024)
}

fn validate_blob(hash: &str, path: &Path) -> Result<()> {
    ensure!(
        hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid blob sha256"
    );
    let mut parts = path.components();
    if let (Some(Component::Normal(dir)), Some(Component::Normal(file)), None) =
        (parts.next(), parts.next(), parts.next())
        && dir == "blobs"
        && file
            .as_encoded_bytes()
            .strip_suffix(b".json")
            .is_some_and(|name| {
                name.iter()
                    .copied()
                    .eq(hash.bytes().map(|byte| byte.to_ascii_lowercase()))
            })
    {
        return Ok(());
    }
    bail!("journal blob path must be blobs/<sha256>.json")
}

pub(crate) fn encoded_bytes(value: &impl serde::Serialize) -> Result<usize> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("encoded record size overflow"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Counter(0);
    serde_json::to_writer(&mut count, value)?;
    Ok(count.0)
}

#[cfg(test)]
#[test]
fn encoded_bytes_matches_compact_json_after_deserialization() {
    for raw in [
        r#" { "name" : "\u3053\u3093", "a": null, "name" : "é" } "#,
        r#"[1e1, -0.0, 1e-300, 18446744073709551615]"#,
        r#""\u0000\t\r\b\f\n\"\\\/😺""#,
        r#"{"empty":{},"array":[[],true,null,false]}"#,
    ] {
        let value: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(
            encoded_bytes(&value).unwrap(),
            serde_json::to_vec(&value).unwrap().len()
        );
    }
}

#[cfg(test)]
#[test]
fn fresh_and_reused_canonical_blob_have_identical_journal_bytes_and_touched_paths() {
    use crate::tests::text;
    use proteus_contracts::domain::{new_record_id, new_session_id, new_thread_id};

    let dir = tempfile::tempdir().unwrap();
    let record = JournalRecord {
        schema_version: JOURNAL_SCHEMA_VERSION,
        record_id: new_record_id(),
        session_seq: 1,
        timestamp_ms: 1234,
        session_id: new_session_id(),
        execution_id: None,
        thread_id: Some(new_thread_id()),
        turn_id: None,
        entry: JournalEntry::HistoryMutated(HistoryMutated {
            previous_revision: 0,
            new_revision: 1,
            mutation: HistoryMutationKind::Append,
            messages: vec![text(&"x".repeat(DEFAULT_BLOB_THRESHOLD))],
            compaction: None,
            tool_results: vec![],
        }),
    };
    let (line, files) = encode(dir.path(), &record, DEFAULT_BLOB_THRESHOLD).unwrap();
    let manifest_name = files.last().unwrap();
    let manifest_path = dir.path().join("blobs").join(manifest_name);
    let manifest_bytes = std::fs::read(&manifest_path).unwrap();
    let parsed_paths = crate::blob_codec::chunk_files(&manifest_bytes).unwrap();
    assert!(!parsed_paths.is_empty());
    assert_eq!(&files[..files.len() - 1], parsed_paths.as_slice());
    assert_eq!(
        encode(dir.path(), &record, DEFAULT_BLOB_THRESHOLD).unwrap(),
        (line.clone(), files)
    );
    assert_eq!(std::fs::read(manifest_path).unwrap(), manifest_bytes);
    assert_eq!(
        decode(dir.path(), record.session_id, &line).unwrap(),
        (record, true)
    );
}
