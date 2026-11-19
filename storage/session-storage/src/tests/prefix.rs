use super::*;

fn compare(f: &Fixture, head: &StorageSnapshot, old: &StorageSnapshot) -> anyhow::Result<bool> {
    let SessionStorageResponse::SnapshotPrefix { matches } =
        f.service.execute(SessionStorageRequest::CompareSnapshots {
            snapshot: head.clone(),
            prefix: old.clone(),
        })?
    else {
        panic!("unexpected response")
    };
    Ok(matches)
}

fn rewrite_text(path: &std::path::Path, from: &str, to: &str) {
    assert_eq!(from.len(), to.len());
    let bytes = std::fs::read_to_string(path).unwrap();
    assert_eq!(bytes.matches(from).count(), 1);
    std::fs::write(path, bytes.replace(from, to)).unwrap();
}

#[test]
fn released_prefix_matches_only_verified_hash_and_revision() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text("oldest")]);
        let oldest = f.snapshot();
        f.append(vec![text("middle")]);
        let old = f.snapshot();
        f.service
            .execute(SessionStorageRequest::ReleaseSnapshot {
                snapshot: old.clone(),
            })
            .unwrap();
        f.append(vec![text("tail")]);
        let head = strict_snapshot(&f);
        crate::diagnostics::reset();
        assert!(compare(&f, &head, &old).unwrap());
        assert_eq!(crate::diagnostics::get().decoded, 0);
        let mut wrong = old.clone();
        wrong.revision.history_revision -= 1;
        assert!(!compare(&f, &head, &wrong).unwrap());
        wrong = old.clone();
        wrong.content_sha256 = "0".repeat(64);
        assert!(!compare(&f, &head, &wrong).unwrap());
        assert!(compare(&f, &head, &head).unwrap());
        assert_eq!(crate::diagnostics::get().decoded, 0);
        assert!(compare(&f, &head, &oldest).unwrap());
        assert!(
            crate::diagnostics::get().decoded > 0,
            "unremembered boundary still supported"
        );
        let warm = f.snapshot();
        crate::diagnostics::reset();
        assert!(compare(&f, &warm, &old).unwrap());
        assert!(
            crate::diagnostics::get().decoded > 0,
            "warm hint is not verification"
        );
    }
}

#[test]
fn jsonl_boundary_is_derived_from_target_and_protects_its_entire_fixed_extent() {
    let mut f = Fixture::new(BackendKind::Jsonl);
    f.append(vec![text("prefix-original")]);
    let old = f.snapshot();
    f.append(vec![text("tail-original")]);
    let path = f.descriptor.record_location.as_ref().unwrap();
    rewrite_text(path, "prefix-original", "prefix-modified");
    let head = strict_snapshot(&f);
    assert!(
        !compare(&f, &head, &old).unwrap(),
        "old cache cannot attest to current bytes"
    );
    rewrite_text(path, "tail-original", "tail-modified");
    assert!(
        compare(&f, &head, &old).is_err(),
        "reject even corruption after requested boundary"
    );
    rewrite_text(path, "tail-modified", "tail-original");
    assert!(compare(&f, &head, &head).unwrap());
}

#[test]
fn jsonl_fixed_digest_does_not_attest_to_external_payloads() {
    let mut f = Fixture::new(BackendKind::Jsonl);
    f.append(vec![text("before external payload")]);
    let old = f.snapshot();
    f.append(vec![text(&"B".repeat(300_000))]);
    let head = strict_snapshot(&f);
    let path = std::fs::read_dir(f.descriptor.local_path.as_ref().unwrap().join("blobs"))
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| entry.file_type().unwrap().is_file())
        .unwrap()
        .path();
    let bytes = std::fs::read(&path).unwrap();
    let mut invalid = bytes.clone();
    invalid[0] ^= 1;
    std::fs::write(&path, invalid).unwrap();
    assert!(
        compare(&f, &head, &old).is_err(),
        "check external suffix, too"
    );
    std::fs::remove_file(&path).unwrap();
    assert!(compare(&f, &head, &head).is_err());
    std::fs::write(path, bytes).unwrap();
    assert!(compare(&f, &head, &old).unwrap());
}

fn strict_snapshot(f: &Fixture) -> StorageSnapshot {
    let SessionStorageResponse::DisplaySnapshot {
        projection: Some(projection),
    } = f
        .service
        .execute(SessionStorageRequest::OpenDisplaySnapshot {
            session: f.descriptor.session.clone(),
        })
        .unwrap()
    else {
        panic!("expected strict display snapshot")
    };
    projection.snapshot
}
