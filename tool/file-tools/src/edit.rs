//! `edit_file` tool: точечная замена текста в существующем файле
//! (opencode edit shape: old_string → new_string, опционально replace_all).

use std::{
    io::{self, BufWriter, Write},
    path::Path,
};

use proteus_contracts::process_module::{ProcessModuleError, ToolModule, ToolModuleHostMut};
use serde_json::json;

use crate::util::{
    err_result, module_error, ok_result, parse_call, parse_invocation_context, required_string,
    workspace_file_for_atomic_write, workspace_path,
};

const MAX_EDIT_FILE_BYTES: u64 = 2 * 1024 * 1024;

pub struct EditFileTool;

impl ToolModule for EditFileTool {
    fn spec_json(&self) -> String {
        let spec = json!({
            "name": "edit_file",
            "description": "Replace an exact text snippet in an existing file inside the workspace. `old_string` must match the file content exactly (including whitespace and indentation) and must be unique unless `replace_all` is true. Use `write_file` to create new files or fully rewrite existing ones.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string", "description": "Exact text to replace." },
                    "new_string": { "type": "string", "description": "Replacement text (must differ from old_string)." },
                    "replace_all": { "type": "boolean", "description": "Replace all occurrences of old_string (default false)." }
                },
                "required": ["path", "old_string", "new_string"]
            },
            "surface": { "kind": "function", "strict": false, "output_schema": null },
            "safety": "WritesFiles",
            "supports_parallel_tool_calls": false,
            "timeout_ms": 60000,
            "metadata": {
                "category": "filesystem",
                "tags": ["filesystem", "edit", "file", "write"],
                "aliases": ["edit file", "replace text", "modify file"],
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
        let old_string = match required_string(&call.args, "old_string", &call.name) {
            Ok(s) => s,
            Err(e) => return err_result(&call.id, &call.name, e),
        };
        let new_string = match required_string(&call.args, "new_string", &call.name) {
            Ok(s) => s,
            Err(e) => return err_result(&call.id, &call.name, e),
        };
        let replace_all = call
            .args
            .get("replace_all")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        if old_string.is_empty() {
            return err_result(
                &call.id,
                &call.name,
                "old_string must not be empty; use write_file to create a new file".to_owned(),
            );
        }
        if old_string == new_string {
            return err_result(
                &call.id,
                &call.name,
                "no changes to apply: old_string and new_string are identical".to_owned(),
            );
        }

        let cwd_path = context.cwd.as_path();
        let target_path = match workspace_path(cwd_path, Path::new(path_str)) {
            Ok(p) => p,
            Err(e) => return err_result(&call.id, &call.name, e),
        };
        match std::fs::metadata(&target_path) {
            Ok(metadata) if metadata.is_dir() => {
                return err_result(
                    &call.id,
                    &call.name,
                    format!("path is a directory, not a file: {}", target_path.display()),
                );
            }
            Ok(metadata) if metadata.len() > MAX_EDIT_FILE_BYTES => {
                return err_result(
                    &call.id,
                    &call.name,
                    format!(
                        "file is too large to edit ({} bytes > {MAX_EDIT_FILE_BYTES})",
                        metadata.len()
                    ),
                );
            }
            Ok(_) => {}
            Err(e) => {
                return err_result(
                    &call.id,
                    &call.name,
                    format!("failed to stat {}: {e}", target_path.display()),
                );
            }
        }

        let content = match std::fs::read_to_string(&target_path) {
            Ok(content) => content,
            Err(e) => {
                return err_result(
                    &call.id,
                    &call.name,
                    format!("failed to read {}: {e}", target_path.display()),
                );
            }
        };

        let mut matches = content.match_indices(old_string);
        let Some(first) = matches.next() else {
            return err_result(
                &call.id,
                &call.name,
                "old_string not found in file content; make sure it matches exactly, including whitespace".to_owned(),
            );
        };
        let replacements = 1 + matches.clone().count();
        if !replace_all && replacements > 1 {
            return err_result(
                &call.id,
                &call.name,
                format!(
                    "old_string matches {replacements} locations; provide more surrounding context to make it unique or set replace_all"
                ),
            );
        }

        let untouched_bytes = content.len() - replacements * old_string.len();
        let Some(bytes_written) = replacements
            .checked_mul(new_string.len())
            .and_then(|bytes| bytes.checked_add(untouched_bytes))
            .filter(|&bytes| bytes <= isize::MAX as usize)
        else {
            return err_result(
                &call.id,
                &call.name,
                "replacement output is too large".to_owned(),
            );
        };

        let mut target = match workspace_file_for_atomic_write(cwd_path, &target_path) {
            Ok(file) => file,
            Err(error) => return err_result(&call.id, &call.name, error),
        };
        let written = if replacements > 1 && new_string.len() <= old_string.len() {
            // Для плотных нерастущих замен результат не больше входа; один
            // write_all оставляет быстрый стандартный replace на этом пути.
            target.write_all(content.replace(old_string, new_string).as_bytes())
        } else {
            write_replacements(
                &mut target,
                &content,
                std::iter::once(first).chain(matches.take(replacements - 1)),
                new_string,
            )
        };
        if let Err(e) = written {
            return err_result(
                &call.id,
                &call.name,
                format!("failed to write {}: {e}", target_path.display()),
            );
        }
        if let Err(error) = target.commit() {
            return err_result(&call.id, &call.name, error);
        }

        let metadata = json!({
            "path": target_path.display().to_string(),
            "replacements": replacements,
            "bytes_written": bytes_written,
        });
        ok_result(
            &call.id,
            &call.name,
            format!(
                "Replaced {replacements} occurrence(s) in {}",
                target_path.display()
            ),
            metadata,
        )
    }
}

/// Расширенный результат не хранится целиком. Ошибка отложенной записи
/// должна вернуться до публикации временного файла.
fn write_replacements<'a>(
    target: impl Write,
    content: &str,
    matches: impl Iterator<Item = (usize, &'a str)>,
    replacement: &str,
) -> io::Result<()> {
    let mut out = BufWriter::new(target);
    let mut cursor = 0;
    for (offset, matched) in matches {
        let prefix = &content.as_bytes()[cursor..offset];
        out.write_all(prefix)?;
        out.write_all(replacement.as_bytes())?;
        cursor = offset + matched.len();
    }
    let tail = &content.as_bytes()[cursor..];
    out.write_all(tail)?;
    out.flush()
}
