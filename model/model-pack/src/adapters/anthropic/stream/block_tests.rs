//! A content block owns both its text and any withheld marker suffix.
use super::super::response::from_anthropic_response;
use super::*;
use serde_json::json;

fn translate(state: &mut AnthropicStreamState, kind: &str, data: Value) -> Vec<ModelStreamEvent> {
    state.translate(kind, &data.to_string())
}

fn start(state: &mut AnthropicStreamState, index: usize, block: Value) -> Vec<ModelStreamEvent> {
    translate(
        state,
        "content_block_start",
        json!({"index": index, "content_block": block}),
    )
}

fn stop(state: &mut AnthropicStreamState, index: usize) -> Vec<ModelStreamEvent> {
    translate(state, "content_block_stop", json!({"index": index}))
}

fn finish(state: &mut AnthropicStreamState, reason: &str) -> CanonicalModelResponse {
    translate(
        state,
        "message_delta",
        json!({"delta": {"stop_reason": reason}}),
    );
    let mut events = state.translate("message_stop", "{}");
    match events.pop().expect("terminal event") {
        ModelStreamEvent::Response { response } => response,
        other => panic!("expected Response, got {other:?}"),
    }
}

fn payloads(response: &CanonicalModelResponse) -> Vec<ContentPart> {
    response
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .map(|part| part.payload.clone())
        .collect()
}

fn text(events: &[ModelStreamEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            ModelStreamEvent::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn text_block_stop_flushes_literal_suffix_once_before_following_tool() {
    for suffix in ["<", "</"] {
        let mut state = AnthropicStreamState::default();
        let source = format!("The comparison operator is {suffix}");
        let mut events = start(&mut state, 0, json!({"type": "text", "text": source}));
        assert_eq!(text(&events), "The comparison operator is ");
        let flushed = stop(&mut state, 0);
        assert_eq!(
            text(&flushed),
            suffix,
            "literal text must not wait for another block"
        );
        events.extend(flushed);
        assert!(stop(&mut state, 0).is_empty(), "stop is idempotent");
        events.extend(start(&mut state, 1,
            json!({"type": "tool_use", "id": "call", "name": "read_file", "input": {"path": "x.rs"}})));
        events.extend(translate(
            &mut state,
            "content_block_delta",
            json!({"index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"path\":\"x.rs\"}"}}),
        ));
        stop(&mut state, 1);
        let last_text = events
            .iter()
            .rposition(|event| matches!(event, ModelStreamEvent::TextDelta { .. }))
            .unwrap();
        let first_tool = events
            .iter()
            .position(|event| matches!(event, ModelStreamEvent::ToolCallDelta { .. }))
            .unwrap();
        assert!(
            last_text < first_tool,
            "flush must precede the following tool delta"
        );
        let actual = finish(&mut state, "tool_use");
        let expected = from_anthropic_response(json!({"content": [
            {"type": "text", "text": source},
            {"type": "tool_use", "id": "call", "name": "read_file", "input": {"path": "x.rs"}}
        ], "stop_reason": "tool_use"}))
        .unwrap();
        assert_eq!(payloads(&actual), payloads(&expected));
        assert_eq!(actual.tool_calls, expected.tool_calls);
        assert_eq!(text(&events), source);
        assert!(events.iter().all(|event| match event {
            ModelStreamEvent::TextDelta {
                message_id, phase, ..
            } => *message_id == actual.messages[0].id && phase.is_none(),
            _ => true,
        }));
    }
}

#[test]
fn text_block_filter_never_borrows_the_next_blocks_text() {
    let mut state = AnthropicStreamState::default();
    start(&mut state, 0, json!({"type": "text", "text": "x <"}));
    stop(&mut state, 0);
    start(&mut state, 1, json!({"type": "text", "text": "y"}));
    stop(&mut state, 1);
    let actual = finish(&mut state, "end_turn");
    let expected = from_anthropic_response(json!({"content": [
        {"type": "text", "text": "x <"}, {"type": "text", "text": "y"}
    ], "stop_reason": "end_turn"}))
    .unwrap();
    assert_eq!(payloads(&actual), payloads(&expected));
}

#[test]
fn unterminated_dsml_in_one_text_block_does_not_erase_the_next_block() {
    let mut state = AnthropicStreamState::default();
    let first = "before <｜｜DSML｜｜tool_calls>hidden";
    let mut events = start(&mut state, 0, json!({"type": "text", "text": first}));
    events.extend(stop(&mut state, 0));
    events.extend(start(
        &mut state,
        1,
        json!({"type": "text", "text": "visible 🌍"}),
    ));
    events.extend(stop(&mut state, 1));
    let actual = finish(&mut state, "end_turn");
    let expected = from_anthropic_response(json!({"content": [
        {"type": "text", "text": first}, {"type": "text", "text": "visible 🌍"}
    ], "stop_reason": "end_turn"}))
    .unwrap();
    assert_eq!(payloads(&actual), payloads(&expected));
    assert_eq!(text(&events), "before visible 🌍");
}

#[test]
fn final_fallback_keeps_an_unstopped_suffix_in_its_source_block() {
    let mut state = AnthropicStreamState::default();
    start(&mut state, 0, json!({"type": "text", "text": "x <"}));
    start(
        &mut state,
        1,
        json!({"type": "tool_use", "id": "call", "name": "read", "input": {}}),
    );
    let actual = finish(&mut state, "tool_use");
    let expected = from_anthropic_response(json!({"content": [
        {"type": "text", "text": "x <"},
        {"type": "tool_use", "id": "call", "name": "read", "input": {}}
    ], "stop_reason": "tool_use"}))
    .unwrap();
    assert_eq!(payloads(&actual), payloads(&expected));
}

#[test]
fn same_block_dsml_filter_is_independent_of_utf8_chunk_boundaries() {
    for (source, expected_stream) in [
        (
            "Привет <｜｜DSML｜｜tool_calls>hidden</｜｜DSML｜｜tool_calls> 🌍",
            "Привет  🌍",
        ),
        (
            "before <｜｜DSML｜｜invoke>visible</｜｜DSML｜｜invoke> after",
            "before visible after",
        ),
        ("literal </", "literal </"),
        ("pending </｜｜DSML｜｜", "pending "),
    ] {
        for split in source
            .char_indices()
            .map(|(index, _)| index)
            .chain([source.len()])
        {
            let mut state = AnthropicStreamState::default();
            let mut events = start(
                &mut state,
                0,
                json!({"type": "text", "text": &source[..split]}),
            );
            events.extend(translate(
                &mut state,
                "content_block_delta",
                json!({"index": 0, "delta": {"type": "text_delta", "text": &source[split..]}}),
            ));
            events.extend(stop(&mut state, 0));
            let actual = finish(&mut state, "end_turn");
            assert_eq!(
                text(&events),
                expected_stream,
                "source={source:?}, split={split}"
            );
            let expected = from_anthropic_response(json!({"content": [
                {"type": "text", "text": expected_stream}
            ], "stop_reason": "end_turn"}))
            .unwrap();
            assert_eq!(payloads(&actual), payloads(&expected));
        }
    }
}

#[test]
fn non_text_stop_does_not_flush_another_blocks_pending_suffix() {
    let mut state = AnthropicStreamState::default();
    start(&mut state, 0, json!({"type": "text", "text": "x <"}));
    start(
        &mut state,
        1,
        json!({"type": "thinking", "thinking": "reason", "signature": "sig"}),
    );
    assert!(stop(&mut state, 1).is_empty());
    assert!(stop(&mut state, 10).is_empty());
    assert_eq!(text(&stop(&mut state, 0)), "<");
    let actual = finish(&mut state, "end_turn");
    assert_eq!(
        payloads(&actual),
        vec![
            ContentPart::Text { text: "x <".into() },
            ContentPart::Reasoning {
                text: "reason".into(),
                signature: Some("sig".into())
            },
        ]
    );
}

#[test]
fn text_delta_without_a_text_owner_is_ignored_without_touching_other_blocks() {
    let mut state = AnthropicStreamState::default();
    start(&mut state, 0, json!({"type": "text", "text": "x <"}));
    start(
        &mut state,
        1,
        json!({"type": "thinking", "thinking": "reason", "signature": "sig"}),
    );
    for index in [1, 10] {
        assert!(
            translate(
                &mut state,
                "content_block_delta",
                json!({
                    "index": index, "delta": {"type": "text_delta", "text": "not owned"}
                })
            )
            .is_empty()
        );
    }
    assert_eq!(text(&stop(&mut state, 0)), "<");
    let actual = finish(&mut state, "end_turn");
    assert_eq!(
        payloads(&actual),
        vec![
            ContentPart::Text { text: "x <".into() },
            ContentPart::Reasoning {
                text: "reason".into(),
                signature: Some("sig".into())
            },
        ]
    );
}
