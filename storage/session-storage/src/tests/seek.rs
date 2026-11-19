use super::*;

fn read(
    f: &Fixture,
    head: &StorageSnapshot,
    after: u64,
    limit: u32,
) -> anyhow::Result<StorageRecordPage> {
    let SessionStorageResponse::Records { page } =
        f.service.execute(SessionStorageRequest::ReadRecords {
            snapshot: head.clone(),
            after_seq: after,
            limit,
        })?
    else {
        panic!("unexpected response");
    };
    Ok(page)
}

#[test]
fn known_jsonl_boundary_resumes_line_hash_but_unknown_and_warm_cuts_replay() {
    let mut f = Fixture::new(BackendKind::Jsonl);
    let body = "a".repeat(45_000);
    for _ in 0..3 {
        f.append(vec![text(&body)]);
    }
    let old = f.snapshot();
    f.service
        .execute(SessionStorageRequest::ReleaseSnapshot { snapshot: old })
        .unwrap();
    f.append(vec![text("first tail")]);
    f.append(vec![text("second tail")]);
    let head = strict_snapshot(&f);
    let bytes = std::fs::metadata(f.descriptor.record_location.as_ref().unwrap())
        .unwrap()
        .len();
    crate::diagnostics::reset();
    let first = read(&f, &head, 3, 1).unwrap();
    assert_eq!(first.records[0].session_seq, 4);
    assert_eq!(first.next_after_seq, Some(4));
    assert_eq!(crate::diagnostics::get().decoded, 1);
    assert_eq!(crate::diagnostics::get().fingerprint_bytes, bytes);
    let rest = read(&f, &head, 4, 10).unwrap();
    assert_eq!(rest.records[0].session_seq, 5);
    assert_eq!(rest.next_after_seq, None);
    assert_eq!(crate::diagnostics::get().decoded, 2);
    assert_eq!(crate::diagnostics::get().fingerprint_bytes, bytes);
    assert_eq!(read(&f, &head, 3, 1).unwrap(), first, "rewind/retry");
    crate::diagnostics::reset();
    assert_eq!(read(&f, &head, 2, 1).unwrap().records[0].session_seq, 3);
    assert_eq!(crate::diagnostics::get().decoded, 3);
    let warm = f.snapshot();
    crate::diagnostics::reset();
    assert_eq!(read(&f, &warm, 3, 1).unwrap().records, first.records);
    assert_eq!(crate::diagnostics::get().decoded, 5 + 4);
}

#[test]
fn corrupted_skipped_blob_is_not_hidden_by_a_seek() {
    let mut f = Fixture::new(BackendKind::Jsonl);
    f.append(vec![text(&"B".repeat(300_000))]);
    let old = f.snapshot();
    f.append(vec![text("tail")]);
    let head = strict_snapshot(&f);
    let path = std::fs::read_dir(f.descriptor.local_path.as_ref().unwrap().join("blobs"))
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| entry.file_type().unwrap().is_file())
        .unwrap()
        .path();
    let original = std::fs::read(&path).unwrap();
    let mut damaged = original.clone();
    damaged[0] ^= 1;
    std::fs::write(&path, damaged).unwrap();
    assert!(read(&f, &head, old.revision.session_seq, 1).is_err());
    std::fs::remove_file(&path).unwrap();
    assert!(read(&f, &head, old.revision.session_seq, 1).is_err());
    std::fs::write(path, original).unwrap();
    assert_eq!(
        read(&f, &head, old.revision.session_seq, 1)
            .unwrap()
            .records[0]
            .session_seq,
        2
    );
}

#[test]
fn seeking_authenticates_both_sides_of_the_cut_without_trusting_source_stamps() {
    use crate::backend::JournalBackend;
    let mut f = Fixture::new(BackendKind::Jsonl);
    f.append(vec![text("head-marker")]);
    f.append(vec![text("tail-marker")]);
    let mut source = crate::backend::Jsonl::new()
        .snapshot(
            f.descriptor.local_path.as_ref().unwrap(),
            f.descriptor.session.session_id,
        )
        .unwrap();
    let first = source.next().unwrap().unwrap();
    source.remember_page_boundary(first.session_seq);
    let second = source.next().unwrap().unwrap();
    assert!(source.next().unwrap().is_none());
    let path = f.descriptor.record_location.as_ref().unwrap();
    let original = std::fs::read_to_string(path).unwrap();
    for marker in ["head-marker", "tail-marker"] {
        std::fs::write(path, original.replace(marker, "fake-marker")).unwrap();
        assert!(source.reset_for_page(1).is_err());
        std::fs::write(path, &original).unwrap();
    }
    source.reset_for_page(1).unwrap();
    assert_eq!(source.next().unwrap().unwrap(), second);
    assert!(source.next().unwrap().is_none());
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

#[test]
fn small_old_cut_before_large_new_tail_keeps_the_short_scan() {
    let mut f = Fixture::new(BackendKind::Jsonl);
    f.append(vec![text("old")]);
    let old = f.snapshot();
    f.append(vec![text("first new page")]);
    f.append(vec![text(&"n".repeat(100_000))]);
    let head = strict_snapshot(&f);
    crate::diagnostics::reset();
    let page = read(&f, &head, old.revision.session_seq, 1).unwrap();
    assert_eq!(page.records[0].session_seq, 2);
    assert_eq!(crate::diagnostics::get().decoded, 2);
    assert_eq!(crate::diagnostics::get().fingerprint_bytes, 0);
}
