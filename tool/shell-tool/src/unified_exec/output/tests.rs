use super::*;

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
    let mut buffer = vec![b'x'; SESSION_BUFFER_LIMIT];
    buffer[SESSION_BUFFER_LIMIT - 2..].copy_from_slice(&[0xf0, 0x9f]);
    let first = rendered(take_ready_bytes(&mut buffer, false), 17, false);
    assert_eq!(first["metadata"]["output_bytes"], SESSION_BUFFER_LIMIT - 2);
    assert_eq!(first["metadata"]["dropped_bytes"], 17);
    assert_eq!(first["metadata"]["truncated"], true);
    assert!(!first["output"].as_str().unwrap().contains('�'));
    assert_eq!(buffer, [0xf0, 0x9f]);
    buffer.extend_from_slice(&[0x98, 0x80]);
    let second = rendered(take_ready_bytes(&mut buffer, true), 0, true);
    assert!(second["output"].as_str().unwrap().ends_with("Output:\n😀"));
}
