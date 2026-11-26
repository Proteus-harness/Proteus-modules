//! Exercise catalog persistence and interleave a detached replay with real writes.
use super::*;
use crate::tests::{Fixture, backends, text};

#[test]
fn resident_catalog_uses_observed_descriptor_with_the_cached_projection() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text("one verified resident message")]);
        let dir = f.descriptor.local_path.as_ref().unwrap();
        let expected = f.service.catalog_summary(dir, false).unwrap();
        let index = dir.join(".storage-catalog-v1.json");
        // There must be no disk hit to mask an accidentally missed resident.
        std::fs::remove_file(&index).unwrap();
        {
            let mut state = f.service.lock_state().unwrap();
            let cached = state.catalog.get_mut(&expected.descriptor.session).unwrap();
            // Model older descriptor data in the disposable, verified cache.
            // The source journal, locator and source stamps are untouched.
            cached.summary.descriptor.created_at_ms ^= 1;
            cached.summary.descriptor.updated_at_ms ^= 1;
            assert_ne!(cached.summary.descriptor, expected.descriptor);
        }
        crate::diagnostics::reset();
        let hit = f.service.catalog_summary(dir, false).unwrap();
        assert_eq!(hit, expected);
        assert_eq!(crate::diagnostics::get().decoded, 0);
        assert!(!index.exists(), "resident hit does not rewrite the index");
    }
}

#[test]
fn warm_writer_reuses_stamp_storage_without_accepting_changed_metadata() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text("ordinary first record")]);
        let dir = f.descriptor.local_path.clone().unwrap();
        let observe = |fixture: &Fixture| {
            let state = fixture.service.lock_state().unwrap();
            let head = state.writers[&fixture.lease.session].head.as_ref().unwrap();
            assert!(head.stamps == source_stamp(&dir, kind).unwrap());
            (head.stamps.as_ptr(), head.extent)
        };
        let (allocation, first_extent) = observe(&f);
        f.append(vec![text("ordinary second record")]);
        let (next_allocation, second_extent) = observe(&f);
        assert_eq!(next_allocation, allocation);
        assert!(second_extent > first_extent);

        // Change only the synthetic session marker's mtime, not its content
        // or any journal/WAL bytes. A stale head must still be discarded.
        let marker = std::fs::OpenOptions::new()
            .write(true)
            .open(dir.join("session.json"))
            .unwrap();
        let before = marker.metadata().unwrap().modified().unwrap();
        marker
            .set_modified(before - std::time::Duration::from_secs(3600))
            .unwrap();
        assert_ne!(marker.metadata().unwrap().modified().unwrap(), before);
        drop(marker);
        f.append(vec![text("ordinary third record")]);
        let state = f.service.lock_state().unwrap();
        assert!(state.writers[&f.lease.session].head.is_none());
    }
}

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
fn inventory_keeps_relative_paths_for_siblings_and_nested_entries() {
    use durable::{blob_stamp_digest, blob_stamps};
    use std::collections::BTreeMap;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("blobs");
    let files = [
        ("short", b"x".as_slice()),
        ("longer Ω name", b"xyz".as_slice()),
        ("nested/coincident", b"abc".as_slice()),
        ("nested/deeper/coincident", b"abcde".as_slice()),
    ];
    let folders = ["nested", "nested/deeper"];
    for folder in folders {
        std::fs::create_dir_all(root.join(folder)).unwrap();
    }
    for (file, bytes) in files {
        std::fs::write(root.join(file), bytes).unwrap();
    }
    let expected: BTreeMap<_, _> = folders
        .into_iter()
        .chain(files.map(|(file, _)| file))
        .map(|relative| {
            let path = PathBuf::from(relative);
            let metadata = std::fs::symlink_metadata(root.join(&path)).unwrap();
            (path, file_stamp(&metadata).unwrap())
        })
        .collect();
    for location in [
        dir.path().to_path_buf(),
        dir.path().join(""),
        dir.path().join(".").join(""),
    ] {
        let observed = blob_stamps(&location).unwrap();
        assert!(observed == expected);
        assert_eq!(blob_stamp_digest(&observed), blob_stamp_digest(&expected));
    }
}

#[cfg(unix)]
#[test]
fn inventory_rejects_child_symlinks_without_following_either_target() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("blobs");
    let nested = root.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(root.join("target"), b"file").unwrap();
    assert!(durable::blob_stamps(dir.path()).is_some());
    for target in [root.join("target"), nested] {
        let link = root.join("link");
        symlink(target, &link).unwrap();
        assert!(durable::blob_stamps(dir.path()).is_none());
        std::fs::remove_file(link).unwrap();
        assert!(durable::blob_stamps(dir.path()).is_some());
    }
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

#[test]
fn refreshing_a_full_catalog_preserves_other_residents_and_renews_eviction_order() {
    let (_, _, (summary, prefix)) = pending(&Fixture::new(BackendKind::Jsonl));
    let service = StorageService::new(BackendKind::Jsonl);
    let mut state = service.lock_state().unwrap();
    let key = |i: usize| SessionLocator {
        key: format!("resident-{i}"),
        ..summary.descriptor.session.clone()
    };
    let remember = |state: &mut State, i, seq| {
        let mut summary = summary.clone();
        summary.descriptor.session = key(i);
        summary.message_count = seq;
        let mut prefix = prefix.clone();
        prefix.revision.session_seq = seq as u64;
        state.remember_prefix(key(i), prefix);
        state.remember_summary(Vec::new(), summary);
    };
    for i in 0..MAX_CATALOG_ENTRIES {
        remember(&mut state, i, 0);
    }
    remember(&mut state, 0, 1);
    remember(&mut state, 0, 2);
    assert_eq!(
        (state.catalog.len(), state.prefixes.len()),
        (MAX_CATALOG_ENTRIES, MAX_CATALOG_ENTRIES),
        "a refresh must not evict any other resident"
    );
    assert_eq!(state.catalog[&key(0)].summary.message_count, 2);
    assert_eq!(state.prefixes[&key(0)].revision.session_seq, 2);

    remember(&mut state, MAX_CATALOG_ENTRIES, 3);
    let expected: VecDeque<_> = (2..MAX_CATALOG_ENTRIES)
        .chain([0, MAX_CATALOG_ENTRIES])
        .map(key)
        .collect();
    assert_eq!(state.catalog_order, expected);
    assert_eq!(state.prefix_order, expected);
    assert!(!state.catalog.contains_key(&key(1)));
    assert!(!state.prefixes.contains_key(&key(1)));
    assert_eq!(
        (state.catalog.len(), state.prefixes.len()),
        (MAX_CATALOG_ENTRIES, MAX_CATALOG_ENTRIES)
    );
}

#[test]
fn sidecar_load_checks_the_canonical_data_not_its_wire_spelling() {
    use serde_json::{Value, json};

    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text("quote \"; slash \\; unicode: café 🦀")]);
        let dir = f.descriptor.local_path.as_ref().unwrap();
        let blobs = dir.join("blobs");
        std::fs::create_dir_all(&blobs).unwrap();
        for i in 0..16 {
            std::fs::write(blobs.join(format!("inventory-{i}")), b"fixture").unwrap();
        }
        let expected = StorageService::new(kind)
            .catalog_summary(dir, false)
            .unwrap();
        let descriptor = sessions::open(kind, dir).unwrap();
        let stamps = source_stamp(dir, kind).unwrap();
        let index = dir.join(".storage-catalog-v1.json");
        let canonical = std::fs::read(&index).unwrap();
        let (_, prefix) = durable::load(dir, &descriptor, &stamps).unwrap();
        let assert_hit = || {
            let (summary, actual) = durable::load(dir, &descriptor, &stamps).unwrap();
            assert_eq!(summary, expected);
            assert_eq!(actual.revision, prefix.revision);
            assert_eq!(actual.hint.extent, prefix.hint.extent);
            assert_eq!(actual.hint.fingerprint, prefix.hint.fingerprint);
            assert_eq!(actual.blob_digest, prefix.blob_digest);
        };

        let envelope: Value = serde_json::from_slice(&canonical).unwrap();
        let checksum = envelope["checksum"].as_str().unwrap();
        let data = &envelope["data"];
        let pretty = serde_json::to_string_pretty(data)
            .unwrap()
            .replace('é', r"\u00e9");
        assert!(pretty.contains(r"\u00e9"));
        let reordered = format!("{{\n  \"data\": {pretty},\n  \"checksum\": \"{checksum}\"\n}}\n");
        std::fs::write(&index, reordered).unwrap();
        assert_hit();

        // Serde also accepts positional struct representations. On this input
        // the canonical object form is larger than the entire sidecar on disk.
        let mut blobs = data["blobs"].clone();
        for stamp in blobs.as_object_mut().unwrap().values_mut() {
            *stamp = json!([
                stamp["len"],
                stamp["modified"],
                stamp["device"],
                stamp["inode"],
                stamp["changed"]
            ]);
        }
        let positional = json!([
            checksum,
            [
                data["summary"],
                data["stamps"],
                blobs,
                data["revision"],
                data["extent"],
                data["fingerprint"]
            ]
        ]);
        let compact = serde_json::to_vec(&positional).unwrap();
        assert!(compact.len() + 87 < canonical.len());
        std::fs::write(&index, &compact).unwrap();
        assert_hit();

        // Keep the checksum but change a typed field. A miss must not be
        // concealed by the ordinary List path's replay-and-rewrite fallback.
        let mut changed = envelope;
        changed["data"]["summary"]["preview"] = json!("different");
        std::fs::write(&index, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(durable::load(dir, &descriptor, &stamps).is_none());
    }
}

#[test]
fn sidecar_load_limit_still_covers_the_original_wire_bytes() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text("bounded input")]);
        let dir = f.descriptor.local_path.as_ref().unwrap();
        let expected = StorageService::new(kind)
            .catalog_summary(dir, false)
            .unwrap();
        let descriptor = sessions::open(kind, dir).unwrap();
        let stamps = source_stamp(dir, kind).unwrap();
        let index = dir.join(".storage-catalog-v1.json");
        let mut bytes = std::fs::read(&index).unwrap();
        let limit = 4 * 1024 * 1024;
        assert!(bytes.len() < limit);
        bytes.resize(limit, b' ');
        std::fs::write(&index, &bytes).unwrap();
        assert_eq!(
            durable::load(dir, &descriptor, &stamps).unwrap().0,
            expected
        );
        bytes.push(b' ');
        std::fs::write(&index, &bytes).unwrap();
        assert!(durable::load(dir, &descriptor, &stamps).is_none());
    }
}
