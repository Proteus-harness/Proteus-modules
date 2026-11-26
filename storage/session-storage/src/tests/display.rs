use super::*;
use proteus_contracts::contracts::{
    ContextPreparationInput, ContextPreparationOutput, ExecutionAttribution, HookEvent, HookInput,
    HookTrace, OperationOwner,
};

fn append_hook(f: &mut Fixture) {
    let execution = proteus_contracts::domain::new_execution_id();
    let messages = vec![text(&"diagnostic history ".repeat(60_000))];
    let event = HookEvent::BeforeContext {
        input: ContextPreparationInput::new(messages.clone(), None),
        output: ContextPreparationOutput::unchanged(messages),
        model_capabilities: None,
        target: None,
    };
    let commit = StorageCommit {
        lease: f.lease.clone(),
        operation_id: new_record_id(),
        expected: f.revision,
        timestamp_ms: 1234,
        attribution: StorageRecordAttribution {
            execution_id: Some(execution),
            thread_id: None,
            turn_id: None,
        },
        entry: JournalEntry::HookInvoked(HookTrace {
            owner: OperationOwner::Workflow,
            input: HookInput {
                event: event.clone(),
                attribution: ExecutionAttribution::detached(execution),
                cwd: f.descriptor.workspace_path.clone(),
                conversation: None,
            },
            steps: Vec::new(),
            output: Some(event),
        }),
    };
    let SessionStorageResponse::Committed { ack } = f
        .service
        .execute(SessionStorageRequest::Commit { commit })
        .unwrap()
    else {
        panic!()
    };
    f.revision = ack.revision;
}

fn open(f: &Fixture) -> StorageDisplaySnapshot {
    let SessionStorageResponse::DisplaySnapshot {
        projection: Some(projection),
    } = f
        .service
        .execute(SessionStorageRequest::OpenDisplaySnapshot {
            session: f.descriptor.session.clone(),
        })
        .unwrap()
    else {
        panic!()
    };
    projection
}

fn page(f: &Fixture, snapshot: &StorageSnapshot, after_seq: u64) -> StorageRecordPage {
    let request = SessionStorageRequest::ReadDisplayRecords {
        snapshot: snapshot.clone(),
        after_seq,
        limit: 1,
    };
    let response = f.service.execute(request.clone()).unwrap();
    request.validate_response(&response).unwrap();
    let SessionStorageResponse::Records { page } = response else {
        panic!()
    };
    page
}

#[test]
fn one_large_history_mutation_uses_canonical_paging_without_duplicating_the_payload() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        let entry = f
            .append(vec![text(&"x".repeat(SESSION_STORAGE_PAGE_BYTE_BUDGET))])
            .entry;
        let request = SessionStorageRequest::OpenDisplaySnapshot {
            session: f.descriptor.session.clone(),
        };
        let response = f.service.execute(request.clone()).unwrap();
        request.validate_response(&response).unwrap();
        assert!(matches!(
            response,
            SessionStorageResponse::DisplaySnapshot { projection: None }
        ));
        let snapshot = f.snapshot();
        let request = SessionStorageRequest::ReadRecords {
            snapshot: snapshot.clone(),
            after_seq: 0,
            limit: 64,
        };
        let response = f.service.execute(request.clone()).unwrap();
        request.validate_response(&response).unwrap();
        let SessionStorageResponse::Records { page } = response else {
            panic!("expected canonical page")
        };
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].entry, entry);
        assert!(page.next_after_seq.is_none());
        f.service
            .execute(SessionStorageRequest::ReleaseSnapshot { snapshot })
            .unwrap();
        f.append(vec![text("subsequent append")]);
        append_hook(&mut f);
        assert_eq!(open(&f).state.conversation.messages.len(), 2);
    }
}

#[test]
fn large_append_only_journal_uses_bounded_canonical_pages() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        let mut expected = Vec::new();
        for index in 0..5 {
            let message = text(&format!("{index}{}", "x".repeat(1024 * 1024)));
            expected.push(f.append(vec![message]).entry);
        }
        let request = SessionStorageRequest::OpenDisplaySnapshot {
            session: f.descriptor.session.clone(),
        };
        let response = f.service.execute(request.clone()).unwrap();
        request.validate_response(&response).unwrap();
        assert!(matches!(
            response,
            SessionStorageResponse::DisplaySnapshot { projection: None }
        ));
        let snapshot = f.snapshot();
        let mut after_seq = 0;
        let mut received = Vec::new();
        loop {
            let request = SessionStorageRequest::ReadRecords {
                snapshot: snapshot.clone(),
                after_seq,
                limit: 2,
            };
            let response = f.service.execute(request.clone()).unwrap();
            request.validate_response(&response).unwrap();
            let SessionStorageResponse::Records { page } = response else {
                panic!("expected canonical page")
            };
            received.extend(page.records.into_iter().map(|r| r.entry));
            let Some(next) = page.next_after_seq else {
                break;
            };
            assert!(next > after_seq);
            after_seq = next;
        }
        assert_eq!(received, expected);
        assert!(after_seq > 0);
        f.service
            .execute(SessionStorageRequest::ReleaseSnapshot { snapshot })
            .unwrap();
    }
}

#[test]
fn jsonl_display_fold_reuses_bounded_fallback_scratch() {
    use crate::backend::{JournalBackend, Jsonl};

    let mut f = Fixture::new(BackendKind::Jsonl);
    let entries: Vec<_> = [
        42_000, 47_000, 51_000, 45_000, 43_000, 180_000, 42_000, 43_000, 44_000, 45_000,
    ]
    .into_iter()
    .map(|n| f.append(vec![text(&"x".repeat(n))]).entry)
    .collect();
    let dir = f.descriptor.local_path.clone().unwrap();
    let id = f.descriptor.session.session_id;
    let raw = std::fs::read(f.descriptor.record_location.as_ref().unwrap()).unwrap();
    let mut source = Jsonl::new().snapshot(&dir, id).unwrap();
    let mut regular = Jsonl::new().snapshot(&dir, id).unwrap();
    let mut scratch = Vec::new();
    let mut consumed = 0;
    let mut reused = false;
    for (i, raw_line) in raw.split_inclusive(|&b| b == b'\n').enumerate() {
        let old = (scratch.as_ptr(), scratch.capacity());
        let record = source.next_with_scratch(&mut scratch).unwrap().unwrap();
        assert_eq!(record, regular.next().unwrap().unwrap());
        assert_eq!(record.entry, entries[i]);
        let crosses_buffer =
            consumed / (64 * 1024) != (consumed + raw_line.len() - 1) / (64 * 1024);
        consumed += raw_line.len();
        assert_eq!(
            source.consumed_fingerprint(),
            crate::io::sha256_hex(&raw[..consumed])
        );
        if i == 5 {
            assert_eq!(scratch.capacity(), 0, "do not retain an oversized line");
        } else {
            assert!(scratch.capacity() <= 128 * 1024);
            if crosses_buffer {
                assert!(scratch == raw_line || scratch == &raw_line[..raw_line.len() - 1]);
                reused |= old.1 != 0 && old == (scratch.as_ptr(), scratch.capacity());
            }
        }
    }
    assert!(reused, "ordinary fallback records reused the allocation");
    assert_eq!(consumed, raw.len());
    assert!(source.next_with_scratch(&mut scratch).unwrap().is_none());
    assert!(regular.next().unwrap().is_none());
    assert_eq!(source.fingerprint(), regular.fingerprint());
    assert!(source.confirm_verified_data().unwrap());
    source.reset().unwrap();
    assert_eq!(
        source
            .next_with_scratch(&mut scratch)
            .unwrap()
            .unwrap()
            .entry,
        entries[0]
    );

    // Exercise actual display publication and an ordinary append while an older
    // projection is held. The old handle still exposes exactly its fixed prefix.
    let projection = open(&f);
    f.append(vec![text("later append")]);
    let command = SessionStorageRequest::ReadDisplayRecords {
        snapshot: projection.snapshot.clone(),
        after_seq: 0,
        limit: 64,
    };
    let result = f.service.execute(command.clone()).unwrap();
    command.validate_response(&result).unwrap();
    let SessionStorageResponse::Records { page } = result else {
        panic!("missing display page");
    };
    assert_eq!(page.next_after_seq, None);
    assert_eq!(
        page.records
            .into_iter()
            .map(|r| r.entry)
            .collect::<Vec<_>>(),
        entries
    );

    // Ordinary external payloads retain the stricter blob-bearing proof rule.
    let external = f.append(vec![text(&"blob".repeat(80_000))]);
    let mut blob_source = Jsonl::new().snapshot(&dir, id).unwrap();
    let mut last = None;
    while let Some(row) = blob_source.next_with_scratch(&mut scratch).unwrap() {
        last = Some(row);
    }
    assert_eq!(last.unwrap().entry, external.entry);
    assert!(!blob_source.confirm_verified_data().unwrap());
}

#[test]
fn display_capture_validates_once_and_pages_without_expanding_diagnostics_again() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        let first = text("first");
        let second = text("second");
        f.append(vec![first.clone()]);
        append_hook(&mut f);
        f.append(vec![second.clone()]);
        append_hook(&mut f);
        crate::diagnostics::reset();
        let projection = open(&f);
        assert_eq!(crate::diagnostics::get().decoded, 4);
        assert_eq!(projection.state.conversation.messages, [first, second]);
        assert_eq!(projection.state.conversation.revision, f.revision);

        crate::diagnostics::reset();
        let first_page = page(&f, &projection.snapshot, 0);
        assert_eq!(first_page.records[0].session_seq, 1);
        assert_eq!(first_page.next_after_seq, Some(1));
        let second_page = page(&f, &projection.snapshot, 1);
        assert_eq!(second_page.records[0].session_seq, 3);
        let empty_tail = page(&f, &projection.snapshot, 3);
        assert!(empty_tail.records.is_empty());
        assert!(empty_tail.next_after_seq.is_none());
        assert_eq!(page(&f, &projection.snapshot, 0), first_page);
        assert_eq!(crate::diagnostics::get().decoded, 0);
        assert!(serde_json::to_vec(&first_page).unwrap().len() < 2048);

        let conversation =
            f.conversation(projection.snapshot.clone(), StorageHistoryPoint::Current);
        assert_eq!(conversation, projection.state.conversation);
        crate::diagnostics::reset();
        assert_eq!(page(&f, &projection.snapshot, 1), second_page);
        assert_eq!(crate::diagnostics::get().decoded, 0);

        f.append(vec![text("new suffix")]);
        assert_eq!(page(&f, &projection.snapshot, 0), first_page);
        f.service
            .execute(SessionStorageRequest::ReleaseSnapshot {
                snapshot: projection.snapshot.clone(),
            })
            .unwrap();
        assert!(
            f.service
                .execute(SessionStorageRequest::ReadDisplayRecords {
                    snapshot: projection.snapshot,
                    after_seq: 0,
                    limit: 1,
                })
                .is_err()
        );
    }
}

#[test]
fn display_capture_rejects_corrupt_replay_only_payload() {
    let mut f = Fixture::new(BackendKind::Jsonl);
    f.append(vec![text("visible")]);
    append_hook(&mut f);
    let dir = f.descriptor.local_path.as_ref().unwrap();
    let journal = std::fs::read_to_string(dir.join("journal.jsonl")).unwrap();
    let hook: serde_json::Value = serde_json::from_str(journal.lines().nth(1).unwrap()).unwrap();
    assert_eq!(hook["payload"]["storage"], "blob");
    std::fs::remove_file(dir.join(hook["payload"]["relative_path"].as_str().unwrap())).unwrap();
    assert!(
        f.service
            .execute(SessionStorageRequest::OpenDisplaySnapshot {
                session: f.descriptor.session,
            })
            .is_err()
    );
}

#[test]
fn display_pages_reject_rewritten_fixed_prefix() {
    let mut f = Fixture::new(BackendKind::Jsonl);
    f.append(vec![text("visible")]);
    let projection = open(&f);
    let journal = f.descriptor.record_location.as_ref().unwrap();
    let mut bytes = std::fs::read(journal).unwrap();
    bytes[0] = b'[';
    std::fs::write(journal, bytes).unwrap();
    assert!(
        f.service
            .execute(SessionStorageRequest::ReadDisplayRecords {
                snapshot: projection.snapshot,
                after_seq: 0,
                limit: 1,
            })
            .is_err()
    );
}

#[test]
fn display_reuses_page_scratch_without_reordering_pending_rows() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text("first small row")]);
        for letter in ['А', 'Б', 'В'] {
            f.append(vec![text(&letter.to_string().repeat(1024 * 1024))]);
        }
        f.append(vec![text("last small row")]);
        append_hook(&mut f);
        let projection = open(&f);
        let read = |after_seq| {
            let req = SessionStorageRequest::ReadDisplayRecords {
                snapshot: projection.snapshot.clone(),
                after_seq,
                limit: 64,
            };
            let response = f.service.execute(req.clone()).unwrap();
            req.validate_response(&response).unwrap();
            let SessionStorageResponse::Records { page } = response else {
                panic!()
            };
            page
        };
        let seqs = |page: &StorageRecordPage| {
            page.records
                .iter()
                .map(|r| r.session_seq)
                .collect::<Vec<_>>()
        };
        let first = read(0);
        assert_eq!(seqs(&first), [1, 2]);
        assert_eq!(first.next_after_seq, Some(2));
        let second = read(2);
        assert_eq!(seqs(&second), [3]);
        assert_eq!(second.next_after_seq, Some(3));
        let final_page = read(3);
        assert_eq!(seqs(&final_page), [4, 5]);
        assert_eq!(final_page.next_after_seq, None);
        assert_eq!(read(0), first);
        assert_eq!(read(2), second);
    }
}

#[test]
fn display_page_reuses_only_the_exact_live_request_snapshot() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text("ordinary first record")]);
        let projection = open(&f);
        let expected = &projection.snapshot;

        let mut wrong = expected.clone();
        wrong.session.key.push('x');
        let error = f
            .service
            .execute(SessionStorageRequest::ReadDisplayRecords {
                snapshot: wrong,
                after_seq: 0,
                limit: 1,
            })
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "snapshot identity/revision/fingerprint mismatch"
        );

        let supplied = expected.clone();
        let owned = (
            supplied.snapshot_id.as_ptr(),
            supplied.session.key.as_ptr(),
            supplied.content_sha256.as_ptr(),
        );
        let read_page = |snapshot| {
            let SessionStorageResponse::Records { page } = f
                .service
                .execute(SessionStorageRequest::ReadDisplayRecords {
                    snapshot,
                    after_seq: 0,
                    limit: 1,
                })
                .unwrap()
            else {
                panic!("expected a display page");
            };
            page
        };
        let page = read_page(supplied);
        assert_eq!(&page.snapshot, expected);
        assert_eq!(
            (
                page.snapshot.snapshot_id.as_ptr(),
                page.snapshot.session.key.as_ptr(),
                page.snapshot.content_sha256.as_ptr(),
            ),
            owned,
        );
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].session_seq, 1);
        assert_eq!(page.next_after_seq, None);

        let mut with_spare = expected.clone();
        with_spare.session.key.reserve_exact(4096);
        let spare = with_spare.session.key.as_ptr();
        let compact = read_page(with_spare);
        assert_eq!(compact, page);
        assert_ne!(compact.snapshot.session.key.as_ptr(), spare);
    }
}
