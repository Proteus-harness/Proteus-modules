//! Bounded output collection and one-shot child process lifecycle.

use std::{
    process::{Child, ExitStatus},
    time::{Duration, Instant},
};

use super::{HEAD_LIMIT_BYTES, TAIL_LIMIT_BYTES};

#[path = "execution/readers.rs"]
mod readers;
use readers::OutputReaders;

/// Head+tail буфер с жёстким потолком памяти: первые `HEAD_LIMIT_BYTES` и
/// последние `TAIL_LIMIT_BYTES` байта, середина дропается.
pub(super) struct BoundedBuffer {
    pub(super) head: Vec<u8>,
    pub(super) tail: std::collections::VecDeque<u8>,
    pub(super) original_len: usize,
}

impl BoundedBuffer {
    pub(super) fn new() -> Self {
        Self {
            head: Vec::new(),
            tail: std::collections::VecDeque::new(),
            original_len: 0,
        }
    }

    pub(super) fn from_bytes(bytes: &[u8]) -> Self {
        let mut buffer = Self::new();
        buffer.push(bytes);
        buffer
    }

    pub(super) fn push(&mut self, data: &[u8]) {
        self.original_len += data.len();
        let mut rest = data;
        if self.head.len() < HEAD_LIMIT_BYTES {
            let take = (HEAD_LIMIT_BYTES - self.head.len()).min(rest.len());
            self.head.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
        }
        if rest.is_empty() {
            return;
        }
        self.tail.extend(rest.iter().copied());
        if self.tail.len() > TAIL_LIMIT_BYTES {
            let excess = self.tail.len() - TAIL_LIMIT_BYTES;
            self.tail.drain(..excess);
        }
    }

    pub(super) fn truncated(&self) -> bool {
        self.original_len > self.head.len() + self.tail.len()
    }

    pub(super) fn to_text(&self) -> String {
        let tail_bytes: Vec<u8> = self.tail.iter().copied().collect();
        if !self.truncated() {
            let mut bytes = self.head.clone();
            bytes.extend_from_slice(&tail_bytes);
            return String::from_utf8_lossy(&bytes).into_owned();
        }
        // A byte limit may split a valid scalar. Trim only the incomplete
        // boundary fragments, keeping genuine malformed bytes lossy-visible.
        let head_end = match std::str::from_utf8(&self.head) {
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            _ => self.head.len(),
        };
        let tail_start = tail_bytes
            .iter()
            .take(3)
            .take_while(|byte| **byte & 0xc0 == 0x80)
            .count();
        let head = String::from_utf8_lossy(&self.head[..head_end]);
        let tail = String::from_utf8_lossy(&tail_bytes[tail_start..]);
        let omitted = self.original_len - head_end - (tail_bytes.len() - tail_start);
        format!(
            "{head}\n{}\n{tail}",
            omitted_marker(omitted, self.original_len)
        )
    }
}

/// Единый формат маркера усечения для терминальных tools (`shell`,
/// `exec_command`/`write_stdin`).
pub(super) fn omitted_marker(omitted: usize, total: usize) -> String {
    format!("[... omitted {omitted} of {total} bytes ...]")
}

pub(super) struct ShellOutput {
    pub(super) status: ExitStatus,
    pub(super) stdout: BoundedBuffer,
    pub(super) stderr: BoundedBuffer,
}

#[cfg(test)]
pub(super) fn wait_with_timeout(
    child: std::process::Child,
    timeout: Duration,
) -> std::io::Result<(ShellOutput, bool)> {
    wait_with_timeout_and_cancel(child, timeout, &mut || Ok(false))
}

pub(super) fn wait_with_timeout_and_cancel(
    mut child: std::process::Child,
    timeout: Duration,
    is_cancelled: &mut dyn FnMut() -> std::io::Result<bool>,
) -> std::io::Result<(ShellOutput, bool)> {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let readers = match OutputReaders::new(stdout, stderr) {
        Ok(readers) => readers,
        Err(error) => {
            kill_child_tree(&mut child);
            let _ = child.wait();
            return Err(error);
        }
    };
    let started = Instant::now();

    let mut exited = None;
    let mut can_signal = true;
    let result = loop {
        match is_cancelled() {
            Ok(false) => {}
            result => {
                break match result {
                    Ok(true) => Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "shell invocation canceled",
                    )),
                    Err(error) => Err(error),
                    Ok(false) => unreachable!(),
                };
            }
        }
        if exited.is_none() {
            #[cfg(unix)]
            {
                match crate::child_status::observe_exit(child.id()) {
                    Ok(status) => exited = status,
                    Err(error) => {
                        // Lost child ownership must not leave a recycled PID
                        // available to numeric process-group cleanup.
                        can_signal = false;
                        break Err(error);
                    }
                }
            }
            #[cfg(not(unix))]
            {
                match child.try_wait() {
                    Ok(status) => exited = status,
                    Err(error) => break Err(error),
                }
            }
        }
        if let Some(status) = exited
            && readers.is_finished()
        {
            // Reap only after inherited pipes have drained. Until now the
            // process-group number is reserved even if the leader exited.
            can_signal = false;
            break child.wait().map(|_| (status, false));
        }
        if started.elapsed() >= timeout {
            kill_child_tree(&mut child);
            can_signal = false;
            break child.wait().map(|status| (exited.unwrap_or(status), true));
        }
        std::thread::sleep(Duration::from_millis(10));
    };

    if result.is_err() && can_signal {
        kill_child_tree(&mut child);
        let _ = child.wait();
    }
    if !matches!(result, Ok((_, false))) {
        // A setsid descendant can keep inherited pipes open after group kill.
        // Nonblocking readers drain briefly, close their own fds, and are joined;
        // no detached blocking threads remain after timeout/cancel/error.
        readers.stop_after_grace();
    }
    let (stdout, stderr) = readers.join();
    let (status, timed_out) = result?;
    Ok((
        ShellOutput {
            status,
            stdout: stdout?,
            stderr: stderr?,
        },
        timed_out,
    ))
}

#[cfg(unix)]
fn kill_child_tree(child: &mut Child) {
    let pgid = child.id() as i32;
    unsafe {
        let _ = libc::kill(-pgid, libc::SIGKILL);
    }
    let _ = child.kill();
}

#[cfg(not(unix))]
fn kill_child_tree(child: &mut Child) {
    let _ = child.kill();
}

#[cfg(test)]
#[path = "execution/tests.rs"]
mod tests;
