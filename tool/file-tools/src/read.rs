//! `read_file` tool: чтение файла целиком или по диапазону строк.

use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
};

use proteus_contracts::process_module::{ProcessModuleError, ToolModule, ToolModuleHostMut};
use serde_json::json;

use crate::util::{
    err_result, module_error, ok_result, optional_positive_usize, parse_call,
    parse_invocation_context, required_string, workspace_path,
};

pub(crate) const MAX_READ_FILE_BYTES: u64 = 2 * 1024 * 1024;
// Range selection may skip a prefix, but it cannot scan an unbounded log.
const MAX_RANGE_SCAN_BYTES: u64 = 16 * 1024 * 1024;

pub struct ReadFileTool;

impl ToolModule for ReadFileTool {
    fn spec_json(&self) -> String {
        let spec = json!({
            "name": "read_file",
            "description": "Read a UTF-8 file inside the current workspace, optionally by line range. Output and each line are limited to 2 MiB; ranged reads scan at most 16 MiB. Truncated ranges report total_lines as unknown.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "start_line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "1-based first line to read. Defaults to 1."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum number of lines to return."
                    },
                    "line_numbers": {
                        "type": "boolean",
                        "description": "Prefix each returned line with its 1-based line number."
                    }
                },
                "required": ["path"]
            },
            "surface": { "kind": "function", "strict": false, "output_schema": null },
            "safety": "ReadOnly",
            "supports_parallel_tool_calls": true,
            "timeout_ms": 60000,
            "metadata": {
                "hot": true,
                "category": "filesystem",
                "tags": ["filesystem", "read", "file", "code"],
                "aliases": ["cat", "open file", "inspect file", "view source"]
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
        let cwd_path = context.cwd.as_path();
        let path = match workspace_path(cwd_path, Path::new(path_str)) {
            Ok(p) => p,
            Err(e) => return err_result(&call.id, &call.name, e),
        };

        let metadata_res = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                return err_result(
                    &call.id,
                    &call.name,
                    format!("failed to inspect {}: {e}", path.display()),
                );
            }
        };
        if metadata_res.is_dir() {
            return err_result(
                &call.id,
                &call.name,
                format!(
                    "path is a directory; use list_dir to list entries: {}",
                    path.display()
                ),
            );
        }

        let options = match ReadOptions::from_args(&call.args, &call.name) {
            Ok(o) => o,
            Err(e) => return err_result(&call.id, &call.name, e),
        };

        let (output, metadata) = if options.is_default() {
            if metadata_res.len() > MAX_READ_FILE_BYTES {
                return err_result(
                    &call.id,
                    &call.name,
                    format!(
                        "file is too large to read fully ({} bytes > {} bytes); use start_line and limit",
                        metadata_res.len(),
                        MAX_READ_FILE_BYTES
                    ),
                );
            }
            let content = match read_full(&path) {
                Ok(c) => c,
                Err(e) => {
                    return err_result(
                        &call.id,
                        &call.name,
                        format!("failed to read {}: {e}", path.display()),
                    );
                }
            };
            let total_lines = content.lines().count();
            (
                content,
                json!({
                    "path": path.display().to_string(),
                    "total_lines": total_lines,
                    "returned_lines": total_lines,
                    "truncated": false,
                }),
            )
        } else {
            match render_read_output_streaming(&path, options) {
                Ok(rendered) => rendered,
                Err(error) => return err_result(&call.id, &call.name, error),
            }
        };
        ok_result(&call.id, &call.name, output, metadata)
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ReadOptions {
    start_line: Option<usize>,
    limit: Option<usize>,
    line_numbers: bool,
}

impl ReadOptions {
    fn from_args(args: &serde_json::Value, tool_name: &str) -> Result<Self, String> {
        Ok(Self {
            start_line: optional_positive_usize(args, "start_line", tool_name)?,
            limit: optional_positive_usize(args, "limit", tool_name)?,
            line_numbers: args
                .get("line_numbers")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        })
    }

    fn is_default(self) -> bool {
        self.start_line.is_none() && self.limit.is_none() && !self.line_numbers
    }
}

fn read_full(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_READ_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_READ_FILE_BYTES {
        return Err("file grew beyond the 2 MiB read budget; use start_line and limit".into());
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

fn render_read_output_streaming(
    path: &Path,
    options: ReadOptions,
) -> Result<(String, serde_json::Value), String> {
    let file = File::open(path).map_err(|e| format!("failed to open {}: {e}", path.display()))?;
    render_range(BufReader::new(file), path, options)
}

fn render_range(
    mut reader: impl BufRead,
    path: &Path,
    options: ReadOptions,
) -> Result<(String, serde_json::Value), String> {
    let start_line = options.start_line.unwrap_or(1);
    let limit = options.limit.unwrap_or(usize::MAX);
    let mut scanned_lines = 0usize;
    let mut scanned_bytes = 0u64;
    let mut returned = 0usize;
    let mut output = String::new();
    let mut line = Vec::new();
    let mut eof = false;
    let mut reason = None;
    loop {
        if returned >= limit {
            // Do not read a new buffer merely to discover a total after the
            // requested range. Without observing EOF the remainder is unknown.
            reason = Some("line_limit");
            break;
        }
        let remaining_scan = MAX_RANGE_SCAN_BYTES - scanned_bytes;
        if remaining_scan == 0 {
            reason = Some("scan_budget");
            break;
        }
        line.clear();
        let allowance = remaining_scan.min(MAX_READ_FILE_BYTES + 1);
        let read = reader
            .by_ref()
            .take(allowance)
            .read_until(b'\n', &mut line)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        scanned_bytes += read as u64;
        if read == 0 {
            eof = true;
            break;
        }
        scanned_lines += 1;
        let complete = line.ends_with(b"\n") || read < allowance as usize;
        if line.ends_with(b"\n") {
            line.pop();
            if line.ends_with(b"\r") {
                line.pop();
            }
        }
        if scanned_lines < start_line {
            if !complete {
                reason = Some("line_budget");
                break;
            }
            continue;
        }
        let prefix = if options.line_numbers {
            format!("{scanned_lines}\t")
        } else {
            String::new()
        };
        let separator = usize::from(returned != 0);
        let available =
            (MAX_READ_FILE_BYTES as usize).saturating_sub(output.len() + prefix.len() + separator);
        if available == 0 {
            reason = Some("output_budget");
            break;
        }
        let mut end = line.len().min(available);
        let text = match std::str::from_utf8(&line[..end]) {
            Ok(text) => text,
            Err(error) if error.error_len().is_none() && (end < line.len() || !complete) => {
                end = error.valid_up_to();
                std::str::from_utf8(&line[..end]).expect("validated UTF-8 prefix")
            }
            Err(error) => return Err(format!("invalid UTF-8 in {}: {error}", path.display())),
        };
        if separator != 0 {
            output.push('\n');
        }
        output.push_str(&prefix);
        output.push_str(text);
        returned += 1;
        if end < line.len() || !complete {
            reason = Some("output_budget");
            break;
        }
        if output.len() == MAX_READ_FILE_BYTES as usize {
            reason = Some("output_budget");
            break;
        }
    }

    let end_line = if returned == 0 {
        None
    } else {
        Some(start_line + returned - 1)
    };
    Ok((
        output,
        json!({
            "path": path.display().to_string(),
            "start_line": start_line,
            "end_line": end_line,
            "limit": options.limit,
            "line_numbers": options.line_numbers,
            "total_lines": eof.then_some(scanned_lines),
            "returned_lines": returned,
            "truncated": !eof,
            "truncation_reason": reason,
            "scanned_bytes": scanned_bytes,
        }),
    ))
}

#[cfg(test)]
#[path = "read_budget_tests.rs"]
mod budget_tests;
