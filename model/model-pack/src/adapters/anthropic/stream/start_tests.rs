use super::super::response::from_anthropic_response;
use super::*;
use serde_json::json;

fn block_trace(block: Value, deltas: &[Value], stop_reason: &str) -> Vec<ModelStreamEvent> {
    let mut state = AnthropicStreamState::default();
    let mut events = state.translate("message_start", r#"{"message":{"usage":{}}}"#);
    events.extend(state.translate(
        "content_block_start",
        &json!({"index": 0, "content_block": block}).to_string(),
    ));
    for delta in deltas {
        events.extend(state.translate(
            "content_block_delta",
            &json!({"index": 0, "delta": delta}).to_string(),
        ));
    }
    events.extend(state.translate("content_block_stop", r#"{"index":0}"#));
    events.extend(state.translate(
        "message_delta",
        &json!({"delta": {"stop_reason": stop_reason}}).to_string(),
    ));
    events.extend(state.translate("message_stop", "{}"));
    events
}

fn response(events: &[ModelStreamEvent]) -> &CanonicalModelResponse {
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ModelStreamEvent::Error { .. }))
    );
    let Some(ModelStreamEvent::Response { response }) = events.last() else {
        panic!("expected final response, got {events:?}");
    };
    response
}

fn payloads(response: &CanonicalModelResponse) -> Vec<ContentPart> {
    response
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .map(|part| part.payload.clone())
        .collect()
}

fn assert_text(events: &[ModelStreamEvent], expected: &str) {
    let response = response(events);
    let mut streamed = String::new();
    for event in events {
        if let ModelStreamEvent::TextDelta {
            message_id,
            phase,
            text,
        } = event
        {
            assert_eq!(*message_id, response.messages[0].id);
            assert_eq!(*phase, None);
            streamed.push_str(text);
        }
    }
    assert_eq!(streamed, expected);
    let nonstream = from_anthropic_response(json!({
        "content": [{"type": "text", "text": expected}], "stop_reason": "end_turn"
    }))
    .unwrap();
    assert_eq!(payloads(response), payloads(&nonstream));
    assert_eq!(response.finish_reason, nonstream.finish_reason);
}

#[test]
fn text_start_is_accumulated_and_emitted_before_optional_delta() {
    for (initial, suffix) in [
        ("1 + 1", None),
        ("1 + 1", Some("")),
        ("1 + 1", Some(" = 2")),
        ("Привет", Some(" 🌍")),
        ("", Some("Hello")),
        ("", None),
    ] {
        let deltas = suffix
            .map(|text| json!({"type": "text_delta", "text": text}))
            .into_iter()
            .collect::<Vec<_>>();
        let events = block_trace(
            json!({"type": "text", "text": initial}),
            &deltas,
            "end_turn",
        );
        assert_text(&events, &format!("{initial}{}", suffix.unwrap_or("")));
        if !initial.is_empty() {
            assert!(
                matches!(&events[0], ModelStreamEvent::TextDelta { text, .. } if text == initial)
            );
        } else if suffix.is_none() {
            assert_eq!(events.len(), 1, "empty start emits no delta");
        }
    }
}

#[test]
fn start_and_delta_share_filter_state_across_a_split_marker() {
    for (initial, suffix) in [
        (
            "before <｜｜DS",
            "ML｜｜tool_calls>hidden</｜｜DSML｜｜tool_calls> after",
        ),
        (
            "before <｜｜DSML｜｜tool_calls>hidden</｜｜DSML｜｜tool_calls> after",
            "",
        ),
    ] {
        let events = block_trace(
            json!({"type": "text", "text": initial}),
            &[json!({"type": "text_delta", "text": suffix})],
            "end_turn",
        );
        assert_text(&events, "before  after");
    }
}

#[test]
fn tool_start_input_survives_absent_and_empty_json_deltas() {
    for input in [
        None,
        Some(Value::Null),
        Some(json!({})),
        Some(json!({"path": "пример.txt"})),
    ] {
        let mut block = json!({"type": "tool_use", "id": "call", "name": "read_file"});
        if let Some(input) = input {
            block["input"] = input;
        }
        let expected = from_anthropic_response(json!({
            "content": [block.clone()], "stop_reason": "tool_use"
        }))
        .unwrap();
        for count in 0..=2 {
            let deltas = vec![json!({"type": "input_json_delta", "partial_json": ""}); count];
            let events = block_trace(block.clone(), &deltas, "tool_use");
            let actual = response(&events);
            assert_eq!(actual.tool_calls, expected.tool_calls);
            assert_eq!(payloads(actual), payloads(&expected));
            assert_eq!(actual.finish_reason, FinishReason::ToolCalls);
            assert_eq!(
                events.len(),
                1,
                "fallback input belongs to the completed call, not a synthetic JSON delta"
            );
        }
    }
}

#[test]
fn nonempty_json_deltas_replace_rather_than_merge_or_append_initial_input() {
    let events = block_trace(
        json!({"type": "tool_use", "id": "call", "name": "read_file", "input": {"seed": true}}),
        &[
            json!({"type": "input_json_delta", "partial_json": ""}),
            json!({"type": "input_json_delta", "partial_json": "{\"path\":"}),
            json!({"type": "input_json_delta", "partial_json": "\"x.txt\"}"}),
            json!({"type": "input_json_delta", "partial_json": ""}),
        ],
        "tool_use",
    );
    let actual = response(&events);
    assert_eq!(actual.tool_calls[0].args, json!({"path": "x.txt"}));
    let partials = events
        .iter()
        .filter_map(|event| match event {
            ModelStreamEvent::ToolCallDelta {
                call_id,
                args_delta,
                ..
            } => {
                assert_eq!(call_id, "call");
                Some(args_delta.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(partials, vec!["{\"path\":", "\"x.txt\"}"]);
}

#[test]
fn malformed_nonempty_delta_is_not_repaired_with_initial_input() {
    for partial in ["{", " ", "invalid"] {
        let events = block_trace(
            json!({"type": "tool_use", "id": "call", "name": "read_file", "input": {}}),
            &[json!({"type": "input_json_delta", "partial_json": partial})],
            "tool_use",
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ModelStreamEvent::Response { .. }))
        );
        assert!(
            matches!(events.last(), Some(ModelStreamEvent::Error { failure })
            if failure.kind == crate::model_standard::ModelFailureKind::Other
                && failure.message.starts_with("invalid Anthropic tool arguments:"))
        );
    }
}

#[test]
fn start_input_does_not_make_non_tool_terminal_executable() {
    for (stop, expected) in [
        ("max_tokens", FinishReason::Length),
        ("end_turn", FinishReason::Stop),
        ("future_reason", FinishReason::Unknown),
    ] {
        for deltas in [
            vec![],
            vec![json!({"type": "input_json_delta", "partial_json": "{"})],
        ] {
            let events = block_trace(
                json!({"type": "tool_use", "id": "call", "name": "read_file", "input": {}}),
                &deltas,
                stop,
            );
            let actual = response(&events);
            assert_eq!(actual.finish_reason, expected);
            assert!(actual.tool_calls.is_empty());
            assert!(
                !payloads(actual)
                    .iter()
                    .any(|part| matches!(part, ContentPart::ToolCall { .. }))
            );
        }
    }
}
