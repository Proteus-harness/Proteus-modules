//! `find_files` tool: поиск файлов по glob через ripgrep `--files`.

use std::{path::Path, process::Command, time::Duration};

use proteus_contracts::process_module::{ProcessModuleError, ToolModule, ToolModuleHostMut};
use serde_json::{Value, json};

use crate::util::{
    err_result, module_error, ok_result, optional_positive_usize, optional_string_array,
    parse_call, parse_invocation_context, required_string, run_lines_limited, workspace_path,
};

const FIND_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_MAX_RESULTS: usize = 100;
const MAX_MAX_RESULTS: usize = 1_000;

pub struct FindFilesTool;

impl ToolModule for FindFilesTool {
    fn spec_json(&self) -> String {
        let spec = json!({
            "name": "find_files",
            "description": "Find workspace files by glob pattern using ripgrep --files. Prefer this over shell find/ls for read-only file discovery.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern, for example \"**/*.rs\" or \"crates/**/Cargo.toml\"."
                    },
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative search root. Defaults to workspace root."
                    },
                    "exclude": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional glob patterns to exclude."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum paths to return. Defaults to 100 and is capped at 1000."
                    }
                },
                "required": ["pattern"]
            },
            "surface": { "kind": "function", "strict": false, "output_schema": null },
            "safety": "ReadOnly",
            "supports_parallel_tool_calls": true,
            "timeout_ms": FIND_TIMEOUT.as_millis() as u64,
            "metadata": {
                "hot": true,
                "category": "filesystem",
                "tags": ["filesystem", "find", "glob", "files"],
                "aliases": ["find files", "list matching files", "file glob"]
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

        let pattern = match required_string(&call.args, "pattern", &call.name) {
            Ok(p) => p.to_owned(),
            Err(e) => return err_result(&call.id, &call.name, e),
        };
        let cwd_path = context.cwd.as_path();
        let base = match std::fs::canonicalize(cwd_path) {
            Ok(path) => path,
            Err(e) => {
                return err_result(
                    &call.id,
                    &call.name,
                    format!("failed to canonicalize cwd {}: {e}", cwd_path.display()),
                );
            }
        };
        let root_arg = call.args.get("path").and_then(Value::as_str).unwrap_or(".");
        let root = match workspace_path(&base, Path::new(root_arg)) {
            Ok(path) => path,
            Err(e) => return err_result(&call.id, &call.name, e),
        };
        let root_relative = match root.strip_prefix(&base) {
            Ok(path) if path.as_os_str().is_empty() => ".".to_owned(),
            Ok(path) => path.display().to_string(),
            Err(_) => return err_result(&call.id, &call.name, "path escapes workspace".to_owned()),
        };
        let max_results = match optional_positive_usize(&call.args, "max_results", &call.name) {
            Ok(value) => value.unwrap_or(DEFAULT_MAX_RESULTS).min(MAX_MAX_RESULTS),
            Err(e) => return err_result(&call.id, &call.name, e),
        };
        let exclude = match optional_string_array(&call.args, "exclude", &call.name) {
            Ok(value) => value,
            Err(e) => return err_result(&call.id, &call.name, e),
        };

        let mut command = Command::new("rg");
        command
            .current_dir(&base)
            .arg("--files")
            .arg("--glob")
            .arg(&pattern);
        for pattern in &exclude {
            command.arg("--glob").arg(format!("!{pattern}"));
        }
        command
            .arg("--")
            .arg(&root_relative)
            .stdin(std::process::Stdio::null());

        let lines = match run_lines_limited(command, max_results, FIND_TIMEOUT) {
            Ok(lines) => lines,
            Err(e) => {
                return err_result(
                    &call.id,
                    &call.name,
                    format!("failed to run ripgrep files search: {e} (is 'rg' installed?)"),
                );
            }
        };
        let match_count = lines.len();
        let output = render_rg_file_paths(lines);
        ok_result(
            &call.id,
            &call.name,
            output,
            json!({
                "pattern": pattern,
                "path": root.display().to_string(),
                "exclude": exclude,
                "match_count": match_count,
                "max_results": max_results,
                "truncated": match_count >= max_results,
            }),
        )
    }
}

fn render_rg_file_paths(paths: Vec<String>) -> String {
    if paths.is_empty() {
        return "(no matches)".to_owned();
    }

    let path_bytes = paths
        .iter()
        .map(|path| normalize_rg_file_path(path).len())
        .sum::<usize>();
    let mut output = String::with_capacity(path_bytes + paths.len() - 1);
    for (index, path) in paths.into_iter().enumerate() {
        if index != 0 {
            output.push('\n');
        }
        output.push_str(normalize_rg_file_path(&path));
    }
    output
}

fn normalize_rg_file_path(path: &str) -> &str {
    path.strip_prefix("./").unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendering_preserves_order_and_strips_exactly_one_leading_prefix() {
        let paths = [".//abs", "./путь.txt", "a/./b", "././a", "./", "", "tail"];
        assert_eq!(
            render_rg_file_paths(paths.map(str::to_owned).into()),
            "/abs\nпуть.txt\na/./b\n./a\n\n\ntail"
        );
        assert_eq!(render_rg_file_paths(vec!["./".to_owned()]), "");
        assert_eq!(render_rg_file_paths(Vec::new()), "(no matches)");
    }
}
