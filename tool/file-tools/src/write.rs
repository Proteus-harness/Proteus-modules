//! `write_file` tool: создаёт или перезаписывает файл внутри workspace.

use std::{io::Write, path::Path};

use proteus_contracts::process_module::{ProcessModuleError, ToolModule, ToolModuleHostMut};
use serde_json::json;

use crate::util::{
    err_result, module_error, ok_result, parse_call, parse_invocation_context, required_string,
    workspace_file_for_atomic_write,
};

pub struct WriteFileTool;

impl ToolModule for WriteFileTool {
    fn spec_json(&self) -> String {
        let spec = json!({
            "name": "write_file",
            "description": "Write UTF-8 content to a file inside the current workspace. Creates missing parent directories and overwrites existing files.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            },
            "surface": { "kind": "function", "strict": false, "output_schema": null },
            "safety": "WritesFiles",
            "supports_parallel_tool_calls": false,
            "timeout_ms": 60000,
            "metadata": {
                "category": "filesystem",
                "tags": ["filesystem", "write", "file"],
                "aliases": ["write file", "create file", "overwrite file"],
                "approval": {
                    "cache_scopes": ["workspace_write"]
                }
            }
        });
        String::from(spec.to_string())
    }

    fn invoke_json(
        &self,
        call_json: String,
        context_json: String,
        _host: &mut ToolModuleHostMut<'_>,
    ) -> Result<String, ProcessModuleError> {
        let call = match parse_call(call_json.as_str()) {
            Ok(c) => c,
            Err(e) => return module_error(e),
        };
        let context = match parse_invocation_context(context_json.as_str()) {
            Ok(context) => context,
            Err(error) => return module_error(error),
        };

        let path_str = match required_string(&call.args, "path", &call.name) {
            Ok(p) => p,
            Err(e) => return err_result(&call.id, &call.name, e),
        };
        let content = match required_string(&call.args, "content", &call.name) {
            Ok(c) => c,
            Err(e) => return err_result(&call.id, &call.name, e),
        };
        let cwd_path = context.cwd.as_path();
        let mut target = match workspace_file_for_atomic_write(cwd_path, Path::new(path_str)) {
            Ok(p) => p,
            Err(e) => return err_result(&call.id, &call.name, e),
        };

        let written_path = target.path().to_path_buf();
        let bytes_written = content.len();
        if let Err(e) = target.write_all(content.as_bytes()) {
            return err_result(
                &call.id,
                &call.name,
                format!("failed to write {}: {e}", written_path.display()),
            );
        }

        if let Err(error) = target.commit() {
            return err_result(&call.id, &call.name, error);
        }

        let metadata = json!({
            "path": written_path.display().to_string(),
            "bytes_written": bytes_written,
        });
        ok_result(
            &call.id,
            &call.name,
            format!("Wrote {bytes_written} bytes to {}", written_path.display()),
            metadata,
        )
    }
}
