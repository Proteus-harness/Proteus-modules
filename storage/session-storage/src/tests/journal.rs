use super::*;

#[test]
fn independent_backends_commit_reopen_and_reconcile_lost_ack_without_duplicate() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        let message = text("one durable effect");
        let commit = f.append(vec![message.clone()]);
        let initial = f
            .service
            .execute(SessionStorageRequest::CommitStatus {
                session: f.descriptor.session.clone(),
                operation_id: commit.operation_id,
            })
            .unwrap();
        let repeated = f
            .service
            .execute(SessionStorageRequest::Commit {
                commit: commit.clone(),
            })
            .unwrap();
        let SessionStorageResponse::CommitStatus { ack: Some(initial) } = initial else {
            panic!()
        };
        assert_eq!(
            repeated,
            SessionStorageResponse::Committed {
                ack: initial.clone()
            }
        );
        let session = f.descriptor.session.clone();
        let expected_revision = f.revision;
        drop(f.service);
        let service = StorageService::new(kind);
        let SessionStorageResponse::Writer { lease, revision } = service
            .execute(SessionStorageRequest::AcquireWriter {
                session: session.clone(),
            })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(revision, expected_revision);
        let mut commit = commit;
        commit.lease = lease;
        assert_eq!(
            service
                .execute(SessionStorageRequest::Commit { commit })
                .unwrap(),
            SessionStorageResponse::Committed { ack: initial }
        );
        let SessionStorageResponse::Snapshot { snapshot } = service
            .execute(SessionStorageRequest::OpenSnapshot { session })
            .unwrap()
        else {
            panic!()
        };
        let SessionStorageResponse::Records { page } = service
            .execute(SessionStorageRequest::ReadRecords {
                snapshot,
                after_seq: 0,
                limit: 10,
            })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(page.records.len(), 1);
        let exists = f.descriptor.local_path.clone().unwrap();
        match kind {
            BackendKind::Jsonl => assert!(
                exists.join("journal.jsonl").is_file() && !exists.join("journal.sqlite").exists()
            ),
            BackendKind::Sqlite => assert!(
                exists.join("journal.sqlite").is_file() && !exists.join("journal.jsonl").exists()
            ),
        }
    }
}

#[test]
fn same_operation_id_with_different_content_is_conflict() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        let mut commit = f.append(vec![text("original")]);
        let JournalEntry::HistoryMutated(change) = &mut commit.entry else {
            panic!()
        };
        change.messages = vec![text("different")];
        assert!(
            f.service
                .execute(SessionStorageRequest::Commit { commit })
                .is_err()
        );
        assert_eq!(
            f.conversation(f.snapshot(), StorageHistoryPoint::Current)
                .messages
                .len(),
            1
        );
    }
}

#[test]
fn snapshots_are_immutable_prefixes_and_released_prefix_can_be_compared() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        let a = text("A");
        let b = text("B");
        f.append(vec![a.clone()]);
        let old = f.snapshot();
        f.append(vec![b.clone()]);
        let current = f.snapshot();
        assert_eq!(
            f.conversation(old.clone(), StorageHistoryPoint::Current)
                .messages,
            [a.clone()]
        );
        assert_eq!(
            f.conversation(current.clone(), StorageHistoryPoint::Current)
                .messages,
            [a, b]
        );
        f.service
            .execute(SessionStorageRequest::ReleaseSnapshot {
                snapshot: old.clone(),
            })
            .unwrap();
        assert_eq!(
            f.service
                .execute(SessionStorageRequest::CompareSnapshots {
                    snapshot: current,
                    prefix: old
                })
                .unwrap(),
            SessionStorageResponse::SnapshotPrefix { matches: true }
        );
    }
}

#[test]
fn opaque_checkpoint_can_read_original_history_without_interpreting_provider_data() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        let user = text("preserve original");
        f.append(vec![user.clone()]);
        let checkpoint = CanonicalMessage::from_parts(
            MessageRole::Assistant,
            vec![CanonicalPart::new(
                PartProvenance::Compactor,
                PartScope::Conversation,
                ContentPart::Opaque {
                    format: "example.opaque/v1".into(),
                    data: serde_json::json!({"cipher":"untouched"}),
                },
            )],
        );
        let mut report = HistoryCompactionReport::unchanged(1, None);
        report.changed = true;
        let commit = StorageCommit {
            lease: f.lease.clone(),
            operation_id: new_record_id(),
            expected: f.revision,
            timestamp_ms: 2,
            attribution: StorageRecordAttribution {
                execution_id: None,
                thread_id: Some(f.thread),
                turn_id: None,
            },
            entry: JournalEntry::HistoryMutated(HistoryMutated {
                previous_revision: 1,
                new_revision: 2,
                mutation: HistoryMutationKind::Replace,
                messages: vec![user.clone(), checkpoint.clone()],
                compaction: Some(report),
                tool_results: vec![],
            }),
        };
        f.service
            .execute(SessionStorageRequest::Commit { commit })
            .unwrap();
        let current = f.snapshot();
        assert_eq!(
            f.conversation(current.clone(), StorageHistoryPoint::Current)
                .messages,
            [user.clone(), checkpoint.clone()]
        );
        assert_eq!(
            f.conversation(
                current,
                StorageHistoryPoint::BeforeReplacement {
                    part_id: checkpoint.parts[0].part_id
                }
            )
            .messages,
            [user]
        );
    }
}

#[test]
fn jsonl_writer_reopen_reuses_the_bounded_spill_for_multi_buffer_lines() {
    use crate::backend::{JournalBackend, Jsonl};

    let mut f = Fixture::new(BackendKind::Jsonl);
    let expected: Vec<_> = (0..6)
        .map(|i| text(&format!("ordinary paragraph {i}: {}", "x".repeat(72 * 1024))))
        .collect();
    for message in &expected {
        f.append(vec![message.clone()]);
    }
    let session = f.descriptor.session.clone();
    let path = f.descriptor.local_path.clone().unwrap();
    let journal = path.join(crate::codec::JOURNAL_FILE);
    let raw = std::fs::read(&journal).unwrap();
    let lengths: Vec<_> = raw
        .split_inclusive(|&byte| byte == b'\n')
        .map(<[u8]>::len)
        .collect();
    assert_eq!(lengths.len(), expected.len());
    assert!(!path.join("blobs").exists());
    assert!(
        lengths.iter().all(|&len| len > 64 * 1024 && len < 128 * 1024),
        "ordinary inline rows must need a temporary spill"
    );

    // The production recovery uses the same borrowed scratch entry point.
    // Verify a real producer's complete >buffer lines stay in that one Vec.
    let mut source = Jsonl::new().snapshot(&path, session.session_id).unwrap();
    let mut scratch = Vec::new();
    for (index, &length) in lengths.iter().enumerate() {
        let allocation = scratch.as_ptr();
        let had_capacity = scratch.capacity() >= length;
        let record = source.next_with_scratch(&mut scratch).unwrap().unwrap();
        assert_eq!(record.session_seq, index as u64 + 1);
        assert_eq!(scratch.len(), length);
        assert!(scratch.capacity() <= 128 * 1024);
        if index > 0 {
            assert!(had_capacity, "ordinary full-sized first fill retains the spill");
            assert_eq!(scratch.as_ptr(), allocation);
        }
    }
    assert_eq!(source.next_with_scratch(&mut scratch).unwrap(), None);
    let fingerprint = source.consumed_fingerprint();
    assert_eq!(fingerprint, crate::io::sha256_hex(&raw));
    drop(source);

    // Actual writer first-open, resident history and head readback unchanged.
    let want = f.revision;
    f.service
        .execute(SessionStorageRequest::ReleaseWriter { lease: f.lease.clone() })
        .unwrap();
    let fresh = StorageService::new(BackendKind::Jsonl);
    let SessionStorageResponse::Writer { lease, revision } = fresh
        .execute(SessionStorageRequest::AcquireWriter { session: session.clone() })
        .unwrap()
    else {
        panic!("reopened writer");
    };
    assert_eq!(revision, want);
    let SessionStorageResponse::Resumed { state } = fresh
        .execute(SessionStorageRequest::Resume { lease })
        .unwrap()
    else {
        panic!("reopened state");
    };
    assert_eq!(state.conversation.revision, revision);
    assert_eq!(state.conversation.messages, expected);
    let SessionStorageResponse::Snapshot { snapshot } = fresh
        .execute(SessionStorageRequest::OpenSnapshot { session })
        .unwrap()
    else {
        panic!("reopened snapshot");
    };
    assert_eq!(snapshot.revision, revision);
    assert_eq!(snapshot.content_sha256, fingerprint);
    assert_eq!(std::fs::read(&journal).unwrap(), raw);
}

#[test]
fn jsonl_torn_tail_is_read_only_until_writer_recovery() {
    use std::io::Write;
    let mut f = Fixture::new(BackendKind::Jsonl);
    let message = text("committed");
    f.append(vec![message.clone()]);
    f.service
        .execute(SessionStorageRequest::ReleaseWriter {
            lease: f.lease.clone(),
        })
        .unwrap();
    let path = f
        .descriptor
        .local_path
        .clone()
        .unwrap()
        .join("journal.jsonl");
    let valid = std::fs::read(&path).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"interrupted\":")
        .unwrap();
    let broken = std::fs::read(&path).unwrap();
    assert_eq!(
        f.conversation(f.snapshot(), StorageHistoryPoint::Current)
            .messages,
        [message]
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        broken,
        "read-only inspection must not repair tail"
    );
    let SessionStorageResponse::Writer { lease, .. } = f
        .service
        .execute(SessionStorageRequest::AcquireWriter {
            session: f.descriptor.session.clone(),
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(std::fs::read(&path).unwrap(), valid);
    f.lease = lease;
    f.append(vec![text("after recovery")]);
}

#[test]
fn corrupted_complete_record_is_not_treated_as_torn_tail() {
    use std::io::Write;
    let mut f = Fixture::new(BackendKind::Jsonl);
    f.append(vec![text("committed")]);
    f.service
        .execute(SessionStorageRequest::ReleaseWriter {
            lease: f.lease.clone(),
        })
        .unwrap();
    let path = f
        .descriptor
        .local_path
        .clone()
        .unwrap()
        .join("journal.jsonl");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"corruption\n")
        .unwrap();
    let original = std::fs::read(&path).unwrap();
    assert!(
        f.service
            .execute(SessionStorageRequest::OpenSnapshot {
                session: f.descriptor.session.clone()
            })
            .is_err()
    );
    assert!(
        f.service
            .execute(SessionStorageRequest::AcquireWriter {
                session: f.descriptor.session
            })
            .is_err()
    );
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[test]
fn explicit_reset_bounds_old_history_lookups() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text("discarded branch")]);
        let commit = StorageCommit {
            lease: f.lease.clone(),
            operation_id: new_record_id(),
            expected: f.revision,
            timestamp_ms: 2,
            attribution: StorageRecordAttribution {
                execution_id: None,
                thread_id: Some(f.thread),
                turn_id: None,
            },
            entry: JournalEntry::HistoryMutated(HistoryMutated {
                previous_revision: 1,
                new_revision: 2,
                mutation: HistoryMutationKind::Replace,
                messages: vec![],
                compaction: None,
                tool_results: vec![],
            }),
        };
        f.service
            .execute(SessionStorageRequest::Commit { commit })
            .unwrap();
        let snapshot = f.snapshot();
        assert!(
            f.service
                .execute(SessionStorageRequest::ReadConversation {
                    snapshot,
                    point: StorageHistoryPoint::Revision {
                        history_revision: 1
                    }
                })
                .is_err()
        );
    }
}

#[test]
fn warm_snapshot_open_uses_only_prefix_metadata_while_execution_recovery_stays_strict() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text(&"large history".repeat(40_000))]);
        let initial = f.snapshot();
        crate::diagnostics::reset();
        let warm = f.snapshot();
        let cost = crate::diagnostics::get();
        assert_eq!(cost.decoded, 0);
        assert_eq!(cost.fingerprint_bytes, 0);
        assert_eq!(initial.revision, warm.revision);
        assert_eq!(initial.content_sha256, warm.content_sha256);
        for page in 0..3 {
            f.service
                .execute(SessionStorageRequest::WriteCache {
                    session: f.descriptor.session.clone(),
                    key: format!("warm-page-{page}"),
                    source: warm.clone(),
                    value: serde_json::json!({"rows": ["display cache"]}),
                })
                .unwrap();
        }
        assert_eq!(crate::diagnostics::get().decoded, 0);
        assert_eq!(crate::diagnostics::get().fingerprint_bytes, 0);
        f.conversation(warm, StorageHistoryPoint::Current);
        assert!(
            crate::diagnostics::get().decoded > 0,
            "canonical history reads still validate their pinned prefix"
        );
        f.service
            .execute(SessionStorageRequest::ReleaseWriter {
                lease: f.lease.clone(),
            })
            .unwrap();
        crate::diagnostics::reset();
        let SessionStorageResponse::Writer { lease, .. } = f
            .service
            .execute(SessionStorageRequest::AcquireWriter {
                session: f.descriptor.session.clone(),
            })
            .unwrap()
        else {
            panic!()
        };
        f.lease = lease;
        assert!(
            crate::diagnostics::get().decoded > 0,
            "writer recovery must not trust the read-only prefix cache"
        );
        crate::diagnostics::reset();
        f.append(vec![text("new history invalidates the prefix cache")]);
        let new = f.snapshot();
        assert_eq!(crate::diagnostics::get().decoded, 0);
        assert_eq!(crate::diagnostics::get().fingerprint_bytes, 0);
        assert_eq!(new.revision.session_seq, 2);
    }
}

#[test]
fn rejected_prospective_validation_reloads_canonical_state_before_next_commit() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        let previous = f.append(vec![text("durable first message")]);
        let mut invalid = previous;
        invalid.operation_id = new_record_id();
        invalid.expected = f.revision;
        // Structurally valid, but the previous history revision is stale.
        assert!(
            f.service
                .execute(SessionStorageRequest::Commit { commit: invalid })
                .is_err()
        );
        f.append(vec![text("valid next message")]);
        let snapshot = f.snapshot();
        let history = f.conversation(snapshot, StorageHistoryPoint::Current);
        assert_eq!(history.revision.session_seq, 2);
        assert_eq!(history.revision.history_revision, 2);
        assert_eq!(history.messages.len(), 2);
    }
}
