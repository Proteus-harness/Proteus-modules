//! Bounded output collection, completion and model-visible rendering.
use super::{
    CANCELLATION_POLL_INTERVAL, EXIT_DRAIN_GRACE, SESSION_BUFFER_LIMIT, invocation_is_cancelled,
    session::{ExecSession, lock},
};
use crate::omitted_marker;
use anyhow::Result;
use proteus_contracts::process_module::ToolModuleHostMut;
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    time::{Duration, Instant},
};

pub(super) struct Collected {
    pub(super) bytes: Vec<u8>,
    pub(super) dropped_bytes: usize,
    pub(super) exited: bool,
    pub(super) exit_code: Option<i32>,
}

/// The model buffer keeps the first half and a circular last half after
/// overflow. Only collection needs the retained suffix in stream order.
#[derive(Default)]
pub(super) struct BoundedOutput {
    bytes: Vec<u8>,
    next_tail: usize,
    dropped_bytes: usize,
}

impl BoundedOutput {
    const HEAD: usize = SESSION_BUFFER_LIMIT / 2;
    const TAIL: usize = SESSION_BUFFER_LIMIT - Self::HEAD;

    pub(super) fn push(&mut self, chunk: &[u8]) {
        let len = self.bytes.len();
        let fit = chunk.len().min(SESSION_BUFFER_LIMIT - len);
        let retained = len + fit;
        let excess = chunk.len() - fit;
        let replaces_tail = excess != 0 && chunk.len() >= Self::TAIL;
        let capacity = self.bytes.capacity();
        if retained > capacity && (capacity > Self::HEAD || replaces_tail) {
            // A large replacement appends twice (head then tail). Reserve
            // both at once so neither that nor an ordinary large append can
            // double a partly filled allocation past the retention limit.
            self.bytes.reserve_exact(SESSION_BUFFER_LIMIT - len);
        }
        if excess == 0 {
            self.bytes.extend_from_slice(chunk);
            return;
        }
        self.dropped_bytes = self.dropped_bytes.saturating_add(excess);
        if replaces_tail {
            let missing_head = Self::HEAD.saturating_sub(len);
            self.bytes.truncate(Self::HEAD);
            self.bytes.extend_from_slice(&chunk[..missing_head]);
            self.bytes.extend_from_slice(&chunk[chunk.len() - Self::TAIL..]);
            self.next_tail = 0;
            return;
        }

        self.bytes.extend_from_slice(&chunk[..fit]);
        let chunk = &chunk[fit..];
        let tail = &mut self.bytes[Self::HEAD..];
        let first = chunk.len().min(Self::TAIL - self.next_tail);
        tail[self.next_tail..self.next_tail + first].copy_from_slice(&chunk[..first]);
        tail[..chunk.len() - first].copy_from_slice(&chunk[first..]);
        self.next_tail = (self.next_tail + chunk.len()) % Self::TAIL;
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.bytes.len()
    }

    #[cfg(test)]
    pub(super) fn contiguous_for_test(&self) -> &[u8] {
        assert_eq!(self.next_tail, 0, "test expected ordered bytes");
        &self.bytes
    }

    pub(super) fn take(&mut self, final_output: bool) -> (Vec<u8>, usize) {
        let next_tail = std::mem::take(&mut self.next_tail);
        if next_tail != 0 {
            self.bytes[Self::HEAD..].rotate_left(next_tail);
        }
        (
            take_ready_bytes(&mut self.bytes, final_output),
            std::mem::take(&mut self.dropped_bytes),
        )
    }
}

/// Ждёт до дедлайна (или до exit + drain) и забирает накопленный вывод.
pub(super) fn wait_and_collect(
    session: &ExecSession,
    yield_time: Duration,
    host: &mut ToolModuleHostMut<'_>,
) -> Result<Collected> {
    let deadline = Instant::now() + yield_time;
    let mut output = lock(&session.output);
    let mut exit_seen_at: Option<Instant> = None;
    loop {
        // A process host round trip must not block the local output readers.
        drop(output);
        let cancelled = invocation_is_cancelled(host)?;
        output = lock(&session.output);
        if cancelled {
            drop(output);
            session.kill();
            anyhow::bail!("tool invocation canceled");
        }
        if output.exited {
            let seen = *exit_seen_at.get_or_insert_with(Instant::now);
            if output.readers.is_closed() || seen.elapsed() >= EXIT_DRAIN_GRACE {
                break;
            }
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let mut wait = deadline - now;
        if let Some(seen) = exit_seen_at {
            let grace_left = EXIT_DRAIN_GRACE.saturating_sub(seen.elapsed());
            wait = wait.min(grace_left.max(Duration::from_millis(1)));
        }
        wait = wait.min(CANCELLATION_POLL_INTERVAL);
        let (guard, _timeout) = session
            .output_cond
            .wait_timeout(output, wait)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        output = guard;
    }
    let final_output = output.exited || output.readers.is_closed();
    let (bytes, dropped_bytes) = output.buffer.take(final_output);
    Ok(Collected {
        bytes,
        dropped_bytes,
        exited: output.exited,
        exit_code: output.exit_code,
    })
}

/// Keep only an incomplete trailing UTF-8 scalar for the next live poll.
/// Invalid bytes and final output still use the existing lossy rendering.
fn take_ready_bytes(buffer: &mut Vec<u8>, final_output: bool) -> Vec<u8> {
    let mut bytes = std::mem::take(buffer);
    if final_output {
        return bytes;
    }
    // An incomplete scalar has at most three bytes. Starting inside an earlier
    // complete scalar is harmless: its continuation bytes are skipped below.
    let mut offset = bytes.len().saturating_sub(3);
    while offset < bytes.len() {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(_) => break,
            Err(error) => {
                offset += error.valid_up_to();
                match error.error_len() {
                    Some(len) => offset += len,
                    None => {
                        *buffer = bytes.split_off(offset);
                        break;
                    }
                }
            }
        }
    }
    bytes
}

pub(super) fn render_result(
    call_id: &str,
    session_id: i64,
    collected: Collected,
    wall_time: Duration,
    max_output_bytes: usize,
    mut metadata: Value,
) -> String {
    let raw_text = if collected.dropped_bytes == 0 {
        String::from_utf8_lossy(&collected.bytes)
    } else {
        let (head, tail) = collected.bytes.split_at(SESSION_BUFFER_LIMIT / 2);
        format!(
            "{}\n{}\n{}",
            String::from_utf8_lossy(head),
            omitted_marker(
                collected.dropped_bytes,
                collected
                    .bytes
                    .len()
                    .saturating_add(collected.dropped_bytes)
            ),
            String::from_utf8_lossy(tail)
        )
        .into()
    };
    let (text, truncated) = truncate_head_tail(&raw_text, max_output_bytes);

    let mut sections = vec![format!("Wall time: {:.4} seconds", wall_time.as_secs_f64())];
    if collected.exited {
        match collected.exit_code {
            Some(code) => sections.push(format!("Process exited with code {code}")),
            None => sections.push("Process terminated without exit code".to_owned()),
        }
    } else {
        sections.push(format!("Process running with session ID {session_id}"));
    }
    if collected.dropped_bytes > 0 {
        sections.push(format!(
            "[{} bytes from the middle of output were dropped from the session buffer]",
            collected.dropped_bytes
        ));
    }
    if let Some(capture) = metadata.get("source_capture") {
        sections.push(format!("Source capture: {} (state={}, complete_stream={}); use read_exec_capture to inspect status or read a sealed retained prefix. This is independent of the command exit code.",
            capture["handle"].as_str().unwrap_or("unavailable"),
            capture["storage_state"].as_str().unwrap_or("unknown"),
            capture["complete_stream"].as_bool().unwrap_or(false)));
    }
    let mut output = sections.join("\n");
    const OUTPUT_LABEL: &str = "\nOutput:\n";
    output.reserve(OUTPUT_LABEL.len() + text.len());
    output.push_str(OUTPUT_LABEL);
    output.push_str(&text);

    if let Some(map) = metadata.as_object_mut() {
        map.insert(
            "session_id".to_owned(),
            if collected.exited {
                Value::Null
            } else {
                json!(session_id)
            },
        );
        map.insert("exited".to_owned(), json!(collected.exited));
        map.insert("exit_code".to_owned(), json!(collected.exit_code));
        map.insert(
            "wall_time_seconds".to_owned(),
            json!(wall_time.as_secs_f64()),
        );
        map.insert("output_bytes".to_owned(), json!(collected.bytes.len()));
        map.insert("dropped_bytes".to_owned(), json!(collected.dropped_bytes));
        map.insert(
            "truncated".to_owned(),
            json!(truncated || collected.dropped_bytes > 0),
        );
    }

    // Parity с upstream Codex (`ExecCommandToolOutput`): unified exec всегда
    // отдаёт success — exit code процесса это данные в тексте/metadata, а не
    // сбой tool-а. Иначе Ctrl-C или опрос умершей сессии выглядят для модели
    // как ошибка write_stdin (dogfood 2026-07-06: слепые повторы).
    Value::Object(
        [
            ("call_id".into(), call_id.into()),
            ("ok".into(), true.into()),
            ("output".into(), Value::String(output)),
            ("content".into(), Value::Array(Vec::new())),
            ("error".into(), Value::Null),
            ("metadata".into(), metadata),
        ]
        .into_iter()
        .collect(),
    )
    .to_string()
}

/// Head+tail усечение: начало и конец видны, середина вырезается — как в
/// Codex, чтобы модель видела и старт команды, и актуальный хвост.
pub(super) fn truncate_head_tail(text: &str, max_bytes: usize) -> (Cow<'_, str>, bool) {
    if text.len() <= max_bytes {
        return (Cow::Borrowed(text), false);
    }
    let head_target = max_bytes / 2;
    let tail_target = max_bytes - head_target;
    let head_end = floor_char_boundary(text, head_target);
    let tail_start = ceil_char_boundary(text, text.len() - tail_target);
    let omitted = tail_start - head_end;
    (
        format!(
            "{}\n{}\n{}",
            &text[..head_end],
            omitted_marker(omitted, text.len()),
            &text[tail_start..]
        )
        .into(),
        true,
    )
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests;
