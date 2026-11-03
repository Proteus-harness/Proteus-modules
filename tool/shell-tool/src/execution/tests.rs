use super::*;

#[test]
fn complete_utf8_is_decoded_across_head_and_input_chunk_boundaries() {
    let input = format!("{}Жz", "x".repeat(HEAD_LIMIT_BYTES - 1));
    for chunk_size in [input.len(), HEAD_LIMIT_BYTES, 1, 17] {
        let mut buffer = BoundedBuffer::new();
        for chunk in input.as_bytes().chunks(chunk_size) {
            buffer.push(chunk);
        }
        assert!(!buffer.truncated());
        assert_eq!(buffer.to_text(), input);
    }
}

#[test]
fn truncated_utf8_omits_boundary_fragments_without_replacements() {
    let input = format!(
        "{}Ж{}Ж{}",
        "x".repeat(HEAD_LIMIT_BYTES - 1),
        "y".repeat(8),
        "z".repeat(TAIL_LIMIT_BYTES - 1)
    );
    for chunk_size in [input.len(), 1, 13] {
        let mut buffer = BoundedBuffer::new();
        for chunk in input.as_bytes().chunks(chunk_size) {
            buffer.push(chunk);
        }
        let text = buffer.to_text();
        assert!(buffer.truncated());
        assert!(!text.contains('�'), "{text}");
        assert!(text.starts_with(&"x".repeat(HEAD_LIMIT_BYTES - 1)));
        assert!(text.ends_with(&"z".repeat(TAIL_LIMIT_BYTES - 1)));
        assert!(text.contains("omitted 12 of"));
    }
}

#[cfg(unix)]
#[test]
fn successful_leader_with_inherited_pipes_reports_timeout_as_failure() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let child =
        crate::spawn_shell("sleep 10 & printf leader-finished; exit 0", cwd, cwd, None).unwrap();
    // Observe exit without reaping before the timeout clock begins, so shell
    // startup cannot turn this into the ordinary killed-leader scenario.
    let deadline = Instant::now() + Duration::from_secs(3);
    while crate::child_status::observe_exit(child.id())
        .unwrap()
        .is_none()
    {
        assert!(Instant::now() < deadline, "fixture leader did not exit");
        std::thread::sleep(Duration::from_millis(5));
    }
    let (output, timed_out) = wait_with_timeout(child, Duration::from_millis(100)).unwrap();
    assert!(output.status.success());
    assert!(timed_out);
    let result = crate::result::render_output(
        "timeout-drain".to_owned(),
        output,
        timed_out,
        100,
        serde_json::json!({}),
    );
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["ok"], false);
    assert_eq!(result["metadata"]["exit_code"], 0);
    assert_eq!(result["metadata"]["timed_out"], true);
    assert_eq!(result["error"], "process timed out after 100ms");
    assert_eq!(result["output"], "leader-finished");
}

#[cfg(target_os = "linux")]
fn detached_pipe_fixture() -> (
    tempfile::TempDir,
    std::process::Child,
    crate::process_test::Cleanup,
) {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let child = crate::spawn_shell(
        "setsid sh -c 'echo $$ > detached.pid; exec sleep 20' & printf leader-finished; printf error-tail >&2; exit 111",
        cwd,
        cwd,
        None,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !dir.path().join("detached.pid").exists() {
        assert!(Instant::now() < deadline, "detached fixture did not start");
        std::thread::sleep(Duration::from_millis(5));
    }
    let pid = loop {
        if let Ok(text) = std::fs::read_to_string(dir.path().join("detached.pid"))
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            break pid;
        }
        assert!(
            Instant::now() < deadline,
            "detached fixture PID is unavailable"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let cleanup = crate::process_test::Cleanup(vec![(
        pid,
        crate::process_test::state(pid)
            .expect("detached fixture alive")
            .0,
    )]);
    while crate::child_status::observe_exit(child.id())
        .unwrap()
        .is_none()
    {
        assert!(Instant::now() < deadline, "fixture leader did not exit");
        std::thread::sleep(Duration::from_millis(5));
    }
    (dir, child, cleanup)
}

#[cfg(target_os = "linux")]
#[test]
fn timeout_joins_readers_while_a_detached_descendant_keeps_pipes_open() {
    let (_dir, child, cleanup) = detached_pipe_fixture();
    let started = Instant::now();
    let (output, timed_out) = wait_with_timeout(child, Duration::from_millis(100)).unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(timed_out);
    assert_eq!(output.status.code(), Some(111));
    assert_eq!(output.stdout.to_text(), "leader-finished");
    assert_eq!(output.stderr.to_text(), "error-tail");
    assert!(crate::process_test::state(cleanup.0[0].0).is_some_and(|(_, running)| running));
}

#[cfg(target_os = "linux")]
#[test]
fn cancel_and_host_error_join_readers_with_detached_inherited_pipes() {
    for host_error in [false, true] {
        let (_dir, child, cleanup) = detached_pipe_fixture();
        let started = Instant::now();
        let error = wait_with_timeout_and_cancel(child, Duration::from_secs(5), &mut || {
            if started.elapsed() < Duration::from_millis(100) {
                Ok(false)
            } else if host_error {
                Err(std::io::Error::other("fixture cancellation channel failed"))
            } else {
                Ok(true)
            }
        })
        .err()
        .expect("interrupted invocation");
        assert!(started.elapsed() < Duration::from_secs(2));
        if host_error {
            assert_eq!(error.to_string(), "fixture cancellation channel failed");
        } else {
            assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        }
        assert!(crate::process_test::state(cleanup.0[0].0).is_some_and(|(_, running)| running));
    }
}
