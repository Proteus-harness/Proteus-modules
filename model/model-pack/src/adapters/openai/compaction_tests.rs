use super::*;
use crate::{
    adapters::test_http,
    model_standard::{InstructionBlock, InstructionKind, ModelOperation},
};
use futures_util::StreamExt;

fn checkpoint() -> Value {
    json!({"type":"compaction","id":"cmp-fixture","encrypted_content":"opaque+/=\nfixture"})
}

fn compact_request() -> CanonicalModelRequest {
    CanonicalModelRequest::new(
        ModelRef::new("subscription", "fixture-model"),
        vec![
            CanonicalMessage::text(MessageRole::User, "keep this user"),
            CanonicalMessage::text(MessageRole::Assistant, "completed work"),
        ],
    )
    .with_operation(ModelOperation::Compact)
    .with_instructions(vec![InstructionBlock::new(
        InstructionKind::Developer,
        "original base instructions",
        10,
    )])
    .with_tools(vec![ToolSpec::new(
        "read_file",
        "Read a file",
        json!({"type":"object","properties":{}}),
        crate::domain::ToolSafety::ReadOnly,
    )])
}

fn event(event: Value) -> String {
    format!("data: {event}\n\n")
}

fn done(item: Value) -> Value {
    json!({"type":"response.output_item.done","output_index":0,"item":item})
}

fn completed() -> Value {
    json!({"type":"response.completed","response":{"status":"completed","output":[],"usage":{"input_tokens":69,"output_tokens":119}}})
}

fn compaction_sse() -> (u16, &'static str, String) {
    (
        200,
        "text/event-stream",
        event(done(checkpoint())) + &event(completed()),
    )
}

async fn terminal(mut stream: ModelEventStream) -> ModelStreamEvent {
    let result = stream.next().await.expect("terminal event").unwrap();
    assert!(stream.next().await.is_none(), "one terminal event only");
    result
}

fn canonical_checkpoint(response: &CanonicalModelResponse) -> &Value {
    assert_eq!(response.finish_reason, FinishReason::Stop);
    assert!(response.tool_calls.is_empty());
    assert_eq!(response.messages.len(), 1);
    assert!(response.messages[0].display_text().is_empty());
    assert!(response.messages[0].reasoning_text().is_empty());
    let [part] = response.messages[0].parts.as_slice() else {
        panic!("one checkpoint part");
    };
    let ContentPart::Opaque { format, data } = &part.payload else {
        panic!("checkpoint must remain opaque");
    };
    assert_eq!(format, compaction::FORMAT);
    assert_eq!(part.scope, crate::model_standard::PartScope::Conversation);
    data
}

#[test]
fn capability_is_native_by_default_and_explicit_for_compatible_endpoints() {
    let native = OpenAiResponsesClient::from_provider_config(json!({})).unwrap();
    let model = ModelRef::new("openai", "fixture");
    assert!(native.capabilities(&model).unwrap().supports_compaction);
    for url in [
        "https://other.example/v1",
        "https://api.openai.com.evil.example/v1",
        "http://api.openai.com/v1",
        "https://api.openai.com/custom",
    ] {
        let client = OpenAiResponsesClient::from_provider_config(json!({"base_url":url})).unwrap();
        assert!(!client.capabilities(&model).unwrap().supports_compaction);
        assert!(client.request_body(&compact_request(), true).is_err());
    }
    let configured = OpenAiResponsesClient::from_provider_config(json!({
        "base_url":"https://other.example/v1", "capabilities":{"supports_compaction":true}
    }))
    .unwrap();
    assert!(configured.capabilities(&model).unwrap().supports_compaction);
    assert!(
        OpenAiResponsesClient::from_provider_config(
            json!({"capabilities":{"supports_compaction":"yes"}})
        )
        .is_err()
    );

    let codex =
        OpenAiResponsesClient::from_codex_config(json!({"auth_file":"not-read.json"})).unwrap();
    assert!(codex.capabilities(&model).unwrap().supports_compaction);
    let disabled = OpenAiResponsesClient::from_codex_config(
        json!({"auth_file":"not-read.json","capabilities":{"supports_compaction":false}}),
    )
    .unwrap();
    assert!(!disabled.capabilities(&model).unwrap().supports_compaction);
}

#[test]
fn compact_encoding_preserves_controls_tools_and_history_but_removes_output_schema() {
    let mut request = compact_request();
    request.reasoning.effort = Some("high".into());
    request.cache.cache_instructions = true;
    request.cache.routing_key = Some("stable-route".into());
    request.response_format = crate::domain::ResponseFormat::JsonSchema {
        name: "fixture".into(),
        schema: json!({"type":"object"}),
        strict: true,
    };
    let before = request.clone();
    let client = OpenAiResponsesClient::from_provider_config(json!({
        "capabilities":{"supports_reasoning_config":true,"supports_json_schema":true}
    }))
    .unwrap();
    let body = client.request_body(&request, true).unwrap();
    assert_eq!(body["instructions"], "original base instructions");
    assert_eq!(body["tools"][0]["name"], "read_file");
    assert_eq!(body["reasoning"]["effort"], "high");
    assert_eq!(body["prompt_cache_key"], "stable-route");
    assert!(body.get("text").is_none());
    assert_eq!(body["input"].as_array().unwrap().len(), 3);
    assert_eq!(body["input"][0]["content"][0]["text"], "keep this user");
    assert_eq!(body["input"][1]["content"][0]["text"], "completed work");
    assert_eq!(body["input"][2], json!({"type":"compaction_trigger"}));
    assert_eq!(request, before);
}

#[test]
fn opaque_round_trip_preserves_sequence_and_repeated_compaction_input() {
    let item = checkpoint();
    let response = from_openai_response(json!({"status":"completed","output":[item]})).unwrap();
    assert_eq!(canonical_checkpoint(&response), &checkpoint());
    let mut messages = vec![CanonicalMessage::text(MessageRole::User, "retained user")];
    messages.extend(response.messages);
    messages.push(CanonicalMessage::text(MessageRole::User, "continue"));
    let request = CanonicalModelRequest::new(ModelRef::new("openai", "fixture"), messages);
    let body = to_openai_request(&request).unwrap();
    assert_eq!(body["input"][1], checkpoint());
    assert_eq!(body["input"][2]["content"][0]["text"], "continue");
    let body = to_openai_request(&request.with_operation(ModelOperation::Compact)).unwrap();
    assert_eq!(body["input"][1], checkpoint());
    assert_eq!(body["input"][3], json!({"type":"compaction_trigger"}));
}

#[test]
fn malformed_checkpoint_cannot_be_decoded_or_replayed_as_another_item() {
    for item in [
        json!({"type":"compaction"}),
        json!({"type":"compaction","encrypted_content":"  "}),
        json!({"type":"compaction","encrypted_content":false}),
        json!({"type":"compaction","encrypted_content":"cipher","id":42}),
        json!({"type":"function_call","encrypted_content":"cipher","name":"danger"}),
    ] {
        let request = CanonicalModelRequest::new(
            ModelRef::new("openai", "fixture"),
            vec![CanonicalMessage::new(
                MessageRole::Assistant,
                vec![ContentPart::Opaque {
                    format: compaction::FORMAT.into(),
                    data: item.clone(),
                }],
            )],
        );
        assert!(to_openai_request(&request).is_err(), "{item}");
        if item["type"] == "compaction" {
            assert!(from_openai_response(json!({"output":[item]})).is_err());
        }
    }
}

#[tokio::test]
async fn checkpoint_survives_cold_history_and_cannot_be_sent_to_an_unsupported_endpoint() {
    use crate::domain::{new_session_id, new_thread_id};
    use proteus_core::core::SessionStore;
    let response = from_openai_response(json!({"output":[checkpoint()]})).unwrap();
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path(), new_session_id()).unwrap();
    store
        .append_history(new_thread_id(), None, &response.messages)
        .await
        .unwrap();
    let path = store.session_dir().to_path_buf();
    drop(store);
    let restored = SessionStore::open(path).unwrap().load_messages().unwrap();
    assert_eq!(restored, response.messages);
    let request = CanonicalModelRequest::new(ModelRef::new("openai", "fixture"), restored);
    assert_eq!(
        to_openai_request(&request).unwrap()["input"][0],
        checkpoint()
    );
    let client =
        OpenAiResponsesClient::from_provider_config(json!({"base_url":"http://127.0.0.1:1"}))
            .unwrap();
    assert!(
        client
            .stream(request)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("cannot replay compaction checkpoints")
    );
}

#[test]
fn strict_collector_waits_for_terminal_and_accepts_upstream_alias() {
    for kind in ["compaction", "compaction_summary"] {
        let mut state = OpenAiStreamState::for_compaction(true);
        let mut item = checkpoint();
        item["type"] = json!(kind);
        assert!(state.translate(&done(item.clone()).to_string()).is_empty());
        let events = state.translate(&completed().to_string());
        let [ModelStreamEvent::Response { response }] = events.as_slice() else {
            panic!("one successful terminal response: {events:?}");
        };
        assert_eq!(canonical_checkpoint(response), &checkpoint());
        let replay = to_openai_request(&CanonicalModelRequest::new(
            ModelRef::new("openai", "fixture"),
            response.messages.clone(),
        ))
        .unwrap();
        assert_eq!(
            replay["input"][0],
            checkpoint(),
            "upstream alias is replayed as canonical compaction"
        );
        assert_eq!(response.usage.as_ref().unwrap().input_tokens, 69);
        assert_eq!(response.usage.as_ref().unwrap().output_tokens, 119);
    }
}

#[test]
fn strict_collector_rejects_missing_duplicate_or_malformed_done_checkpoint() {
    for items in [
        vec![],
        vec![checkpoint(), checkpoint()],
        vec![json!({"type":"compaction","encrypted_content":""})],
    ] {
        let mut state = OpenAiStreamState::for_compaction(true);
        for item in items {
            assert!(state.translate(&done(item).to_string()).is_empty());
        }
        let mut end = completed();
        // A final output array is not a substitute for an OutputItemDone.
        end["response"]["output"] = json!([checkpoint()]);
        assert!(matches!(
            state.translate(&end.to_string()).as_slice(),
            [ModelStreamEvent::Error { .. }]
        ));
    }
}

#[tokio::test]
async fn api_compaction_forces_sse_then_replays_checkpoint_in_ordinary_request() {
    let (url, server) = test_http::server(vec![
        compaction_sse(),
        (200,"application/json",json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"continued"}]}]}).to_string()),
    ]).await;
    let client = OpenAiResponsesClient::from_provider_config(json!({
        "base_url":url,"api_key":"fixture-api-key","stream":false,
        "capabilities":{"supports_compaction":true,"supports_parallel_tool_calls":true}
    }))
    .unwrap();
    let ModelStreamEvent::Response { response } =
        terminal(client.stream(compact_request()).await.unwrap()).await
    else {
        panic!("compact success");
    };
    assert_eq!(canonical_checkpoint(&response), &checkpoint());
    let mut request =
        CanonicalModelRequest::new(ModelRef::new("openai", "fixture"), response.messages);
    request
        .messages
        .push(CanonicalMessage::text(MessageRole::User, "continue"));
    let ModelStreamEvent::Response { response } =
        terminal(client.stream(request).await.unwrap()).await
    else {
        panic!("continuation success");
    };
    assert_eq!(response.messages[0].display_text(), "continued");
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2);
    let first: Value = serde_json::from_str(&requests[0].body).unwrap();
    let second: Value = serde_json::from_str(&requests[1].body).unwrap();
    assert_eq!(requests[0].target, "POST /responses HTTP/1.1");
    assert_eq!(first["stream"], true);
    assert_eq!(first["tools"][0]["name"], "read_file");
    assert_eq!(first["instructions"], "original base instructions");
    assert_eq!(
        first["input"].as_array().unwrap().last().unwrap(),
        &json!({"type":"compaction_trigger"})
    );
    assert_eq!(second["input"][0], checkpoint());
    assert!(
        second["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x["type"] != "compaction_trigger")
    );
}

#[tokio::test]
async fn codex_compaction_uses_existing_oauth_identity_in_both_stream_modes() {
    let root = tempfile::tempdir().unwrap();
    let auth_file = codex_tests::auth_file(root.path());
    for stream in [false, true] {
        let (url, server) = test_http::server(vec![compaction_sse()]).await;
        let client = OpenAiResponsesClient::from_codex_config(
            json!({"base_url":url,"auth_file":auth_file,"stream":stream}),
        )
        .unwrap();
        let mut request = compact_request();
        request
            .client_metadata
            .insert("session_id".into(), "fixture-session".into());
        request
            .client_metadata
            .insert("thread_id".into(), "fixture-thread".into());
        let ModelStreamEvent::Response { response } =
            terminal(client.stream(request).await.unwrap()).await
        else {
            panic!("compact success");
        };
        assert_eq!(canonical_checkpoint(&response), &checkpoint());
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].headers["authorization"],
            "Bearer fixture-access"
        );
        assert_eq!(requests[0].headers["chatgpt-account-id"], "fixture-account");
        assert_eq!(requests[0].headers["originator"], "proteus");
        assert_eq!(requests[0].headers["accept"], "text/event-stream");
        assert_eq!(requests[0].headers["session-id"], "fixture-session");
        assert_eq!(requests[0].headers["thread-id"], "fixture-thread");
    }
}

#[tokio::test]
async fn interrupted_or_failed_compaction_never_publishes_a_checkpoint() {
    let variants = [
        event(done(checkpoint())),
        event(done(checkpoint()))
            + &event(
                json!({"type":"response.failed","response":{"error":{"code":"invalid_prompt","message":"fixture rejected"}}}),
            ),
        event(done(checkpoint()))
            + &event(
                json!({"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}}),
            ),
    ];
    for body in variants {
        let (url, server) = test_http::server(vec![(200, "text/event-stream", body)]).await;
        let client = OpenAiResponsesClient::from_provider_config(
            json!({"base_url":url,"api_key":"fixture-api-key","supports_compaction":true}),
        )
        .unwrap();
        assert!(matches!(
            terminal(client.stream(compact_request()).await.unwrap()).await,
            ModelStreamEvent::Error { .. }
        ));
        assert_eq!(server.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn unsupported_adapter_rejects_compact_before_auth_or_network() {
    let client =
        OpenAiResponsesClient::from_provider_config(json!({"base_url":"http://127.0.0.1:1"}))
            .unwrap();
    assert!(
        client
            .stream(compact_request())
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("does not support native compaction")
    );
    let client = crate::adapters::AnthropicMessagesClient::from_provider_config(
        json!({"base_url":"http://127.0.0.1:1"}),
    )
    .unwrap();
    assert!(
        client
            .stream(compact_request())
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("does not support native compaction")
    );
}

#[tokio::test]
async fn compact_never_uses_diagnostic_inference_fallback_after_a_broken_stream() {
    use std::time::Duration;
    use tokio::{io::AsyncWriteExt, net::TcpListener};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        test_http::read(&mut socket).await;
        let prefix = event(done(checkpoint()));
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",prefix.len()+1).as_bytes()).await.unwrap();
        socket.write_all(prefix.as_bytes()).await.unwrap();
        socket.write_all(&[0xff]).await.unwrap();
        drop(socket);
        if let Ok(Ok((mut socket, _))) =
            tokio::time::timeout(Duration::from_millis(250), listener.accept()).await
        {
            test_http::read(&mut socket).await;
            test_http::reply(&mut socket, 400, "application/json", "{}").await;
            2
        } else {
            1
        }
    });
    let client = OpenAiResponsesClient::from_provider_config(json!({
        "base_url":url,"api_key":"fixture-only","supports_compaction":true,
        "stream_error_fallback":true
    }))
    .unwrap();
    let ModelStreamEvent::Error { failure } =
        terminal(client.stream(compact_request()).await.unwrap()).await
    else {
        panic!("broken compaction must fail");
    };
    assert_eq!(failure.kind, ModelFailureKind::StreamDisconnected);
    assert_eq!(
        server.await.unwrap(),
        1,
        "compaction must never replay as an ordinary inference request"
    );
}
