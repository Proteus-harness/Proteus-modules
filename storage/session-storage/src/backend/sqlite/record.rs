//! Self-contained SQLite rows with the explicit durable payload representation.
use crate::types::{JournalEntry, JournalKind, JournalRecord};
use proteus_contracts::domain::{ExecutionId, RecordId, SessionId, ThreadId, TurnId};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: u32,
    record_id: RecordId,
    session_seq: u64,
    timestamp_ms: i64,
    session_id: SessionId,
    execution_id: Option<ExecutionId>,
    thread_id: Option<ThreadId>,
    turn_id: Option<TurnId>,
    entry: Entry,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    kind: JournalKind,
    payload: serde_json::Value,
}

pub(super) fn encode(record: &JournalRecord) -> serde_json::Result<Vec<u8>> {
    if !matches!(&record.entry, JournalEntry::HookInvoked(_)) {
        return serde_json::to_vec(record);
    }
    serde_json::to_vec(&Record {
        schema_version: record.schema_version,
        record_id: record.record_id,
        session_seq: record.session_seq,
        timestamp_ms: record.timestamp_ms,
        session_id: record.session_id,
        execution_id: record.execution_id,
        thread_id: record.thread_id,
        turn_id: record.turn_id,
        entry: Entry {
            kind: record.entry.kind(),
            payload: record.entry.stored_payload_value()?,
        },
    })
}

pub(super) fn decode(bytes: &[u8]) -> serde_json::Result<JournalRecord> {
    let canonical_error = match serde_json::from_slice::<JournalRecord>(bytes) {
        Ok(record) => return Ok(record),
        Err(error) => error,
    };
    let record: Record = serde_json::from_slice(bytes)?;
    if record.entry.kind != JournalKind::HookInvoked
        || record.entry.payload.get("encoding").is_none()
    {
        return Err(canonical_error);
    }
    Ok(JournalRecord {
        schema_version: record.schema_version,
        record_id: record.record_id,
        session_seq: record.session_seq,
        timestamp_ms: record.timestamp_ms,
        session_id: record.session_id,
        execution_id: record.execution_id,
        thread_id: record.thread_id,
        turn_id: record.turn_id,
        entry: JournalEntry::from_stored_kind_and_payload(record.entry.kind, record.entry.payload)?,
    })
}
