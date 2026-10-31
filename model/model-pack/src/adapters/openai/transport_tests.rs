use super::*;
use crate::{
    adapters::test_http,
    contracts::Model,
    domain::{CacheHints, ModelRef},
    model_standard::{CanonicalMessage, MessageRole},
};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn affinity_request(
    session: Option<&str>,
    thread: Option<&str>,
    turn: &str,
) -> CanonicalModelRequest {
    let mut metadata = BTreeMap::from([("turn_id".into(), turn.into())]);
    if let Some(session) = session {
        metadata.insert("session_id".into(), session.into());
    }
    if let Some(thread) = thread {
        metadata.insert("thread_id".into(), thread.into());
    }
    CanonicalModelRequest::new(
        ModelRef::new("fixture", "fixture-model"),
        vec![CanonicalMessage::text(MessageRole::User, turn)],
    )
    .with_client_metadata(metadata)
    .with_cache(CacheHints::new(true, true).with_routing_key("canonical-routing-key"))
}

#[tokio::test]
async fn codex_cache_affinity_is_per_request_and_independent_of_body_overrides() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let root = tempfile::tempdir().unwrap();
        let auth_file = super::super::codex_tests::auth_file(root.path());
        let response = json!({"status":"completed", "output":[]});
        for codex in [false, true] {
            for stream in [false, true] {
                let identities = [
                    (Some("session-a"), Some("thread-a"), "turn-1"),
                    (Some("session-a"), Some("thread-a"), "turn-2"),
                    (Some("session-b"), Some("thread-b"), "turn-1"),
                    (None, None, "no-identity"),
                    (Some("session-c"), None, "session-only"),
                    (None, Some("thread-c"), "thread-only"),
                ];
                let replies = identities
                    .iter()
                    .map(|_| {
                        if codex || stream {
                            test_http::sse(response.clone())
                        } else {
                            (200, "application/json", response.to_string())
                        }
                    })
                    .collect();
                let (url, server) = test_http::server(replies).await;
                let mut config = json!({
                    "base_url":url, "stream":stream, "prompt_cache_key":"shared-custom-key",
                    "client_metadata":{"session_id":"configured-session", "thread_id":"configured-thread"},
                });
                let mut client = if codex {
                    config["auth_file"] = json!(auth_file);
                    OpenAiResponsesClient::from_codex_config(config).unwrap()
                } else {
                    config["api_key"] = json!("fixture-api-key");
                    OpenAiResponsesClient::from_provider_config(config).unwrap()
                };
                // Keep this wire regression local even with ambient proxy vars.
                client.http = reqwest::Client::builder().no_proxy().build().unwrap();
                for (session, thread, turn) in identities {
                    let mut events = client
                        .stream(affinity_request(session, thread, turn))
                        .await
                        .unwrap();
                    let mut completed = false;
                    while let Some(event) = events.next().await {
                        completed |= matches!(event.unwrap(), ModelStreamEvent::Response { .. });
                    }
                    assert!(completed);
                }
                let requests = server.await.unwrap();
                assert_eq!(requests.len(), identities.len());
                for (request, (session, thread, turn)) in requests.iter().zip(identities) {
                    let header = |name: &str| request.headers.get(name).map(String::as_str);
                    assert_eq!(header("session-id"), if codex { session } else { None });
                    assert_eq!(header("thread-id"), if codex { thread } else { None });
                    let body: Value = serde_json::from_str(&request.body).unwrap();
                    assert_eq!(body["prompt_cache_key"], "shared-custom-key");
                    assert_eq!(body["client_metadata"]["turn_id"], turn);
                    assert_eq!(
                        body["client_metadata"]["session_id"],
                        session.unwrap_or("configured-session"),
                    );
                }
            }
        }
    })
    .await
    .expect("cache-affinity wire fixture timed out");
}

#[tokio::test]
async fn codex_identity_headers_survive_auth_refresh() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let root = tempfile::tempdir().unwrap();
        let auth_file = super::super::codex_tests::auth_file(root.path());
        let (url, server) = test_http::server(vec![
            (401, "application/json", "{}".into()),
            (
                200,
                "application/json",
                json!({"access_token":"rotated", "refresh_token":"next", "expires_in":3600})
                    .to_string(),
            ),
            test_http::sse(json!({"status":"completed", "output":[]})),
        ])
        .await;
        let mut client = OpenAiResponsesClient::from_codex_config(json!({
            "base_url":url, "oauth_issuer":url, "auth_file":auth_file,
        }))
        .unwrap();
        client.http = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut events = client
            .stream(affinity_request(
                Some("session-a"),
                Some("thread-a"),
                "turn-1",
            ))
            .await
            .unwrap();
        while let Some(event) = events.next().await {
            event.unwrap();
        }
        let requests = server.await.unwrap();
        assert_eq!(requests[1].target, "POST /oauth/token HTTP/1.1");
        for index in [0, 2] {
            assert_eq!(requests[index].headers["session-id"], "session-a");
            assert_eq!(requests[index].headers["thread-id"], "thread-a");
        }
        assert!(!requests[1].headers.contains_key("session-id"));
        assert!(!requests[1].headers.contains_key("thread-id"));
    })
    .await
    .expect("cache-affinity auth-refresh fixture timed out");
}

#[tokio::test]
async fn invalid_codex_identity_fails_before_auth_without_exposing_value() {
    let root = tempfile::tempdir().unwrap();
    let missing_auth = root.path().join("not-logged-in.json");
    let client =
        OpenAiResponsesClient::from_codex_config(json!({"auth_file":missing_auth})).unwrap();
    for (key, header) in [("session_id", "session-id"), ("thread_id", "thread-id")] {
        let mut request = affinity_request(None, None, "turn-1");
        request
            .client_metadata
            .insert(key.into(), "private-identity\r\ninvalid".into());
        let error = client.stream(request).await.err().unwrap();
        assert_eq!(
            format!("{error:#}"),
            format!("invalid ChatGPT {header} header")
        );
    }
    assert!(!missing_auth.exists());
}

#[tokio::test]
async fn shared_constructor_preserves_selected_redirect_policy_without_ambient_proxy() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let root = tempfile::tempdir().unwrap();
        let missing_auth = root.path().join("not-logged-in.json");
        for codex in [false, true] {
            for http1_only in [false, true] {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("http://{}", listener.local_addr().unwrap());
                let config = json!({
                    "base_url": url, "auth_file": missing_auth, "http1_only": http1_only,
                });
                let policy = if codex {
                    reqwest::redirect::Policy::none()
                } else {
                    reqwest::redirect::Policy::default()
                };
                // Exercise the shared construction path without letting ambient
                // proxy settings route a loopback fixture request off-machine.
                // Actual API/Codex policy selection is reviewed at its call sites.
                let client = OpenAiResponsesClient::from_provider_config_with_http(
                    config,
                    reqwest::Client::builder().no_proxy().redirect(policy),
                )
                .unwrap();

                // Both futures belong to this timeout: no detached fixture server
                // survives a failed assertion, protocol regression, or deadline.
                let server = async move {
                    for index in 0..if codex { 1 } else { 2 } {
                        let (mut socket, _) = listener.accept().await.unwrap();
                        let mut request = Vec::new();
                        while !request.ends_with(b"\r\n\r\n") {
                            assert!(request.len() < 16 * 1024, "fixture headers exceeded bound");
                            request.push(socket.read_u8().await.unwrap());
                        }
                        let target = if index == 0 { "initial" } else { "final" };
                        assert!(request.starts_with(format!("GET /{target} HTTP/1.1\r\n").as_bytes()));
                        let response = if index == 0 {
                            "HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        } else {
                            "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndone"
                        };
                        socket.write_all(response.as_bytes()).await.unwrap();
                    }
                };
                let request = async {
                    // Inspect the shared constructor's transport directly; no
                    // credentials or provider APIs participate in this fixture.
                    let response = client.http.get(format!("{url}/initial")).send().await.unwrap();
                    if codex {
                        assert_eq!(response.status(), StatusCode::FOUND);
                        assert_eq!(response.url().path(), "/initial");
                        assert!(response.bytes().await.unwrap().is_empty());
                    } else {
                        assert_eq!(response.status(), StatusCode::OK);
                        assert_eq!(response.url().path(), "/final");
                        assert_eq!(response.text().await.unwrap(), "done");
                    }
                };
                tokio::join!(server, request);
            }
        }
        assert!(!missing_auth.exists());
    })
    .await
    .expect("constructor redirect fixture timed out");
}

#[test]
fn constructors_keep_config_validation_order_without_accessing_auth() {
    let root = tempfile::tempdir().unwrap();
    let missing_auth = root.path().join("not-logged-in.json");
    for codex in [false, true] {
        let construct = |config| {
            if codex {
                OpenAiResponsesClient::from_codex_config(config)
            } else {
                OpenAiResponsesClient::from_provider_config(config)
            }
        };
        let mut config = json!({
            "auth_file": missing_auth, "request_max_retries": "invalid", "http1_only": "invalid",
        });
        assert_eq!(
            construct(config.clone()).unwrap_err().to_string(),
            "openai request_max_retries must be a non-negative integer",
        );
        config
            .as_object_mut()
            .unwrap()
            .remove("request_max_retries");
        assert_eq!(
            construct(config).unwrap_err().to_string(),
            "openai http1_only must be a boolean",
        );
    }
    assert_eq!(
        OpenAiResponsesClient::from_codex_config(json!({
            "auth_file": "", "request_max_retries": "invalid",
        }))
        .unwrap_err()
        .to_string(),
        "openai_codex auth_file must be a non-empty string",
    );
    assert!(!missing_auth.exists());
}
