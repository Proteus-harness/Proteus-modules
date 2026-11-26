use super::*;
use crate::adapters::test_http;
use crate::contracts::Model;
use crate::domain::ModelRef;
use crate::model_standard::{CanonicalMessage, CanonicalModelRequest, MessageRole};
use futures_util::StreamExt;
use rusqlite::Connection;
use std::collections::BTreeMap;

#[test]
fn credentials_use_active_saved_key_before_environment_and_reject_oauth() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keys.sqlite");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL;
       CREATE TABLE credential (id TEXT, integration_id TEXT, active INT, time_created INT, value TEXT);
       INSERT INTO credential VALUES ('a','opencode-go',0,2,'{\"type\":\"key\",\"key\":\"newer\"}'),
         ('b','opencode-go',1,1,'{\"type\":\"key\",\"key\":\"active\"}');").unwrap();
    let mut cfg = json!({"database_path":path});
    assert_eq!(credentials::key(&cfg).unwrap(), "active");
    cfg["api_key"] = json!("override");
    assert_eq!(credentials::key(&cfg).unwrap(), "override");
    db.execute(
        "UPDATE credential SET value=?1 WHERE active=1",
        [r#"{"type":"oauth","access":"private"}"#],
    )
    .unwrap();
    cfg.as_object_mut().unwrap().remove("api_key");
    assert!(
        !credentials::key(&cfg)
            .unwrap_err()
            .to_string()
            .contains("private")
    );
}

#[test]
fn quota_preserves_limit_reset_and_per_model_amount() {
    let meter = json!({"status":"ok","resetsAt":"2026-11-21T15:00:00Z","percent":15.5});
    let mut meters = json!({"usage":{"rolling":meter,"weekly":meter,"monthly":meter}});
    meters["usage"]["weekly"]["status"] = json!("rate-limited");
    let mut got = quota::usage(&serde_json::to_vec(&meters).unwrap(), 5).unwrap();
    let rule = json!({"ruleID":"r","scope":"each","coverage":"models","per":"model","resetsAt":"2026-11-21T15:00:00Z","shared":false,"picked":true,
      "limitDollars":12.0,"spentDollars":30.0,"models":[{"providerID":"p","modelID":"m","name":"Model","spentDollars":3.0}],"vendors":[]});
    got.buckets
        .extend(quota::budgets(&serde_json::to_vec(&vec![rule]).unwrap()).unwrap());
    got.validate().unwrap();
    assert_eq!(got.buckets[0].allowed, Some(false));
    assert_eq!(got.buckets[0].windows[0].duration_seconds, Some(18000));
    assert!(got.buckets[1].informational);
    assert_eq!(got.buckets[1].windows[0].used_percent, 25.);
}

#[tokio::test]
async fn go_streams_responses_tools_with_session_affinity() {
    let go = Go::new().unwrap();
    let mut client = go
        .transport(
            json!({"implementation":"opencode_go","api_key":"test-only"}),
            true,
        )
        .unwrap();
    let id = "deepseek-v4.1-flash";
    assert!(go.capabilities(id).unwrap().supports_image_input);
    assert!(go.capabilities("missing").is_err());
    let response = json!({"status":"completed","output":[{"type":"function_call","id":"item-1","call_id":"call-1","name":"fixture","arguments":"{\"x\":1}"}]});
    let (url, server) = test_http::server(vec![test_http::sse(response)]).await;
    client.http = Client::builder().no_proxy().build().unwrap();
    client.base_url = url;
    let request = CanonicalModelRequest::new(
        ModelRef::new("opencode", id),
        vec![CanonicalMessage::text(MessageRole::User, "fixture")],
    );
    assert!(client.request_identity_headers(&request).is_err());
    let request =
        request.with_client_metadata(BTreeMap::from([("session_id".into(), "case-1".into())]));
    let mut stream = client.stream(request).await.unwrap();
    let mut terminal = None;
    while let Some(event) = stream.next().await {
        if let crate::model_standard::ModelStreamEvent::Response { response } = event.unwrap() {
            terminal = Some(response);
        }
    }
    assert!(
        serde_json::to_string(&terminal.unwrap())
            .unwrap()
            .contains("call-1")
    );
    let requests = server.await.unwrap();
    let r = &requests[0];
    assert_eq!(r.headers["authorization"], "Bearer test-only");
    for h in [
        "x-opencode-session",
        "x-session-id",
        "x-session-affinity",
        "x-opencode-session-id",
    ] {
        assert_eq!(r.headers[h], "case-1");
    }
    assert_eq!(r.headers["x-opencode-client"], "proteus");
    assert!(r.target == "POST /responses HTTP/1.1");
}

#[test]
fn catalog_omits_non_chat_and_sampling_uses_model_flags() {
    let raw=br#"{"data":[{"id":"embed","type":"embedding"},{"id":"g","type":"chat","capabilities":{"reasoning":{"supported":true,"effort":["low","high"]}},"modalities":{"input":["image/png"]},"limit":{"context":100,"output":12}}]}"#;
    let models = Models::parse(raw).unwrap();
    assert_eq!(models.catalog().models.len(), 1);
    assert_eq!(
        models.catalog().models[0].reasoning_efforts,
        ["low", "high"]
    );
    let entry = models.get("g").unwrap();
    assert_eq!(entry.sampling(), (false, false));
    assert_eq!(entry.descriptor().max_output_tokens, Some(12));
}

#[tokio::test]
async fn live_catalog_is_shared_and_invalid_updates_keep_offline_models() {
    let mut go = Go::new().unwrap();
    Arc::get_mut(&mut go).unwrap().http = Client::builder().no_proxy().build().unwrap();
    let seed = go.models.read().unwrap().catalog();
    assert!(seed.models.iter().any(|entry| entry.id == "deepseek-v4.1-flash"));
    for invalid in [
        json!({}),
        json!({"data":[{"id":"next-model","type":"chat",
            "capabilities":{"reasoning":{"effort":["low","low"]}}}]}),
    ] {
        *go.refresh_after.lock().await = None;
        let (bad, bad_server) =
            test_http::server(vec![(200, "application/json", invalid.to_string())]).await;
        assert_eq!(go.catalog_from(&bad).await.unwrap(), seed);
        assert_eq!(bad_server.await.unwrap().len(), 1);
    }
    *go.refresh_after.lock().await = None;
    let fixture = json!({"data":[
        {"id":"ignored","type":"embedding"},
        {"id":"next-model","type":"chat","limit":{"input":900,"output":100}},
        {"id":"legacy","type":"chat","name":"Former model","description":"Old",
         "status":"deprecated","capabilities":{"reasoning":{"effort":["low","high"]}}}
    ]});
    let (url, server) =
        test_http::server(vec![(200, "application/json", fixture.to_string())]).await;
    let (a, b) = tokio::join!(go.catalog_from(&url), go.catalog_from(&url));
    let a = a.unwrap();
    assert_eq!(a, b.unwrap());
    assert_eq!(
        serde_json::to_value(&a).unwrap(),
        json!({"models":[
            {"id":"next-model","display_name":"next-model","description":null,"hidden":false,
             "reasoning_efforts":[],"default_reasoning_effort":null},
            {"id":"legacy","display_name":"Former model","description":"Old","hidden":true,
             "reasoning_efforts":["low","high"],"default_reasoning_effort":null}
        ]})
    );
    assert_eq!(
        go.capabilities("next-model").unwrap().max_input_tokens,
        Some(900)
    );
    assert_eq!(server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn denied_optional_budgets_preserve_usage_and_cache_is_key_scoped() {
    let mut go = Go::new().unwrap();
    Arc::get_mut(&mut go).unwrap().http = Client::builder().no_proxy().build().unwrap();
    let m = json!({"status":"ok","percent":5,"resetsAt":"2026-11-25T12:00:00Z"});
    let usage = json!({"usage":{"rolling":m,"weekly":m,"monthly":m}}).to_string();
    let (u, us) = test_http::server(vec![
        (200, "application/json", usage.clone()),
        (200, "application/json", usage),
    ])
    .await;
    let (b, bs) = test_http::server(vec![
        (403, "application/json", "{}".into()),
        (200, "application/json", "[]".into()),
    ])
    .await;
    for key in ["a", "a", "b"] {
        let snapshot = go
            .quota_from(&json!({"api_key":key}), &u, &b)
            .await
            .unwrap();
        assert_eq!(snapshot.notices.is_empty(), key == "b");
        assert_eq!(snapshot.buckets[0].windows.len(), 3);
    }
    for requests in [us.await.unwrap(), bs.await.unwrap()] {
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].headers["authorization"], "Bearer a");
        assert_eq!(requests[1].headers["authorization"], "Bearer b");
    }
}
