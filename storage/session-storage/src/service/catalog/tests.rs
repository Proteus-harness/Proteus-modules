//! Deterministically interleave a completed, unpublished cold replay with real writes.
use super::*;
use crate::tests::{Fixture, backends, text};

fn pending(f: &Fixture) -> (PathBuf, CatalogRead, Projection) {
    let dir = f.descriptor.local_path.clone().unwrap();
    let descriptor = sessions::open(f.service.kind, &dir).unwrap();
    let read = CatalogRead {
        descriptor,
        stamps: source_stamp(&dir, f.service.kind).unwrap(),
        blobs: durable::blob_stamps(&dir),
    };
    let result = read.replay(f.service.kind, &dir).unwrap();
    (dir, read, result)
}

#[test]
fn intervening_commit_revalidates_before_publishing_a_cold_catalog() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text("old")]);
        let (dir, read, old) = pending(&f);
        assert_eq!(old.0.message_count, 1);
        f.append(vec![text("new")]);
        let current = f
            .service
            .finish_catalog(&dir, read, Ok(old), false)
            .unwrap();
        assert_eq!(current.message_count, 2);
        let cold = StorageService::new(kind);
        crate::diagnostics::reset();
        assert_eq!(cold.catalog_summary(&dir, false).unwrap(), current);
        assert_eq!(crate::diagnostics::get().decoded, 0);
    }
}

#[test]
fn retired_replay_cannot_publish_into_a_recreated_session() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text("retired")]);
        let (dir, read, old) = pending(&f);
        f.service
            .execute(SessionStorageRequest::Delete {
                lease: f.lease.clone(),
                operation_id: Uuid::new_v4(),
            })
            .unwrap();
        assert!(!dir.exists());
        let SessionStorageResponse::Session { descriptor } = f
            .service
            .execute(SessionStorageRequest::Create {
                config_root: f.root.path().into(),
                workspace_path: f.descriptor.workspace_path.clone(),
                session_id: f.descriptor.session.session_id,
                parent_session_id: None,
            })
            .unwrap()
        else {
            panic!("expected session")
        };
        assert_ne!(descriptor.session, read.descriptor.session);
        f.service
            .execute(SessionStorageRequest::AcquireWriter {
                session: descriptor.session,
            })
            .unwrap();
        let index = dir.join(".storage-catalog-v1.json");
        let before = std::fs::read(&index).ok();
        assert!(
            f.service
                .finish_catalog(&dir, read, Ok(old), false)
                .is_err()
        );
        assert_eq!(before, std::fs::read(&index).ok());
        assert_eq!(
            f.service.catalog_summary(&dir, true).unwrap().message_count,
            0
        );
    }
}
