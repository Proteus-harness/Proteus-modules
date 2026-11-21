use super::*;
use crate::tests::{Fixture, text};
use std::cell::RefCell;

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
