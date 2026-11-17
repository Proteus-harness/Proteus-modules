use super::*;
use std::time::Instant;

fn percentile(values: &mut [u128], percentile: usize) -> u128 {
    values.sort_unstable();
    values[(values.len() - 1) * percentile / 100]
}

#[test]
#[ignore = "manual local I/O measurement; no model or network"]
fn sixteen_mib_history_warm_snapshot_and_head_publication_cost() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text(&"H".repeat(16 * 1024 * 1024))]);
        let cold_start = Instant::now();
        let snapshot = f.snapshot();
        let cold_us = cold_start.elapsed().as_micros();
        crate::diagnostics::reset();
        let mut opens = Vec::new();
        let mut heads = Vec::new();
        for iteration in 0..100 {
            let start = Instant::now();
            f.snapshot();
            opens.push(start.elapsed().as_micros());
            let start = Instant::now();
            f.service
                .execute(SessionStorageRequest::WriteCache {
                    session: f.descriptor.session.clone(),
                    key: "display-head".into(),
                    source: snapshot.clone(),
                    value: serde_json::json!({"page": iteration, "rows": 1}),
                })
                .unwrap();
            heads.push(start.elapsed().as_micros());
        }
        let counters = crate::diagnostics::get();
        assert_eq!(counters.decoded, 0);
        assert_eq!(counters.fingerprint_bytes, 0);
        eprintln!(
            "backend={kind:?} history_bytes=16777216 iterations=100 cold_snapshot_us={cold_us} warm_snapshot_p50_us={} warm_snapshot_p95_us={} head_write_p50_us={} head_write_p95_us={} decoded={} fingerprint_bytes={}",
            percentile(&mut opens, 50),
            percentile(&mut opens, 95),
            percentile(&mut heads, 50),
            percentile(&mut heads, 95),
            counters.decoded,
            counters.fingerprint_bytes
        );
    }
}
