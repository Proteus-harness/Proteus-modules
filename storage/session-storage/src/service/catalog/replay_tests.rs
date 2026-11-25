use super::*;
use crate::tests::{Fixture, text};
use std::cell::RefCell;
#[cfg(unix)]
use std::{cell::Cell, fs::OpenOptions, io::Write};

#[cfg(unix)]
#[test]
fn jsonl_cold_snapshot_and_catalog_replay_hash_the_complete_prefix_once() {
    let mut f = Fixture::new(BackendKind::Jsonl);
    let message = "x".repeat(32 * 1024);
    for i in 0..24 {
        f.append(vec![text(&format!("строка {i} {message}"))]);
    }
    let dir = f.descriptor.local_path.as_ref().unwrap();
    let journal = f.descriptor.record_location.as_ref().unwrap();
    let prefix = std::fs::read(journal).unwrap();
    let hash = crate::io::sha256_hex(&prefix);
    let bytes = prefix.len() as u64;
    let tail = b"{\"incomplete\":";
    OpenOptions::new()
        .append(true)
        .open(journal)
        .unwrap()
        .write_all(tail)
        .unwrap();

    let id = f.descriptor.session.session_id;
    crate::diagnostics::reset();
    let mut eager = Jsonl::new().snapshot(dir, id).unwrap();
    let mut fold = Fold::default();
    while let Some(record) = eager.next().unwrap() {
        fold.apply(id, &record).unwrap();
    }
    let eager_io = crate::diagnostics::get();
    assert_eq!(eager_io.fingerprint_bytes, bytes);
    assert_eq!(eager_io.jsonl_replay_bytes, bytes);
    assert_eq!(eager.fingerprint(), hash);

    let catalog = StorageService::new(BackendKind::Jsonl);
    crate::diagnostics::reset();
    let summary = catalog.catalog_summary(dir, true).unwrap();
    let catalog_io = crate::diagnostics::get();
    assert_eq!(summary.message_count, 24);
    let state = catalog.lock_state().unwrap();
    let head = &state.prefixes[&summary.descriptor.session];
    assert_eq!(head.revision, fold.revision());
    assert_eq!(head.hint.fingerprint, hash);
    assert_eq!(head.hint.extent, bytes);
    drop(state);

    let open = SessionStorageRequest::OpenSnapshot {
        session: f.descriptor.session.clone(),
    };
    let cold = StorageService::new(BackendKind::Jsonl);
    crate::diagnostics::reset();
    let SessionStorageResponse::Snapshot { snapshot } = cold.execute(open.clone()).unwrap() else {
        panic!()
    };
    let open_io = crate::diagnostics::get();
    assert_eq!(snapshot.revision, fold.revision());
    assert_eq!(snapshot.content_sha256, hash);
    for work in [catalog_io, open_io] {
        assert_eq!(work.fingerprint_bytes, 0);
        assert_eq!(work.jsonl_replay_bytes, bytes);
        assert_eq!(work.decoded, 24);
    }
    assert_eq!(
        std::fs::read(journal).unwrap(),
        [&prefix[..], tail].concat()
    );
    eprintln!(
        "jsonl rows=24 replay_bytes={bytes} eager_before={} eager_after={}",
        eager_io.fingerprint_bytes, open_io.fingerprint_bytes
    );

    // Once the torn line becomes complete, its invalid JSON is corruption.
    OpenOptions::new()
        .append(true)
        .open(journal)
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    assert!(
        StorageService::new(BackendKind::Jsonl)
            .execute(open)
            .is_err()
    );
    assert!(catalog.catalog_summary(dir, true).is_err());
}

#[cfg(unix)]
#[test]
fn jsonl_cold_replay_reauthenticates_the_original_extent_after_a_live_append() {
    let mut f = Fixture::new(BackendKind::Jsonl);
    for i in 0..3 {
        f.append(vec![text(&format!("old {i}"))]);
    }
    let dir = f.descriptor.local_path.clone().unwrap();
    let id = f.descriptor.session.session_id;
    let journal = f.descriptor.record_location.clone().unwrap();
    let prefix = std::fs::read(&journal).unwrap();
    let f = RefCell::new(f);
    let appended = Cell::new(false);
    crate::diagnostics::reset();
    let (source, fold) = Jsonl::replay_snapshot(&dir, id, None, |fold: &mut Fold, record, _| {
        if record.session_seq == 1 && !appended.replace(true) {
            f.borrow_mut().append(vec![text("concurrent append")]);
        }
        fold.apply(id, record)
    })
    .unwrap();
    let work = crate::diagnostics::get();
    assert_eq!(source.extent(), prefix.len() as u64);
    assert_eq!(source.fingerprint(), crate::io::sha256_hex(&prefix));
    assert_eq!(fold.revision().session_seq, 3);
    assert_eq!(work.fingerprint_bytes, prefix.len() as u64);
    assert_eq!(work.jsonl_replay_bytes, 2 * prefix.len() as u64);
    assert_eq!(work.decoded, 6);
    assert!(std::fs::read(journal).unwrap().len() > prefix.len());
}

#[cfg(unix)]
#[test]
fn jsonl_cold_replay_never_publishes_a_fold_from_before_a_prefix_rewrite() {
    let mut f = Fixture::new(BackendKind::Jsonl);
    f.append(vec![text("original")]);
    f.append(vec![text("also original")]);
    let dir = f.descriptor.local_path.as_ref().unwrap();
    let journal = f.descriptor.record_location.as_ref().unwrap();
    let mut corrupt = std::fs::read(journal).unwrap();
    let old = b"\"session_seq\":1";
    let sequence = corrupt
        .windows(old.len())
        .position(|window| window == old)
        .unwrap();
    corrupt[sequence + old.len() - 1] = b'9';
    let id = f.descriptor.session.session_id;
    let rewritten = Cell::new(false);
    let result = Jsonl::replay_snapshot(dir, id, None, |fold: &mut Fold, record, _| {
        if !rewritten.replace(true) {
            std::fs::write(journal, &corrupt).unwrap();
            std::fs::File::open(journal)
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH)
                .unwrap();
        }
        fold.apply(id, record)
    });
    assert!(result.is_err());
}

#[cfg(unix)]
#[test]
fn jsonl_cold_replay_rejects_a_valid_same_size_prefix_rewrite() {
    let mut f = Fixture::new(BackendKind::Jsonl);
    let original = "before";
    let replacement = "after!";
    let mut expected = text(original);
    f.append(vec![expected.clone()]);
    let dir = f.descriptor.local_path.as_ref().unwrap();
    let journal = f.descriptor.record_location.as_ref().unwrap();
    let prefix = std::fs::read_to_string(journal).unwrap();
    let changed = prefix.replace(original, replacement);
    assert_eq!(changed.len(), prefix.len());
    assert_ne!(changed, prefix);

    let id = f.descriptor.session.session_id;
    let result = Jsonl::replay_snapshot(dir, id, None, |fold: &mut Fold, record, _| {
        std::fs::write(journal, &changed).unwrap();
        std::fs::File::open(journal)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH)
            .unwrap();
        fold.apply(id, record)
    });
    assert!(result.is_err());

    let (source, fold) = Jsonl::replay_snapshot(dir, id, None, |fold: &mut Fold, record, _| {
        fold.apply(id, record)
    })
    .unwrap();
    assert_eq!(
        source.fingerprint(),
        crate::io::sha256_hex(changed.as_bytes())
    );
    expected.parts[0].payload = proteus_contracts::model_standard::ContentPart::Text {
        text: replacement.into(),
    };
    assert_eq!(fold.validation.history(), [expected]);
}

fn raw_proof(path: &Path) -> (String, u64) {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let mut stmt = conn
        .prepare("SELECT payload FROM journal ORDER BY sequence")
        .unwrap();
    let mut rows = stmt.query([]).unwrap();
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    let mut total = 0;
    while let Some(row) = rows.next().unwrap() {
        let value = row.get_ref(0).unwrap();
        let bytes = value.as_blob().unwrap();
        hash.update(bytes);
        total += bytes.len() as u64;
    }
    (crate::io::digest_hex(hash.finish()), total)
}

#[test]
fn sqlite_cold_catalog_replay_hashes_the_strict_rows_once() {
    for count in [0, 1, 24] {
        let mut f = Fixture::new(BackendKind::Sqlite);
        let text_payload = "x".repeat(32 * 1024);
        for i in 0..count {
            f.append(vec![text(&format!("строка {i}: {text_payload}"))]);
        }
        let dir = f.descriptor.local_path.clone().unwrap();
        let (hash, payload_bytes) = raw_proof(f.descriptor.record_location.as_ref().unwrap());
        // The stock catalog uses this identical complete snapshot + Fold
        // sequence, including the eager scan and the cursor's EOF check.
        let id = f.descriptor.session.session_id;
        crate::diagnostics::reset();
        let mut stock_source = Sqlite::default().snapshot(&dir, id).unwrap();
        let mut stock_fold = Fold::default();
        while let Some(record) = stock_source.next().unwrap() {
            stock_fold.apply(id, &record).unwrap();
        }
        let stock_work = crate::diagnostics::get();
        assert_eq!(stock_source.fingerprint(), hash);
        assert_eq!(stock_work.fingerprint_bytes, payload_bytes);
        assert_eq!(stock_work.decoded, count as u64);
        drop(stock_source);

        let storage = StorageService::new(BackendKind::Sqlite);
        crate::diagnostics::reset();
        let summary = storage.catalog_summary(&dir, false).unwrap();
        let work = crate::diagnostics::get();
        assert_eq!(summary.message_count, count);
        assert_eq!(work.decoded, count as u64);
        assert_eq!(
            work.fingerprint_bytes, 0,
            "no supplementary fingerprint scan"
        );

        let prefix = &storage.lock_state().unwrap().prefixes[&summary.descriptor.session];
        assert_eq!(prefix.hint.extent, count as u64);
        assert_eq!(prefix.revision, stock_fold.revision());
        assert_eq!(prefix.hint.fingerprint, hash);
        let index: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join(".storage-catalog-v1.json")).unwrap())
                .unwrap();
        assert_eq!(index["data"]["extent"], count);
        assert_eq!(index["data"]["fingerprint"], hash);
        eprintln!(
            "sqlite rows={} replay_payload_bytes={payload_bytes} eager_before={} eager_after={}",
            work.decoded, stock_work.fingerprint_bytes, work.fingerprint_bytes
        );
    }
}

#[test]
fn sqlite_catalog_transaction_keeps_its_exact_rows_across_a_live_commit() {
    let mut f = Fixture::new(BackendKind::Sqlite);
    for i in 0..3 {
        f.append(vec![text(&format!("old {i}"))]);
    }
    let dir = f.descriptor.local_path.clone().unwrap();
    let id = f.descriptor.session.session_id;
    let journal = f.descriptor.record_location.clone().unwrap();
    let (before, _) = raw_proof(&journal);
    let f = RefCell::new(f);
    let (fold, prefix) = Sqlite::replay_catalog(&dir, id, |fold: &mut Fold, record| {
        if record.session_seq == 1 {
            f.borrow_mut()
                .append(vec![text("new during the live read transaction")]);
        }
        fold.apply(id, record)
    })
    .unwrap();
    assert_eq!(fold.revision().session_seq, 3);
    assert_eq!(prefix.extent, 3);
    assert_eq!(prefix.fingerprint, before);
    let (after, _) = raw_proof(&journal);
    assert_ne!(before, after);
    let (fold, prefix) =
        Sqlite::replay_catalog(&dir, id, |fold: &mut Fold, record| fold.apply(id, record)).unwrap();
    assert_eq!(fold.revision().session_seq, 4);
    assert_eq!(prefix.extent, 4);
    assert_eq!(prefix.fingerprint, after);
}
