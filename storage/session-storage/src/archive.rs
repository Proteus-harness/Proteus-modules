//! Lossless physical compaction of archived JSONL payloads under the writer lease.
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    blob_codec,
    io::{MAX_PAYLOAD_BYTES, atomic_write, bounded_read},
    lease::SessionWriteOwnership,
};

/// Counts only directly referenced payload files. Shared content is reported separately.
#[derive(Debug, Default, Serialize)]
pub struct ArchiveCompaction {
    pub payloads: usize,
    pub compacted: usize,
    pub original_bytes: u64,
    pub stored_bytes: u64,
    pub shared_bytes: u64,
}

#[derive(Deserialize)]
struct Envelope {
    payload: Payload,
}

#[derive(Deserialize)]
#[serde(tag = "storage", rename_all = "snake_case")]
enum Payload {
    Inline {},
    Blob {
        sha256: String,
        bytes: u64,
        relative_path: PathBuf,
    },
}

/// Rewrite each referenced blob atomically while preserving journal bytes and payload hashes.
/// An interrupted invocation can be repeated. Existing unpacked payloads remain readable.
pub fn compact_jsonl_archive(session: &Path) -> Result<ArchiveCompaction> {
    let session = std::fs::canonicalize(session)?;
    let _ownership = SessionWriteOwnership::acquire_slot(&session)?;
    crate::identity::resolve_session_identity(&session)?;
    ensure!(
        !session.join("journal.sqlite").try_exists()?,
        "archive compaction requires a JSONL session"
    );
    let journal = session.join(crate::codec::JOURNAL_FILE);
    let mut reader = BufReader::new(File::open(&journal)?);
    let mut payloads = BTreeMap::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        let count = reader
            .by_ref()
            .take((MAX_PAYLOAD_BYTES * 2 + 1) as u64)
            .read_until(b'\n', &mut line)?;
        if count == 0 {
            break;
        }
        ensure!(
            line.len() <= MAX_PAYLOAD_BYTES * 2 && line.ends_with(b"\n"),
            "archive contains an incomplete or oversized journal record"
        );
        let envelope: Envelope = serde_json::from_slice(&line)?;
        if let Payload::Blob {
            sha256,
            bytes,
            relative_path,
        } = envelope.payload
        {
            ensure!(
                sha256.len() == 64 && sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "invalid archive payload hash"
            );
            ensure!(
                relative_path == Path::new("blobs").join(format!("{sha256}.json")),
                "archive payload path differs from its hash"
            );
            ensure!(
                bytes <= MAX_PAYLOAD_BYTES as u64,
                "archive payload exceeds maximum"
            );
            if let Some(previous) = payloads.insert(sha256, bytes) {
                ensure!(previous == bytes, "archive payload has conflicting sizes");
            }
        }
    }
    let mut report = ArchiveCompaction::default();
    for (hash, expected_bytes) in payloads {
        let path = session.join("blobs").join(format!("{hash}.json"));
        let stored = bounded_read(&path, MAX_PAYLOAD_BYTES)?;
        let original = blob_codec::unpack_expected(&session, &stored, &hash, expected_bytes)?;
        report.payloads += 1;
        report.original_bytes += original.len() as u64;
        // An owned result was reconstructed from an already verified manifest.
        if matches!(original, std::borrow::Cow::Owned(_)) {
            report.stored_bytes += stored.len() as u64;
            continue;
        }
        let packed = blob_codec::pack(&session, &original)?;
        if packed.len() < stored.len() {
            ensure!(
                blob_codec::unpack(&session, &packed)? == original,
                "compacted payload differs from the original"
            );
            atomic_write(&path, &packed)
                .with_context(|| format!("replace archive payload {}", path.display()))?;
            report.compacted += 1;
            report.stored_bytes += packed.len() as u64;
        } else {
            report.stored_bytes += stored.len() as u64;
        }
    }
    let shared = session.join("blobs/shared");
    if shared.is_dir() {
        for entry in std::fs::read_dir(shared)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                report.shared_bytes += entry.metadata()?.len();
            }
        }
    }
    crate::StorageService::new(crate::BackendKind::Jsonl).prime_catalog(&session)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BackendKind, StorageService,
        types::{HistoryMutated, HistoryMutationKind, JournalEntry},
    };
    use proteus_contracts::{
        contracts::{
            SessionStorageRequest, SessionStorageResponse, StorageCommit, StorageRecordAttribution,
        },
        domain::{new_record_id, new_session_id, new_thread_id},
        model_standard::{CanonicalMessage, MessageRole},
    };

    #[test]
    fn compaction_preserves_journal_snapshot_and_is_repeatable() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let service = StorageService::new(BackendKind::Jsonl);
        let SessionStorageResponse::Session { descriptor } = service
            .execute(SessionStorageRequest::Create {
                config_root: root.path().into(),
                workspace_path: workspace.path().into(),
                session_id: new_session_id(),
                parent_session_id: None,
            })
            .unwrap()
        else {
            panic!()
        };
        let SessionStorageResponse::Writer { lease, revision } = service
            .execute(SessionStorageRequest::AcquireWriter {
                session: descriptor.session.clone(),
            })
            .unwrap()
        else {
            panic!()
        };
        let message =
            CanonicalMessage::text(MessageRole::User, "shared history payload ".repeat(30_000));
        service
            .execute(SessionStorageRequest::Commit {
                commit: StorageCommit {
                    lease: lease.clone(),
                    operation_id: new_record_id(),
                    expected: revision,
                    timestamp_ms: 1,
                    attribution: StorageRecordAttribution {
                        execution_id: None,
                        thread_id: Some(new_thread_id()),
                        turn_id: None,
                    },
                    entry: JournalEntry::HistoryMutated(HistoryMutated {
                        previous_revision: 0,
                        new_revision: 1,
                        mutation: HistoryMutationKind::Append,
                        messages: vec![message.clone()],
                        compaction: None,
                        tool_results: vec![],
                    }),
                },
            })
            .unwrap();
        let dir = descriptor.local_path.unwrap();
        // Build an existing unpacked archive through the normal writer first.
        for entry in std::fs::read_dir(dir.join("blobs")).unwrap() {
            let path = entry.unwrap().path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                let stored = std::fs::read(&path).unwrap();
                let original = blob_codec::unpack(&dir, &stored).unwrap();
                std::fs::write(path, original).unwrap();
            }
        }
        let before = std::fs::read(dir.join("journal.jsonl")).unwrap();
        assert!(
            compact_jsonl_archive(&dir).is_err(),
            "active writer excludes archive mutation"
        );
        service
            .execute(SessionStorageRequest::ReleaseWriter { lease })
            .unwrap();
        let report = compact_jsonl_archive(&dir).unwrap();
        assert_eq!(report.payloads, 1);
        assert_eq!(report.compacted, 1);
        assert_eq!(std::fs::read(dir.join("journal.jsonl")).unwrap(), before);
        assert_eq!(compact_jsonl_archive(&dir).unwrap().compacted, 0);
        let reopened = StorageService::new(BackendKind::Jsonl);
        let SessionStorageResponse::Snapshot { snapshot } = reopened
            .execute(SessionStorageRequest::OpenSnapshot {
                session: descriptor.session,
            })
            .unwrap()
        else {
            panic!()
        };
        let SessionStorageResponse::Conversation { conversation } = reopened
            .execute(SessionStorageRequest::ReadConversation {
                snapshot,
                point: proteus_contracts::contracts::StorageHistoryPoint::Current,
            })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(conversation.messages, vec![message]);
    }
}
