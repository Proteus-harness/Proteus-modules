use super::*;
use proteus_contracts::{
    contracts::ExecutionAttribution,
    domain::{new_execution_id, new_session_id, new_thread_id, new_turn_id},
    process_module::ToolModuleInvocationContext,
};

#[test]
fn startup_receiver_loss_terminates_and_reaps_both_process_backends() {
    for tty in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let context = ToolModuleInvocationContext {
            skills: Default::default(),
            cwd: root.path().into(),
            attribution: ExecutionAttribution::for_turn(
                new_execution_id(),
                new_session_id(),
                new_thread_id(),
                new_turn_id(),
            ),
            config: serde_json::json!({}),
        };
        let owner = ExecSessionOwner::from_context(&context, root.path().to_str().unwrap());
        let launch = Launch {
            program: EXEC_SHELL.into(),
            args: vec!["-lc".into(), "sleep 20".into()],
            workdir: root.path().to_string_lossy().into_owned(),
            sandbox: None,
            capture: None,
            terminal: Arc::new(super::super::terminal::TerminalRecord::new(
                owner.clone(),
                tty,
                "test",
            )),
            owner,
        };
        let (ready, receiver) = mpsc::sync_channel(0);
        let (finished, completion) = mpsc::sync_channel(1);
        drop(receiver);
        let worker = std::thread::spawn(move || {
            let result = if tty {
                run_pty(launch, &ready)
            } else {
                run_pipes(launch, &ready)
            };
            finished.send(result).unwrap();
        });
        completion
            .recv_timeout(Duration::from_secs(3))
            .expect("abandoned startup must kill, drain and reap")
            .unwrap();
        worker.join().unwrap();
    }
}
