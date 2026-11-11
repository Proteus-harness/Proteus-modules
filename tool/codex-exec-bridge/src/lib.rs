//! Native Codex V8, synchronous invocation-bound subset. Not exec/wait parity.
use proteus_contracts::{
    domain::{ToolCall, ToolResult, ToolSafety, ToolSpec},
    process_module::{ProcessModuleError, ProcessModuleResult, ToolModule, ToolModuleHost},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    io::{Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const MAX_FRAME: usize = 8 * 1024 * 1024;

/// Trusted setup data, never supplied in model tool arguments. Tool invocation
/// still goes through the bound Core registry, policy, lineage and cancellation.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnabledTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

pub struct NativeSyncTool {
    binary: PathBuf,
    tools: Vec<EnabledTool>,
}

impl NativeSyncTool {
    pub fn new(binary: PathBuf, tools: Vec<EnabledTool>) -> Result<Self, String> {
        let mut names = HashSet::new();
        for tool in &tools {
            if tool.name.is_empty() || !names.insert(tool.name.clone()) {
                return Err("empty or duplicate trusted tool name".into());
            }
            if tool.name == "codex_exec_sync" {
                return Err("recursive native bridge tool is forbidden".into());
            }
        }
        Ok(Self { binary, tools })
    }

    /// Keeps the borrowed host inside this stack frame until native V8 ends.
    /// Yielded observations are consumed here; they never become live cell IDs
    /// accepted by another invocation. Every return kills and joins the child.
    pub fn execute(
        &self,
        parent_id: &str,
        source: &str,
        timeout: Duration,
        host: &dyn ToolModuleHost,
    ) -> Result<Value, String> {
        let deadline = Instant::now() + timeout;
        let mut peer = Peer::spawn(&self.binary)?;
        peer.send(json!({"type":"connection/hello","supportedVersions":[1],
            "requiredCapabilities":[],"optionalCapabilities":[]}))?;
        let hello = peer.next(deadline, host)?;
        if hello["type"] != "connection/ready" || hello["selectedVersion"] != 1 {
            return Err("native host did not negotiate supported protocol V1".into());
        }
        peer.request(1, json!({"method":"session/open","sessionId":"bound"}))?;
        let opened = peer.next(deadline, host)?;
        if opened["result"]["value"]["type"] != "session/ready" {
            return Err(format!("native session open failed: {opened}"));
        }
        let definitions: Vec<_> = self
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name":tool.name,"tool_name":{"name":tool.name,"namespace":null},
                    "description":tool.description,"kind":"function",
                    "input_schema":tool.input_schema,"output_schema":null,
                })
            })
            .collect();
        peer.request(
            2,
            json!({"method":"session/execute","sessionId":"bound","request":{
                "tool_call_id":parent_id,"enabled_tools":definitions,"source":source,
                "yield_time_ms":10,"max_output_tokens":1000,
            }}),
        )?;
        let mut request_id = 2;
        let mut observed_items = Vec::new();
        let mut observed_bytes = 0usize;
        loop {
            let message = peer.next(deadline, host)?;
            match message["type"].as_str() {
                Some("delegate/request") => {
                    // Admission is checked again immediately before any effect.
                    check_live(deadline, host)?;
                    let request = &message["request"];
                    if request["type"] != "tool/invoke" {
                        return Err(
                            "notifications need a persistent native session contract".into()
                        );
                    }
                    let invocation = &request["invocation"];
                    let name = invocation["tool_name"]["name"]
                        .as_str()
                        .ok_or("native callback is missing a tool name")?;
                    if message["sessionId"] != "bound"
                        || !invocation["tool_name"]["namespace"].is_null()
                        || invocation["tool_kind"] != "function"
                        || !self.tools.iter().any(|tool| tool.name == name)
                    {
                        return Err(
                            "native callback is outside the invocation tool allowlist".into()
                        );
                    }
                    let runtime_id = invocation["runtime_tool_call_id"]
                        .as_str()
                        .ok_or("native callback is missing its call ID")?;
                    let call = ToolCall::new(
                        format!("{parent_id}/{runtime_id}"),
                        name,
                        invocation["input"].clone(),
                    );
                    let call_json = serde_json::to_string(&call).map_err(|e| e.to_string())?;
                    let response = host
                        .execute_tool_json(call_json)
                        .map_err(|e| e.to_string())?;
                    let result: ToolResult =
                        serde_json::from_str(&response).map_err(|e| e.to_string())?;
                    if result.call_id != call.id {
                        return Err("bound callback returned wrong call ID".into());
                    }
                    check_live(deadline, host)?;
                    peer.send(
                        json!({"type":"delegate/response","id":message["id"],"result":{
                            "status":"ok","value":{"type":"tool/result","result":result},
                        }}),
                    )?;
                }
                Some("execute/initialResponse") | Some("operation/response") => {
                    if message["result"]["status"] == "error" {
                        return Err(message["result"]["message"].to_string());
                    }
                    let value = &message["result"]["value"];
                    let runtime = if value["type"] == "wait/completed" {
                        value["outcome"]
                            .get("LiveCell")
                            .or_else(|| value["outcome"].get("MissingCell"))
                    } else if message["type"] == "execute/initialResponse" {
                        Some(value)
                    } else {
                        None
                    };
                    if let Some(runtime) = runtime {
                        if runtime.get("Terminated").is_some() {
                            return Err("native cell was terminated".into());
                        }
                        let observation = runtime
                            .get("Result")
                            .or_else(|| runtime.get("Yielded"))
                            .ok_or("native response is missing its observation")?;
                        for item in observation["content_items"]
                            .as_array()
                            .ok_or("native content is not an array")?
                        {
                            observed_bytes = observed_bytes.saturating_add(item.to_string().len());
                            if observed_bytes > 128 * 1024 {
                                return Err("native synchronous output budget exceeded".into());
                            }
                            observed_items.push(item.clone());
                        }
                        if runtime.get("Result").is_some() {
                            let mut result = runtime.clone();
                            result["Result"]["content_items"] = Value::Array(observed_items);
                            return Ok(result);
                        }
                        if let Some(yielded) = runtime.get("Yielded") {
                            request_id += 1;
                            peer.request(
                                request_id,
                                json!({"method":"session/wait","sessionId":"bound",
                                "request":{"cell_id":yielded["cell_id"],"yield_time_ms":10}}),
                            )?;
                        }
                    }
                }
                Some("cell/closed") | Some("delegate/cancel") => {}
                _ => return Err(format!("unexpected native host message: {message}")),
            }
        }
    }
}

fn check_live(deadline: Instant, host: &dyn ToolModuleHost) -> Result<(), String> {
    if Instant::now() >= deadline {
        return Err("native synchronous invocation deadline exceeded".into());
    }
    if host.is_cancelled().map_err(|e| e.to_string())? {
        return Err("native invocation cancelled".into());
    }
    Ok(())
}

struct Peer {
    child: Child,
    input: ChildStdin,
    frames: Receiver<Result<Value, String>>,
    reader: Option<JoinHandle<()>>,
}
impl Peer {
    fn spawn(binary: &PathBuf) -> Result<Self, String> {
        let mut child = Command::new(binary)
            .args(["--listen", "stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap();
        let (sender, frames) = mpsc::sync_channel(16);
        let reader = thread::spawn(move || {
            loop {
                let frame = (|| {
                    let mut size = [0u8; 4];
                    output.read_exact(&mut size).map_err(|e| e.to_string())?;
                    let size = u32::from_le_bytes(size) as usize;
                    if size > MAX_FRAME {
                        return Err("native IPC frame exceeds prototype budget".into());
                    }
                    let mut bytes = vec![0; size];
                    output.read_exact(&mut bytes).map_err(|e| e.to_string())?;
                    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
                })();
                let failed = frame.is_err();
                if sender.send(frame).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            input,
            frames,
            reader: Some(reader),
        })
    }
    fn send(&mut self, value: Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_FRAME {
            return Err("native IPC request too large".into());
        }
        self.input
            .write_all(&(bytes.len() as u32).to_le_bytes())
            .and_then(|_| self.input.write_all(&bytes))
            .and_then(|_| self.input.flush())
            .map_err(|e| e.to_string())
    }
    fn request(&mut self, id: i64, request: Value) -> Result<(), String> {
        self.send(json!({"type":"operation/request","id":id,"request":request}))
    }
    fn next(&self, deadline: Instant, host: &dyn ToolModuleHost) -> Result<Value, String> {
        loop {
            check_live(deadline, host)?;
            match self.frames.recv_timeout(Duration::from_millis(5)) {
                Ok(frame) => return frame,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("native host disconnected".into());
                }
            }
        }
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Drain queued frames before joining a bounded sender. No callback is
        // admitted during teardown; they are inert protocol data at this point.
        while self.frames.try_recv().is_ok() {}
        // Drop the receiver before joining so a producer cannot remain blocked.
        let (_, replacement) = mpsc::channel();
        drop(std::mem::replace(&mut self.frames, replacement));
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    code: String,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
}
fn default_timeout() -> u64 {
    30_000
}

impl ToolModule for NativeSyncTool {
    fn spec_json(&self) -> String {
        serde_json::to_string(&ToolSpec::new("codex_exec_sync",
            "Experimental synchronous native Codex V8. Use tools.NAME(args) and text(value). Await every call. No persistent cells or public wait; this is not native exec/wait parity.",
            json!({"type":"object","properties":{"code":{"type":"string","maxLength":65536},
                   "timeout_ms":{"type":"integer","minimum":1,"maximum":60000}},
                   "required":["code"],"additionalProperties":false}),ToolSafety::ReadOnly).with_timeout(65_000)).unwrap()
    }
    fn invoke_json(
        &self,
        call_json: String,
        _context_json: String,
        host: &mut dyn ToolModuleHost,
    ) -> ProcessModuleResult<String> {
        let call: ToolCall =
            serde_json::from_str(&call_json).map_err(|e| ProcessModuleError::new(e.to_string()))?;
        let result = match serde_json::from_value::<Arguments>(call.args) {
            Ok(args) if args.code.len() <= 65536 && (1..=60000).contains(&args.timeout_ms) => {
                match self.execute(
                    &call.id.to_string(),
                    &args.code,
                    Duration::from_millis(args.timeout_ms),
                    host,
                ) {
                    Ok(value) => {
                        if let Some(error) = value["Result"]["error_text"].as_str() {
                            ToolResult::error(call.id, error)
                        } else {
                            ToolResult::ok(call.id, value.to_string())
                        }
                    }
                    Err(error) => ToolResult::error(call.id, error),
                }
            }
            _ => ToolResult::error(call.id, "invalid native synchronous arguments"),
        };
        serde_json::to_string(&result).map_err(|e| ProcessModuleError::new(e.to_string()))
    }
}

#[cfg(test)]
mod tests;

// Reuse the current Core validators read-only in the isolated proof crate.
// These facades preserve their crate-relative imports; no copy of validator
// logic and no modification of the shared Core source is involved.
#[cfg(test)]
#[allow(unused_imports)]
mod contracts {
    pub use proteus_contracts::contracts::*;
}
#[cfg(test)]
#[allow(unused_imports)]
mod domain {
    pub use proteus_contracts::domain::*;
}
#[cfg(test)]
#[allow(unused_imports)]
mod model_standard {
    pub use proteus_contracts::model_standard::*;
}
#[cfg(test)]
#[allow(dead_code)]
mod core_history_capture {
    include!(concat!(env!("OUT_DIR"), "/core_history_capture.rs"));
}
#[cfg(test)]
#[allow(dead_code)]
mod core_replay_normalize {
    include!(concat!(env!("OUT_DIR"), "/core_replay_normalize.rs"));
}
#[cfg(test)]
mod workflow_boundary_tests;
