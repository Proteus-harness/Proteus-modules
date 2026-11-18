use super::*;
use crate::backend::{JournalBackend, Jsonl, Sqlite};
use proteus_contracts::contracts::{
    ContextPreparationInput, ContextPreparationOutput, ExecutionAttribution, HookEvent, HookInput,
    HookTrace, OperationOwner,
};

fn hook_record() -> JournalRecord {
    let messages = vec![text(&"bound history ".repeat(3000))];
    let event = HookEvent::BeforeContext {
        input: ContextPreparationInput::new(messages.clone(), None),
        output: ContextPreparationOutput::unchanged(messages),
        model_capabilities: None,
        target: None,
    };
    JournalRecord {
        schema_version: JOURNAL_SCHEMA_VERSION,
        record_id: new_record_id(),
        session_seq: 1,
        timestamp_ms: 1234,
        session_id: new_session_id(),
        execution_id: None,
        thread_id: None,
        turn_id: None,
        entry: JournalEntry::HookInvoked(HookTrace {
            owner: OperationOwner::Workflow,
            input: HookInput {
                event: event.clone(),
                attribution: ExecutionAttribution::detached(
                    proteus_contracts::domain::new_execution_id(),
                ),
                cwd: ".".into(),
                conversation: None,
            },
            steps: Vec::new(),
            output: Some(event),
        }),
    }
}

#[test]
fn jsonl_persists_compact_hooks_inline_or_in_blobs_and_reads_old_inline_trace() {
    let dir = tempfile::tempdir().unwrap();
    let record = hook_record();
    for threshold in [0, usize::MAX] {
        let (line, _) = crate::codec::encode(dir.path(), &record, threshold).unwrap();
        assert_eq!(
            crate::codec::decode(dir.path(), record.session_id, &line).unwrap(),
            record
        );
        if threshold == usize::MAX {
            let mut old: serde_json::Value = serde_json::from_slice(&line).unwrap();
            assert_eq!(
                old["payload"]["value"]["encoding"],
                "shared_hook_messages_v1"
            );
            old["payload"]["value"] = record.entry.payload_value().unwrap();
            assert_eq!(
                crate::codec::decode(
                    dir.path(),
                    record.session_id,
                    &serde_json::to_vec(&old).unwrap()
                )
                .unwrap(),
                record
            );
        }
    }
}

#[test]
fn both_backends_roundtrip_compact_hook_records_and_keep_sqlite_self_contained() {
    for kind in backends() {
        let dir = tempfile::tempdir().unwrap();
        let record = hook_record();
        let mut backend: Box<dyn JournalBackend> = match kind {
            BackendKind::Jsonl => Box::new(Jsonl::new()),
            BackendKind::Sqlite => Box::new(Sqlite::default()),
        };
        backend.recover(dir.path(), record.session_id).unwrap();
        backend.append(dir.path(), &record).unwrap();
        let mut snapshot = backend.snapshot(dir.path(), record.session_id).unwrap();
        assert_eq!(snapshot.next().unwrap(), Some(record.clone()));
        assert_eq!(snapshot.next().unwrap(), None);
        if matches!(kind, BackendKind::Sqlite) {
            assert!(!dir.path().join("blobs").exists());
            let conn = rusqlite::Connection::open(dir.path().join("journal.sqlite")).unwrap();
            let bytes: Vec<u8> = conn
                .query_row("SELECT payload FROM journal WHERE sequence=1", [], |row| {
                    row.get(0)
                })
                .unwrap();
            let row: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                row["entry"]["payload"]["encoding"],
                "shared_hook_messages_v1"
            );
        }
    }
}

#[test]
fn sqlite_non_hook_rows_keep_the_direct_canonical_encoding() {
    let mut f = Fixture::new(BackendKind::Sqlite);
    f.append(vec![text("ordinary history")]);
    let conn = rusqlite::Connection::open(f.descriptor.record_location.as_ref().unwrap()).unwrap();
    let bytes: Vec<u8> = conn
        .query_row("SELECT payload FROM journal WHERE sequence=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let record: JournalRecord = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(bytes, serde_json::to_vec(&record).unwrap());
    assert!(matches!(record.entry, JournalEntry::HistoryMutated(_)));
}

#[test]
fn sqlite_reads_old_full_hook_rows_before_new_compact_rows() {
    let dir = tempfile::tempdir().unwrap();
    let old = hook_record();
    let mut backend = Sqlite::default();
    backend.recover(dir.path(), old.session_id).unwrap();
    let conn = rusqlite::Connection::open(dir.path().join("journal.sqlite")).unwrap();
    conn.execute(
        "INSERT INTO journal(sequence,record_id,payload) VALUES(?1,?2,?3)",
        rusqlite::params![
            1,
            old.record_id.to_string(),
            serde_json::to_vec(&old).unwrap()
        ],
    )
    .unwrap();
    let mut new = old.clone();
    new.session_seq = 2;
    new.record_id = new_record_id();
    backend.append(dir.path(), &new).unwrap();
    let mut snapshot = backend.snapshot(dir.path(), old.session_id).unwrap();
    assert_eq!(snapshot.next().unwrap(), Some(old));
    assert_eq!(snapshot.next().unwrap(), Some(new));
    assert_eq!(snapshot.next().unwrap(), None);
}
