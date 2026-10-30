use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Instant,
};

use anyhow::{Context, Result, bail};
use proteus_contracts::domain::{ExecutionId, new_execution_id};
use serde_json::Value;

use super::super::{
    reader::ReaderCompletions,
    session::{ExecSessionOwner, lock},
};
use super::{CaptureTap, Failure, QUEUE_CHUNKS, Record, State, StorageState, writer};
use crate::capture_config::{CaptureConfig, MAX_PATH_BYTES};

/// Budget has no map/store backreference. A resource cannot create an Arc cycle.
pub(super) struct QuotaState {
    pub(super) used: AtomicUsize,
    limit: usize,
    pub(super) readers: AtomicUsize,
}

pub(super) struct Reservation(Arc<QuotaState>);

impl QuotaState {
    pub(super) fn reserve(self: &Arc<Self>) -> Result<Reservation> {
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used < self.limit).then_some(used + 1)
            })
            .map_err(|_| anyhow::anyhow!("capture quota exhausted; command was not started"))?;
        Ok(Reservation(self.clone()))
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.0.used.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) struct CaptureStore {
    pub(super) config: CaptureConfig,
    pub(super) generation: ExecutionId,
    pub(super) quota: Arc<QuotaState>,
    entries: Mutex<Entries>,
}
struct Entries {
    next: u64,
    records: HashMap<u64, Arc<Record>>,
}

impl CaptureStore {
    pub(crate) fn new(config: CaptureConfig) -> Arc<Self> {
        Arc::new(Self {
            generation: new_execution_id(),
            quota: Arc::new(QuotaState {
                used: AtomicUsize::new(0),
                limit: config.slots,
                readers: AtomicUsize::new(0),
            }),
            config,
            entries: Mutex::new(Entries {
                next: 1,
                records: HashMap::new(),
            }),
        })
    }

    pub(super) fn expired(&self, record: &Record, now: Instant) -> bool {
        lock(&record.state)
            .terminal_at
            .is_some_and(|at| now.saturating_duration_since(at) >= self.config.retention)
    }

    fn cleanup(&self) {
        let retired = {
            let mut entries = lock(&self.entries);
            let now = Instant::now();
            let ids: Vec<_> = entries
                .records
                .iter()
                .filter(|(_, r)| self.expired(r, now))
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| entries.records.remove(&id))
                .collect::<Vec<_>>()
        };
        // File close is outside the store mutex; actual leases may retain it.
        drop(retired);
    }

    pub(super) fn find(&self, id: u64) -> Option<Arc<Record>> {
        self.cleanup();
        lock(&self.entries).records.get(&id).cloned()
    }

    pub(in crate::unified_exec) fn metadata(&self, tap: Option<&CaptureTap>) -> Option<Value> {
        let tap = tap.filter(|tap| tap.generation == self.generation)?;
        let record = tap.record.upgrade()?;
        if self.expired(&record, Instant::now()) {
            return None;
        }
        Some(record.receipt())
    }

    /// Must run BEFORE spawn_session/register_session: they can prune a live
    /// command before calling their spawn closure.
    pub(in crate::unified_exec) fn prepare(
        self: &Arc<Self>,
        owner: ExecSessionOwner,
        tty: bool,
    ) -> Result<PreparedCapture> {
        if owner.workspace_bytes() > MAX_PATH_BYTES {
            bail!("capture owner workspace exceeds 4096 encoded bytes");
        }
        self.cleanup();
        let reservation = self.quota.reserve()?;
        let id = {
            let mut entries = lock(&self.entries);
            let id = entries.next;
            entries.next = id
                .checked_add(1)
                .context("capture handle counter exhausted")?;
            id
        };
        // Anonymous: no path is published, no named artifact survives last close.
        let file = tempfile::tempfile_in(&self.config.directory)
            .context("capture storage unavailable; command was not started")?;
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CHUNKS);
        let record = Arc::new(Record {
            generation: self.generation,
            id,
            owner,
            cap: self.config.cap,
            state: Mutex::new(State {
                readers: ReaderCompletions::new(tty),
                expected: if tty { 1 } else { 2 },
                observed: 0,
                saturated: false,
                queued: 0,
                confirmed: 0,
                unconfirmed: 0,
                storage: StorageState::Capturing,
                failure: None,
                terminal_at: None,
                process_started: false,
                attached: false,
            }),
            sender,
            file: Mutex::new(None),
            _reservation: reservation,
        });
        writer::start(record.clone(), receiver, file)
            .context("capture writer unavailable; command was not started")?;
        lock(&self.entries).records.insert(id, record.clone());
        Ok(PreparedCapture {
            record,
            store: Arc::downgrade(self),
            attached: false,
        })
    }
}

impl Drop for CaptureStore {
    fn drop(&mut self) {
        // No writer owns Arc<Store>. Binding drop actively stops live storage;
        // waiting for an impossible future lookup/EOF would leak its thread.
        let entries = self.entries.get_mut().unwrap_or_else(|e| e.into_inner());
        for record in entries.records.values() {
            record.abort(Failure::BindingDropped);
        }
        entries.records.clear();
    }
}

pub(in crate::unified_exec) struct PreparedCapture {
    record: Arc<Record>,
    store: Weak<CaptureStore>,
    attached: bool,
}

impl PreparedCapture {
    /// Immediately after successful OS spawn, before fallible backend setup.
    pub(in crate::unified_exec) fn process_started(&mut self) {
        lock(&self.record.state).process_started = true;
    }
    /// Transfer recording to ExecSession before starting either output reader.
    pub(in crate::unified_exec) fn attach(mut self) -> CaptureTap {
        lock(&self.record.state).attached = true;
        self.attached = true;
        CaptureTap {
            generation: self.record.generation,
            record: Arc::downgrade(&self.record),
        }
    }
}

impl Drop for PreparedCapture {
    fn drop(&mut self) {
        if self.attached {
            return;
        }
        let started = lock(&self.record.state).process_started;
        self.record.abort(if started {
            Failure::AbortBeforeReaders
        } else {
            Failure::AbortBeforeSpawn
        });
        if let Some(store) = self.store.upgrade() {
            let retired = lock(&store.entries).records.remove(&self.record.id);
            drop(retired);
        }
        // Writer owns its file and reservation until it observes abort and closes.
        // No reader EOF is invented; started is never rewritten to NotStarted.
    }
}
