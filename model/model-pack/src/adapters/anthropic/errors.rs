use serde_json::Value;

use crate::model_standard::{ModelFailure, ModelFailureKind};

/// A generated-but-truncated HTTP 200 response needs context recovery, not
/// an unchanged retry. Output-token limits and unknown statuses remain unchanged.
pub(super) fn context_window_stop_failure(
    status: reqwest::StatusCode,
    stop_reason: Option<&str>,
) -> Option<ModelFailure> {
    if status == reqwest::StatusCode::OK && stop_reason == Some("model_context_window_exceeded") {
        Some(ModelFailure::new(
            ModelFailureKind::ContextWindowExceeded,
            "Anthropic response filled the model context window before finishing the turn",
        ))
    } else {
        None
    }
}

pub(super) fn http_failure(status: reqwest::StatusCode, body: &str) -> ModelFailure {
    let parsed = serde_json::from_str::<Value>(body).ok();
    failure_from_error_value(
        parsed.as_ref().and_then(|value| value.get("error")),
        Some(status.as_u16()),
        format!("Anthropic API error {status}: {body}"),
    )
}

/// Only HTTP 200 establishes the documented Messages SSE recovery semantics.
/// Other statuses previously admitted by error_for_status retain Other failures.
pub(super) fn stream_failure_kind(
    status: reqwest::StatusCode,
    kind: ModelFailureKind,
) -> ModelFailureKind {
    if status == reqwest::StatusCode::OK {
        kind
    } else {
        ModelFailureKind::Other
    }
}

/// HTTP status plus provider error type identify the narrow recovery cases.
/// None is an error event in an established SSE stream, not an unknown HTTP status.
pub(super) fn failure_from_error_value(
    error: Option<&Value>,
    http_status: Option<u16>,
    fallback_message: impl Into<String>,
) -> ModelFailure {
    let provider_type = error
        .and_then(|error| error.get("type"))
        .and_then(Value::as_str);
    let provider_message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str);
    let kind = match (http_status, provider_type, provider_message) {
        (Some(400), Some("invalid_request_error"), Some(message))
            if message == "prompt is too long" || message.starts_with("prompt is too long:") =>
        {
            ModelFailureKind::ContextWindowExceeded
        }
        (None | Some(529), Some("overloaded_error"), Some(_)) => ModelFailureKind::Retryable {
            retry_delay_ms: None,
        },
        _ => ModelFailureKind::Other,
    };
    // Keep the existing non-stream HTTP diagnostic, including status and raw body.
    // SSE keeps the provider's message or its existing fallback.
    let message = match (http_status, provider_message) {
        (None, Some(message)) => message.to_owned(),
        _ => fallback_message.into(),
    };
    ModelFailure::new(kind, message)
}

#[cfg(test)]
mod stop_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_documented_http_status_and_error_pairs_enable_recovery() {
        for status in [200, 302, 400, 413, 422, 429, 500, 529, 598] {
            for (error_type, message, expected_status, expected_kind) in [
                (
                    "invalid_request_error",
                    "prompt is too long: 200001 tokens > 200000 maximum",
                    400,
                    ModelFailureKind::ContextWindowExceeded,
                ),
                (
                    "overloaded_error",
                    "Overloaded",
                    529,
                    ModelFailureKind::Retryable {
                        retry_delay_ms: None,
                    },
                ),
            ] {
                let body = json!({
                    "type": "error",
                    "error": { "type": error_type, "message": message }
                })
                .to_string();
                let status_code = reqwest::StatusCode::from_u16(status).unwrap();
                let failure = http_failure(status_code, &body);
                assert_eq!(
                    failure.kind,
                    if status == expected_status {
                        expected_kind
                    } else {
                        ModelFailureKind::Other
                    }
                );
                assert_eq!(
                    failure.message,
                    format!("Anthropic API error {status_code}: {body}")
                );
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

    #[test]
    fn non_200_stream_statuses_never_gain_recovery() {
        for status in [101, 200, 201, 204, 302, 400, 529, 598] {
            for kind in [
                ModelFailureKind::StreamDisconnected,
                ModelFailureKind::Retryable {
                    retry_delay_ms: None,
                },
                ModelFailureKind::Other,
            ] {
                assert_eq!(
                    stream_failure_kind(reqwest::StatusCode::from_u16(status).unwrap(), kind),
                    if status == 200 {
                        kind
                    } else {
                        ModelFailureKind::Other
                    }
                );
            }
        }
    }

    #[test]
    fn unrelated_or_malformed_errors_remain_other() {
        for error in [
            json!({"type": "invalid_request_error", "message": "max_tokens is invalid"}),
            json!({"type": "invalid_request_error", "message": "prefix: prompt is too long"}),
            json!({"type": "invalid_request_error", "message": "prompt is too longish"}),
            json!({"type": "future_error", "message": "prompt is too long"}),
            json!({"type": "rate_limit_error", "message": "slow down"}),
            json!({"type": "overloaded_error", "message": 1}),
            json!({"type": "overloaded_error"}),
            json!({"message": "prompt is too long"}),
            Value::Null,
        ] {
            for status in [None, Some(400), Some(529)] {
                assert_eq!(
                    failure_from_error_value(Some(&error), status, "fallback").kind,
                    ModelFailureKind::Other
                );
            }
        }
        for body in ["not JSON", "null", "{}"] {
            assert_eq!(
                http_failure(reqwest::StatusCode::BAD_REQUEST, body).kind,
                ModelFailureKind::Other
            );
        }
        let exact = json!({"type": "invalid_request_error", "message": "prompt is too long"});
        assert_eq!(
            failure_from_error_value(Some(&exact), Some(400), "fallback").kind,
            ModelFailureKind::ContextWindowExceeded
        );
        assert_eq!(
            failure_from_error_value(Some(&exact), None, "fallback").kind,
            ModelFailureKind::Other
        );
    }

    #[test]
    fn sse_error_mapping_preserves_message_and_never_completes_partial_text() {
        for (error, expected_kind, expected_message) in [
            (
                json!({"type": "overloaded_error", "message": "Overloaded"}),
                ModelFailureKind::Retryable {
                    retry_delay_ms: None,
                },
                "Overloaded",
            ),
            (
                json!({"type": "future_error", "message": "unknown diagnosis"}),
                ModelFailureKind::Other,
                "unknown diagnosis",
            ),
            (
                Value::Null,
                ModelFailureKind::Other,
                "unknown anthropic error",
            ),
        ] {
            let mut state = super::super::stream::AnthropicStreamState::default();
            let _ = state.translate(
                "content_block_start",
                &json!({
                    "index": 0, "content_block": {"type": "text", "text": ""}
                })
                .to_string(),
            );
            let _ = state.translate(
                "content_block_delta",
                &json!({
                    "index": 0, "delta": {"type": "text_delta", "text": "partial"}
                })
                .to_string(),
            );
            let events = state.translate(
                "error",
                &json!({"type": "error", "error": error}).to_string(),
            );
            assert_eq!(events.len(), 1);
            let crate::model_standard::ModelStreamEvent::Error { failure } = &events[0] else {
                panic!("expected failure, not a completed partial message");
            };
            assert_eq!(failure.kind, expected_kind);
            assert_eq!(failure.message, expected_message);
            assert!(failure.completed_messages.is_empty());
        }
        let mut state = super::super::stream::AnthropicStreamState::default();
        let events = state.translate("error", "not JSON");
        let crate::model_standard::ModelStreamEvent::Error { failure } = &events[0] else {
            panic!("expected parse error");
        };
        assert_eq!(failure.kind, ModelFailureKind::Other);
    }
}
