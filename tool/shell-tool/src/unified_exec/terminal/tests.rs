use super::*;
use proteus_contracts::{
    contracts::ExecutionAttribution,
    domain::{new_session_id, new_thread_id, new_turn_id},
    process_module::{ProcessModuleError, ToolModuleHost},
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc,
};

fn invocation_context(cwd: &std::path::Path) -> ToolModuleInvocationContext {
    ToolModuleInvocationContext {
        cwd: cwd.into(),
        attribution: ExecutionAttribution::for_turn(
            new_execution_id(),
            new_session_id(),
            new_thread_id(),
            new_turn_id(),
        ),
        skills: Default::default(),
        config: json!({}),
    }
}
#[test]
fn forgotten_terminal_returns_typed_unavailable_for_read_and_input() {
    let dir = tempfile::tempdir().unwrap();
    let context = invocation_context(dir.path());
    let handle = new_execution_id().to_string();
    for action in [
        ToolTerminalAction::Read {
            cursor: None,
            max_bytes: 1,
            wait_ms: 0,
        },
        ToolTerminalAction::Input {
            data_base64: STANDARD.encode(b"synthetic-never-delivered"),
        },
    ] {
        assert_eq!(
            control(
                ToolTerminalRequest {
                    handle: handle.clone(),
                    action
                },
                context.clone()
            )
            .unwrap(),
            ToolTerminalResponse::Unavailable,
        );
    }
}
fn record(context: &ToolModuleInvocationContext, tty: bool) -> TerminalRecord {
    TerminalRecord::new(
        ExecSessionOwner::from_context(context, context.cwd.to_str().unwrap()),
        tty,
        "fixture",
    )
}
#[test]
fn cursor_ring_preserves_binary_bytes_and_reports_dropped_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let context = invocation_context(dir.path());
    let record = record(&context, true);
    record.append(&vec![b'a'; BYTE_LIMIT]);
    record.append(&[0, 255, 27]);
    let snapshot = record.read(Some(0), 10, 0).unwrap();
    assert!(snapshot.gap);
    assert_eq!(snapshot.start_cursor, 3);
    assert_eq!(snapshot.next_cursor, 13);
    assert_eq!(snapshot.end_cursor, BYTE_LIMIT as u64 + 3);
    let tail = record.read(Some(BYTE_LIMIT as u64), u32::MAX, 0).unwrap();
    assert_eq!(STANDARD.decode(tail.data_base64).unwrap(), [0, 255, 27]);
    assert_eq!(
        record.read(Some(3), u32::MAX, 0).unwrap().next_cursor,
        3 + READ_LIMIT as u64
    );
    assert!(record.read(Some(snapshot.end_cursor + 1), 10, 0).is_err());
}
#[test]
fn record_retention_is_bounded_and_expiration_does_not_remove_active_records() {
    let dir = tempfile::tempdir().unwrap();
    let context = invocation_context(dir.path());
    let active = Arc::new(record(&context, true));
    let finished = Arc::new(record(&context, false));
    finished.lifecycle(true, Some(0));
    let mut records = Records::from([
        (active.handle.clone(), active.clone()),
        (finished.handle.clone(), finished.clone()),
    ]);
    prune(
        &mut records,
        Instant::now() + RETENTION + Duration::from_secs(1),
    );
    assert!(records.contains_key(&active.handle));
    assert!(!records.contains_key(&finished.handle));
    assert_eq!(Arc::strong_count(&active), 2);
    assert!(lock(&active.session).upgrade().is_none());
}
// Ошибка проверки не должна оставлять fixture, ожидающую stdin до janitor.
struct Cleanup(ExecSessionOwner);
impl Drop for Cleanup {
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
struct Host {
    descriptor: mpsc::Sender<ToolTerminalDescriptor>,
}
impl ToolModuleHost for Host {
    fn supports_terminal_publish(&self) -> bool {
        true
    }
    fn is_cancelled(&self) -> Result<bool, ProcessModuleError> {
        Ok(false)
    }
    fn publish_terminal(
        &self,
        descriptor: ToolTerminalDescriptor,
    ) -> Result<(), ProcessModuleError> {
        if descriptor.title.len() > 512 {
            return Err(ProcessModuleError::new("terminal title too long"));
        }
        self.descriptor.send(descriptor).unwrap();
        Ok(())
    }
}
fn action(
    handle: &str,
    action: ToolTerminalAction,
    context: &ToolModuleInvocationContext,
) -> Result<ToolTerminalResponse> {
    control(
        ToolTerminalRequest {
            handle: handle.into(),
            action,
        },
        context.clone(),
    )
}
fn read(
    handle: &str,
    cursor: Option<u64>,
    context: &ToolModuleInvocationContext,
) -> ToolTerminalSnapshot {
    match action(
        handle,
        ToolTerminalAction::Read {
            cursor,
            max_bytes: READ_LIMIT as u32,
            wait_ms: 1000,
        },
        context,
    )
    .unwrap()
    {
        ToolTerminalResponse::Read(snapshot) => snapshot,
        _ => panic!("read response"),
    }
}
#[test]
#[cfg(unix)]
fn ui_can_supply_hidden_input_before_model_yield_without_stealing_output() {
    let dir = tempfile::tempdir().unwrap();
    // No sudo, package manager, password store or real credential is involved.
    std::fs::write(dir.path().join("prompt.py"), "import os,sys,termios\na=termios.tcgetattr(0)\nb=a.copy(); b[3] &= ~termios.ECHO\ntermios.tcsetattr(0,termios.TCSANOW,b)\nprint('READY_FOR_INPUT',flush=True)\nsecret=sys.stdin.readline()\ntermios.tcsetattr(0,termios.TCSANOW,a)\nprint('INPUT_ACCEPTED',flush=True)\nprint('GRID=%sx%s' % os.get_terminal_size(0),flush=True)\n").unwrap();
    let context = invocation_context(dir.path());
    let _cleanup = Cleanup(ExecSessionOwner::from_context(
        &context,
        context.cwd.to_str().unwrap(),
    ));
    let (tx, rx) = mpsc::channel();
    let thread_context = context.clone();
    let capture = super::super::capture::CaptureStore::new(crate::capture_config::CaptureConfig {
        directory: dir.path().into(),
        cap: 4096,
        slots: 1,
        retention: Duration::from_secs(300),
    });
    let thread_capture = capture.clone();
    let finished = Arc::new(AtomicBool::new(false));
    let thread_finished = finished.clone();
    let invoke = std::thread::spawn(move || {
        let call = json!({"id":"terminal_fixture","name":"exec_command","args":{"cmd":"python3 prompt.py","tty":true,"with_escalated_permissions":true,"yield_time_ms":30000}});
        let result = super::super::exec_command_impl(
            &call.to_string(),
            &serde_json::to_string(&thread_context).unwrap(),
            Some(&thread_capture),
            &mut Host { descriptor: tx },
        );
        thread_finished.store(true, Ordering::SeqCst);
        result.unwrap()
    });
    let descriptor = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut bytes = Vec::new();
    let mut cursor = Some(0);
    let prompt_deadline = Instant::now() + Duration::from_secs(5);
    while !String::from_utf8_lossy(&bytes).contains("READY_FOR_INPUT") {
        let snapshot = read(&descriptor.handle, cursor, &context);
        cursor = Some(snapshot.next_cursor);
        bytes.extend(STANDARD.decode(snapshot.data_base64).unwrap());
        assert!(!snapshot.finished, "prompt process exited before input");
        assert!(Instant::now() < prompt_deadline, "prompt did not appear");
    }
    assert!(!finished.load(Ordering::SeqCst));
    let foreign = invocation_context(dir.path());
    assert!(
        action(
            &descriptor.handle,
            ToolTerminalAction::Input {
                data_base64: STANDARD.encode(b"foreign\n")
            },
            &foreign
        )
        .is_err()
    );
    action(
        &descriptor.handle,
        ToolTerminalAction::Resize {
            columns: 101,
            rows: 37,
        },
        &context,
    )
    .unwrap();
    let first = read(&descriptor.handle, Some(0), &context);
    let second = read(&descriptor.handle, Some(0), &context);
    assert_eq!(first.data_base64, second.data_base64);
    action(
        &descriptor.handle,
        ToolTerminalAction::Input {
            data_base64: STANDARD.encode(b"synthetic-hidden-token\n"),
        },
        &context,
    )
    .unwrap();
    let result = invoke.join().unwrap();
    assert!(result.contains("READY_FOR_INPUT"), "{result}");
    assert!(result.contains("INPUT_ACCEPTED"), "{result}");
    assert!(result.contains("GRID=101x37"), "{result}");
    assert!(!result.contains("synthetic-hidden-token"));
    let result_value: serde_json::Value = serde_json::from_str(&result).unwrap();
    let handle = result_value["metadata"]["source_capture"]["handle"]
        .as_str()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        use proteus_contracts::process_module::ToolModule;
        let (tx, _rx) = mpsc::channel();
        let output = super::super::capture::ReadCaptureTool(capture.clone())
            .invoke_json(
                json!({"id":"read_fixture","name":"read_exec_capture","args":{"handle":handle}})
                    .to_string(),
                serde_json::to_string(&context).unwrap(),
                &mut Host { descriptor: tx },
            )
            .unwrap();
        assert!(!output.contains("synthetic-hidden-token"));
        if output.contains("INPUT_ACCEPTED") {
            break;
        }
        assert!(Instant::now() < deadline, "capture did not seal: {output}");
        std::thread::sleep(Duration::from_millis(10));
    }

    // The model removed its finished numeric session; terminal output is retained.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = read(&descriptor.handle, cursor, &context);
        cursor = Some(snapshot.next_cursor);
        bytes.extend(STANDARD.decode(snapshot.data_base64).unwrap());
        if snapshot.finished && snapshot.next_cursor == snapshot.end_cursor {
            break;
        }
        assert!(Instant::now() < deadline);
    }
    let raw = String::from_utf8_lossy(&bytes);
    assert!(raw.contains("INPUT_ACCEPTED"));
    assert!(!raw.contains("synthetic-hidden-token"));
    assert!(
        action(
            &descriptor.handle,
            ToolTerminalAction::Input {
                data_base64: STANDARD.encode(b"late\n")
            },
            &context
        )
        .is_err()
    );
}

#[test]
fn admission_never_evicts_active_records_and_prefers_finished_records() {
    let dir = tempfile::tempdir().unwrap();
    let context = invocation_context(dir.path());
    let mut records = Records::new();
    for _ in 0..RECORD_LIMIT {
        admit_record(&mut records, &Arc::new(record(&context, true))).unwrap();
    }
    let new = Arc::new(record(&context, true));
    assert!(admit_record(&mut records, &new).is_err());
    assert_eq!(records.len(), RECORD_LIMIT);
    let finished = records.values().next().unwrap().clone();
    finished.lifecycle(true, Some(0));
    admit_record(&mut records, &new).unwrap();
    assert_eq!(records.len(), RECORD_LIMIT);
    assert!(!records.contains_key(&finished.handle));
    assert!(records.contains_key(&new.handle));
}

#[test]
fn finished_requires_exit_and_reader_completion_and_does_not_consume_model_bytes() {
    struct NoControl;
    impl super::super::session::ProcessControl for NoControl {
        fn write(&self, _: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn interrupt(&self) -> std::io::Result<()> {
            Ok(())
        }
        fn kill(&self) {}
    }
    let dir = tempfile::tempdir().unwrap();
    let context = invocation_context(dir.path());
    let session = ExecSession::new(
        Box::new(NoControl),
        false,
        None,
        ExecSessionOwner::from_context(&context, context.cwd.to_str().unwrap()),
    );
    session.push_output(b"head");
    session.mark_exited(Some(0));
    session.mark_reader_ended(super::super::reader::ReaderEnd::Eof);
    assert!(!session.terminal.read(None, 100, 0).unwrap().finished);
    session.push_output(b"tail");
    session.mark_reader_ended(super::super::reader::ReaderEnd::Eof);
    let snapshot = session.terminal.read(None, 100, 0).unwrap();
    assert!(snapshot.finished);
    assert_eq!(STANDARD.decode(snapshot.data_base64).unwrap(), b"headtail");
    assert_eq!(lock(&session.output).buffer, b"headtail");
}

#[test]
fn unicode_title_obeys_core_byte_cap_without_failing_valid_exec() {
    let emoji = "😀".repeat(160);
    let title = bounded_title(&emoji);
    assert_eq!(title.len(), 512);
    assert_eq!(title.chars().count(), 128);
    assert_eq!(bounded_title(&"a".repeat(200)).chars().count(), 160);

    let dir = tempfile::tempdir().unwrap();
    let context = invocation_context(dir.path());
    let _cleanup = Cleanup(ExecSessionOwner::from_context(
        &context,
        context.cwd.to_str().unwrap(),
    ));
    let (tx, rx) = mpsc::channel();
    let result = super::super::exec_command_impl(
        &json!({"id":"unicode_title","name":"exec_command","args":{
            "cmd":format!("printf unicode-title-ok # {emoji}"),
            "with_escalated_permissions":true,"yield_time_ms":1000
        }})
        .to_string(),
        &serde_json::to_string(&context).unwrap(),
        None,
        &mut Host { descriptor: tx },
    )
    .unwrap();
    let descriptor = rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(descriptor.title.len() <= 512);
    assert!(descriptor.title.chars().count() <= 160);
    assert!(result.contains("unicode-title-ok"), "{result}");
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["metadata"]["exit_code"], 0);
}
