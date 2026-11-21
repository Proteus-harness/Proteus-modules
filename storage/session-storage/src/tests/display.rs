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
