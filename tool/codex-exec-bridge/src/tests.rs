use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

pub(super) fn binary() -> PathBuf {
    std::env::var_os("CODEX_CODE_MODE_HOST")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/usr/lib/chatgpt/resources/codex-code-mode-host".into())
}
#[derive(Default)]
pub(super) struct Host {
    cancelled: AtomicBool,
    calls: Mutex<Vec<ToolCall>>,
    delay_ms: u64,
}
impl ToolModuleHost for Host {
    fn is_cancelled(&self) -> ProcessModuleResult<bool> {
        Ok(self.cancelled.load(Ordering::SeqCst))
    }
    fn execute_tool_json(&self, call: String) -> ProcessModuleResult<String> {
        let call: ToolCall = serde_json::from_str(&call).unwrap();
        self.calls.lock().unwrap().push(call.clone());
        thread::sleep(Duration::from_millis(self.delay_ms));
        Ok(serde_json::to_string(&ToolResult::ok(call.id, call.args.to_string())).unwrap())
    }
}
pub(super) fn tool() -> NativeSyncTool {
    NativeSyncTool::new(
        binary(),
        vec![EnabledTool {
            name: "echo".into(),
            description: "Test bound registry callback".into(),
            input_schema: json!({"type":"object","additionalProperties":true}),
        }],
    )
    .unwrap()
}
pub(super) fn text(value: &Value) -> String {
    value["Result"]["content_items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn actual_native_v8_routes_callback_to_borrowed_v6_host() {
    let mut host = Host::default();
    let call = ToolCall::new(
        "parent-authority",
        "codex_exec_sync",
        json!({
            "code":"const result = await tools.echo({value:42}); text(result);",
        }),
    );
    let result = tool()
        .invoke_json(
            serde_json::to_string(&call).unwrap(),
            "{}".into(),
            &mut host,
        )
        .unwrap();
    let result: ToolResult = serde_json::from_str(&result).unwrap();
    assert!(result.ok, "{result:?}");
    let value: Value = serde_json::from_str(&result.output).unwrap();
    assert!(text(&value).contains("42"));
    let calls = host.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "echo");
    assert!(calls[0].id.to_string().starts_with("parent-authority/"));
}

#[test]
fn actual_native_unknown_tool_never_reaches_registry() {
    let host = Host::default();
    let result = tool()
        .execute(
            "parent",
            "text(await tools.untrusted({}));",
            Duration::from_secs(2),
            &host,
        )
        .unwrap();
    assert!(result["Result"]["error_text"].is_string());
    assert!(host.calls.lock().unwrap().is_empty());
}

#[test]
fn actual_native_internal_yield_is_drained_within_one_invocation() {
    let host = Host::default();
    let result=tool().execute("parent", "text('before-yield'); await new Promise(r=>setTimeout(r,35)); text(await tools.echo({afterYield:true}));", Duration::from_secs(2), &host).unwrap();
    assert!(text(&result).contains("before-yield"));
    assert!(text(&result).contains("afterYield"));
    assert_eq!(host.calls.lock().unwrap().len(), 1);
}

#[test]
fn actual_native_deadline_kills_cell_before_late_callback() {
    let host = Host::default();
    let result = tool().execute(
        "parent",
        "await new Promise(r=>setTimeout(r,120)); await tools.echo({late:true});",
        Duration::from_millis(25),
        &host,
    );
    assert!(result.unwrap_err().contains("deadline"));
    thread::sleep(Duration::from_millis(150));
    assert!(host.calls.lock().unwrap().is_empty());
}

#[test]
fn actual_native_parent_cancellation_stops_pending_cell() {
    let host = Host::default();
    thread::scope(|scope| {
        scope.spawn(|| {
            thread::sleep(Duration::from_millis(25));
            host.cancelled.store(true, Ordering::SeqCst);
        });
        let result = tool().execute(
            "parent",
            "await new Promise(r=>setTimeout(r,120)); await tools.echo({late:true});",
            Duration::from_secs(2),
            &host,
        );
        assert!(result.unwrap_err().contains("cancelled"));
    });
    assert!(host.calls.lock().unwrap().is_empty());
}

#[test]
fn actual_native_started_callback_finishes_before_invocation_returns() {
    let host = Host {
        delay_ms: 45,
        ..Host::default()
    };
    let started = Instant::now();
    let result = tool().execute(
        "parent",
        "await tools.echo({started:true});",
        Duration::from_millis(25),
        &host,
    );
    assert!(result.unwrap_err().contains("deadline"));
    assert!(started.elapsed() >= Duration::from_millis(45));
    assert_eq!(host.calls.lock().unwrap().len(), 1);
}

#[test]
fn black_box_yield_returns_before_new_callback_request() {
    // Concrete lifetime mismatch: execute/initialResponse can finish, while a
    // *new* native tool callback is emitted later. A borrowed invocation host
    // must not be retained to dispatch that callback after invoke_json returns.
    let host = Host::default();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut peer = Peer::spawn(&binary()).unwrap();
    peer.send(json!({"type":"connection/hello","supportedVersions":[1],"requiredCapabilities":[],"optionalCapabilities":[]})).unwrap();
    assert_eq!(
        peer.next(deadline, &host).unwrap()["type"],
        "connection/ready"
    );
    peer.request(
        1,
        json!({"method":"session/open","sessionId":"yield-proof"}),
    )
    .unwrap();
    peer.next(deadline, &host).unwrap();
    peer.request(
        2,
        json!({"method":"session/execute","sessionId":"yield-proof","request":{
            "tool_call_id":"original-parent","enabled_tools":[{
                "name":"echo","tool_name":{"name":"echo","namespace":null},"description":"echo",
                "kind":"function","input_schema":{"type":"object"},"output_schema":null}],
            "source":"await new Promise(r=>setTimeout(r,45)); text(await tools.echo({late:true}));",
            "yield_time_ms":1,"max_output_tokens":100,
        }}),
    )
    .unwrap();
    let yielded = loop {
        let message = peer.next(deadline, &host).unwrap();
        if message["type"] == "execute/initialResponse" {
            break message;
        }
    };
    assert!(yielded["result"]["value"]["Yielded"].is_object());
    let late = peer.next(deadline, &host).unwrap();
    assert_eq!(late["type"], "delegate/request");
    assert_eq!(late["request"]["type"], "tool/invoke");
    assert_eq!(late["request"]["invocation"]["input"], json!({"late":true}));
    println!(
        "YIELD_LIFETIME_PROOF {}",
        json!({"first":yielded,"later":late})
    );
    drop(peer); // Fail closed: no retained host, no late effect.
    assert!(host.calls.lock().unwrap().is_empty());
}
