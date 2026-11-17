//! Schema-21 JSONL encoding. Kept byte-format compatible with existing sessions.
use crate::{
    io::{MAX_PAYLOAD_BYTES, atomic_write, bounded_read, sha256_hex},
    types::*,
};
use anyhow::{Context, Result, bail, ensure};
use proteus_contracts::domain::{ExecutionId, RecordId, SessionId, ThreadId, TurnId};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

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

pub(crate) fn canonicalize_entry(entry: &JournalEntry) -> Result<JournalEntry> {
    let kind = entry.kind();
    let mut payload = entry.payload_value()?;
    proteus_session_semantics::redact_sensitive_values(&mut payload);
    ensure!(
        serde_json::to_vec(&payload)?.len() <= MAX_PAYLOAD_BYTES,
        "journal payload exceeds maximum"
    );
    JournalEntry::from_kind_and_payload(kind, payload).map_err(Into::into)
}

pub(crate) fn encode(
    session: &Path,
    record: &JournalRecord,
    blob_threshold: usize,
) -> Result<Vec<u8>> {
    let kind = record.entry.kind();
    let value = record.entry.payload_value()?;
    let bytes = serde_json::to_vec(&value)?;
    ensure!(
        bytes.len() <= MAX_PAYLOAD_BYTES,
        "journal payload exceeds maximum"
    );
    let payload = if bytes.len() < blob_threshold {
        StoredPayload::Inline { value }
    } else {
        let hash = sha256_hex(&bytes);
        let relative_path = PathBuf::from("blobs").join(format!("{hash}.json"));
        let path = session.join(&relative_path);
        if path.try_exists()? {
            ensure!(
                bounded_read(&path, MAX_PAYLOAD_BYTES)? == bytes,
                "content-addressed blob collision"
            );
        } else {
            atomic_write(&path, &bytes)?;
        }
        StoredPayload::Blob {
            sha256: hash,
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
    Ok(line)
}

pub(crate) fn decode(session: &Path, session_id: SessionId, line: &[u8]) -> Result<JournalRecord> {
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
    let entry = match stored.payload {
        StoredPayload::Inline { value } => {
            ensure!(
                serde_json::to_vec(&value)?.len() <= MAX_PAYLOAD_BYTES,
                "inline payload exceeds maximum"
            );
            JournalEntry::from_kind_and_payload(stored.kind, value)?
        }
        StoredPayload::Blob {
            sha256,
            bytes,
            relative_path,
        } => {
            validate_blob(&sha256, &relative_path)?;
            ensure!(
                bytes <= MAX_PAYLOAD_BYTES as u64,
                "declared blob exceeds maximum"
            );
            let content = bounded_read(&session.join(relative_path), MAX_PAYLOAD_BYTES)?;
            ensure!(content.len() as u64 == bytes, "journal blob size mismatch");
            ensure!(sha256_hex(&content) == sha256, "journal blob hash mismatch");
            JournalEntry::from_kind_and_bytes(stored.kind, &content)?
        }
    };
    Ok(JournalRecord {
        schema_version: stored.schema_version,
        record_id: stored.record_id,
        session_seq: stored.session_seq,
        timestamp_ms: stored.timestamp_ms,
        session_id: stored.session_id,
        execution_id: stored.execution_id,
        thread_id: stored.thread_id,
        turn_id: stored.turn_id,
        entry,
    })
}

fn validate_blob(hash: &str, path: &Path) -> Result<()> {
    ensure!(
        hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid blob sha256"
    );
    let name = format!("{}.json", hash.to_ascii_lowercase());
    match path.components().collect::<Vec<_>>().as_slice() {
        [Component::Normal(dir), Component::Normal(file)]
            if *dir == "blobs" && *file == std::ffi::OsStr::new(&name) =>
        {
            Ok(())
        }
        _ => bail!("journal blob path must be blobs/<sha256>.json"),
    }
}
