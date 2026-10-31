use std::{
    cell::Cell,
    io,
    sync::{Arc, atomic::Ordering, mpsc},
    time::{Duration, Instant},
};

use proteus_contracts::{
    contracts::ExecutionAttribution,
    domain::{new_execution_id, new_session_id, new_thread_id, new_turn_id},
    process_module::{ProcessModuleError, ToolModule, ToolModuleHost, ToolModuleInvocationContext},
};

use super::super::session::{ExecSession, ProcessControl};
use super::*;
use crate::capture_config::CaptureConfig;

mod process_paths;
mod read_ranges;
mod writer_wake;

struct Host {
    cancelled: bool,
}
impl ToolModuleHost for Host {
    fn is_cancelled(&self) -> Result<bool, ProcessModuleError> {
        Ok(self.cancelled)
    }
}
struct NoopControl;
impl ProcessControl for NoopControl {
    fn write(&self, _: &[u8]) -> io::Result<()> {
        Ok(())
    }
    fn interrupt(&self) -> io::Result<()> {
        Ok(())
    }
    fn kill(&self) {}
}

// Process fixtures clean up their unique owner even if an assertion panics.
// Do not depend on the production 30-minute janitor to settle a failed test.
struct OwnedSessionsCleanup(ExecSessionOwner);
impl Drop for OwnedSessionsCleanup {
    fn drop(&mut self) {
        let victims = {
            let mut sessions = lock(super::super::session::sessions());
            let ids: Vec<_> = sessions
                .iter()
                .filter(|(_, session)| session.owner == self.0)
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| sessions.remove(&id))
                .collect::<Vec<_>>()
        };
        for victim in victims {
            victim.kill();
        }
    }
}

fn context(path: &std::path::Path) -> ToolModuleInvocationContext {
    ToolModuleInvocationContext {
        cwd: path.to_path_buf(),
        skills: Default::default(),
        config: json!({}),
        attribution: ExecutionAttribution::for_turn(
            new_execution_id(),
            new_session_id(),
            new_thread_id(),
            new_turn_id(),
        ),
    }
}
fn owner(context: &ToolModuleInvocationContext) -> ExecSessionOwner {
    ExecSessionOwner::from_context(
        context,
        context.cwd.canonicalize().unwrap().to_str().unwrap(),
    )
}
fn fixture(
    cap: u64,
    slots: usize,
) -> (
    tempfile::TempDir,
    Arc<CaptureStore>,
    ToolModuleInvocationContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let context = context(directory.path());
    let store = CaptureStore::new(CaptureConfig {
        directory: directory.path().to_path_buf(),
        cap,
        slots,
        retention: Duration::from_secs(300),
    });
    (directory, store, context)
}
fn attached(
    store: &Arc<CaptureStore>,
    context: &ToolModuleInvocationContext,
    tty: bool,
) -> (ExecSession, Arc<Record>) {
    let mut prepared = store.prepare(owner(context), tty).unwrap();
    let record = store.find(1).unwrap();
    prepared.process_started();
    let session = ExecSession::new(Box::new(NoopControl), tty, None, owner(context))
        .with_capture(Some(prepared));
    (session, record)
}
fn wait_for(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < deadline, "capture worker did not settle");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn sealed(record: &Record) {
    wait_for(|| lock(&record.state).terminal_at.is_some());
    assert_eq!(lock(&record.state).storage, StorageState::Sealed);
}
fn read(
    store: &Arc<CaptureStore>,
    context: &ToolModuleInvocationContext,
    args: Value,
) -> Result<Value, ProcessModuleError> {
    let tool = ReadCaptureTool(store.clone());
    let call = json!({ "id": "capture_read", "name": "read_exec_capture", "args": args });
    tool.invoke_json(
        call.to_string(),
        serde_json::to_string(context).unwrap(),
        &mut Host { cancelled: false },
    )
    .map(|result| {
        let typed: proteus_contracts::domain::ToolResult = serde_json::from_str(&result).unwrap();
        let envelope: Value = serde_json::from_str(&typed.text_or_status()).unwrap();
        let mut result = serde_json::to_value(typed).unwrap();
        result["envelope"] = envelope;
        result
    })
}

#[test]
fn cap_minus_one_exact_cap_and_cap_plus_one_have_distinct_receipts() {
    for length in [3, 4, 5] {
        let (_dir, store, context) = fixture(4, 1);
        let (session, record) = attached(&store, &context, true);
        session.push_output(&vec![b'x'; length]);
        session.mark_reader_ended(ReaderEnd::Eof);
        sealed(&record);
        let receipt = record.receipt();
        assert_eq!(receipt["observed_bytes"], length);
        assert_eq!(receipt["confirmed_bytes"], length.min(4));
        assert_eq!(receipt["cap_reached"], length >= 4);
        assert_eq!(receipt["prefix_truncated"], length > 4);
        assert_eq!(receipt["complete_stream"], length <= 4);
        let result = read(&store, &context, json!({"handle": record.handle()})).unwrap();
        assert_eq!(result["envelope"]["text"], "x".repeat(length.min(4)));
        assert_eq!(result["metadata"]["range_eof"], true);
        assert_eq!(lock(&session.output).buffer.len(), length);
    }
}

#[test]
fn cap_and_read_failure_are_both_preserved_not_prioritized_away() {
    let (_dir, store, context) = fixture(3, 1);
    let (session, record) = attached(&store, &context, false);
    session.push_output(b"abcdef");
    session.mark_reader_ended(ReaderEnd::ReadFailed);
    assert_eq!(record.receipt()["readers"]["pending"], 1);
    session.mark_reader_ended(ReaderEnd::Eof);
    sealed(&record);
    let receipt = record.receipt();
    assert_eq!(receipt["prefix_truncated"], true);
    assert_eq!(receipt["readers"]["read_failed"], 1);
    assert_eq!(receipt["readers"]["eof"], 1);
    assert_eq!(receipt["complete_stream"], false);
    assert_eq!(
        read(&store, &context, json!({"handle": record.handle()})).unwrap()["envelope"]["text"],
        "abc"
    );
}

#[test]
fn process_exit_and_drain_expiry_are_not_eof_or_early_sealing() {
    let (_dir, store, context) = fixture(100, 1);
    let (session, record) = attached(&store, &context, false);
    session.push_output(b"tail");
    session.mark_exited(Some(7));
    assert_eq!(record.receipt()["complete_stream"], false);
    assert!(record.receipt()["readable_bytes"].is_null());
    session.mark_reader_ended(ReaderEnd::Eof);
    assert_eq!(record.receipt()["readers"]["pending"], 1);
    session.mark_reader_ended(ReaderEnd::DrainExpired);
    sealed(&record);
    assert_eq!(record.receipt()["complete_stream"], false);
    assert_eq!(record.receipt()["readers"]["drain_expired"], 1);
    assert_eq!(lock(&session.output).exit_code, Some(7));
}

#[test]
fn saturated_observation_count_never_yields_complete() {
    let (_dir, store, context) = fixture(16, 1);
    let (session, record) = attached(&store, &context, true);
    lock(&record.state).observed = u64::MAX;
    session.push_output(b"x");
    session.mark_reader_ended(ReaderEnd::Eof);
    sealed(&record);
    assert_eq!(record.receipt()["count_saturated"], true);
    assert_eq!(record.receipt()["complete_stream"], false);
}

#[test]
fn queue_full_is_irreversible_but_preview_and_source_drain_continue() {
    let (_dir, store, context) = fixture(1024 * 1024, 1);
    let (sender, _undrained) = mpsc::sync_channel(QUEUE_CHUNKS);
    let record = Arc::new(Record {
        generation: store.generation,
        id: 1,
        owner: owner(&context),
        cap: 1024 * 1024,
        state: Mutex::new(State {
            readers: ReaderCompletions::new(true),
            expected: 1,
            observed: 0,
            saturated: false,
            queued: 0,
            confirmed: 0,
            unconfirmed: 0,
            storage: StorageState::Capturing,
            failure: None,
            terminal_at: None,
            process_started: true,
            attached: true,
        }),
        sender,
        changed: Condvar::new(),
        file: Mutex::new(None),
        _reservation: store.quota.reserve().unwrap(),
    });
    let mut session = ExecSession::new(Box::new(NoopControl), true, None, owner(&context));
    session.capture = Some(CaptureTap {
        generation: store.generation,
        record: Arc::downgrade(&record),
    });
    for _ in 0..QUEUE_CHUNKS + 2 {
        session.push_output(&[255; CHUNK_BYTES]);
    }
    session.mark_reader_ended(ReaderEnd::Eof);
    let receipt = record.receipt();
    assert_eq!(receipt["storage_failure"], "queue_full");
    assert_eq!(receipt["complete_stream"], false);
    assert!(receipt["readable_bytes"].is_null());
    assert_eq!(receipt["observed_bytes"], (QUEUE_CHUNKS + 2) * CHUNK_BYTES);
    assert_eq!(
        lock(&session.output).buffer.len(),
        (QUEUE_CHUNKS + 2) * CHUNK_BYTES
    );
    assert_eq!(
        lock(&record.state).queued,
        (QUEUE_CHUNKS * CHUNK_BYTES) as u64
    );
}

#[test]
fn failed_append_receipt_preserves_uncertainty_and_exposes_no_readable_file() {
    use std::io::Write;
    struct FailAfterPrefix(usize);
    impl Write for FailAfterPrefix {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 += 1;
            if self.0 == 1 {
                Ok(bytes.len().min(2))
            } else {
                Err(io::Error::other("fault"))
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("must not publish failed storage")
        }
    }
    let (_dir, store, context) = fixture(16, 1);
    let (sender, _receiver) = mpsc::sync_channel(QUEUE_CHUNKS);
    let record = Record {
        generation: store.generation,
        id: 1,
        owner: owner(&context),
        cap: 16,
        state: Mutex::new(State {
            readers: ReaderCompletions::new(true),
            expected: 1,
            observed: 0,
            saturated: false,
            queued: 0,
            confirmed: 0,
            unconfirmed: 0,
            storage: StorageState::Capturing,
            failure: None,
            terminal_at: None,
            process_started: true,
            attached: true,
        }),
        sender,
        changed: Condvar::new(),
        file: Mutex::new(None),
        _reservation: store.quota.reserve().unwrap(),
    };
    record.observe(b"abcdef");
    let mut sink = FailAfterPrefix(0);
    record.append_result(6, writer::append(&mut sink, b"abcdef"));
    record.ended(ReaderEnd::Eof);
    let receipt = record.receipt();
    assert_eq!(sink.0, 2, "no retry after failing write");
    assert_eq!(receipt["confirmed_bytes"], 2);
    assert_eq!(receipt["unconfirmed_append_bytes"], 4);
    assert_eq!(receipt["storage_state"], "failed");
    assert_eq!(receipt["storage_failure"], "storage_io");
    assert_eq!(receipt["complete_stream"], false);
    assert!(receipt["readable_bytes"].is_null());
}

#[test]
fn failed_capture_admission_never_enters_session_prune_or_spawn_closure() {
    let (_dir, store, context) = fixture(16, 1);
    let _first = store.prepare(owner(&context), true).unwrap();
    let entered = Cell::new(false);
    let result = admit(Some(&store), owner(&context), true, |_, _| {
        entered.set(true);
        Ok(())
    });
    assert!(result.unwrap_err().to_string().contains("quota"));
    assert!(
        !entered.get(),
        "prune/spawn must be after capture admission"
    );
}

#[test]
fn pre_spawn_and_post_spawn_pre_attach_abort_do_not_invent_eof() {
    for started in [false, true] {
        let (_dir, store, context) = fixture(16, 1);
        let mut prepared = store.prepare(owner(&context), false).unwrap();
        let record = store.find(1).unwrap();
        if started {
            prepared.process_started();
        }
        drop(prepared); // models failure/cancellation before any reader exists
        wait_for(|| lock(&record.state).terminal_at.is_some());
        let state = lock(&record.state);
        assert_eq!(state.process_started, started);
        assert_eq!(
            state.failure,
            Some(if started {
                Failure::AbortBeforeReaders
            } else {
                Failure::AbortBeforeSpawn
            })
        );
        assert_eq!(state.readers.eof, 0);
        assert_eq!(state.readers.read_failed + state.readers.drain_expired, 0);
        drop(state);
        assert!(store.find(1).is_none());
        assert!(record.file.lock().unwrap().is_none());
        drop(record);
        wait_for(|| store.quota.used.load(Ordering::Acquire) == 0);
    }
}

#[test]
fn expired_map_entry_does_not_refund_an_outstanding_resource_lease() {
    let (_dir, store, context) = fixture(16, 1);
    let (session, record) = attached(&store, &context, true);
    session.push_output(b"abc");
    session.mark_reader_ended(ReaderEnd::Eof);
    sealed(&record);
    lock(&record.state).terminal_at = Some(Instant::now() - Duration::from_secs(301));
    assert!(store.find(1).is_none());
    assert_eq!(store.quota.used.load(Ordering::Acquire), 1);
    assert!(store.prepare(owner(&context), true).is_err());
    drop(record); // sole actual read/resource lease; session tap is weak
    wait_for(|| store.quota.used.load(Ordering::Acquire) == 0);
    assert!(store.prepare(owner(&context), true).is_ok());
    assert!(store.metadata(session.capture.as_ref()).is_none());
}

#[test]
fn binding_drop_stops_writer_without_future_lookup_or_reader_eof() {
    let (_dir, store, context) = fixture(16, 1);
    let (session, record) = attached(&store, &context, true);
    let weak = Arc::downgrade(&record);
    let quota = store.quota.clone();
    drop(record);
    drop(store);
    wait_for(|| weak.upgrade().is_none());
    assert_eq!(quota.used.load(Ordering::Acquire), 0);
    session.push_output(b"preview still works");
    assert_eq!(lock(&session.output).buffer, b"preview still works");
}

#[test]
fn sealed_capture_does_not_depend_on_exec_session_lifetime() {
    let (_dir, store, context) = fixture(16, 1);
    let (session, record) = attached(&store, &context, true);
    session.push_output(b"kept");
    session.mark_reader_ended(ReaderEnd::Eof);
    drop(session);
    sealed(&record);
    assert_eq!(
        read(&store, &context, json!({"handle": record.handle()})).unwrap()["envelope"]["text"],
        "kept"
    );
}

#[test]
fn actual_exec_code_seven_keeps_original_success_and_exit_semantics() {
    let (_dir, store, context) = fixture(128, 1);
    let _cleanup = OwnedSessionsCleanup(owner(&context));
    let call = json!({"id":"exec_capture_7", "name":"exec_command", "args": {
        "cmd":"printf capture-seven; exit 7", "with_escalated_permissions":true, "yield_time_ms":1000 }});
    let result = super::super::exec_command_impl(
        &call.to_string(),
        &serde_json::to_string(&context).unwrap(),
        Some(&store),
        &mut Host { cancelled: false },
    )
    .unwrap();
    let result: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["metadata"]["exit_code"], 7);
    assert!(result["metadata"]["session_id"].is_null());
    let handle = result["metadata"]["source_capture"]["handle"]
        .as_str()
        .unwrap();
    assert!(result["output"].as_str().unwrap().contains(handle));
    let record = store.find(1).unwrap();
    sealed(&record);
    assert_eq!(
        read(&store, &context, json!({"handle":handle})).unwrap()["envelope"]["text"],
        "capture-seven"
    );
}
