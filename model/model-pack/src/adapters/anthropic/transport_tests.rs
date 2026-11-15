//! Real adapter HTTP/SSE boundaries against an in-process loopback fixture.
use super::*;
use crate::{
    adapters::test_http,
    domain::{ToolCall, ToolContent, ToolResult},
};
use futures_util::StreamExt;

fn request() -> CanonicalModelRequest {
    CanonicalModelRequest::new(
        ModelRef::new("fixture", "fixture-model"),
        vec![CanonicalMessage::text(MessageRole::User, "hello")],
    )
}

fn sse(events: &[(&str, Value)]) -> String {
    events
        .iter()
        .map(|(kind, body)| format!("event: {kind}\ndata: {body}\n\n"))
        .collect()
}

fn client(url: &str, stream: bool) -> AnthropicMessagesClient {
    AnthropicMessagesClient::from_provider_config(json!({
        "base_url": url, "api_key": "loopback-fixture-only", "stream": stream,
        "http1_only": true,
    }))
    .unwrap()
}

#[tokio::test]
async fn http_failures_keep_typed_recovery_in_both_modes() {
    for stream in [false, true] {
        for (status, error_type, message, kind) in [
            (
                400,
                "invalid_request_error",
                "prompt is too long: 2 > 1",
                ModelFailureKind::ContextWindowExceeded,
            ),
            (
                529,
                "overloaded_error",
                "Overloaded",
                ModelFailureKind::Retryable {
                    retry_delay_ms: None,
                },
            ),
            (
                400,
                "invalid_request_error",
                "invalid max_tokens",
                ModelFailureKind::Other,
            ),
        ] {
            let body = json!({"error":{"type":error_type,"message":message}}).to_string();
            let (url, server) = test_http::server(vec![(status, "application/json", body)]).await;
            let error = match client(&url, stream).stream(request()).await {
                Err(error) => error,
                Ok(_) => panic!("HTTP failure unexpectedly admitted a stream"),
            };
            assert_eq!(ModelFailure::from_error(&error).kind, kind);
            let requests = server.await.unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].target, "POST /v1/messages HTTP/1.1");
        }
    }
}

#[tokio::test]
async fn sse_preserves_initial_content_and_inline_tool_images_on_the_wire() {
    let body = sse(&[
        (
            "message_start",
            json!({"message":{"usage":{"input_tokens":3,"output_tokens":0}}}),
        ),
        (
            "content_block_start",
            json!({"index":0,"content_block":{"type":"text","text":"initial "}}),
        ),
        (
            "content_block_delta",
            json!({"index":0,"delta":{"type":"text_delta","text":"text"}}),
        ),
        ("content_block_stop", json!({"index":0})),
        (
            "content_block_start",
            json!({"index":1,"content_block":{"type":"tool_use","id":"next","name":"read_file","input":{"path":"next.rs"}}}),
        ),
        ("content_block_stop", json!({"index":1})),
        (
            "message_delta",
            json!({"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":4}}),
        ),
        ("message_stop", json!({})),
    ]);
    let (url, server) = test_http::server(vec![(200, "text/event-stream", body)]).await;
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";
    let mut request = request();
    let mut result = ToolResult::ok("image-call".into(), "image output");
    result.content.push(ToolContent::Image {
        mime_type: "image/png".into(),
        data: png.into(),
    });
    request.messages.extend([
        CanonicalMessage::new(
            MessageRole::Assistant,
            vec![ContentPart::ToolCall {
                call: ToolCall::new("image-call", "inspect", json!({})),
            }],
        ),
        CanonicalMessage::new(MessageRole::Tool, vec![ContentPart::ToolResult { result }]),
    ]);
    let events = client(&url, true)
        .stream(request)
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            Ok(ModelStreamEvent::TextDelta { text, .. }) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "initial text");
    let response = events
        .iter()
        .find_map(|event| match event {
            Ok(ModelStreamEvent::Response { response }) => Some(response),
            _ => None,
        })
        .expect("terminal response");
    assert_eq!(response.tool_calls[0].args, json!({"path":"next.rs"}));
    assert_eq!(response.messages[0].display_text(), "initial text");
    let requests = server.await.unwrap();
    let wire: Value = serde_json::from_str(&requests[0].body).unwrap();
    assert_eq!(wire["stream"], true);
    let blocks = wire["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().unwrap());
    let result = blocks
        .into_iter()
        .find(|block| block["type"] == "tool_result")
        .unwrap();
    assert_eq!(result["tool_use_id"], "image-call");
    assert_eq!(result["content"][0]["text"], "image output");
    assert_eq!(
        result["content"][1],
        json!({"type":"image","source":{
            "type":"base64","media_type":"image/png","data":png,
        }})
    );
}

#[tokio::test]
async fn sse_error_and_clean_eof_never_complete_a_partial_response() {
    let prefix = sse(&[(
        "content_block_start",
        json!({"index":0,"content_block":{"type":"text","text":"partial"}}),
    )]);
    for (tail, expected) in [
        (String::new(), ModelFailureKind::StreamDisconnected),
        (
            sse(&[(
                "error",
                json!({"error":{"type":"overloaded_error","message":"Overloaded"}}),
            )]),
            ModelFailureKind::Retryable {
                retry_delay_ms: None,
            },
        ),
        (
            "event: message_delta\ndata: {\n\n".into(),
            ModelFailureKind::Other,
        ),
    ] {
        let (url, server) =
            test_http::server(vec![(200, "text/event-stream", format!("{prefix}{tail}"))]).await;
        let events = client(&url, true)
            .stream(request())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        let failures: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Ok(ModelStreamEvent::Error { failure }) => Some(failure),
                _ => None,
            })
            .collect();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].kind, expected);
        assert!(failures[0].completed_messages.is_empty());
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Ok(ModelStreamEvent::Response { .. })))
        );
        assert_eq!(server.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn context_stop_is_recoverable_but_output_token_limit_is_a_response() {
    for stream in [false, true] {
        for stop in ["model_context_window_exceeded", "max_tokens"] {
            let (content_type, body) = if stream {
                (
                    "text/event-stream",
                    sse(&[
                        (
                            "content_block_start",
                            json!({"index":0,"content_block":{"type":"text","text":"partial"}}),
                        ),
                        ("message_delta", json!({"delta":{"stop_reason":stop}})),
                        ("message_stop", json!({})),
                    ]),
                )
            } else {
                (
                    "application/json",
                    json!({"type":"message","role":"assistant",
                    "content":[{"type":"text","text":"partial"}],"stop_reason":stop})
                    .to_string(),
                )
            };
            let (url, server) = test_http::server(vec![(200, content_type, body)]).await;
            let outcome = client(&url, stream).stream(request()).await;
            let events = match outcome {
                Ok(events) => events.collect::<Vec<_>>().await,
                Err(error) => vec![Ok(ModelStreamEvent::Error {
                    failure: ModelFailure::from_error(&error),
                })],
            };
            if stop == "model_context_window_exceeded" {
                assert!(events.iter().any(
                    |event| matches!(event, Ok(ModelStreamEvent::Error { failure })
                    if failure.kind == ModelFailureKind::ContextWindowExceeded)
                ));
                assert!(
                    !events
                        .iter()
                        .any(|event| matches!(event, Ok(ModelStreamEvent::Response { .. })))
                );
            } else {
                assert!(events.iter().any(
                    |event| matches!(event, Ok(ModelStreamEvent::Response { response })
                    if response.finish_reason == FinishReason::Length)
                ));
            }
            assert_eq!(server.await.unwrap().len(), 1);
        }
    }
}
