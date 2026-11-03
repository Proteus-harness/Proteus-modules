//! Pinned Codex terminal branches exercised with real local processes.
use super::*;
#[cfg(target_os = "linux")]
use crate::process_test::{Cleanup as TestProcessCleanup, state as test_process_state};
use std::sync::atomic::AtomicUsize;

struct ObservedHost {
    checks: Arc<AtomicUsize>,
}

impl ToolModuleHost for ObservedHost {
    fn is_cancelled(&self) -> Result<bool, ProcessModuleError> {
        self.checks.fetch_add(1, AtomicOrdering::SeqCst);
        Ok(false)
    }
}

struct CountedControl(Arc<AtomicUsize>);

impl ProcessControl for CountedControl {
    fn write(&self, _bytes: &[u8]) -> std::io::Result<()> {
        Ok(())
    }

    fn interrupt(&self) -> std::io::Result<()> {
        Ok(())
    }

    fn kill(&self) {
        self.0.fetch_add(1, AtomicOrdering::SeqCst);
    }
}

#[test]
fn busy_session_cap_rejects_spawn_and_only_prunes_an_available_handle() {
    let root = tempfile::tempdir().unwrap();
    let context = invocation_context(root.path());
    let workspace = root.path().canonicalize().unwrap();
    let owner = ExecSessionOwner::from_context(&context, workspace.to_str().unwrap());
    let kills = Arc::new(AtomicUsize::new(0));
    let handles = (0..MAX_SESSIONS)
        .map(|_| {
            Arc::new(ExecSession::new(
                Box::new(CountedControl(kills.clone())),
                false,
                None,
                owner.clone(),
            ))
        })
        .collect::<Vec<_>>();
    let mut store: SessionMap = handles
        .iter()
        .enumerate()
        .map(|(id, handle)| (-(id as i64) - 1, handle.clone()))
        .collect();
    let mut guards = handles
        .iter()
        .map(|handle| lock(&handle.interaction))
        .collect::<Vec<_>>();
    let mut spawned = false;
    let error = register_session(&mut store, || {
        spawned = true;
        Ok(handles[0].clone())
    })
    .err()
    .expect("all busy handles must reject admission");
    assert!(error.to_string().contains("all sessions are busy"));
    assert!(!spawned, "rejected launch must have no command effects");
    assert_eq!(store.len(), MAX_SESSIONS);
    assert_eq!(kills.load(AtomicOrdering::SeqCst), 0);
    for (index, handle) in handles.iter().enumerate() {
        assert!(Arc::ptr_eq(&store[&(-(index as i64) - 1)], handle));
    }

    // Even if every other handle is older or exited, an in-flight interaction
    // cannot be evicted. Only this released handle is an eligible victim.
    for handle in &handles[..MAX_SESSIONS - 1] {
        handle.mark_exited(Some(0));
    }
    drop(guards.pop());
    let new_handle = Arc::new(ExecSession::new(
        Box::new(CountedControl(kills.clone())),
        false,
        None,
        owner,
    ));
    let (id, _) = register_session(&mut store, || Ok(new_handle.clone())).unwrap();
    assert_eq!(store.len(), MAX_SESSIONS);
    assert_eq!(kills.load(AtomicOrdering::SeqCst), 1);
    assert!(!store.contains_key(&(-(MAX_SESSIONS as i64))));
    assert!(Arc::ptr_eq(&store[&id], &new_handle));
}

#[test]
fn pipes_are_default_and_stdin_is_eof_while_tty_is_explicit() {
    let root = tempfile::tempdir().unwrap();
    for tty in [false, true] {
        let mut args = json!({"cmd": "if [ -t 0 ] && [ -t 1 ] && [ -t 2 ]; then printf tty; else cat; printf pipes-eof; fi"});
        if tty {
            args["tty"] = json!(true);
        }
        let result = exec_command(root.path(), args);
        assert_eq!(result["metadata"]["tty"], tty);
        assert_eq!(result["metadata"]["exit_code"], 0);
        assert!(result["output"].as_str().unwrap().ends_with(if tty {
            "tty"
        } else {
            "pipes-eof"
        }));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn exited_pipe_leader_cleans_up_its_group_and_preserves_nonzero_exit_and_tail() {
    let root = tempfile::tempdir().unwrap();
    let result = exec_command(
        root.path(),
        json!({"cmd": "sleep 20 & child=$!; echo $child > child.pid; cat /proc/$child/stat > child.stat; printf tail; exit 111", "yield_time_ms": 1000}),
    );
    let pid: u32 = std::fs::read_to_string(root.path().join("child.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // Record identity inside the still-owned group, before the leader exits.
    // By the time invoke returns the PID can already have been reaped/reused.
    let identity = crate::process_test::recorded_state(
        &std::fs::read_to_string(root.path().join("child.stat")).unwrap(),
    )
    .unwrap()
    .0;
    let _cleanup = TestProcessCleanup(vec![(pid, identity)]);
    assert_eq!(result["ok"], true);
    assert_eq!(result["metadata"]["exited"], true);
    assert_eq!(result["metadata"]["exit_code"], 111);
    assert_eq!(result["metadata"]["session_id"], Value::Null);
    assert!(result["output"].as_str().unwrap().contains("tail"));
    let deadline = Instant::now() + Duration::from_secs(2);
    while test_process_state(pid).is_some_and(|(_, running)| running) {
        assert!(
            Instant::now() < deadline,
            "descendant survived its pipe session"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(unix)]
#[test]
fn pipe_stdin_rejects_input_but_ctrl_c_interrupts_the_command() {
    let root = tempfile::tempdir().unwrap();
    let context = invocation_context(root.path());
    let result =
        exec_command_with_context(&context, json!({"cmd": "sleep 20", "yield_time_ms": 250}));
    let id = result["metadata"]["session_id"].as_i64().unwrap();
    for chars in ["hello\n", "\u{4}", "\u{3}extra"] {
        let error =
            write_stdin_result(&context, json!({"session_id": id, "chars": chars})).unwrap_err();
        assert_eq!(
            error.to_string(),
            "stdin is closed for this session; rerun exec_command with tty=true to keep stdin open"
        );
        assert!(lock(sessions()).contains_key(&id));
    }
    let result = write_stdin(
        &context,
        json!({"session_id": id, "chars": "\u{3}", "yield_time_ms": 5000}),
    );
    assert_eq!(result["ok"], true);
    assert_eq!(result["metadata"]["exit_code"], 130, "{result}");
    assert_eq!(result["metadata"]["session_id"], Value::Null);
    assert!(!lock(sessions()).contains_key(&id));
}

#[test]
fn long_empty_poll_returns_final_stdout_and_stderr_once() {
    let root = tempfile::tempdir().unwrap();
    let context = invocation_context(root.path());
    let first = exec_command_with_context(
        &context,
        json!({"cmd": "printf first; while [ ! -e finish ]; do sleep 0.01; done; printf last-out; printf last-err >&2; exit 7", "yield_time_ms": 250}),
    );
    let id = first["metadata"]["session_id"].as_i64().unwrap();
    let session = owned_session(id, &context).unwrap();
    let interaction = lock(&session.interaction);
    let checks = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
    let started = Instant::now();
    let results = std::thread::scope(|scope| {
        let calls = checks
            .iter()
            .map(|checks| {
                let context_json = serde_json::to_string(&context).unwrap();
                scope.spawn(move || {
                    let mut host = ObservedHost {
                        checks: checks.clone(),
                    };
                    let call = json!({"id": "poll", "name": "write_stdin", "args": {
                        "session_id": id, "yield_time_ms": 300000
                    }});
                    write_stdin_impl(&call.to_string(), &context_json, None, &mut host)
                })
            })
            .collect::<Vec<_>>();
        // Both calls have obtained their Arc and are trying to acquire the
        // interaction lock. Release the command and the lock together so the
        // queued call must recheck the store after terminal removal.
        let deadline = Instant::now() + Duration::from_secs(2);
        while checks
            .iter()
            .any(|checks| checks.load(AtomicOrdering::SeqCst) < 2)
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        let both_queued = checks
            .iter()
            .all(|checks| checks.load(AtomicOrdering::SeqCst) >= 2);
        std::fs::write(root.path().join("finish"), "").unwrap();
        drop(interaction);
        let results = calls
            .into_iter()
            .map(|call| call.join().unwrap())
            .collect::<Vec<_>>();
        assert!(both_queued, "polls did not reach the interaction lock");
        results
    });
    let mut terminal = None;
    let mut stale_calls = 0;
    for result in results {
        match result {
            Ok(result) => {
                assert!(terminal.is_none(), "duplicate terminal result: {result}");
                terminal = Some(serde_json::from_str::<Value>(&result).unwrap());
            }
            Err(error) => {
                assert!(
                    error.to_string().contains("unknown exec session"),
                    "{error}"
                );
                stale_calls += 1;
            }
        }
    }
    assert_eq!(stale_calls, 1);
    let last = terminal.expect("one poll collects the final output");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(last["metadata"]["yield_time_ms"], 300000);
    assert_eq!(last["metadata"]["exit_code"], 7);
    assert_eq!(last["ok"], true);
    let text = last["output"].as_str().unwrap();
    let combined = format!("{}{text}", first["output"].as_str().unwrap());
    assert_eq!(combined.matches("first").count(), 1, "{combined}");
    assert!(
        text.contains("last-out") && text.contains("last-err"),
        "{text}"
    );
    assert!(write_stdin_result(&context, json!({"session_id": id})).is_err());
}

#[test]
fn bounded_buffer_keeps_the_original_head_and_latest_tail() {
    let root = tempfile::tempdir().unwrap();
    let result = exec_command(
        root.path(),
        json!({
            "cmd": r"printf original-head; head -c 1200000 /dev/zero | tr '\000' x; printf latest-tail",
            "max_output_tokens": 350000
        }),
    );
    assert_eq!(result["metadata"]["exit_code"], 0);
    assert_eq!(result["metadata"]["output_bytes"], SESSION_BUFFER_LIMIT);
    assert_eq!(
        result["metadata"]["dropped_bytes"],
        1200024 - SESSION_BUFFER_LIMIT
    );
    let output = result["output"].as_str().unwrap();
    assert!(output.contains("original-head"));
    assert!(output.ends_with("latest-tail"));
    assert!(output.contains("omitted"));
    assert_eq!(result["metadata"]["truncated"], true);
    assert!(
        output.len() > 100000,
        "explicit token budget must not silently cap at 25000"
    );
    let zero = exec_command(
        root.path(),
        json!({"cmd": "printf hidden", "max_output_tokens": 0}),
    );
    assert!(!zero["output"].as_str().unwrap().contains("hidden"));
}

#[test]
fn poll_and_input_have_different_timeout_bounds() {
    assert_eq!(resolve_write_yield_time_ms(None, ""), 5000);
    assert_eq!(resolve_write_yield_time_ms(None, "input"), 250);
    let args = json!({"yield_time_ms": u64::MAX});
    assert_eq!(resolve_write_yield_time_ms(Some(&args), ""), 300000);
    assert_eq!(resolve_write_yield_time_ms(Some(&args), "input"), 30000);
    assert_eq!(
        resolve_yield_time_ms(Some(&args), DEFAULT_EXEC_YIELD_MS),
        30000
    );
}

#[cfg(target_os = "linux")]
#[test]
fn cancel_poll_kills_process_group_and_removes_session() {
    for tty in [false, true] {
        cancel_poll_kills_process_group(tty);
    }
}

#[cfg(target_os = "linux")]
fn cancel_poll_kills_process_group(tty: bool) {
    let root = tempfile::tempdir().unwrap();
    let context = invocation_context(root.path());
    let first = exec_command_with_context(
        &context,
        json!({
            "cmd": "trap '' HUP; echo $$ > leader.pid; sleep 20 & echo $! > child.pid; wait; printf survived > late-marker",
            "tty": tty, "yield_time_ms": 250
        }),
    );
    let id = first["metadata"]["session_id"].as_i64().unwrap();
    let pids = ["leader.pid", "child.pid"].map(|file| {
        std::fs::read_to_string(root.path().join(file))
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap()
    });
    let _cleanup = TestProcessCleanup(
        pids.iter()
            .map(|pid| (*pid, test_process_state(*pid).unwrap().0))
            .collect(),
    );
    let cancelled = Arc::new(AtomicBool::new(false));
    let trigger = cancelled.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        trigger.store(true, AtomicOrdering::SeqCst);
    });
    let call = json!({"id": "cancel_poll", "name": "write_stdin", "args": {"session_id": id, "yield_time_ms": 300000}});
    let error = with_host(cancelled, |host| {
        write_stdin_impl(
            &call.to_string(),
            &serde_json::to_string(&context).unwrap(),
            None,
            host,
        )
    })
    .unwrap_err();
    canceller.join().unwrap();
    assert!(error.to_string().contains("canceled"));
    assert!(!lock(sessions()).contains_key(&id));
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let running = pids
            .iter()
            .any(|pid| test_process_state(*pid).is_some_and(|(_, running)| running));
        if !running {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "HUP-resistant command or descendant survived cancellation (tty={tty})"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!root.path().join("late-marker").exists());
}
