use super::*;

#[test]
fn metadata_cas_receipts_survive_reopen_and_reject_conflicts() {
    for kind in backends() {
        let f = Fixture::new(kind);
        let op = uuid::Uuid::new_v4();
        let request = SessionStorageRequest::CompareExchangeMetadata {
            lease: f.lease.clone(),
            operation_id: op,
            key: "settings".into(),
            expected_etag: None,
            value: Some(serde_json::json!({"theme":"dark"})),
        };
        let result = f.service.execute(request.clone()).unwrap();
        assert_eq!(result, f.service.execute(request.clone()).unwrap());
        let mut changed = request.clone();
        let SessionStorageRequest::CompareExchangeMetadata { value, .. } = &mut changed else {
            panic!()
        };
        *value = Some(serde_json::json!({"theme":"other"}));
        assert!(f.service.execute(changed).is_err());
        drop(f.service);
        let service = StorageService::new(kind);
        let SessionStorageResponse::Writer { lease, .. } = service
            .execute(SessionStorageRequest::AcquireWriter {
                session: f.descriptor.session,
            })
            .unwrap()
        else {
            panic!()
        };
        let mut request = request;
        let SessionStorageRequest::CompareExchangeMetadata { lease: target, .. } = &mut request
        else {
            panic!()
        };
        *target = lease;
        assert_eq!(result, service.execute(request).unwrap());
    }
}

#[test]
fn cache_is_disposable_source_bound_and_does_not_acquire_writer() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text("one")]);
        let old = f.snapshot();
        f.service
            .execute(SessionStorageRequest::ReleaseWriter {
                lease: f.lease.clone(),
            })
            .unwrap();
        let cached = f
            .service
            .execute(SessionStorageRequest::WriteCache {
                session: f.descriptor.session.clone(),
                key: "view.head".into(),
                source: old.clone(),
                value: serde_json::json!({"title":"cached"}),
            })
            .unwrap();
        assert!(matches!(
            cached,
            SessionStorageResponse::CacheWritten { etag: Some(_) }
        ));
        let equal_prefix = f.snapshot();
        assert_ne!(old.snapshot_id, equal_prefix.snapshot_id);
        assert!(matches!(
            f.service
                .execute(SessionStorageRequest::ReadCache {
                    session: f.descriptor.session.clone(),
                    key: "view.head".into(),
                    source: Some(equal_prefix)
                })
                .unwrap(),
            SessionStorageResponse::Metadata { document: Some(_) }
        ));
        let outsider = StorageService::new(kind);
        let SessionStorageResponse::Writer { lease, .. } = outsider
            .execute(SessionStorageRequest::AcquireWriter {
                session: f.descriptor.session.clone(),
            })
            .unwrap()
        else {
            panic!()
        };
        outsider
            .execute(SessionStorageRequest::ReleaseWriter { lease })
            .unwrap();
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
        f.append(vec![text("two")]);
        let current = f.snapshot();
        assert!(matches!(
            f.service
                .execute(SessionStorageRequest::ReadCache {
                    session: f.descriptor.session.clone(),
                    key: "view.head".into(),
                    source: Some(current)
                })
                .unwrap(),
            SessionStorageResponse::Metadata { document: None }
        ));
        assert!(matches!(
            f.service
                .execute(SessionStorageRequest::ReadCache {
                    session: f.descriptor.session.clone(),
                    key: "view.head".into(),
                    source: None
                })
                .unwrap(),
            SessionStorageResponse::Metadata { document: Some(_) }
        ));
        assert!(matches!(
            f.service
                .execute(SessionStorageRequest::WriteCache {
                    session: f.descriptor.session,
                    key: "view.head".into(),
                    source: old,
                    value: serde_json::json!({})
                })
                .unwrap(),
            SessionStorageResponse::CacheWritten { etag: None }
        ));
    }
}

#[test]
fn metadata_imports_old_filename_once_and_deletion_does_not_resurrect_it() {
    for kind in backends() {
        let f = Fixture::new(kind);
        let file = f
            .descriptor
            .local_path
            .clone()
            .unwrap()
            .join("model-selection.json");
        let original = b"{\"model\":\"before\"}";
        std::fs::write(&file, original).unwrap();
        let SessionStorageResponse::Metadata {
            document: Some(old),
        } = f
            .service
            .execute(SessionStorageRequest::ReadMetadata {
                session: f.descriptor.session.clone(),
                key: "model-selection.json".into(),
            })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(old.value, serde_json::json!({"model":"before"}));
        let SessionStorageResponse::Metadata {
            document: Some(new),
        } = f
            .service
            .execute(SessionStorageRequest::CompareExchangeMetadata {
                lease: f.lease.clone(),
                operation_id: uuid::Uuid::new_v4(),
                key: "model-selection.json".into(),
                expected_etag: Some(old.etag),
                value: Some(serde_json::json!({"model":"after"})),
            })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            std::fs::read(&file).unwrap(),
            original,
            "import must not rewrite old source"
        );
        f.service
            .execute(SessionStorageRequest::CompareExchangeMetadata {
                lease: f.lease,
                operation_id: uuid::Uuid::new_v4(),
                key: "model-selection.json".into(),
                expected_etag: Some(new.etag),
                value: None,
            })
            .unwrap();
        assert_eq!(
            f.service
                .execute(SessionStorageRequest::ReadMetadata {
                    session: f.descriptor.session,
                    key: "model-selection.json".into()
                })
                .unwrap(),
            SessionStorageResponse::Metadata { document: None }
        );
    }
}

#[test]
fn durable_catalog_reopens_without_decoding_and_canonical_reads_remain_strict() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text(&"long history ".repeat(30_000))]);
        let request = SessionStorageRequest::List {
            config_root: f.root.path().into(),
            workspace_path: None,
            audit: false,
        };
        let expected = f.service.execute(request.clone()).unwrap();
        let cold = StorageService::new(kind);
        crate::diagnostics::reset();
        assert_eq!(cold.execute(request).unwrap(), expected);
        assert_eq!(crate::diagnostics::get().decoded, 0);
        assert_eq!(crate::diagnostics::get().fingerprint_bytes, 0);
        let SessionStorageResponse::Snapshot { snapshot } = cold
            .execute(SessionStorageRequest::OpenSnapshot {
                session: f.descriptor.session.clone(),
            })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(crate::diagnostics::get().decoded, 0);
        let SessionStorageResponse::Conversation { conversation } = cold
            .execute(SessionStorageRequest::ReadConversation {
                snapshot,
                point: StorageHistoryPoint::Current,
            })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(conversation.messages.len(), 1);
        assert!(crate::diagnostics::get().decoded > 0);
    }
}

#[test]
fn durable_catalog_discards_damaged_index_and_rechecks_audit() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text("unchanged title")]);
        let config_root = f.root.path().to_owned();
        let list = |audit| SessionStorageRequest::List {
            config_root: config_root.clone(),
            workspace_path: None,
            audit,
        };
        let expected = f.service.execute(list(false)).unwrap();
        let dir = f.descriptor.local_path.as_ref().unwrap();
        std::fs::write(dir.join(".storage-catalog-v1.json"), b"{invalid").unwrap();
        crate::diagnostics::reset();
        assert_eq!(
            StorageService::new(kind).execute(list(false)).unwrap(),
            expected
        );
        assert!(crate::diagnostics::get().decoded > 0);
        crate::diagnostics::reset();
        assert_eq!(
            StorageService::new(kind).execute(list(true)).unwrap(),
            expected
        );
        assert!(crate::diagnostics::get().decoded > 0);
        f.append(vec![text("updated history")]);
        let SessionStorageResponse::Sessions { sessions } =
            StorageService::new(kind).execute(list(false)).unwrap()
        else {
            panic!()
        };
        assert_eq!(sessions[0].message_count, 2);
    }
}

#[test]
fn compacted_catalog_reopens_and_invalidates_when_a_shared_chunk_is_corrupt() {
    let mut f = Fixture::new(BackendKind::Jsonl);
    f.append(vec![text(&"shared history ".repeat(30_000))]);
    let dir = f.descriptor.local_path.as_ref().unwrap().clone();
    f.service
        .execute(SessionStorageRequest::ReleaseWriter {
            lease: f.lease.clone(),
        })
        .unwrap();
    crate::archive::compact_jsonl_archive(&dir).unwrap();
    let list = SessionStorageRequest::List {
        config_root: f.root.path().into(),
        workspace_path: None,
        audit: false,
    };
    crate::diagnostics::reset();
    let SessionStorageResponse::Sessions { sessions } = StorageService::new(BackendKind::Jsonl)
        .execute(list.clone())
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(sessions.len(), 1);
    assert_eq!(crate::diagnostics::get().decoded, 0);
    let chunk = std::fs::read_dir(dir.join("blobs/shared"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let bytes = std::fs::read(&chunk).unwrap();
    std::fs::write(chunk, vec![b'!'; bytes.len()]).unwrap();
    let SessionStorageResponse::Sessions { sessions } = StorageService::new(BackendKind::Jsonl)
        .execute(list)
        .unwrap()
    else {
        panic!()
    };
    assert!(sessions.is_empty());
}

#[test]
fn durable_catalog_rejects_rewritten_blob_even_with_unchanged_size_and_mtime() {
    let mut f = Fixture::new(BackendKind::Jsonl);
    f.append(vec![text(&"large payload ".repeat(30_000))]);
    let list = SessionStorageRequest::List {
        config_root: f.root.path().into(),
        workspace_path: None,
        audit: false,
    };
    f.service.execute(list.clone()).unwrap();
    let blob = std::fs::read_dir(f.descriptor.local_path.as_ref().unwrap().join("blobs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let old_modified = std::fs::metadata(&blob).unwrap().modified().unwrap();
    let mut bytes = std::fs::read(&blob).unwrap();
    bytes[0] = b'!';
    std::fs::write(&blob, bytes).unwrap();
    std::fs::File::open(&blob)
        .unwrap()
        .set_modified(old_modified)
        .unwrap();
    let SessionStorageResponse::Sessions { sessions } = StorageService::new(BackendKind::Jsonl)
        .execute(list)
        .unwrap()
    else {
        panic!()
    };
    assert!(sessions.is_empty());
}

#[test]
fn catalog_cache_invalidates_after_committed_history_changes() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text("first title")]);
        let request = SessionStorageRequest::List {
            config_root: f.root.path().into(),
            workspace_path: None,
            audit: false,
        };
        let SessionStorageResponse::Sessions { sessions } =
            f.service.execute(request.clone()).unwrap()
        else {
            panic!()
        };
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].message_count, 1);
        assert_eq!(sessions[0].preview.as_deref(), Some("first title"));
        f.append(vec![text("later conversation")]);
        let SessionStorageResponse::Sessions { sessions } = f.service.execute(request).unwrap()
        else {
            panic!()
        };
        assert_eq!(sessions[0].message_count, 2);
        assert_eq!(sessions[0].preview.as_deref(), Some("first title"));
        let record_path = f.descriptor.record_location.as_ref().unwrap();
        let SessionStorageResponse::Session { descriptor } = f
            .service
            .execute(SessionStorageRequest::Open {
                path: record_path.clone(),
            })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(descriptor.session, f.descriptor.session);
    }
}

#[test]
fn generic_blob_api_preserves_legacy_image_paths_and_reads_unknown_sizes_by_hash() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    for kind in backends() {
        let f = Fixture::new(kind);
        let bytes = b"\x89PNG\r\n\x1a\nsynthetic-image";
        let blob = StorageBlobRef {
            sha256: crate::io::sha256_hex(bytes),
            bytes: bytes.len() as u64,
        };
        let request = SessionStorageRequest::PutBlob {
            lease: f.lease.clone(),
            operation_id: uuid::Uuid::new_v4(),
            data_base64: STANDARD.encode(bytes),
            expected: blob.clone(),
        };
        let SessionStorageResponse::Blob {
            blob: stored,
            local_path: Some(path),
        } = f.service.execute(request.clone()).unwrap()
        else {
            panic!()
        };
        assert_eq!(stored, blob);
        assert_eq!(
            path,
            f.descriptor
                .local_path
                .as_ref()
                .unwrap()
                .join("images")
                .join(&blob.sha256)
        );
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert_eq!(
            f.service.execute(request).unwrap(),
            SessionStorageResponse::Blob {
                blob: blob.clone(),
                local_path: Some(
                    f.descriptor
                        .local_path
                        .as_ref()
                        .unwrap()
                        .join("images")
                        .join(&blob.sha256)
                )
            }
        );
        let read = SessionStorageRequest::ReadBlobByHash {
            session: f.descriptor.session.clone(),
            sha256: blob.sha256.clone(),
            max_bytes: 5 * 1024 * 1024,
        };
        assert_eq!(
            f.service.execute(read).unwrap(),
            SessionStorageResponse::BlobData {
                blob: blob.clone(),
                data_base64: STANDARD.encode(bytes)
            }
        );
        assert!(
            f.service
                .execute(SessionStorageRequest::ReadBlobByHash {
                    session: f.descriptor.session,
                    sha256: blob.sha256,
                    max_bytes: 1
                })
                .is_err()
        );
    }
}

#[test]
fn normal_catalog_skips_corrupt_entries_but_audit_reports_them() {
    for kind in backends() {
        for corrupt_identity in [false, true] {
            let mut f = Fixture::new(kind);
            f.append(vec![text("this entry will become corrupt")]);
            let list = |audit| SessionStorageRequest::List {
                config_root: f.root.path().to_owned(),
                workspace_path: None,
                audit,
            };
            // Prime the warm catalog before the backing files change.
            f.service.execute(list(false)).unwrap();
            let SessionStorageResponse::Session { descriptor: valid } = f
                .service
                .execute(SessionStorageRequest::Create {
                    config_root: f.root.path().to_owned(),
                    workspace_path: f.descriptor.workspace_path.clone(),
                    session_id: new_session_id(),
                    parent_session_id: None,
                })
                .unwrap()
            else {
                panic!()
            };
            let SessionStorageResponse::Writer { lease, .. } = f
                .service
                .execute(SessionStorageRequest::AcquireWriter {
                    session: valid.session.clone(),
                })
                .unwrap()
            else {
                panic!()
            };
            f.service
                .execute(SessionStorageRequest::ReleaseWriter { lease })
                .unwrap();
            f.service
                .execute(SessionStorageRequest::ReleaseWriter {
                    lease: f.lease.clone(),
                })
                .unwrap();
            let corrupt = if corrupt_identity {
                f.descriptor
                    .local_path
                    .as_ref()
                    .unwrap()
                    .join("session.json")
            } else {
                f.descriptor.record_location.clone().unwrap()
            };
            std::fs::write(corrupt, b"definitely corrupt complete record\n").unwrap();
            let SessionStorageResponse::Sessions { sessions } =
                f.service.execute(list(false)).unwrap()
            else {
                panic!()
            };
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].descriptor.session, valid.session);
            assert!(f.service.execute(list(true)).is_err());
        }
    }
}
