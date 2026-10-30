use super::super::super::{exec_command_impl, write_stdin_impl};
use super::*;

#[test]
fn tty_repeated_waits_keep_one_capture_and_foreign_binding_does_not_disclose_it() {
    let (_dir, store, context) = fixture(1024, 2);
    let _cleanup = OwnedSessionsCleanup(owner(&context));
    let call = json!({"id":"tty_start","name":"exec_command","args":{
        "cmd":"read -r first; printf 'one:%s\\n' \"$first\"; read -r second; printf 'two:%s\\n' \"$second\"",
        "tty":true,"yield_time_ms":250,"with_escalated_permissions":true}});
    let context_json = serde_json::to_string(&context).unwrap();
    let mut host = Host { cancelled: false };
    let result =
        exec_command_impl(&call.to_string(), &context_json, Some(&store), &mut host).unwrap();
    let result: Value = serde_json::from_str(&result).unwrap();
    let id = result["metadata"]["session_id"].as_i64().unwrap();
    let handle = result["metadata"]["source_capture"]["handle"]
        .as_str()
        .unwrap()
        .to_owned();
    let first = json!({"id":"tty_one","name":"write_stdin","args":{"session_id":id,"chars":"alpha\n","yield_time_ms":250}});
    let first: Value = serde_json::from_str(
        &write_stdin_impl(&first.to_string(), &context_json, Some(&store), &mut host).unwrap(),
    )
    .unwrap();
    assert_eq!(first["metadata"]["session_id"], id);
    assert_eq!(first["metadata"]["source_capture"]["handle"], handle);
    let other = CaptureStore::new(CaptureConfig {
        directory: context.cwd.clone(),
        cap: 1024,
        slots: 2,
        retention: Duration::from_secs(300),
    });
    let second = json!({"id":"tty_two","name":"write_stdin","args":{"session_id":id,"chars":"beta\n","yield_time_ms":1000}});
    let second: Value = serde_json::from_str(
        &write_stdin_impl(&second.to_string(), &context_json, Some(&other), &mut host).unwrap(),
    )
    .unwrap();
    assert!(second["metadata"]["session_id"].is_null());
    assert!(second["metadata"].get("source_capture").is_none());
    assert_eq!(
        other.quota.used.load(Ordering::Acquire),
        0,
        "write must not replace capture"
    );
    let record = store.find(1).unwrap();
    sealed(&record);
    let bytes = read(&store, &context, json!({"handle":handle})).unwrap();
    let text = bytes["envelope"]["text"].as_str().unwrap();
    assert!(text.contains("one:alpha"), "{text}");
    assert!(text.contains("two:beta"), "{text}");
    assert_eq!(
        bytes["metadata"]["source_capture"]["readers"]["expected"],
        1
    );
}

#[test]
fn cancellation_after_spawn_preserves_started_fact_and_does_not_rerun_command() {
    use std::sync::atomic::AtomicUsize;
    struct CancelAfterLaunch(AtomicUsize);
    impl ToolModuleHost for CancelAfterLaunch {
        fn is_cancelled(&self) -> Result<bool, ProcessModuleError> {
            // Entry/execute/admission/interaction checks precede wait checks.
            Ok(self.0.fetch_add(1, Ordering::AcqRel) >= 5)
        }
    }
    let (_dir, store, context) = fixture(100, 1);
    let _cleanup = OwnedSessionsCleanup(owner(&context));
    let call = json!({"id":"cancel_capture","name":"exec_command","args":{
        "cmd":"printf started; sleep 5","with_escalated_permissions":true,"yield_time_ms":1000}});
    let error = exec_command_impl(
        &call.to_string(),
        &serde_json::to_string(&context).unwrap(),
        Some(&store),
        &mut CancelAfterLaunch(AtomicUsize::new(0)),
    )
    .unwrap_err();
    assert!(error.to_string().contains("canceled"));
    let record = store.find(1).expect("capture admitted before launch");
    assert!(lock(&record.state).process_started);
    assert!(lock(&record.state).attached);
    wait_for(|| lock(&record.state).terminal_at.is_some());
    assert_ne!(lock(&record.state).failure, Some(Failure::AbortBeforeSpawn));
    // EOF after cancellation is a source fact, not a claim of command success.
    // No assertion turns cancellation request into a confirmed exit code.
    assert_eq!(store.quota.used.load(Ordering::Acquire), 1);
}
