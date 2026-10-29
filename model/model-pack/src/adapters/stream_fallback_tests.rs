use super::{AnthropicMessagesClient, OpenAiResponsesClient, test_http};
use crate::{
    contracts::Model,
    domain::ModelRef,
    model_standard::{
        CanonicalMessage, CanonicalModelRequest, InstructionBlock, InstructionKind, MessageRole,
        ModelStreamEvent,
    },
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::{io::AsyncWriteExt, net::TcpListener};

#[tokio::test]
async fn transport_fallback_preserves_the_owned_request_for_each_provider() {
    for anthropic in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let streaming = test_http::read(&mut socket).await;
            // A truncated HTTP body forces a transport error after SSE has started.
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 999\r\nConnection: close\r\n\r\ndata: {}\n\n").await.unwrap();
            drop(socket);
            let (mut socket, _) = listener.accept().await.unwrap();
            let fallback = test_http::read(&mut socket).await;
            let response = if anthropic {
                json!({"type":"message", "role":"assistant", "content":[{"type":"text", "text":"recovered"}], "stop_reason":"end_turn"})
            } else {
                json!({"status":"completed", "output":[{"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":"recovered"}]}]})
            };
            test_http::reply(&mut socket, 200, "application/json", &response.to_string()).await;
            (streaming, fallback)
        });
        let config = json!({
            "base_url":endpoint, "api_key":"local-fixture-only",
            "stream":true, "stream_error_fallback":true, "http1_only":true,
        });
        let client: Box<dyn Model> = if anthropic {
            Box::new(AnthropicMessagesClient::from_provider_config(config).unwrap())
        } else {
            Box::new(OpenAiResponsesClient::from_provider_config(config).unwrap())
        };
        let request = CanonicalModelRequest::new(
            ModelRef::new("fixture", "fixture-model"),
            vec![CanonicalMessage::text(
                MessageRole::User,
                "preserve this prompt",
            )],
        )
        .with_instructions(vec![InstructionBlock::new(
            InstructionKind::System,
            "preserve instructions",
            100,
        )]);
        let events = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            client
                .stream(request)
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await
        })
        .await
        .unwrap();
        let [Ok(ModelStreamEvent::Response { response })] = events.as_slice() else {
            panic!("one recovered terminal response expected: {events:?}");
        };
        assert_eq!(response.messages[0].display_text(), "recovered");
        let (streaming, fallback) = server.await.unwrap();
        assert_eq!(streaming.target, fallback.target);
        let mut streaming: Value = serde_json::from_str(&streaming.body).unwrap();
        let mut fallback: Value = serde_json::from_str(&fallback.body).unwrap();
        assert_eq!(streaming["stream"], true);
        assert_ne!(fallback["stream"], true);
        streaming.as_object_mut().unwrap().remove("stream");
        fallback.as_object_mut().unwrap().remove("stream");
        assert_eq!(streaming, fallback);
    }
}
