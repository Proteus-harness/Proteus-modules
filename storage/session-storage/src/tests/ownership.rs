use super::*;
use uuid::Uuid;

#[test]
fn independent_writer_handles_share_lease_until_last_release() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        let SessionStorageResponse::Writer { lease: second, .. } = f
            .service
            .execute(SessionStorageRequest::AcquireWriter {
                session: f.descriptor.session.clone(),
            })
            .unwrap()
        else {
            panic!()
        };
        assert_ne!(second.token, f.lease.token);
        f.service
            .execute(SessionStorageRequest::ReleaseWriter {
                lease: f.lease.clone(),
            })
            .unwrap();
        f.lease = second;
        f.append(vec![text("second handle remains valid")]);
        let outsider = StorageService::new(kind);
        assert!(
            outsider
                .execute(SessionStorageRequest::AcquireWriter {
                    session: f.descriptor.session.clone()
                })
                .is_err()
        );
        f.service
            .execute(SessionStorageRequest::ReleaseWriter { lease: f.lease })
            .unwrap();
        outsider
            .execute(SessionStorageRequest::AcquireWriter {
                session: f.descriptor.session,
            })
            .unwrap();
    }
}

#[test]
fn delete_recreate_fences_old_handles_and_cache_cannot_resurrect() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text("old")]);
        let old = f.snapshot();
        let op = uuid::Uuid::new_v4();
        let delete = SessionStorageRequest::Delete {
            lease: f.lease.clone(),
            operation_id: op,
        };
        f.service.execute(delete.clone()).unwrap();
        assert!(!f.descriptor.local_path.clone().unwrap().exists());
        assert!(matches!(
            f.service
                .execute(SessionStorageRequest::WriteCache {
                    session: f.descriptor.session.clone(),
                    key: "view".into(),
                    source: old,
                    value: serde_json::json!({})
                })
                .unwrap(),
            SessionStorageResponse::CacheWritten { etag: None }
        ));
        assert!(!f.descriptor.local_path.clone().unwrap().exists());
        let SessionStorageResponse::Session { descriptor } = f
            .service
            .execute(SessionStorageRequest::Create {
                config_root: f.root.path().to_owned(),
                workspace_path: f.descriptor.workspace_path.clone(),
                session_id: f.descriptor.session.session_id,
                parent_session_id: None,
            })
            .unwrap()
        else {
            panic!()
        };
        let SessionStorageResponse::Writer { lease, .. } = f
            .service
            .execute(SessionStorageRequest::AcquireWriter {
                session: descriptor.session.clone(),
            })
            .unwrap()
        else {
            panic!()
        };
        assert_ne!(lease.generation, f.lease.generation);
        f.service.execute(delete).unwrap();
        assert!(
            descriptor.local_path.clone().unwrap().exists(),
            "old idempotent delete must not remove recreated session"
        );
        assert!(
            f.service
                .execute(SessionStorageRequest::Resume { lease: f.lease })
                .is_err()
        );
    }
}

#[test]
fn selecting_another_backend_never_silently_opens_existing_history_as_empty() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text("must not disappear")]);
        let session = f.descriptor.session.clone();
        drop(f.service);
        let other = StorageService::new(match kind {
            BackendKind::Jsonl => BackendKind::Sqlite,
            BackendKind::Sqlite => BackendKind::Jsonl,
        });
        assert!(
            other
                .execute(SessionStorageRequest::Open {
                    path: f.descriptor.local_path.clone().unwrap(),
                })
                .is_err()
        );
        assert!(
            other
                .execute(SessionStorageRequest::Create {
                    config_root: f.root.path().to_owned(),
                    workspace_path: f.descriptor.workspace_path.clone(),
                    session_id: session.session_id,
                    parent_session_id: None,
                })
                .is_err()
        );
        assert!(
            other
                .execute(SessionStorageRequest::OpenSnapshot {
                    session: session.clone()
                })
                .is_err()
        );
        assert!(
            other
                .execute(SessionStorageRequest::AcquireWriter { session })
                .is_err()
        );
    }
}

#[test]
fn lazy_create_locator_survives_service_restart_before_first_writer() {
    for kind in backends() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let parent = proteus_contracts::domain::new_session_id();
        let service = StorageService::new(kind);
        let SessionStorageResponse::Session { descriptor } = service
            .execute(SessionStorageRequest::Create {
                config_root: root.path().into(),
                workspace_path: workspace.path().into(),
                session_id: proteus_contracts::domain::new_session_id(),
                parent_session_id: Some(parent),
            })
            .unwrap()
        else {
            panic!()
        };
        assert!(!descriptor.local_path.as_ref().unwrap().exists());
        drop(service);
        let service = StorageService::new(kind);
        assert_eq!(
            service
                .execute(SessionStorageRequest::Describe {
                    session: descriptor.session.clone()
                })
                .unwrap(),
            SessionStorageResponse::Session {
                descriptor: descriptor.clone()
            }
        );
        let SessionStorageResponse::Writer { lease, revision } = service
            .execute(SessionStorageRequest::AcquireWriter {
                session: descriptor.session.clone(),
            })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(revision, StorageRevision::default());
        let SessionStorageResponse::Session { descriptor: opened } = service
            .execute(SessionStorageRequest::Open {
                path: descriptor.local_path.unwrap(),
            })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(opened.session, descriptor.session);
        assert_eq!(opened.parent_session_id, Some(parent));
        service
            .execute(SessionStorageRequest::ReleaseWriter { lease })
            .unwrap();
    }
}

#[test]
fn retired_precreate_locator_cannot_resurrect_and_fresh_creates_share_one_incarnation() {
    for kind in backends() {
        let f = Fixture::new(kind);
        let old = f.descriptor.session.clone();
        let path = f.descriptor.local_path.clone().unwrap();
        f.service
            .execute(SessionStorageRequest::Delete {
                lease: f.lease.clone(),
                operation_id: Uuid::new_v4(),
            })
            .unwrap();
        drop(f.service);
        let first = StorageService::new(kind);
        assert!(
            first
                .execute(SessionStorageRequest::AcquireWriter { session: old })
                .is_err()
        );
        assert!(
            !path.exists(),
            "stale pre-create locator must not mkdir after delete"
        );
        let create = SessionStorageRequest::Create {
            config_root: f.root.path().into(),
            workspace_path: f.descriptor.workspace_path,
            session_id: f.descriptor.session.session_id,
            parent_session_id: None,
        };
        let SessionStorageResponse::Session { descriptor: a } =
            first.execute(create.clone()).unwrap()
        else {
            panic!()
        };
        let second = StorageService::new(kind);
        let SessionStorageResponse::Session { descriptor: b } = second.execute(create).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            a.session, b.session,
            "concurrent fresh reservations name one incarnation"
        );
        assert!(!path.exists());
        // A fresh reservation itself survives another service restart.
        drop(first);
        let first = StorageService::new(kind);
        let SessionStorageResponse::Writer { lease, revision } = first
            .execute(SessionStorageRequest::AcquireWriter {
                session: a.session.clone(),
            })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(revision, StorageRevision::default());
        assert!(
            second
                .execute(SessionStorageRequest::AcquireWriter {
                    session: b.session.clone()
                })
                .is_err()
        );
        let SessionStorageResponse::Session { descriptor: opened } = first
            .execute(SessionStorageRequest::Open { path: path.clone() })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(opened.session, a.session);
        let mut committed = StorageCommit {
            lease: lease.clone(),
            operation_id: new_record_id(),
            expected: StorageRevision::default(),
            timestamp_ms: 1,
            attribution: StorageRecordAttribution {
                execution_id: None,
                thread_id: Some(f.thread),
                turn_id: None,
            },
            entry: JournalEntry::HistoryMutated(HistoryMutated {
                previous_revision: 0,
                new_revision: 1,
                mutation: HistoryMutationKind::Append,
                messages: vec![text("first fresh creator committed")],
                compaction: None,
                tool_results: vec![],
            }),
        };
        first
            .execute(SessionStorageRequest::Commit {
                commit: committed.clone(),
            })
            .unwrap();
        first
            .execute(SessionStorageRequest::ReleaseWriter { lease })
            .unwrap();
        let SessionStorageResponse::Writer { lease, revision } = second
            .execute(SessionStorageRequest::AcquireWriter { session: b.session })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(revision.session_seq, 1);
        assert_eq!(revision.history_revision, 1);
        committed.lease = lease.clone();
        committed.operation_id = new_record_id();
        assert!(
            second
                .execute(SessionStorageRequest::Commit { commit: committed })
                .is_err(),
            "another creator's stale expected revision must not overwrite canonical state"
        );
        second
            .execute(SessionStorageRequest::Delete {
                lease,
                operation_id: Uuid::new_v4(),
            })
            .unwrap();
        assert!(!path.exists());
        let remaining = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            remaining.len(),
            2,
            "only persistent lock and deletion ledger remain: {remaining:?}"
        );
        assert!(remaining.iter().all(|name| name.starts_with('.')
            && (name.ends_with(".lock") || name.ends_with(".storage-control.json"))));
    }
}

#[test]
fn pending_delete_receipt_after_crash_fences_acquire_until_reconciled() {
    for kind in backends() {
        let f = Fixture::new(kind);
        let path = f.descriptor.local_path.clone().unwrap();
        let operation_id = Uuid::new_v4();
        let delete = SessionStorageRequest::Delete {
            lease: f.lease.clone(),
            operation_id,
        };
        f.service.execute(delete.clone()).unwrap();
        // Simulate crash after directory removal but before the completed
        // tombstone replace; this is the durable predecessor of the final ACK.
        let control_path = path.parent().unwrap().join(format!(
            ".{}.storage-control.json",
            path.file_name().unwrap().to_string_lossy()
        ));
        let mut control: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&control_path).unwrap()).unwrap();
        control["deleted"] = serde_json::json!(false);
        control["deletes"][operation_id.to_string()]["complete"] = serde_json::json!(false);
        std::fs::write(control_path, serde_json::to_vec(&control).unwrap()).unwrap();
        drop(f.service);
        let cold = StorageService::new(kind);
        assert!(
            cold.execute(SessionStorageRequest::AcquireWriter {
                session: f.descriptor.session
            })
            .is_err()
        );
        assert!(!path.exists());
        cold.execute(delete.clone()).unwrap();
        cold.execute(delete).unwrap();
        assert!(
            !path.exists(),
            "reconciliation cannot recreate a deleted session"
        );
    }
}

#[test]
fn delete_requires_the_only_live_writer_handle_but_receipt_retries_remain_idempotent() {
    for kind in backends() {
        let f = Fixture::new(kind);
        let SessionStorageResponse::Writer { lease: other, .. } = f
            .service
            .execute(SessionStorageRequest::AcquireWriter {
                session: f.descriptor.session.clone(),
            })
            .unwrap()
        else {
            panic!()
        };
        let delete = SessionStorageRequest::Delete {
            lease: f.lease.clone(),
            operation_id: Uuid::new_v4(),
        };
        let error = f.service.execute(delete.clone()).unwrap_err();
        assert!(error.to_string().contains("other live writer handles"));
        assert!(f.descriptor.local_path.as_ref().unwrap().exists());
        f.service
            .execute(SessionStorageRequest::Resume {
                lease: f.lease.clone(),
            })
            .unwrap();
        f.service
            .execute(SessionStorageRequest::ReleaseWriter { lease: other })
            .unwrap();
        f.service.execute(delete.clone()).unwrap();
        f.service.execute(delete).unwrap();
        assert!(!f.descriptor.local_path.as_ref().unwrap().exists());
    }
}

#[test]
fn metadata_only_empty_sessions_allow_either_format_without_confusing_legacy_view_cache() {
    let f = Fixture::new(BackendKind::Jsonl);
    let path = f.descriptor.local_path.clone().unwrap();
    f.service
        .execute(SessionStorageRequest::ReleaseWriter { lease: f.lease })
        .unwrap();
    assert!(!path.join("journal.jsonl").exists());
    // This disposable legacy cache is not the canonical SQLite journal.
    std::fs::write(
        path.join(".read-view.sqlite"),
        b"legacy disposable view cache",
    )
    .unwrap();
    let sqlite = StorageService::new(BackendKind::Sqlite);
    let SessionStorageResponse::Session { descriptor } = sqlite
        .execute(SessionStorageRequest::Create {
            config_root: f.root.path().to_owned(),
            workspace_path: f.descriptor.workspace_path,
            session_id: f.descriptor.session.session_id,
            parent_session_id: None,
        })
        .unwrap()
    else {
        panic!()
    };
    let SessionStorageResponse::Writer { lease, revision } = sqlite
        .execute(SessionStorageRequest::AcquireWriter {
            session: descriptor.session,
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(revision, StorageRevision::default());
    assert!(path.join("journal.sqlite").is_file());
    assert!(!path.join("journal.jsonl").exists());
    sqlite
        .execute(SessionStorageRequest::ReleaseWriter { lease })
        .unwrap();
}
