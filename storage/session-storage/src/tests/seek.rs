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

#[test]
fn freshly_validated_terminal_page_decodes_each_pinned_record_once() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        for i in 0..8 {
            f.append(vec![text(&format!("record {i}"))]);
        }
        let oracle = f.snapshot();
        let expected = read(&f, &oracle, 7, 1).unwrap().records;
        for limit in [1, 32] {
            let warm = f.snapshot();
            crate::diagnostics::reset();
            let page = read(&f, &warm, 7, limit).unwrap();
            assert_eq!(page.snapshot, warm);
            assert_eq!(page.records, expected);
            assert!(page.next_after_seq.is_none());
            assert_eq!(
                crate::diagnostics::get().decoded,
                match kind {
                    BackendKind::Jsonl => 8,
                    BackendKind::Sqlite => 16, // Unchanged: no physical version.
                }
            );
            if matches!(kind, BackendKind::Jsonl) {
                let bytes = std::fs::metadata(f.descriptor.record_location.as_ref().unwrap())
                    .unwrap()
                    .len();
                assert_eq!(crate::diagnostics::get().fingerprint_bytes, bytes);
            }
            assert!(read(&f, &warm, 8, limit).unwrap().records.is_empty());
            assert_eq!(read(&f, &warm, 7, limit).unwrap(), page);
            assert_eq!(read(&f, &warm, 0, 1).unwrap().records[0].session_seq, 1);
        }
    }
}

#[test]
fn jsonl_terminal_fold_is_pinned_and_fails_closed() {
    use std::fs::{self, OpenOptions};
    let mut f = Fixture::new(BackendKind::Jsonl);
    f.append(vec![text("first pinned")]);
    f.append(vec![text("pinned terminal")]);
    let oracle = f.snapshot();
    let expected = read(&f, &oracle, 1, 1).unwrap().records;
    let warm = f.snapshot();
    let path = f.descriptor.record_location.clone().unwrap();
    let prefix = fs::read_to_string(&path).unwrap();
    f.append(vec![text("later append")]);
    let mut forged = warm.clone();
    forged.revision.session_seq += 1;
    assert!(read(&f, &forged, 2, 1).is_err());
    assert_eq!(read(&f, &warm, 1, 1).unwrap().records, expected);
    let with_append = fs::read_to_string(&path).unwrap();
    for (needle, changed) in [
        ("first pinned", "first forged"),
        ("pinned terminal", "forged terminal"),
    ] {
        assert_eq!(with_append.matches(needle).count(), 1);
        fs::write(&path, with_append.replace(needle, changed)).unwrap();
        assert!(read(&f, &warm, 1, 1).is_err());
        fs::write(&path, &with_append).unwrap();
        assert_eq!(read(&f, &warm, 1, 1).unwrap().records, expected);
    }
    OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(prefix.len() as u64 - 1)
        .unwrap();
    assert!(read(&f, &warm, 1, 1).is_err());
}

#[test]
fn jsonl_terminal_fold_reads_external_predecessors_even_on_retry() {
    let mut f = Fixture::new(BackendKind::Jsonl);
    f.append(vec![text(&"B".repeat(300_000))]);
    f.append(vec![text("inline terminal")]);
    let _ = f.snapshot();
    let warm = f.snapshot();
    let blob = std::fs::read_dir(f.descriptor.local_path.as_ref().unwrap().join("blobs"))
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.file_type().unwrap().is_file())
        .unwrap()
        .path();
    let original = std::fs::read(&blob).unwrap();
    std::fs::remove_file(&blob).unwrap();
    assert!(read(&f, &warm, 1, 1).is_err());
    std::fs::write(&blob, &original).unwrap();
    assert_eq!(read(&f, &warm, 1, 1).unwrap().records[0].session_seq, 2);
    std::fs::remove_file(blob).unwrap();
    assert!(read(&f, &warm, 1, 1).is_err());
}

// Change only the physical file after the LAST line was read, not merely before
// the request. Wrapping the real fixed source makes this interleaving repeatable.
#[test]
fn jsonl_terminal_fold_discards_its_capture_on_append_during_validation() {
    use crate::backend::{JournalBackend, RecordSnapshot, SourceVersion};
    use std::io::Write;
    struct AppendAtEof {
        inner: Box<dyn RecordSnapshot>,
        append: Option<std::fs::File>,
    }
    impl RecordSnapshot for AppendAtEof {
        fn next(&mut self) -> anyhow::Result<Option<JournalRecord>> {
            let record = self.inner.next()?;
            if record.is_none()
                && let Some(mut file) = self.append.take()
            {
                file.write_all(b"concurrent suffix\n")?;
            }
            Ok(record)
        }
        fn reset(&mut self) -> anyhow::Result<()> {
            self.inner.reset()
        }
        fn source_version(&self) -> anyhow::Result<Option<SourceVersion>> {
            self.inner.source_version()
        }
        fn verify_fixed_prefix(&self) -> anyhow::Result<()> {
            self.inner.verify_fixed_prefix()
        }
        fn confirm_verified_data(&self) -> anyhow::Result<bool> {
            self.inner.confirm_verified_data()
        }
        fn fingerprint(&self) -> &str {
            self.inner.fingerprint()
        }
        fn read_hash(&self) -> ring::digest::Context {
            self.inner.read_hash()
        }
        fn extent(&self) -> u64 {
            self.inner.extent()
        }
        fn prefix_fingerprint(&mut self, seq: u64) -> anyhow::Result<Option<String>> {
            self.inner.prefix_fingerprint(seq)
        }
    }
    let mut f = Fixture::new(BackendKind::Jsonl);
    f.append(vec![text("pinned first")]);
    f.append(vec![text("pinned last")]);
    let identity = f.snapshot();
    let expected = read(&f, &identity, 1, 1).unwrap();
    let dir = f.descriptor.local_path.as_ref().unwrap();
    let inner = crate::backend::Jsonl::new()
        .snapshot(dir, identity.session.session_id)
        .unwrap();
    let append = Some(
        std::fs::OpenOptions::new()
            .append(true)
            .open(f.descriptor.record_location.as_ref().unwrap())
            .unwrap(),
    );
    let mut snapshot = crate::snapshot::SnapshotState::new(
        identity,
        Box::new(AppendAtEof { inner, append }),
        false,
        None,
    )
    .unwrap();
    crate::diagnostics::reset();
    assert_eq!(snapshot.page(1, 1).unwrap(), expected);
    assert_eq!(
        crate::diagnostics::get().decoded,
        4,
        "changed source uses the ordinary second scan"
    );
}
