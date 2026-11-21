use super::super::{response::from_anthropic_response, stream::AnthropicStreamState};
use super::*;
use crate::model_standard::{FinishReason, ModelStreamEvent};
use serde_json::json;

#[test]
fn only_exact_http_200_context_stop_enables_recovery() {
    for status in [101, 200, 201, 204, 302, 400, 429, 529, 598] {
        for stop in [
            None,
            Some("model_context_window_exceeded"),
            Some("model_context_window_exceeded_extra"),
            Some("max_tokens"),
            Some("tool_use"),
            Some("end_turn"),
            Some("future_stop"),
            Some(""),
        ] {
            let failure =
                context_window_stop_failure(reqwest::StatusCode::from_u16(status).unwrap(), stop);
            assert_eq!(
                failure.is_some(),
                status == 200 && stop == Some("model_context_window_exceeded")
            );
            if let Some(failure) = failure {
                assert_eq!(failure.kind, ModelFailureKind::ContextWindowExceeded);
                assert!(failure.completed_messages.is_empty());
                let wrapped = anyhow::Error::new(failure.clone()).context("model callback");
                assert_eq!(ModelFailure::from_error(&wrapped), failure);
                let wire = serde_json::to_string(&failure).unwrap();
                assert_eq!(
                    serde_json::from_str::<ModelFailure>(&wire).unwrap(),
                    failure
                );
            }
        }
    }
}

fn trace(status: u16, stop: Option<&str>, args: &str) -> Vec<ModelStreamEvent> {
    let mut state = AnthropicStreamState::new(reqwest::StatusCode::from_u16(status).unwrap());
    let mut events = Vec::new();
    for (kind, data) in [
        (
            "message_start",
            json!({"message": {"usage": {"input_tokens": 3, "output_tokens": 1}}}),
        ),
        (
            "content_block_start",
            json!({"index": 0, "content_block": {"type": "text", "text": ""}}),
        ),
        (
            "content_block_delta",
            json!({"index": 0, "delta": {"type": "text_delta", "text": "partial"}}),
        ),
        ("content_block_stop", json!({"index": 0})),
        (
            "content_block_start",
            json!({"index": 1, "content_block": {"type": "tool_use", "id": "call", "name": "read_file", "input": {}}}),
        ),
        (
            "content_block_delta",
            json!({"index": 1, "delta": {"type": "input_json_delta", "partial_json": args}}),
        ),
        ("content_block_stop", json!({"index": 1})),
        (
            "message_delta",
            json!({"delta": {"stop_reason": stop}, "usage": {"output_tokens": 4}}),
        ),
        ("message_stop", json!({})),
    ] {
        events.extend(state.translate(kind, &data.to_string()));
    }
    events
}

#[test]
fn context_stop_fails_without_completing_partial_text_or_tools() {
    let events = trace(200, Some("model_context_window_exceeded"), "{");
    assert!(events.iter().any(
        |event| matches!(event, ModelStreamEvent::TextDelta { text, .. } if text == "partial")
    ));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ModelStreamEvent::ToolCallDelta { .. }))
    );
    assert!(!events.iter().any(|event| matches!(
        event,
        ModelStreamEvent::Response { .. }
            | ModelStreamEvent::MessageCompleted { .. }
            | ModelStreamEvent::ToolCallFinished { .. }
    )));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ModelStreamEvent::Error { .. }))
            .count(),
        1
    );
    let Some(ModelStreamEvent::Error { failure }) = events.last() else {
        panic!("expected terminal failure");
    };
    assert_eq!(failure.kind, ModelFailureKind::ContextWindowExceeded);
    assert!(failure.completed_messages.is_empty());
}

#[test]
fn other_statuses_stops_and_successful_tool_use_keep_their_projection() {
    for status in [101, 200, 201, 204, 302] {
        for (stop, finish) in [
            (Some("model_context_window_exceeded"), FinishReason::Length),
            (Some("max_tokens"), FinishReason::Length),
            (Some("future_stop"), FinishReason::Unknown),
            (None, FinishReason::Unknown),
            (Some("end_turn"), FinishReason::Stop),
            (Some("stop_sequence"), FinishReason::Stop),
            (Some("tool_use"), FinishReason::ToolCalls),
        ] {
            if status == 200 && stop == Some("model_context_window_exceeded") {
                continue;
            }
            let events = trace(status, stop, r#"{"x":1}"#);
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, ModelStreamEvent::Error { .. }))
            );
            let Some(ModelStreamEvent::Response { response }) = events.last() else {
                panic!("expected original Response shape");
            };
            assert_eq!(response.finish_reason, finish);
            assert_eq!(
                response.tool_calls.len(),
                usize::from(finish == FinishReason::ToolCalls)
            );
            let usage = response.usage.as_ref().unwrap();
            assert_eq!((usage.input_tokens, usage.output_tokens), (3, 4));
        }
    }
    let events = trace(200, Some("tool_use"), "{");
    assert!(
        matches!(events.last(), Some(ModelStreamEvent::Error { failure }) if failure.kind == ModelFailureKind::Other)
    );
}

#[test]
fn nonstream_decoder_keeps_error_precedence_and_stop_metadata() {
    for body in [
        json!({"error": {"message": "provider error"}, "stop_reason": "model_context_window_exceeded", "content": []}),
        json!({"stop_reason": "model_context_window_exceeded"}),
    ] {
        let error = from_anthropic_response(body).unwrap_err();
        assert_eq!(
            ModelFailure::from_error(&error).kind,
            ModelFailureKind::Other
        );
    }
    for stop in ["model_context_window_exceeded", "max_tokens", "future_stop"] {
        let response = from_anthropic_response(json!({
            "content": [{"type": "text", "text": "partial"}],
            "stop_reason": stop,
            "usage": {"input_tokens": 3, "output_tokens": 4}
        }))
        .unwrap();
        assert_eq!(response.provider_metadata["stop_reason"], stop);
        assert_eq!(response.usage.as_ref().unwrap().output_tokens, 4);
        for status in [200, 201, 204] {
            let failure = context_window_stop_failure(
                reqwest::StatusCode::from_u16(status).unwrap(),
                response
                    .provider_metadata
                    .get("stop_reason")
                    .and_then(Value::as_str),
            );
            assert_eq!(
                failure.is_some(),
                status == 200 && stop == "model_context_window_exceeded"
            );
        }
    }
}
