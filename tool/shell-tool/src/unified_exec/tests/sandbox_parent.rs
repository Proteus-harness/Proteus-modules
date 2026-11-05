//! bwrap parent-death follows the creating OS thread, not the module process.
use super::*;

fn launch_from_finished_invocation(
    context: &ToolModuleInvocationContext,
    args: Value,
) -> Option<Value> {
    let policy = SandboxPolicy::detect(context.cwd.to_str().unwrap());
    if policy.select(false, false).is_err() {
        eprintln!("bwrap namespaces unavailable; sandbox parent regression not exercised");
        return None;
    }
    let context = context.clone();
    Some(
        std::thread::spawn(move || {
            let call = json!({"id":"sandbox-parent", "name":"exec_command", "args":args});
            let result = with_host(Arc::new(AtomicBool::new(false)), |host| {
                exec_command_impl(
                    &call.to_string(),
                    &serde_json::to_string(&context).unwrap(),
                    None,
                    host,
                )
            })
            .unwrap();
            serde_json::from_str(&result).unwrap()
        })
        .join()
        .unwrap(),
    )
}

#[test]
fn sandbox_session_survives_invocation_thread_exit_with_pty_input() {
    let root = tempfile::tempdir().unwrap();
    let context = invocation_context(root.path());
    let Some(started) = launch_from_finished_invocation(
        &context,
        json!({"cmd":"printf READY; read answer; printf 'ACCEPTED:%s' \"$answer\"", "tty":true, "yield_time_ms":250}),
    ) else {
        return;
    };
    let id = started["metadata"]["session_id"].as_i64().unwrap();
    let session = owned_session(id, &context).unwrap();
    let finished = write_stdin_result(
        &context,
        json!({"session_id":id,"chars":"synthetic-input\n","yield_time_ms":5000}),
    );
    // Cleanup even when checking the old, broken implementation.
    terminate_session(id, &session);
    let finished: Value = serde_json::from_str(&finished.unwrap()).unwrap();
    assert_eq!(finished["metadata"]["exit_code"], 0, "{finished}");
    assert!(
        finished["output"]
            .as_str()
            .unwrap()
            .contains("ACCEPTED:synthetic-input"),
        "{finished}"
    );
}

#[test]
fn sandbox_session_survives_invocation_thread_exit_with_pipes() {
    let root = tempfile::tempdir().unwrap();
    let context = invocation_context(root.path());
    let Some(started) = launch_from_finished_invocation(
        &context,
        json!({"cmd":"printf READY; while [ ! -f finish ]; do sleep 0.01; done; printf SURVIVED", "yield_time_ms":250}),
    ) else {
        return;
    };
    let id = started["metadata"]["session_id"].as_i64().unwrap();
    let session = owned_session(id, &context).unwrap();
    std::fs::write(root.path().join("finish"), "").unwrap();
    let finished = write_stdin_result(&context, json!({"session_id":id,"yield_time_ms":5000}));
    terminate_session(id, &session);
    let finished: Value = serde_json::from_str(&finished.unwrap()).unwrap();
    assert_eq!(finished["metadata"]["exit_code"], 0, "{finished}");
    assert!(
        finished["output"].as_str().unwrap().contains("SURVIVED"),
        "{finished}"
    );
}

#[test]
fn pty_signal_exit_preserves_the_signal_instead_of_reporting_one() {
    let root = tempfile::tempdir().unwrap();
    let result = exec_command(root.path(), json!({"cmd":"kill -TERM $$", "tty":true}));
    assert_eq!(
        result["metadata"]["exit_code"],
        128 + libc::SIGTERM,
        "{result}"
    );
}

#[test]
fn cancellation_still_stops_sandbox_sessions_after_invocation_thread_exit() {
    for tty in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let context = invocation_context(root.path());
        let Some(started) = launch_from_finished_invocation(
            &context,
            json!({"cmd":"printf READY; sleep 20", "tty":tty, "yield_time_ms":250}),
        ) else {
            return;
        };
        let id = started["metadata"]["session_id"].as_i64().unwrap();
        let session = owned_session(id, &context).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let trigger = cancelled.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            trigger.store(true, AtomicOrdering::SeqCst);
        });
        let call = json!({"id":"cancel-sandbox", "name":"write_stdin", "args":{
            "session_id":id,"yield_time_ms":300000
        }});
        let result = with_host(cancelled, |host| {
            write_stdin_impl(
                &call.to_string(),
                &serde_json::to_string(&context).unwrap(),
                None,
                host,
            )
        });
        canceller.join().unwrap();
        assert!(result.unwrap_err().to_string().contains("canceled"));
        assert!(!lock(sessions()).contains_key(&id));
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let output = lock(&session.output);
            if output.exited && output.readers.is_closed() {
                break;
            }
            drop(output);
            assert!(
                Instant::now() < deadline,
                "cancelled sandbox still alive (tty={tty})"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
