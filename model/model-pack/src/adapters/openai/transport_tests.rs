use super::*;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

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
