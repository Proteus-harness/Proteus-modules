use super::*;
use proteus_contracts::domain::{AgentTask, TurnId, new_execution_id, new_turn_id};

fn admit(f: &mut Fixture) -> TurnId {
    let turn = new_turn_id();
    let commit = StorageCommit {
        lease: f.lease.clone(),
        operation_id: new_record_id(),
        expected: f.revision,
        timestamp_ms: 42,
        attribution: StorageRecordAttribution {
            execution_id: Some(new_execution_id()),
            thread_id: Some(f.thread),
            turn_id: Some(turn),
        },
        entry: JournalEntry::TurnOpened(TurnOpened {
            task: AgentTask::new("hot context lookup", f.descriptor.workspace_path.clone()),
            intent: None,
            base_history_revision: f.revision.history_revision,
            module_epoch: 0,
            config_snapshot: serde_json::json!({}),
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
    turn
}

#[test]
fn exact_verified_resident_context_lookup_avoids_decoding_and_preserves_old_snapshot_semantics() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        for _ in 0..8 {
            f.append(vec![text(&"H".repeat(256 * 1024))]);
        }
        let first_turn = admit(&mut f);
        let old = f.snapshot();
        let first = SessionStorageRequest::ReadModelContextBeforeTurn {
            snapshot: old.clone(),
            turn_id: first_turn,
        };
        crate::diagnostics::reset();
        let expected = f.service.execute(first.clone()).unwrap();
        assert_eq!(
            expected,
            SessionStorageResponse::ModelContext {
                observations: vec![]
            }
        );
        assert_eq!(
            crate::diagnostics::get().decoded,
            0,
            "exact verified resident revision must not rescan the journal"
        );
        assert_eq!(crate::diagnostics::get().fingerprint_bytes, 0);
        assert_eq!(f.service.execute(first.clone()).unwrap(), expected);
        assert_eq!(crate::diagnostics::get().decoded, 0);

        let second_turn = admit(&mut f);
        crate::diagnostics::reset();
        assert_eq!(
            f.service.execute(first).unwrap(),
            expected,
            "newer resident state cannot replace old snapshot semantics"
        );
        assert_eq!(crate::diagnostics::get().decoded, 9);
        let current = f.snapshot();
        crate::diagnostics::reset();
        f.service
            .execute(SessionStorageRequest::ReadModelContextBeforeTurn {
                snapshot: current.clone(),
                turn_id: second_turn,
            })
            .unwrap();
        assert_eq!(crate::diagnostics::get().decoded, 0);
        assert!(
            f.service
                .execute(SessionStorageRequest::ReadModelContextBeforeTurn {
                    snapshot: current,
                    turn_id: first_turn,
                })
                .is_err(),
            "only the latest admitted turn has a before-turn projection"
        );

        // A warm descriptor is provisional: it must validate the fixed prefix
        // before resident state can serve canonical observations.
        let warm = f.snapshot();
        crate::diagnostics::reset();
        f.service
            .execute(SessionStorageRequest::ReadModelContextBeforeTurn {
                snapshot: warm,
                turn_id: second_turn,
            })
            .unwrap();
        assert_eq!(crate::diagnostics::get().decoded, 10);
        f.service
            .execute(SessionStorageRequest::ReleaseWriter {
                lease: f.lease.clone(),
            })
            .unwrap();
        crate::diagnostics::reset();
        f.service
            .execute(SessionStorageRequest::ReadModelContextBeforeTurn {
                snapshot: old,
                turn_id: first_turn,
            })
            .unwrap();
        assert_eq!(
            crate::diagnostics::get().decoded,
            9,
            "cold/nonresident behavior stays strict"
        );
    }
}

#[test]
fn lease_bound_runtime_context_read_needs_no_snapshot_and_recovers_only_after_hibernate() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        for _ in 0..8 {
            f.append(vec![text(&"H".repeat(256 * 1024))]);
        }
        let turn_id = admit(&mut f);
        let request = SessionStorageRequest::ReadWriterModelContextBeforeTurn {
            lease: f.lease.clone(),
            turn_id,
        };
        crate::diagnostics::reset();
        let expected = f.service.execute(request.clone()).unwrap();
        assert_eq!(
            expected,
            SessionStorageResponse::ModelContext {
                observations: vec![]
            }
        );
        assert_eq!(crate::diagnostics::get().decoded, 0);
        assert_eq!(crate::diagnostics::get().fingerprint_bytes, 0);
        for _ in 0..10 {
            assert_eq!(f.service.execute(request.clone()).unwrap(), expected);
        }
        assert_eq!(
            crate::diagnostics::get().decoded,
            0,
            "ordinary runtime reads must never create/scan snapshots"
        );
        f.service
            .execute(SessionStorageRequest::HibernateWriter {
                lease: f.lease.clone(),
            })
            .unwrap();
        crate::diagnostics::reset();
        assert_eq!(f.service.execute(request.clone()).unwrap(), expected);
        assert_eq!(
            crate::diagnostics::get().decoded,
            9,
            "hibernated writer reloads its canonical state once"
        );
        crate::diagnostics::reset();
        assert_eq!(f.service.execute(request).unwrap(), expected);
        assert_eq!(crate::diagnostics::get().decoded, 0);
        let mut invalid = f.lease;
        invalid.token = uuid::Uuid::new_v4();
        assert_eq!(
            f.service
                .execute(SessionStorageRequest::ReadWriterModelContextBeforeTurn {
                    lease: invalid,
                    turn_id,
                })
                .unwrap_err()
                .downcast_ref::<SessionStorageFailure>(),
            Some(&SessionStorageFailure::InvalidWriterLease)
        );
    }
}
