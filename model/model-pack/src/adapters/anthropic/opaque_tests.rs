use super::*;
use crate::domain::{ToolResult, new_session_id, new_thread_id};
use proteus_core::core::SessionStore;

fn mixed_blocks() -> Value {
    json!([
        {"type": "thinking", "thinking": "Check the fixture.", "signature": "sig+/=\n"},
        {"type": "redacted_thinking", "data": "opaque+/=\nПример", "extension": {"version": 2}},
        {"type": "text", "text": "I will read the file."},
        {"type": "redacted_thinking", "data": "second-opaque=="},
        {"type": "tool_use", "id": "call_read", "name": "read_file", "input": {"path": "пример.txt", "limit": 2}}
    ])
}

fn nonstream_response(blocks: &Value) -> CanonicalModelResponse {
    from_anthropic_response(json!({"content": blocks, "stop_reason": "tool_use"})).unwrap()
}

fn stream_response(blocks: &Value) -> CanonicalModelResponse {
    let mut state = AnthropicStreamState::default();
    let mut events = Vec::new();
    events.extend(state.translate(
        "message_start",
        &json!({"message": {"usage": {}}}).to_string(),
    ));
    for (index, block) in blocks.as_array().unwrap().iter().enumerate() {
        let mut start = block.clone();
        match block["type"].as_str().unwrap() {
            "thinking" => {
                start["thinking"] = json!("");
                start.as_object_mut().unwrap().remove("signature");
            }
            "text" => start["text"] = json!(""),
            "tool_use" => start["input"] = json!({}),
            "redacted_thinking" => {}
            other => panic!("unexpected fixture block: {other}"),
        }
        let start_events = state.translate(
            "content_block_start",
            &json!({"index": index, "content_block": start}).to_string(),
        );
        // Encrypted data belongs only to the canonical response, never a display delta.
        if block["type"] == "redacted_thinking" {
            assert!(start_events.is_empty());
        }
        events.extend(start_events);
        let deltas = match block["type"].as_str().unwrap() {
            "thinking" => vec![
                json!({"type": "thinking_delta", "thinking": block["thinking"]}),
                json!({"type": "signature_delta", "signature": block["signature"]}),
            ],
            "text" => vec![json!({"type": "text_delta", "text": block["text"]})],
            "tool_use" => {
                let input = block["input"].to_string();
                // Split the JSON before its multibyte filename, like independent SSE chunks.
                let (first, second) = input.split_at(1);
                vec![
                    json!({"type": "input_json_delta", "partial_json": first}),
                    json!({"type": "input_json_delta", "partial_json": second}),
                ]
            }
            _ => Vec::new(),
        };
        for delta in deltas {
            events.extend(state.translate(
                "content_block_delta",
                &json!({"index": index, "delta": delta}).to_string(),
            ));
        }
        events.extend(state.translate("content_block_stop", &json!({"index": index}).to_string()));
    }
    events.extend(state.translate("message_delta", r#"{"delta":{"stop_reason":"tool_use"}}"#));
    events.extend(state.translate("message_stop", "{}"));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ModelStreamEvent::Error { .. }))
    );
    let mut responses = events.into_iter().filter_map(|event| match event {
        ModelStreamEvent::Response { response } => Some(response),
        _ => None,
    });
    let response = responses.next().expect("terminal response");
    assert!(responses.next().is_none());
    response
}

fn serialized_history(response: CanonicalModelResponse) -> Vec<CanonicalMessage> {
    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
    assert_eq!(response.tool_calls.len(), 1);
    let message = &response.messages[0];
    assert_eq!(message.parts.len(), 5);
    for index in [1, 3] {
        let ContentPart::Opaque { format, data } = &message.parts[index].payload else {
            panic!("redacted block lost at index {index}");
        };
        assert_eq!(format, REDACTED_THINKING_FORMAT);
        assert_eq!(data, &mixed_blocks()[index]);
    }
    assert!(!message.display_text().contains("opaque"));
    let encoded = serde_json::to_vec(&response.messages).unwrap();
    let mut history: Vec<CanonicalMessage> = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(history, response.messages);
    history.push(CanonicalMessage::new(
        MessageRole::Tool,
        vec![ContentPart::ToolResult {
            result: ToolResult::ok("call_read".to_owned(), "file contents"),
        }],
    ));
    history
}

fn assert_next_request(history: Vec<CanonicalMessage>, blocks: &Value) {
    let body = to_anthropic_request(&CanonicalModelRequest::new(
        ModelRef::new("anthropic", "fixture"),
        history,
    ))
    .unwrap();
    assert_eq!(
        body["messages"],
        json!([
            {"role": "assistant", "content": blocks},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call_read", "content": "file contents", "is_error": false}]}
        ])
    );
}

#[test]
fn redacted_nonstream_tool_round_trip_preserves_all_blocks_in_order() {
    let blocks = mixed_blocks();
    assert_next_request(serialized_history(nonstream_response(&blocks)), &blocks);
}

#[test]
fn redacted_sse_tool_round_trip_preserves_all_blocks_in_order() {
    let blocks = mixed_blocks();
    assert_next_request(serialized_history(stream_response(&blocks)), &blocks);
}

#[tokio::test]
async fn redacted_stream_and_nonstream_survive_cold_journal_and_tool_continuation() {
    let blocks = mixed_blocks();
    for response in [nonstream_response(&blocks), stream_response(&blocks)] {
        let history = serialized_history(response);
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path(), new_session_id()).unwrap();
        store
            .append_history(new_thread_id(), None, &history)
            .await
            .unwrap();
        let dir = store.session_dir().to_path_buf();
        drop(store);
        let restored = SessionStore::open(dir).unwrap().load_messages().unwrap();
        assert_eq!(restored, history);
        assert_next_request(restored, &blocks);
    }
}

#[test]
fn anthropic_rejects_foreign_opaque_format_and_malformed_redacted_payload() {
    for (format, data, expected) in [
        (
            "other.provider.block",
            json!({"type": "redacted_thinking", "data": "opaque"}),
            "does not support opaque format",
        ),
        (
            REDACTED_THINKING_FORMAT,
            json!({"type": "text", "data": "opaque"}),
            "invalid opaque redacted_thinking block",
        ),
        (
            REDACTED_THINKING_FORMAT,
            json!({"type": "redacted_thinking", "data": 42}),
            "invalid opaque redacted_thinking block",
        ),
    ] {
        let request = CanonicalModelRequest::new(
            ModelRef::new("anthropic", "fixture"),
            vec![CanonicalMessage::new(
                MessageRole::Assistant,
                vec![ContentPart::Opaque {
                    format: format.into(),
                    data,
                }],
            )],
        );
        let error = to_anthropic_request(&request).unwrap_err().to_string();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn malformed_redacted_response_is_an_error_in_both_transports() {
    for block in [
        json!({"type": "redacted_thinking"}),
        json!({"type": "redacted_thinking", "data": 42}),
    ] {
        assert!(
            from_anthropic_response(json!({"content": [block.clone()], "stop_reason": "end_turn"}))
                .is_err()
        );
        let events = AnthropicStreamState::default().translate(
            "content_block_start",
            &json!({"index": 0, "content_block": block}).to_string(),
        );
        assert!(matches!(
            events.as_slice(),
            [ModelStreamEvent::Error { .. }]
        ));
    }
}
