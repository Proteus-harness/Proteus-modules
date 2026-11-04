//! Независимый от выдачи модели ограниченный журнал живого терминала.
use super::session::{ExecSession, ExecSessionOwner, lock};
use anyhow::{Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use proteus_contracts::{
    contracts::{
        ToolTerminalAction, ToolTerminalDescriptor, ToolTerminalRequest, ToolTerminalResponse,
        ToolTerminalSnapshot,
    },
    domain::new_execution_id,
    process_module::ToolModuleInvocationContext,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Condvar, Mutex, OnceLock, Weak},
    time::{Duration, Instant},
};

const RECORD_LIMIT: usize = 32;
const BYTE_LIMIT: usize = 1024 * 1024;
const READ_LIMIT: usize = 64 * 1024;
const RETENTION: Duration = Duration::from_secs(30 * 60);

pub(super) struct TerminalRecord {
    handle: String,
    title: String,
    owner: ExecSessionOwner,
    tty: bool,
    created: Instant,
    state: Mutex<State>,
    changed: Condvar,
    session: Mutex<Weak<ExecSession>>,
}
struct State {
    bytes: VecDeque<u8>,
    end: u64,
    finished: bool,
    exit_code: Option<i32>,
    finished_at: Option<Instant>,
    columns: u16,
    rows: u16,
}
impl TerminalRecord {
    pub(super) fn new(owner: ExecSessionOwner, tty: bool, title: &str) -> Self {
        Self {
            handle: new_execution_id().to_string(),
            title: bounded_title(title),
            owner,
            tty,
            created: Instant::now(),
            state: Mutex::new(State {
                bytes: VecDeque::new(),
                end: 0,
                finished: false,
                exit_code: None,
                finished_at: None,
                columns: 80,
                rows: 24,
            }),
            changed: Condvar::new(),
            session: Mutex::new(Weak::new()),
        }
    }
    pub(super) fn descriptor(&self) -> ToolTerminalDescriptor {
        let state = lock(&self.state);
        ToolTerminalDescriptor {
            handle: self.handle.clone(),
            title: self.title.clone(),
            tty: self.tty,
            columns: state.columns,
            rows: state.rows,
        }
    }
    // Called while SessionOutput is locked, preserving the model's merged stream order.
    pub(super) fn append(&self, chunk: &[u8]) {
        let mut state = lock(&self.state);
        state.end = state.end.saturating_add(chunk.len() as u64);
        if chunk.len() >= BYTE_LIMIT {
            state.bytes.clear();
            state.bytes.extend(&chunk[chunk.len() - BYTE_LIMIT..]);
        } else {
            let excess = (state.bytes.len() + chunk.len()).saturating_sub(BYTE_LIMIT);
            state.bytes.drain(..excess);
            state.bytes.extend(chunk);
        }
        drop(state);
        self.changed.notify_all();
    }
    pub(super) fn lifecycle(&self, finished: bool, exit_code: Option<i32>) {
        let mut state = lock(&self.state);
        state.exit_code = exit_code;
        if finished && !state.finished {
            state.finished = true;
            state.finished_at = Some(Instant::now());
        }
        drop(state);
        self.changed.notify_all();
    }
    fn read(
        &self,
        cursor: Option<u64>,
        max_bytes: u32,
        wait_ms: u32,
    ) -> Result<ToolTerminalSnapshot> {
        let mut state = lock(&self.state);
        if cursor.is_some_and(|cursor| cursor > state.end) {
            bail!("terminal cursor exceeds stream end");
        }
        let available_start = state.end - state.bytes.len() as u64;
        let requested = cursor.unwrap_or(available_start);
        if requested == state.end && !state.finished && wait_ms > 0 {
            let deadline = Instant::now() + Duration::from_millis(u64::from(wait_ms.min(1000)));
            while requested == state.end && !state.finished {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                state = self
                    .changed
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(|p| p.into_inner())
                    .0;
            }
        }
        let available_start = state.end - state.bytes.len() as u64;
        let start = requested.max(available_start);
        let count = ((state.end - start) as usize).min((max_bytes as usize).clamp(1, READ_LIMIT));
        let data: Vec<u8> = state
            .bytes
            .iter()
            .skip((start - available_start) as usize)
            .take(count)
            .copied()
            .collect();
        Ok(ToolTerminalSnapshot {
            start_cursor: start,
            next_cursor: start + count as u64,
            end_cursor: state.end,
            data_base64: STANDARD.encode(data),
            gap: requested < available_start || (cursor.is_none() && available_start > 0),
            finished: state.finished,
            exit_code: state.exit_code,
            tty: self.tty,
            columns: state.columns,
            rows: state.rows,
        })
    }
    fn evictable(&self) -> bool {
        lock(&self.state).finished
    }
    fn expired(&self, now: Instant) -> bool {
        lock(&self.state)
            .finished_at
            .is_some_and(|at| now.saturating_duration_since(at) >= RETENTION)
    }
}

// Core ограничивает title байтами; не разрезаем UTF-8 и сохраняем краткий preview.
fn bounded_title(title: &str) -> String {
    let end = title
        .char_indices()
        .take(160)
        .map(|(offset, character)| offset + character.len_utf8())
        .take_while(|end| *end <= 512)
        .last()
        .unwrap_or(0);
    title[..end].to_owned()
}

type Records = HashMap<String, Arc<TerminalRecord>>;
fn records() -> &'static Mutex<Records> {
    static RECORDS: OnceLock<Mutex<Records>> = OnceLock::new();
    RECORDS.get_or_init(|| Mutex::new(HashMap::new()))
}
/// Reserves capacity before OS spawn; failure must never execute the command.
pub(super) struct Admission {
    record: Arc<TerminalRecord>,
    committed: bool,
}
impl Admission {
    pub(super) fn prepare(owner: ExecSessionOwner, tty: bool, title: &str) -> Result<Self> {
        let record = Arc::new(TerminalRecord::new(owner, tty, title));
        let mut records = lock(records());
        admit_record(&mut records, &record)?;
        Ok(Self {
            record,
            committed: false,
        })
    }
    pub(super) fn record(&self) -> Arc<TerminalRecord> {
        self.record.clone()
    }
    pub(super) fn commit(mut self, session: &Arc<ExecSession>) {
        *lock(&self.record.session) = Arc::downgrade(session);
        self.committed = true;
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        if !self.committed {
            lock(records()).remove(&self.record.handle);
        }
    }
}
fn admit_record(records: &mut Records, record: &Arc<TerminalRecord>) -> Result<()> {
    prune(records, Instant::now());
    while records.len() >= RECORD_LIMIT {
        let victim = records
            .iter()
            .filter(|(_, record)| record.evictable())
            .min_by_key(|(_, record)| record.created)
            .map(|(key, _)| key.clone());
        let Some(victim) = victim else {
            bail!("terminal record limit ({RECORD_LIMIT}) reached; all terminals are active");
        };
        records.remove(&victim);
    }
    records.insert(record.handle.clone(), record.clone());
    Ok(())
}
fn prune(records: &mut Records, now: Instant) {
    records.retain(|_, record| !record.expired(now));
}
pub(super) fn prune_expired() {
    prune(&mut lock(records()), Instant::now());
}

pub(super) fn control(
    request: ToolTerminalRequest,
    context: ToolModuleInvocationContext,
) -> Result<ToolTerminalResponse> {
    let record = {
        let mut records = lock(records());
        prune(&mut records, Instant::now());
        let Some(record) = records.get(&request.handle).cloned() else {
            return Ok(ToolTerminalResponse::Unavailable);
        };
        record
    };
    if !record.owner.matches(&context) {
        bail!("terminal is not owned by the current execution context/workspace");
    }
    if let ToolTerminalAction::Read {
        cursor,
        max_bytes,
        wait_ms,
    } = request.action
    {
        return Ok(ToolTerminalResponse::Read(
            record.read(cursor, max_bytes, wait_ms)?,
        ));
    }
    let session = lock(&record.session)
        .upgrade()
        .ok_or_else(|| anyhow::anyhow!("terminal process is no longer available"))?;
    if lock(&record.state).finished {
        bail!("terminal process has finished");
    }
    match request.action {
        ToolTerminalAction::Input { data_base64 } => {
            if !record.tty {
                bail!("terminal input requires tty: true");
            }
            if data_base64.len() > READ_LIMIT * 2 {
                bail!("terminal input exceeds byte limit");
            }
            let bytes = STANDARD.decode(data_base64)?;
            if bytes.len() > READ_LIMIT {
                bail!("terminal input exceeds byte limit");
            }
            // The PTY writer serializes a complete write; never take interaction.
            session.control.write(&bytes)?;
            if !bytes.is_empty() {
                session.touch();
            }
        }
        ToolTerminalAction::Resize { columns, rows } => {
            if !record.tty {
                bail!("terminal resize requires tty: true");
            }
            if !(2..=1000).contains(&columns) || !(1..=500).contains(&rows) {
                bail!("invalid terminal dimensions");
            }
            session.control.resize(columns, rows)?;
            let mut state = lock(&record.state);
            state.columns = columns;
            state.rows = rows;
        }
        ToolTerminalAction::Stop => session.control.kill(),
        ToolTerminalAction::Read { .. } => unreachable!(),
    }
    Ok(ToolTerminalResponse::Accepted)
}

#[cfg(test)]
mod tests;
