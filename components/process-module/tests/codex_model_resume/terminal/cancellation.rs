use super::*;
use proteus_contracts::contracts::CancellationToken;
use proteus_core::core::ToolCallRecordPhase;

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_long_poll_stops_process_and_preserves_completed_launch_in_cold_history() {
    for tty in [false, true] {
        cancel_long_poll_and_check_cold_history(tty).await;
    }
}

#[cfg(target_os = "linux")]
async fn cancel_long_poll_and_check_cold_history(tty: bool) {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = terminal_config(&format!("http://{}", listener.local_addr().unwrap()));
    let config_path = root.path().join("config.json");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let runtime = std::sync::Arc::new(
        AgentRuntime::builder(config.clone(), root.path().to_owned())
            .with_config_path(Some(&config_path))
            .build_async()
            .await
            .unwrap(),
    );
    let session = runtime.session_dir().unwrap().to_owned();
    let cancel = CancellationToken::new();
    let run = tokio::spawn({
        let runtime = runtime.clone();
        let cancel = cancel.clone();
        async move {
            runtime
                .run_with_cancellation("Check terminal.".into(), cancel)
                .await
        }
    });
    let (mut socket, _) = accept(&listener).await;
    respond(
        &mut socket,
        json!([call(
            "call_exec",
            "exec_command",
            json!({
                "cmd": "trap '' HUP; sleep 20 & echo $! > child.pid; wait", "tty": tty,
                "yield_time_ms": 250, "with_escalated_permissions": true
            })
        )]),
    )
    .await;
    let (mut socket, request) = accept(&listener).await;
    let id = session_id(&request);
    wait_for_file(&root.path().join("child.pid")).await;
    respond(
        &mut socket,
        json!([call(
            "call_poll",
            "write_stdin",
            json!({"session_id": id, "yield_time_ms": 300000})
        )]),
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let projection = SessionStore::open(session.clone())
            .unwrap()
            .load_projection()
            .unwrap();
        if projection.records.iter().any(|record| matches!(&record.entry,
            JournalEntry::ToolCallRecorded(tool) if tool.call.id == "call_poll" && matches!(tool.phase, ToolCallRecordPhase::Requested { .. }))) { break; }
        assert!(
            tokio::time::Instant::now() < deadline,
            "poll never dispatched"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The process tool must be inside its wait, not merely queued for dispatch.
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    let pid = std::fs::read_to_string(root.path().join("child.pid")).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid.trim()));
        if !stat.is_ok_and(|stat| !stat.split_once(") ").unwrap().1.starts_with(['Z', 'X'])) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "canceled process descendant survived"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let projection = SessionStore::open(session.clone())
        .unwrap()
        .load_projection()
        .unwrap();
    assert_eq!(projection.history, runtime.history().await.unwrap());
    assert!(
        history_results(&projection.history)
            .iter()
            .any(|result| result.call_id == "call_exec" && result.ok)
    );
    assert!(
        projection
            .records
            .iter()
            .any(|record| matches!(&record.entry,
        JournalEntry::TurnSettled(settled) if settled.status == TurnSettlementStatus::Canceled))
    );
    drop(runtime);
    let cold = proteus_core::app_server::AgentAppServer::launch_resumed(
        config,
        root.path().to_owned(),
        Some(&config_path),
        session,
    )
    .await
    .unwrap();
    let transcript = cold.transcript().await.unwrap();
    assert!(
        transcript
            .iter()
            .filter_map(|item| item.tool.as_ref())
            .any(|tool| tool.call_id == "call_exec" && tool.status == "done")
    );
    assert!(
        !transcript
            .iter()
            .filter_map(|item| item.tool.as_ref())
            .any(|tool| tool.call_id == "call_poll" && tool.status == "done")
    );
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_one_shot_shell_kills_descendants_and_records_canceled_turn() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = terminal_config(&format!("http://{}", listener.local_addr().unwrap()));
    config.tools.enabled = vec!["shell".into()];
    let config_path = root.path().join("config.json");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let runtime = std::sync::Arc::new(
        AgentRuntime::builder(config.clone(), root.path().to_owned())
            .with_config_path(Some(&config_path))
            .build_async()
            .await
            .unwrap(),
    );
    let session = runtime.session_dir().unwrap().to_owned();
    let cancel = CancellationToken::new();
    let run = tokio::spawn({
        let runtime = runtime.clone();
        let cancel = cancel.clone();
        async move {
            runtime
                .run_with_cancellation("Run shell.".into(), cancel)
                .await
        }
    });
    let (mut socket, request) = accept(&listener).await;
    assert!(
        request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "shell")
    );
    respond(
        &mut socket,
        json!([call(
            "call_shell",
            "shell",
            json!({
                // One leaf process owns readiness, waiting and the late marker.
                // A killed parent cannot suppress its marker; HUP is ignored.
                // Its one-second lifetime also bounds cleanup on test failure.
                "command": r#"python3 -c 'import pathlib, signal, time
signal.signal(signal.SIGHUP, signal.SIG_IGN)
pathlib.Path("child.stat").write_text(pathlib.Path("/proc/self/stat").read_text())
pathlib.Path("started").write_text("ready")
time.sleep(1)
pathlib.Path("late-marker").write_text("late")
' & wait"#,
                "with_escalated_permissions": true
            })
        )]),
    )
    .await;
    wait_for_file(&root.path().join("started")).await;
    let child = process_identity(&std::fs::read_to_string(root.path().join("child.stat")).unwrap());
    assert!(!matches!(child.2, 'Z' | 'X'));
    let child_path = format!("/proc/{}/stat", child.0);
    let live = process_identity(&std::fs::read_to_string(&child_path).unwrap());
    assert_eq!((live.0, live.1), (child.0, child.1));
    assert!(!matches!(live.2, 'Z' | 'X'));
    cancel.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        match std::fs::read_to_string(&child_path) {
            Ok(stat) => {
                let current = process_identity(&stat);
                if current.1 != child.1 || matches!(current.2, 'Z' | 'X') {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => panic!("cannot inspect descendant: {error}"),
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "canceled descendant with matching start time is still alive"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !root.path().join("late-marker").exists(),
        "descendant survived cancellation and wrote its own late marker"
    );
    let projection = SessionStore::open(session.clone())
        .unwrap()
        .load_projection()
        .unwrap();
    assert!(projection.records.iter().any(|record| matches!(
        &record.entry,
        JournalEntry::TurnSettled(settled) if settled.status == TurnSettlementStatus::Canceled
    )));
    assert!(
        !history_results(&projection.history)
            .iter()
            .any(|result| result.call_id == "call_shell")
    );
    drop(runtime);
    let cold = proteus_core::app_server::AgentAppServer::launch_resumed(
        config,
        root.path().to_owned(),
        Some(&config_path),
        session,
    )
    .await
    .unwrap();
    assert!(
        !cold
            .transcript()
            .await
            .unwrap()
            .iter()
            .filter_map(|item| item.tool.as_ref())
            .any(|tool| tool.call_id == "call_shell" && tool.status == "done")
    );
}

#[cfg(target_os = "linux")]
fn process_identity(stat: &str) -> (u32, u64, char) {
    let (pid, _) = stat.split_once(" (").unwrap();
    let (_, fields) = stat.rsplit_once(") ").unwrap();
    let mut fields = fields.split_whitespace();
    let state = fields.next().unwrap().chars().next().unwrap();
    // starttime is field 22; the consumed state is field 3.
    let start_ticks = fields.nth(18).unwrap().parse().unwrap();
    (pid.parse().unwrap(), start_ticks, state)
}
