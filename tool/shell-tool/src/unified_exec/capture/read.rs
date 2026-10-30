use std::{
    io::{Read, Seek, SeekFrom},
    sync::{Arc, TryLockError, atomic::Ordering},
};

use anyhow::{Context, Result, bail};
use proteus_contracts::process_module::{
    ProcessModuleError, ToolModule, ToolModuleHostMut, ToolModuleInvocationContext,
};
use serde_json::{Value, json};

use super::super::ensure_not_cancelled;
use super::{CaptureStore, HANDLE_BYTES, MAX_READ_BYTES, MAX_READERS, store::QuotaState};

pub(crate) struct ReadCaptureTool(pub(crate) Arc<CaptureStore>);

struct ReadLease(Arc<QuotaState>);
impl Drop for ReadLease {
    fn drop(&mut self) {
        self.0.readers.fetch_sub(1, Ordering::AcqRel);
    }
}

impl ReadCaptureTool {
    fn invoke(
        &self,
        call_json: &str,
        context_json: &str,
        host: &mut ToolModuleHostMut<'_>,
    ) -> Result<String> {
        ensure_not_cancelled(host)?;
        let call: Value =
            serde_json::from_str(call_json).context("failed to parse capture ToolCall")?;
        let context: ToolModuleInvocationContext =
            serde_json::from_str(context_json).context("failed to parse capture context")?;
        let args = call.get("args").context("capture args required")?;
        let handle = args
            .get("handle")
            .and_then(Value::as_str)
            .context("capture handle required")?;
        if handle.len() != HANDLE_BYTES || !handle.is_ascii() {
            bail!("unknown or unavailable exec capture");
        }
        let (generation, index) = handle
            .split_once(':')
            .context("unknown or unavailable exec capture")?;
        if generation != self.0.generation.to_string() || index.len() != 16 {
            bail!("unknown or unavailable exec capture");
        }
        let id = u64::from_str_radix(index, 16).context("unknown or unavailable exec capture")?;
        let offset = args
            .get("offset")
            .map(|v| {
                v.as_u64()
                    .context("capture offset must be a nonnegative integer")
            })
            .transpose()?
            .unwrap_or(0);
        let requested = args
            .get("max_bytes")
            .map(|v| v.as_u64().context("max_bytes must be an integer"))
            .transpose()?
            .unwrap_or(4096);
        if !(1..=MAX_READ_BYTES as u64).contains(&requested)
            || offset.checked_add(requested).is_none()
        {
            bail!("capture range exceeds supported bounds");
        }
        let format = args
            .get("format")
            .map(|v| v.as_str().context("capture format must be utf8 or hex"))
            .transpose()?
            .unwrap_or("utf8");
        if !matches!(format, "utf8" | "hex") {
            bail!("capture format must be utf8 or hex");
        }
        let record = self
            .0
            .find(id)
            .context("unknown or unavailable exec capture")?;
        if !record.owner.matches(&context) {
            bail!("unknown or unavailable exec capture");
        }
        // Owner and logical expiry are rechecked for every range, not cached as authority.
        if self.0.expired(&record, std::time::Instant::now()) {
            bail!("unknown or unavailable exec capture");
        }
        let (receipt, length) = record.snapshot();
        let call_id = call.get("id").and_then(Value::as_str).unwrap_or("");
        let Some(length) = length else {
            // Metadata alone is not model-visible in ToolResult::text_or_status.
            // Copy only this bounded status snapshot into the visible envelope.
            let output = json!({ "source_capture": &receipt, "readable": false,
                "range": { "offset": offset, "max_bytes": requested, "format": format },
                "retry_read": matches!(receipt["storage_state"].as_str(), Some("capturing" | "finalizing")),
                "retry_command": false }).to_string();
            return Ok(json!({ "call_id": call_id, "ok": true,
                "output": output,
                "content": [], "error": null, "metadata": { "source_capture": receipt } })
            .to_string());
        };
        if offset > length {
            bail!("capture offset exceeds readable length");
        }
        self.0
            .quota
            .readers
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_READERS).then_some(n + 1)
            })
            .map_err(|_| {
                anyhow::anyhow!("capture reader busy; retry this read, not the command")
            })?;
        let _lease = ReadLease(self.0.quota.clone());
        ensure_not_cancelled(host)?;
        let mut file = match record.file.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => {
                bail!("capture reader busy; retry this read, not the command")
            }
            Err(TryLockError::Poisoned(_)) => bail!("capture storage unavailable"),
        };
        let file_ref = file.as_mut().context("capture storage unavailable")?;
        let count = requested.min(length - offset) as usize;
        let mut bytes = vec![0; count];
        // Immutable contents do not imply independent cursors. The single file
        // mutex covers seek AND the entire bounded read; no clone shares a cursor.
        file_ref
            .seek(SeekFrom::Start(offset))
            .context("capture range seek failed")?;
        file_ref
            .read_exact(&mut bytes)
            .context("capture range read failed")?;
        drop(file);
        ensure_not_cancelled(host)?;
        let output = match format {
            "utf8" => String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("capture byte range is not valid UTF-8 (possibly a split codepoint); request format=hex"))?,
            "hex" => hex(&bytes),
            _ => unreachable!(),
        };
        let mut metadata = json!({ "format": format, "offset": offset,
            "bytes_returned": count, "next_offset": offset + count as u64,
            "range_eof": offset + count as u64 == length });
        let mut envelope = json!({ "source_capture": &receipt, "range": &metadata,
            "readable": true, "retry_command": false });
        envelope[if format == "utf8" {
            "text"
        } else {
            "bytes_hex"
        }] = output.into();
        metadata["source_capture"] = receipt;
        Ok(
            json!({ "call_id": call_id, "ok": true, "output": envelope.to_string(),
            "content": [], "error": null, "metadata": metadata })
            .to_string(),
        )
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 15) as usize] as char);
    }
    text
}

impl ToolModule for ReadCaptureTool {
    fn spec_json(&self) -> String {
        json!({ "name": "read_exec_capture",
            "description": "Read a bounded range of an explicitly retained exec_command raw output prefix from this same shell binding/worker generation and current runtime session/thread/workspace. Output is a JSON envelope containing the capture receipt, range facts, and exact text or bytes_hex field; status-only replies distinguish pending from failed. Captures are ephemeral, may expire, and are readable only after their writer seals. A sealed prefix can still be incomplete or truncated: inspect complete_stream and reader facts, not range_eof. UTF-8 byte ranges that split a codepoint or contain binary data fail; use format=hex for exact bytes. A failed read never means rerun the command. No arbitrary paths. Safety: ReadOnly.",
            "input_schema": { "type": "object", "properties": {
                "handle": { "type": "string", "minLength": HANDLE_BYTES, "maxLength": HANDLE_BYTES },
                "offset": { "type": "integer", "minimum": 0, "description": "Byte offset, default zero." },
                "max_bytes": { "type": "integer", "minimum": 1, "maximum": MAX_READ_BYTES, "description": "Raw byte range ceiling, default 4096." },
                "format": { "type": "string", "enum": ["utf8", "hex"], "description": "Strict utf8 text (default) or exact hex bytes." }
            }, "required": ["handle"], "additionalProperties": false },
            "surface": { "kind": "function", "strict": false, "output_schema": null },
            "safety": "ReadOnly", "supports_parallel_tool_calls": true, "timeout_ms": 30000,
            "metadata": { "category": "terminal", "tags": ["capture", "output", "read"] }
        }).to_string()
    }
    fn invoke_json(
        &self,
        call_json: String,
        context_json: String,
        host: &mut ToolModuleHostMut<'_>,
    ) -> Result<String, ProcessModuleError> {
        self.invoke(&call_json, &context_json, host)
            .map_err(|error| ProcessModuleError::new(format!("{error:#}")))
    }
}
