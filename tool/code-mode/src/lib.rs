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
            "Run JavaScript to orchestrate registered Proteus tools. Top-level await, loops and Promise.all are supported. ",
            "Prefer this tool for multi-call workflows, filtering many results, and deterministic loops; print only a useful summary. ",
            "Use await tools.call(name, args) to receive the full ToolResult; inspect .ok and .error before using .output. ",
            "Only text(value) and console.log(...values) enter model context; child results remain in the execution journal. ",
            "Example: const r = await tools.call('read_file', {path:'README.md'}); if (!r.ok) throw Error(r.error); text(r.output). ",
            "Use existing tool descriptions; workflow-owned discovery tools cannot be called from this script. ",
            "Pass the registered argument object, e.g. tools.call('apply_patch', {patch:patchText}); tools.call accepts exactly two arguments. ",
            "No filesystem, network, process, imports, timers, persistent state or background jobs are available in JavaScript. ",
            "Every delegated tool goes through normal policy/approval. Await every delegated operation. ",
            "Default limits: 30s, 32 MiB JS heap, 256 calls, 4 concurrent callbacks, 32 KiB printed output."
        ), json!({"type":"object","properties":{
            "code":{"type":"string","description":"JavaScript program; top-level await is supported.","maxLength":65536},
            "timeout_ms":{"type":"integer","minimum":1,"maximum":60000,"description":"Optional script budget, no greater than configured timeout_ms."}
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
