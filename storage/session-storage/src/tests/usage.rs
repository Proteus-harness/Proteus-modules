use super::*;
use proteus_contracts::{
    contracts::{ModelCallOrigin, OperationOwner},
    domain::{
        AgentOutput, AgentTask, ModelRef, ModelUsageStatus, new_exchange_id, new_execution_id,
        new_turn_id,
    },
    model_standard::{
        CanonicalModelRequest, CanonicalModelResponse, FinishReason, ModelFailure,
        ModelFailureKind, TokenUsage,
    },
};

fn commit(f: &mut Fixture, attribution: StorageRecordAttribution, entry: JournalEntry) {
    let SessionStorageResponse::Committed { ack } = f
        .service
        .execute(SessionStorageRequest::Commit {
            commit: StorageCommit {
                lease: f.lease.clone(),
                operation_id: new_record_id(),
                expected: f.revision,
                timestamp_ms: 100 + f.revision.session_seq as i64,
                attribution,
                entry,
            },
        })
        .unwrap()
    else {
        panic!("missing commit");
    };
    f.revision = ack.revision;
}

fn add_turn(f: &mut Fixture, failed: bool) {
    let turn = new_turn_id();
    let owner = || StorageRecordAttribution {
        execution_id: Some(new_execution_id()),
        thread_id: Some(f.thread),
        turn_id: Some(turn),
    };
    let model_owner = owner();
    let exchange_id = new_exchange_id();
    let path = f.descriptor.workspace_path.clone();
    let revision = f.revision.history_revision;
    commit(
        f,
        model_owner.clone(),
        JournalEntry::TurnOpened(TurnOpened {
            task: AgentTask::new("usage fixture", path),
            intent: None,
            base_history_revision: revision,
            module_epoch: 0,
            config_snapshot: serde_json::json!({}),
        }),
    );
    commit(
        f,
        model_owner.clone(),
        JournalEntry::ModelRequestRecorded(ModelRequestRecorded {
            exchange_id,
            origin: ModelCallOrigin::Direct,
            owner: OperationOwner::Workflow,
            request: CanonicalModelRequest::new(
                ModelRef::new("synthetic", "usage"),
                vec![text("ordinary model request")],
            ),
        }),
    );
    let outcome = if failed {
        ModelResponseOutcome::Error {
            failure: ModelFailure::new(ModelFailureKind::Other, "fixture model unavailable"),
        }
    } else {
        ModelResponseOutcome::Response {
            response: CanonicalModelResponse::new(
                CanonicalMessage::text(MessageRole::Assistant, "ordinary answer"),
                vec![],
                FinishReason::Stop,
            )
            .with_usage(TokenUsage::new(120, 30)),
        }
    };
    commit(
        f,
        model_owner.clone(),
        JournalEntry::ModelResponseRecorded(ModelResponseRecorded {
            exchange_id,
            outcome,
        }),
    );
    commit(
        f,
        StorageRecordAttribution {
            execution_id: None,
            ..model_owner
        },
        JournalEntry::TurnSettled(TurnSettled {
            status: if failed {
                TurnSettlementStatus::Error
            } else {
                TurnSettlementStatus::Success
            },
            output: (!failed).then(|| AgentOutput::text("done")),
            error: failed.then(|| "fixture model unavailable".into()),
        }),
    );
}

fn read(f: &Fixture, snapshot: &StorageSnapshot) -> SessionStorageResponse {
    let req = SessionStorageRequest::ReadUsage {
        snapshot: snapshot.clone(),
    };
    let out = f.service.execute(req.clone()).unwrap();
    req.validate_response(&out).unwrap();
    out
}

#[test]
fn read_usage_holds_the_same_prefix_without_losing_conversation_context() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        let a = text("first retained row");
        f.append(vec![a.clone()]);
        add_turn(&mut f, false);
        add_turn(&mut f, true);
        let older = f.snapshot();
        let old_usage = read(&f, &older);
        let SessionStorageResponse::Usage { usage } = &old_usage else {
            panic!("missing usage")
        };
        assert_eq!(usage.requests.len(), 2);
        assert_eq!(usage.requests[0].status, ModelUsageStatus::Completed);
        assert_eq!(usage.requests[0].usage, Some(TokenUsage::new(120, 30)));
        assert_eq!(usage.requests[1].status, ModelUsageStatus::Error);
        let current_history = f.conversation(older.clone(), StorageHistoryPoint::Current);
        assert_eq!(current_history.messages, [a]);
        assert!(!current_history.model_context.is_empty());

        add_turn(&mut f, false);
        assert_eq!(old_usage, read(&f, &older));
        assert_eq!(
            current_history,
            f.conversation(older.clone(), StorageHistoryPoint::Current)
        );
        let new = f.snapshot();
        let SessionStorageResponse::Usage { usage } = read(&f, &new) else {
            panic!("missing usage")
        };
        assert_eq!(usage.requests.len(), 3);
        assert_eq!(new.revision.session_seq, older.revision.session_seq + 4);

        let terminal = SessionStorageRequest::ReadRecords {
            snapshot: older.clone(),
            after_seq: older.revision.session_seq - 1,
            limit: 2,
        };
        let reply = f.service.execute(terminal.clone()).unwrap();
        terminal.validate_response(&reply).unwrap();
        let SessionStorageResponse::Records { page } = reply else {
            panic!("missing terminal page")
        };
        assert_eq!(page.records.len(), 1);
        assert!(matches!(
            page.records[0].entry,
            JournalEntry::TurnSettled(_)
        ));
        assert!(page.next_after_seq.is_none());
    }
}
