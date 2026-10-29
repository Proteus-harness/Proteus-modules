use super::*;
use futures_util::StreamExt;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

async fn read_body(socket: &mut TcpStream) -> Value {
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        headers.push(socket.read_u8().await.expect("fixture request headers"));
        assert!(headers.len() < 16 * 1024);
    }
    let headers = String::from_utf8(headers).unwrap();
    assert!(headers.starts_with("POST /responses HTTP/1.1\r\n"));
    let length = headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .unwrap()
        .1
        .trim()
        .parse::<usize>()
        .unwrap();
    assert!(length < 64 * 1024);
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

async fn exercise_transport_failure(fallback: bool, fallback_fails: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let mut server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let first = read_body(&mut socket).await;
        // Invalid UTF-8 causes an SSE transport error rather than a clean EOF.
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 1\r\nConnection: close\r\n\r\n\xff").await.unwrap();
        drop(socket);
        let mut bodies = vec![first];
        if fallback {
            let (mut socket, _) = listener.accept().await.unwrap();
            bodies.push(read_body(&mut socket).await);
            let (status, body) = if fallback_fails {
                (
                    "400 Bad Request",
                    json!({"error":{"message":"fixture fallback rejected"}}),
                )
            } else {
                ("200 OK", json!({"status":"completed","output":[]}))
            };
            let body = body.to_string();
            socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
        bodies
    });
    let mut client = OpenAiResponsesClient::from_provider_config(json!({
        "base_url": endpoint,
        "api_key": "local-fixture-only",
        "http1_only": true,
        "stream_error_fallback": fallback,
        "request_max_retries": 0,
    }))
    .unwrap();
    client.http = reqwest::Client::builder()
        .no_proxy()
        .http1_only()
        .build()
        .unwrap();
    let request = CanonicalModelRequest::new(
        ModelRef::new("openai", "fixture"),
        vec![CanonicalMessage::text(
            MessageRole::User,
            "Юникод\n preserved",
        )],
    );
    let expected_first = client.request_body(&request, true).unwrap();
    let expected_fallback = client.request_body(&request, false).unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        let events = client
            .stream_response(request)
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        let bodies = (&mut server).await.unwrap();
        (events, bodies)
    })
    .await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            server.abort();
            panic!("local transport fixture timed out: {error}");
        }
    };
    let (events, bodies) = outcome;
    assert_eq!(bodies[0], expected_first);
    if fallback {
        assert_eq!(bodies, vec![expected_first, expected_fallback]);
    } else {
        assert_eq!(bodies.len(), 1);
    }
    assert_eq!(events.len(), 1, "exactly one terminal event");
    match events.into_iter().next().unwrap().unwrap() {
        ModelStreamEvent::Response { .. } => assert!(fallback && !fallback_fails),
        ModelStreamEvent::Error { failure } => {
            assert!(!fallback || fallback_fails);
            assert!(failure.message.starts_with("sse transport error:"));
            if fallback {
                assert!(failure.message.contains("non-stream fallback failed:"));
                assert!(failure.message.contains("fixture fallback rejected"));
            } else {
                assert_eq!(failure.kind, ModelFailureKind::StreamDisconnected);
            }
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn transport_failure_without_fallback_emits_one_error_without_replay() {
    exercise_transport_failure(false, false).await;
}

#[tokio::test]
async fn diagnostic_fallback_replays_the_request_without_streaming() {
    exercise_transport_failure(true, false).await;
}

#[tokio::test]
async fn diagnostic_fallback_preserves_both_transport_and_completion_errors() {
    exercise_transport_failure(true, true).await;
}
