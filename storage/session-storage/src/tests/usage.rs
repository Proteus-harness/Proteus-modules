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
fn current_conversation_matches_the_full_display_projection_at_the_pinned_revision() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text("current-only usage-rich fixture")]);
        add_turn(&mut f, false);
        add_turn(&mut f, true);
        let SessionStorageResponse::DisplaySnapshot {
            projection: Some(full),
        } = f
            .service
            .execute(SessionStorageRequest::OpenDisplaySnapshot {
                session: f.descriptor.session.clone(),
            })
            .unwrap()
        else {
            panic!("missing display projection");
        };

        let old = full.snapshot;
        let expected = SessionStorageResponse::Conversation {
            conversation: full.state.conversation,
        };
        let SessionStorageResponse::Conversation { conversation } = &expected else {
            unreachable!()
        };
        assert!(!conversation.model_context.is_empty());
        assert_eq!(full.state.usage.requests.len(), 2);
        let expected_usage = SessionStorageResponse::Usage {
            usage: full.state.usage,
        };
        let read_current = |fixture: &Fixture| {
            let req = SessionStorageRequest::ReadConversation {
                snapshot: old.clone(),
                point: StorageHistoryPoint::Current,
            };
            let out = fixture.service.execute(req.clone()).unwrap();
            req.validate_response(&out).unwrap();
            assert_eq!(
                serde_json::to_vec(&out).unwrap(),
                serde_json::to_vec(&expected).unwrap()
            );
        };
        crate::diagnostics::reset();
        read_current(&f);
        assert_eq!(
            crate::diagnostics::get().decoded,
            old.revision.session_seq,
            "the read-only fold still decodes the complete fixed prefix"
        );
        assert_eq!(read(&f, &old), expected_usage);
        // An unrelated later writer suffix is not visible through the old
        // display handle, even though current conversation has another fold.
        add_turn(&mut f, false);
        read_current(&f);
        assert_eq!(read(&f, &old), expected_usage);
    }
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

#[test]
fn cold_catalog_matches_strict_model_turn_history_and_audit() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text("  первый чат 🦀  ")]);
        add_turn(&mut f, false);
        add_turn(&mut f, true);
        f.append(vec![text("latest history message")]);

        // The full display fold provides an independent, strictly validated
        // reference for the catalog's history, revision and content digest.
        let SessionStorageResponse::DisplaySnapshot {
            projection: Some(full),
        } = f
            .service
            .execute(SessionStorageRequest::OpenDisplaySnapshot {
                session: f.descriptor.session.clone(),
            })
            .unwrap()
        else {
            panic!("missing strict display snapshot");
        };
        assert_eq!(full.state.conversation.messages.len(), 2);
        assert_eq!(full.state.usage.requests.len(), 2);
        assert!(!full.state.conversation.model_context.is_empty());

        // Use a fresh reader and remove only the fixture's disposable index,
        // so a success from either warm layer cannot mask the cold traversal.
        let index_path = f
            .descriptor
            .local_path
            .as_ref()
            .unwrap()
            .join(".storage-catalog-v1.json");
        std::fs::remove_file(&index_path).unwrap();
        let catalog = StorageService::new(kind);
        let list = |audit| SessionStorageRequest::List {
            config_root: f.root.path().to_owned(),
            workspace_path: None,
            audit,
        };
        crate::diagnostics::reset();
        let normal = catalog.execute(list(false)).unwrap();
        assert_eq!(
            crate::diagnostics::get().decoded,
            full.snapshot.revision.session_seq
        );
        let SessionStorageResponse::Sessions { sessions } = &normal else {
            panic!("missing catalog sessions");
        };
        let [summary] = sessions.as_slice() else {
            panic!("expected a single catalog entry");
        };
        assert_eq!(summary.descriptor.session, full.snapshot.session);
        assert_eq!(summary.message_count, full.state.conversation.messages.len());
        assert_eq!(summary.preview.as_deref(), Some("первый чат 🦀"));

        let index: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&index_path).unwrap()).unwrap();
        assert_eq!(
            index["data"]["revision"],
            serde_json::to_value(full.snapshot.revision).unwrap()
        );
        assert_eq!(
            index["data"]["fingerprint"].as_str(),
            Some(full.snapshot.content_sha256.as_str())
        );

        crate::diagnostics::reset();
        assert_eq!(catalog.execute(list(false)).unwrap(), normal);
        assert_eq!(crate::diagnostics::get().decoded, 0);
        assert_eq!(catalog.execute(list(true)).unwrap(), normal);
        assert_eq!(
            crate::diagnostics::get().decoded,
            full.snapshot.revision.session_seq
        );
    }
}
