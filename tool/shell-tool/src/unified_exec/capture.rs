//! Optional source-side capture owned by one bound shell_tools generation.
//! No command strings, paths from tool arguments, output logs, or network I/O.
use std::{
    fs::File,
    sync::{
        Condvar, Mutex, Weak,
        mpsc::{SyncSender, TrySendError},
    },
    time::Instant,
};

use proteus_contracts::domain::ExecutionId;
use serde_json::{Value, json};

use super::{
    reader::{ReaderCompletions, ReaderEnd},
    session::{ExecSessionOwner, lock},
};

mod read;
mod store;
mod writer;
pub(crate) use read::ReadCaptureTool;
pub(crate) use store::CaptureStore;
pub(super) use store::PreparedCapture;

const CHUNK_BYTES: usize = 8192;
const QUEUE_CHUNKS: usize = 8;
const MAX_READ_BYTES: usize = 65536;
const MAX_READERS: usize = 4;
const HANDLE_BYTES: usize = 53;

/// The closure includes session pruning and spawn. Capture rejection must not
/// enter either; this also gives regression sources the exact admission seam.
pub(super) fn admit<T>(
    store: Option<&std::sync::Arc<CaptureStore>>,
    owner: ExecSessionOwner,
    tty: bool,
    spawn: impl FnOnce(ExecSessionOwner, Option<PreparedCapture>) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    if store.is_some() && owner.workspace_bytes() > crate::capture_config::MAX_PATH_BYTES {
        anyhow::bail!("capture owner workspace exceeds 4096 encoded bytes");
    }
    let prepared = store
        .map(|store| store.prepare(owner.clone(), tty))
        .transpose()?;
    spawn(owner, prepared)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StorageState {
    Capturing,
    Finalizing,
    Sealed,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    QueueFull,
    WriterLost,
    Io,
    Protocol,
    AbortBeforeSpawn,
    AbortBeforeReaders,
    BindingDropped,
}

impl Failure {
    fn label(self) -> &'static str {
        match self {
            Self::QueueFull => "queue_full",
            Self::WriterLost => "writer_lost",
            Self::Io => "storage_io",
            Self::Protocol => "capture_protocol",
            Self::AbortBeforeSpawn => "abort_before_spawn",
            Self::AbortBeforeReaders => "abort_before_readers",
            Self::BindingDropped => "binding_dropped",
        }
    }
}

struct State {
    readers: ReaderCompletions,
    expected: u8,
    observed: u64,
    saturated: bool,
    queued: u64,
    confirmed: u64,
    unconfirmed: u64,
    storage: StorageState,
    failure: Option<Failure>,
    terminal_at: Option<Instant>,
    process_started: bool,
    attached: bool,
}

struct Record {
    generation: ExecutionId,
    id: u64,
    owner: ExecSessionOwner,
    cap: u64,
    state: Mutex<State>,
    changed: Condvar,
    sender: SyncSender<Box<[u8]>>,
    // One actual File. Writer owns it until seal; reads serialize seek+read here.
    file: Mutex<Option<File>>,
    // Field order plus explicit Drop guarantee close-before-reservation-release.
    _reservation: store::Reservation,
}

impl Drop for Record {
    fn drop(&mut self) {
        let file = self
            .file
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        drop(file);
        // Reservation field is dropped only after this method returns.
    }
}

impl Record {
    fn append_result(&self, attempted: usize, result: Result<(), writer::AppendFailure>) {
        let mut state = lock(&self.state);
        match result {
            Ok(()) => state.confirmed += attempted as u64,
            Err(failure) => {
                state.confirmed += failure.confirmed as u64;
                state.unconfirmed = failure.uncertain as u64;
                state.failure.get_or_insert(Failure::Io);
                state.storage = StorageState::Failed;
            }
        }
    }
    fn handle(&self) -> String {
        format!("{}:{:016x}", self.generation, self.id)
    }

    fn observe(&self, chunk: &[u8]) {
        let mut state = lock(&self.state);
        match state.observed.checked_add(chunk.len() as u64) {
            Some(total) => state.observed = total,
            None => {
                state.observed = u64::MAX;
                state.saturated = true;
            }
        }
        if state.failure.is_some() {
            return;
        }
        if chunk.len() > CHUNK_BYTES || !state.attached || state.readers.is_closed() {
            state.failure = Some(Failure::Protocol);
            state.storage = StorageState::Failed;
            self.changed.notify_one();
            return;
        }
        let offered = chunk
            .len()
            .min(self.cap.saturating_sub(state.queued) as usize);
        if offered == 0 {
            return;
        }
        // Called under the existing output mutex: only one enqueue allocation
        // per capture can coexist with 8 queued + 1 writer-held payloads.
        match self.sender.try_send(chunk[..offered].into()) {
            Ok(()) => state.queued += offered as u64,
            Err(error) => {
                state.failure = Some(match error {
                    TrySendError::Full(_) => Failure::QueueFull,
                    TrySendError::Disconnected(_) => Failure::WriterLost,
                });
                state.storage = StorageState::Failed;
            }
        }
        self.changed.notify_one();
    }

    fn ended(&self, end: ReaderEnd) {
        let mut state = lock(&self.state);
        state.readers.record(end);
        if state.readers.is_closed() && state.failure.is_none() {
            state.storage = StorageState::Finalizing;
        }
        self.changed.notify_one();
    }

    fn abort(&self, reason: Failure) {
        let mut state = lock(&self.state);
        state.failure.get_or_insert(reason);
        state.storage = StorageState::Failed;
        self.changed.notify_one();
    }

    fn receipt(&self) -> Value {
        self.snapshot().0
    }

    /// Range readiness and its visible receipt must describe one state, even
    /// when the writer seals between two successive tool-side instructions.
    fn snapshot(&self) -> (Value, Option<u64>) {
        let state = lock(&self.state);
        let sealed = state.storage == StorageState::Sealed && state.failure.is_none();
        let complete = sealed
            && state.readers.eof == state.expected
            && !state.saturated
            && state.observed == state.confirmed;
        let receipt = json!({
            "version": 1, "handle": self.handle(), "stream": "merged_host_order",
            "storage_state": match state.storage {
                StorageState::Capturing => "capturing", StorageState::Finalizing => "finalizing",
                StorageState::Sealed => "sealed", StorageState::Failed => "failed",
            },
            "observed_bytes": state.observed, "count_saturated": state.saturated,
            "confirmed_bytes": state.confirmed, "unconfirmed_append_bytes": state.unconfirmed,
            "cap_bytes": self.cap, "cap_reached": state.confirmed == self.cap,
            "prefix_truncated": state.observed > self.cap,
            "readable_bytes": if sealed { Some(state.confirmed) } else { None },
            "readers": { "expected": state.expected, "eof": state.readers.eof,
                "read_failed": state.readers.read_failed, "drain_expired": state.readers.drain_expired,
                "pending": state.expected - state.readers.eof - state.readers.read_failed - state.readers.drain_expired },
            "storage_failure": state.failure.map(Failure::label),
            "complete_stream": complete,
        });
        (receipt, sealed.then_some(state.confirmed))
    }
}

/// Session taps are weak: a finished session must not defeat TTL or binding drop.
pub(super) struct CaptureTap {
    generation: ExecutionId,
    record: Weak<Record>,
}

impl CaptureTap {
    pub(super) fn observe(&self, chunk: &[u8]) {
        if let Some(record) = self.record.upgrade() {
            record.observe(chunk);
        }
    }
    pub(super) fn ended(&self, end: ReaderEnd) {
        if let Some(record) = self.record.upgrade() {
            record.ended(end);
        }
    }
}

#[cfg(test)]
mod tests;
