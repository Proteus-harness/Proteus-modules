use super::*;

#[test]
fn record_pages_obey_byte_budget_without_truncating_records() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        for ch in ['A', 'B', 'C'] {
            f.append(vec![text(&ch.to_string().repeat(2 * 1024 * 1024))]);
        }
        let snapshot = f.snapshot();
        let mut after = 0;
        let mut collected = 0;
        loop {
            let SessionStorageResponse::Records { page } = f
                .service
                .execute(SessionStorageRequest::ReadRecords {
                    snapshot: snapshot.clone(),
                    after_seq: after,
                    limit: 1024,
                })
                .unwrap()
            else {
                panic!()
            };
            assert!(serde_json::to_vec(&page.records).unwrap().len() < 4 * 1024 * 1024);
            collected += page.records.len();
            if let Some(next) = page.next_after_seq {
                assert!(next > after);
                after = next;
            } else {
                break;
            }
        }
        assert_eq!(collected, 3);
    }
}

#[test]
fn paged_reads_decode_once_per_monotonic_pass_and_support_retries_and_interleaving() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        for index in 0..24 {
            f.append(vec![text(&format!("{index}:{}", "H".repeat(256 * 1024)))]);
        }
        let snapshot = f.snapshot();
        crate::diagnostics::reset();
        let mut after = 0;
        let mut read = 0;
        loop {
            let SessionStorageResponse::Records { page } = f
                .service
                .execute(SessionStorageRequest::ReadRecords {
                    snapshot: snapshot.clone(),
                    after_seq: after,
                    limit: 2,
                })
                .unwrap()
            else {
                panic!()
            };
            read += page.records.len();
            match page.next_after_seq {
                Some(next) => after = next,
                None => break,
            }
        }
        assert_eq!(read, 24);
        assert_eq!(
            crate::diagnostics::get().decoded,
            24,
            "one source decode per record, no rescanned tail"
        );
        let request = SessionStorageRequest::ReadRecords {
            snapshot: snapshot.clone(),
            after_seq: 4,
            limit: 2,
        };
        let expected = f.service.execute(request.clone()).unwrap();
        assert_eq!(
            f.service.execute(request.clone()).unwrap(),
            expected,
            "lost page ACK retry"
        );
        f.conversation(
            snapshot.clone(),
            StorageHistoryPoint::Revision {
                history_revision: 1,
            },
        );
        assert_eq!(
            f.service.execute(request).unwrap(),
            expected,
            "another read resets the cursor safely"
        );
        let warm = f.snapshot();
        crate::diagnostics::reset();
        let SessionStorageResponse::Records { page } = f
            .service
            .execute(SessionStorageRequest::ReadRecords {
                snapshot: warm,
                after_seq: 0,
                limit: 2,
            })
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(page.records.len(), 2);
        assert_eq!(
            crate::diagnostics::get().decoded,
            26,
            "warm canonical exposure validates prefix once then streams first page"
        );
    }
}

#[test]
fn jsonl_paging_reverifies_after_fixed_file_changes_before_exposing_next_page() {
    let mut f = Fixture::new(BackendKind::Jsonl);
    f.append(vec![text("first")]);
    f.append(vec![text("second")]);
    let snapshot = f.snapshot();
    f.service
        .execute(SessionStorageRequest::ReadRecords {
            snapshot: snapshot.clone(),
            after_seq: 0,
            limit: 1,
        })
        .unwrap();
    let path = f.descriptor.record_location.as_ref().unwrap();
    let mut bytes = std::fs::read(path).unwrap();
    let index = bytes.iter().position(|b| *b == b'{').unwrap();
    bytes[index] = b'[';
    std::fs::write(path, bytes).unwrap();
    assert!(
        f.service
            .execute(SessionStorageRequest::ReadRecords {
                snapshot,
                after_seq: 1,
                limit: 1
            })
            .is_err()
    );
}

#[test]
fn jsonl_suffix_appends_during_validation_and_between_pages_preserve_fixed_prefix() {
    use crate::backend::JournalBackend;
    let mut f = Fixture::new(BackendKind::Jsonl);
    f.append(vec![text("first pinned record")]);
    f.append(vec![text("second pinned record")]);
    let path = f.descriptor.local_path.clone().unwrap();
    let mut backend = crate::backend::Jsonl::new();
    let mut source = backend
        .snapshot(&path, f.descriptor.session.session_id)
        .unwrap();
    let before = source.source_version().unwrap();
    assert_eq!(source.next().unwrap().unwrap().session_seq, 1);
    std::thread::scope(|scope| {
        let append = scope.spawn(|| f.append(vec![text("concurrent suffix")]));
        assert_eq!(source.next().unwrap().unwrap().session_seq, 2);
        append.join().unwrap();
    });
    assert!(
        source.next().unwrap().is_none(),
        "snapshot ends before appended suffix"
    );
    assert!(source.source_version().unwrap() != before);
    source.verify_fixed_prefix().unwrap();
    source.reset().unwrap();
    assert_eq!(source.next().unwrap().unwrap().session_seq, 1);
    source.verify_fixed_prefix().unwrap();
    assert_eq!(
        source.next().unwrap().unwrap().session_seq,
        2,
        "same-FD prefix verification must preserve the streaming cursor"
    );
    assert!(source.next().unwrap().is_none());

    let snapshot = f.snapshot();
    let SessionStorageResponse::Records { page: first } = f
        .service
        .execute(SessionStorageRequest::ReadRecords {
            snapshot: snapshot.clone(),
            after_seq: 0,
            limit: 1,
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(first.records[0].session_seq, 1);
    f.append(vec![text("another suffix after page one")]);
    let SessionStorageResponse::Records { page: rest } = f
        .service
        .execute(SessionStorageRequest::ReadRecords {
            snapshot,
            after_seq: 1,
            limit: 100,
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        rest.records
            .iter()
            .map(|r| r.session_seq)
            .collect::<Vec<_>>(),
        [2, 3]
    );
    assert!(rest.next_after_seq.is_none());
}
