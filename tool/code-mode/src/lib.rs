//! Programmatic tool calling через invocation-bound host callbacks.
mod bridge;
mod config;
mod engine;

pub use config::config_schema;

use proteus_contracts::{
    domain::{ToolCall, ToolResult, ToolSafety, ToolSpec},
    process_module::{
        ModuleRegistry, ProcessModuleError, ProcessModuleResult, ToolModule, ToolModuleHost,
    },
    tool_support::parse_invocation_context,
};
use serde::Deserialize;
use serde_json::json;

pub struct CodeModeTool;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    code: String,
    timeout_ms: Option<u64>,
}

impl ToolModule for CodeModeTool {
    fn spec_json(&self) -> String {
        serde_json::to_string(&ToolSpec::new("code_mode", concat!(
            "Run JavaScript for multi-call workflows with top-level await, loops and Promise.all. ",
            "tools.call(name, args) takes exactly two arguments and returns a raw ToolResult: .output is a string; .content and .metadata are separate fields. ",
            "Check .ok on EVERY result; stop on failure unless an explicit recovery handles and verifies it. Parse .output as JSON only when the tool contract says it contains JSON. ",
            "Before automating an unfamiliar response, make one small read-only probe and print a bounded sample; use the observed shape in the next script. ",
            "Assert required fields, types, item identities and pagination cursors before looping or causing effects. Never guess alternate field names or turn missing fields/parse errors into empty arrays or a successful end of pagination. ",
            "When changing a selected set, enumerate and validate that selection before writes; stop on incomplete coverage, duplicates, cursor loops or unexpected data. ",
            "After writes, read back and verify task-required postconditions before any final submit/send/publish. Make one-time finalization the last effect, after coverage and verification pass; do not submit a placeholder or incomplete success report. ",
            "Exceptions do not roll back earlier effects: inspect current state before retrying mutations. Script success alone does not prove task completion. ",
            "Only text(value) and console.log(...values) enter model context; print a concise coverage/verification summary. Child results remain in the execution journal. ",
            "Use existing tool descriptions; workflow-owned discovery tools cannot be called from this script. ",
            "Pass registered argument objects, e.g. tools.call('apply_patch', {patch:patchText}). ",
            "No filesystem, network, process, imports, timers, persistent state or background jobs are available in JavaScript. ",
            "Every delegated tool goes through normal policy/approval. Await every delegated operation. ",
            "Default limits: 30s, 32 MiB JS heap, 256 calls, 4 concurrent callbacks, 32 KiB printed output."
        ), json!({"type":"object","properties":{
            "code":{"type":"string","description":"JavaScript program. Probe unfamiliar results first; validate shape and complete selection before effects, and verify postconditions before finalization.","maxLength":65536},
            "timeout_ms":{"type":"integer","minimum":1,"maximum":60000,"description":"Optional script budget in ms. Omit to use the configured default (normally 30000). A supplied value must not exceed that configured ceiling; 60000 is accepted only when explicitly configured."}
        },"required":["code"],"additionalProperties":false}), ToolSafety::ReadOnly)
            .with_timeout(65_000)
            .with_metadata(json!({"category":"orchestration","hot":true,"tags":["code","script","batch","javascript"],"aliases":["codemode","programmatic tool calling"]})))
            .expect("static code_mode ToolSpec")
    }

    fn invoke_json(
        &self,
        call_json: String,
        context_json: String,
        host: &mut dyn ToolModuleHost,
    ) -> ProcessModuleResult<String> {
        let call: ToolCall = serde_json::from_str(&call_json)
            .map_err(|error| ProcessModuleError::new(format!("invalid ToolCall: {error}")))?;
        let context = parse_invocation_context(&context_json).map_err(ProcessModuleError::new)?;
        let result = match (
            serde_json::from_value::<Arguments>(call.args),
            config::Limits::parse(context.config),
        ) {
            (Err(error), _) => {
                ToolResult::error(call.id, format!("invalid code_mode arguments: {error}"))
            }
            (_, Err(error)) => ToolResult::error(call.id, error),
            (Ok(args), Ok(limits)) => match limits.validate_script(&args.code, args.timeout_ms) {
                Ok(timeout) => engine::execute(call.id, &args.code, limits, timeout, host),
                Err(error) => ToolResult::error(call.id, error),
            },
        };
        serde_json::to_string(&result).map_err(|error| ProcessModuleError::new(error.to_string()))
    }
}

pub fn register_modules(registry: &mut dyn ModuleRegistry) -> ProcessModuleResult<()> {
    config::Limits::parse(registry.module_config().clone()).map_err(ProcessModuleError::new)?;
    registry.register_tool(Box::new(CodeModeTool))
}

#[cfg(test)]
mod tests;
