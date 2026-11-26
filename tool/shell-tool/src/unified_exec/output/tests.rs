use super::*;

#[test]
fn circular_output_matches_append_then_drain_across_chunk_boundaries() {
    const LIMIT: usize = SESSION_BUFFER_LIMIT;
    const HEAD: usize = BoundedOutput::HEAD;
    const TAIL: usize = BoundedOutput::TAIL;
    // Short output, exact fill, crossing the cutoff, the ring wraps, and a
    // single large chunk supplies both a missing head and its latest tail.
    for lengths in [
        &[0, 17, 1][..],
        &[HEAD - 3, 1, TAIL + 2, 1, HEAD / 5, TAIL - 7, 29],
        &[3, 3 * LIMIT + 17, 5],
        &[LIMIT, TAIL, TAIL + 1, LIMIT + 2],
        &[HEAD - 37, TAIL - 41, 4096, TAIL / 3, TAIL / 3, TAIL / 3, 32768],
    ] {
        let mut state = BoundedOutput::default();
        let mut stock = Vec::new();
        let mut dropped = 0usize;
        let mut byte_state = 7u32;
        for len in lengths.iter().copied() {
            let chunk = (0..len)
                .map(|_| {
                    byte_state ^= byte_state << 13;
                    byte_state ^= byte_state >> 17;
                    byte_state ^= byte_state << 5;
                    byte_state as u8
                })
                .collect::<Vec<_>>();
            state.push(&chunk);
            stock.extend_from_slice(&chunk);
            let excess = stock.len().saturating_sub(LIMIT);
            if excess != 0 {
                stock.drain(HEAD..HEAD + excess);
            }
            dropped = dropped.saturating_add(excess);
            assert_eq!(state.bytes.len(), stock.len());
        }
        assert_eq!(state.take(true), (stock, dropped));
        assert_eq!(state.take(true), (Vec::new(), 0));
    }
}

#[test]
fn full_ring_does_not_reallocate_for_a_small_overflowing_chunk() {
    let mut state = BoundedOutput::default();
    state.push(&vec![b'x'; SESSION_BUFFER_LIMIT]);
    let allocation = (state.bytes.as_ptr(), state.bytes.capacity());
    state.push(b"new-tail");
    assert_eq!((state.bytes.as_ptr(), state.bytes.capacity()), allocation);
    let (bytes, dropped) = state.take(true);
    assert_eq!(dropped, 8);
    assert_eq!(bytes.len(), SESSION_BUFFER_LIMIT);
    assert!(bytes.ends_with(b"new-tail"));
}

#[test]
fn huge_chunk_reserves_head_and_tail_together_after_a_partial_head() {
    let mut state = BoundedOutput::default();
    let old_len = 400 * 1024;
    state.push(&vec![b'a'; old_len]);
    assert_eq!(state.bytes.capacity(), old_len);
    assert!(old_len < BoundedOutput::HEAD && old_len * 3 > SESSION_BUFFER_LIMIT);

    state.push(&vec![b'b'; SESSION_BUFFER_LIMIT + 17]);
    assert_eq!(state.bytes.capacity(), SESSION_BUFFER_LIMIT);
    let (bytes, dropped) = state.take(true);
    assert_eq!(dropped, old_len + 17);
    assert_eq!(bytes.len(), SESSION_BUFFER_LIMIT);
    assert!(bytes[..old_len].iter().all(|&byte| byte == b'a'));
    assert!(bytes[old_len..].iter().all(|&byte| byte == b'b'));
}

#[test]
fn every_pair_of_poll_boundaries_preserves_valid_utf8() {
    for text in ["", "ASCII", "é", "中", "😀", "aé中😀z"] {
        let input = text.as_bytes();
        for first in 0..=input.len() {
            for second in first..=input.len() {
                let mut buffer = input[..first].to_vec();
                let a = take_ready_bytes(&mut buffer, false);
                assert!(buffer.len() <= 3);
                buffer.extend_from_slice(&input[first..second]);
                let b = take_ready_bytes(&mut buffer, false);
                assert!(buffer.len() <= 3);
                buffer.extend_from_slice(&input[second..]);
                let c = take_ready_bytes(&mut buffer, true);
                assert!(buffer.is_empty());
                assert_eq!(
                    format!(
                        "{}{}{}",
                        String::from_utf8_lossy(&a),
                        String::from_utf8_lossy(&b),
                        String::from_utf8_lossy(&c)
                    ),
                    text,
                    "poll boundaries {first}, {second}"
                );
            }
        }
    }
}

#[test]
fn one_byte_and_empty_polls_do_not_consume_an_incomplete_scalar() {
    let text = "é中😀";
    let mut buffer = Vec::new();
    let mut rendered = String::new();
    for byte in text.bytes() {
        buffer.push(byte);
        let ready = take_ready_bytes(&mut buffer, false);
        rendered.push_str(&String::from_utf8_lossy(&ready));
        let pending = buffer.clone();
        assert!(take_ready_bytes(&mut buffer, false).is_empty());
        assert_eq!(buffer, pending);
        assert!(buffer.len() <= 3);
    }
    assert!(take_ready_bytes(&mut buffer, true).is_empty());
    assert_eq!(rendered, text);
}

#[test]
fn definite_invalid_binary_bytes_are_not_held_for_future_polls() {
    let cases: &[&[u8]] = &[
        &[0xff],
        &[0x80, 0x81],
        &[0xc0, 0xaf],
        &[0xe0, 0x80],
        &[0xed, 0xa0],
        &[0xf4, 0x90],
        &[0xf5],
        &[0xe2, b'x'],
    ];
    for input in cases {
        let mut buffer = input.to_vec();
        assert_eq!(take_ready_bytes(&mut buffer, false), *input);
        assert!(buffer.is_empty());
    }

    let mut buffer = vec![0xff, b'x', 0xf0, 0x9f];
    let ready = take_ready_bytes(&mut buffer, false);
    assert_eq!(ready, [0xff, b'x']);
    assert_eq!(String::from_utf8_lossy(&ready), "�x");
    assert_eq!(buffer, [0xf0, 0x9f]);
    buffer.extend_from_slice(&[0x98, 0x80]);
    assert_eq!(
        String::from_utf8_lossy(&take_ready_bytes(&mut buffer, false)),
        "😀"
    );
    assert!(buffer.is_empty());
}

#[test]
fn final_collection_flushes_incomplete_bytes_with_existing_lossy_semantics() {
    for input in [&[0xc3][..], &[0xe4, 0xb8][..], &[0xf0, 0x9f, 0x98][..]] {
        let mut buffer = input.to_vec();
        assert!(take_ready_bytes(&mut buffer, false).is_empty());
        assert_eq!(buffer, input);
        let ready = take_ready_bytes(&mut buffer, true);
        assert_eq!(ready, input);
        assert_eq!(
            String::from_utf8_lossy(&ready),
            String::from_utf8_lossy(input)
        );
        assert!(buffer.is_empty());
    }
}

fn rendered(bytes: Vec<u8>, dropped_bytes: usize, exited: bool) -> Value {
    serde_json::from_str(&render_result(
        "utf8-call",
        42,
        Collected {
            bytes,
            dropped_bytes,
            exited,
            exit_code: exited.then_some(0),
        },
        Duration::ZERO,
        SESSION_BUFFER_LIMIT * 2,
        json!({}),
    ))
    .unwrap()
}

#[test]
fn split_scalar_renders_once_and_output_bytes_count_delivered_bytes() {
    let mut buffer = vec![0xf0, 0x9f];
    let first = rendered(take_ready_bytes(&mut buffer, false), 0, false);
    assert_eq!(first["metadata"]["output_bytes"], 0);
    assert_eq!(first["metadata"]["truncated"], false);
    assert!(first["output"].as_str().unwrap().ends_with("Output:\n"));
    buffer.extend_from_slice(&[0x98, 0x80]);
    let second = rendered(take_ready_bytes(&mut buffer, true), 0, true);
    assert_eq!(second["metadata"]["output_bytes"], 4);
    assert_eq!(second["metadata"]["truncated"], false);
    assert_eq!(second["metadata"]["session_id"], Value::Null);
    assert!(second["output"].as_str().unwrap().ends_with("Output:\n😀"));
    assert!(buffer.is_empty());
}

#[test]
fn retained_suffix_does_not_break_existing_dropped_middle_rendering() {
    let mut buffer = BoundedOutput::default();
    buffer.push(&vec![b'x'; SESSION_BUFFER_LIMIT]);
    buffer.push(b"0123456789abcde");
    buffer.push(&[0xf0, 0x9f]);
    let (bytes, dropped) = buffer.take(false);
    let first = rendered(bytes, dropped, false);
    assert_eq!(first["metadata"]["output_bytes"], SESSION_BUFFER_LIMIT - 2);
    assert_eq!(first["metadata"]["dropped_bytes"], 17);
    assert_eq!(first["metadata"]["truncated"], true);
    assert!(!first["output"].as_str().unwrap().contains('�'));
    assert_eq!(buffer.bytes, [0xf0, 0x9f]);
    buffer.push(&[0x98, 0x80]);
    let (bytes, dropped) = buffer.take(true);
    assert_eq!(dropped, 0);
    let second = rendered(bytes, dropped, true);
    assert!(second["output"].as_str().unwrap().ends_with("Output:\n😀"));
}

#[test]
fn display_budget_counts_lossy_utf8_bytes_not_raw_process_bytes() {
    // The invalid byte expands into a three-byte replacement character. Neither
    // edge of the render budget may split it, even though the raw input fits.
    let result: Value = serde_json::from_str(&render_result(
        "call",
        42,
        Collected {
            bytes: vec![b'a', 0xff, b'z'],
            dropped_bytes: 0,
            exited: true,
            exit_code: Some(2),
        },
        Duration::ZERO,
        3,
        json!({"nested": ["unchanged"]}),
    ))
    .unwrap();
    assert_eq!(
        result["output"],
        "Wall time: 0.0000 seconds\nProcess exited with code 2\nOutput:\na\n[... omitted 3 of 5 bytes ...]\nz"
    );
    assert_eq!(result["ok"], true);
    assert_eq!(result["metadata"]["output_bytes"], 3);
    assert_eq!(result["metadata"]["truncated"], true);
    assert_eq!(result["metadata"]["nested"], json!(["unchanged"]));
}
