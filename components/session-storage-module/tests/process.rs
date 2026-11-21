//! Real SDK process boundary: transport loss cannot duplicate a durable record.
use proteus_contracts::{
    contracts::*,
    domain::{new_record_id, new_session_id, new_thread_id},
    model_standard::{CanonicalMessage, MessageRole},
    storage::journal::*,
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

struct Client {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    sequence: u64,
    module: String,
}
impl Client {
    fn new(module: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_proteus-session-storage-module"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut this = Self {
            child,
            input,
            output,
            sequence: 0,
            module: module.into(),
        };
        let init = json!({"jsonrpc":"2.0","id":"h:1:0","method":"initialize","params":{
            "protocol_version":"v3","component_id":"storage-test","exports":[{"slot":"session_storage","module_id":module,
            "contract_version":"v2","composition":"select_one","module_config":{},"host_features":[]}]}});
        this.send_frame(&init);
        let response = this.receive();
        assert!(response.get("result").is_some(), "{response}");
        this
    }
    fn send_frame(&mut self, frame: &Value) {
        serde_json::to_writer(&mut self.input, frame).unwrap();
        self.input.write_all(b"\n").unwrap();
        self.input.flush().unwrap();
    }
    fn receive(&mut self) -> Value {
        let mut line = String::new();
        assert_ne!(self.output.read_line(&mut line).unwrap(), 0);
        serde_json::from_str(&line).unwrap()
    }
    fn send(&mut self, request: &SessionStorageRequest) {
        self.sequence += 1;
        let id = format!("h:1:{}", self.sequence);
        let frame = json!({"jsonrpc":"2.0","id":id,"method":"execute","params":{
            "export":{"slot":"session_storage","module_id":self.module},
            "lineage":{"root_invocation_id":id,"parent_invocation_id":null,"depth":0},"params":request}});
        self.send_frame(&frame);
    }
    fn execute(&mut self, request: SessionStorageRequest) -> SessionStorageResponse {
        self.send(&request);
        let value = self.receive();
        assert!(value.get("error").is_none(), "{value}");
        serde_json::from_value(value["result"].clone()).unwrap()
    }
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.kill();
    }
}

#[test]
fn committed_record_survives_process_death_before_application_receives_ack() {
    for module in ["jsonl", "sqlite"] {
        let config = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut client = Client::new(module);
        let SessionStorageResponse::Session { descriptor } =
            client.execute(SessionStorageRequest::Create {
                config_root: config.path().into(),
                workspace_path: workspace.path().into(),
                session_id: new_session_id(),
                parent_session_id: None,
            })
        else {
            panic!()
        };
        let SessionStorageResponse::Writer { lease, revision } =
            client.execute(SessionStorageRequest::AcquireWriter {
                session: descriptor.session.clone(),
            })
        else {
            panic!()
        };
        let message = CanonicalMessage::text(MessageRole::User, "one confirmed effect");
        let commit = StorageCommit {
            lease,
            operation_id: new_record_id(),
            expected: revision,
            timestamp_ms: 42,
            attribution: StorageRecordAttribution {
                execution_id: None,
                thread_id: Some(new_thread_id()),
                turn_id: None,
            },
            entry: JournalEntry::HistoryMutated(HistoryMutated {
                previous_revision: 0,
                new_revision: 1,
                mutation: HistoryMutationKind::Append,
                messages: vec![message.clone()],
                compaction: None,
                tool_results: vec![],
            }),
        };
        client.send(&SessionStorageRequest::Commit {
            commit: commit.clone(),
        });
        // SDK starts output only after execute has completed its durable transaction.
        // Discard the transport while the application still has no complete ACK.
        let mut first = [0];
        client.output.read_exact(&mut first).unwrap();
        assert_eq!(first[0], b'{');
        client.kill();
        let mut resumed = Client::new(module);
        let SessionStorageResponse::CommitStatus { ack: Some(ack) } =
            resumed.execute(SessionStorageRequest::CommitStatus {
                session: descriptor.session.clone(),
                operation_id: commit.operation_id,
            })
        else {
            panic!()
        };
        assert_eq!(
            ack.revision,
            StorageRevision {
                session_seq: 1,
                history_revision: 1
            }
        );
        let SessionStorageResponse::Writer { lease, .. } =
            resumed.execute(SessionStorageRequest::AcquireWriter {
                session: descriptor.session.clone(),
            })
        else {
            panic!()
        };
        let mut retry = commit;
        retry.lease = lease;
        assert_eq!(
            resumed.execute(SessionStorageRequest::Commit { commit: retry }),
            SessionStorageResponse::Committed { ack }
        );
        let SessionStorageResponse::Snapshot { snapshot } =
            resumed.execute(SessionStorageRequest::OpenSnapshot {
                session: descriptor.session,
            })
        else {
            panic!()
        };
        let SessionStorageResponse::Conversation { conversation } =
            resumed.execute(SessionStorageRequest::ReadConversation {
                snapshot,
                point: StorageHistoryPoint::Current,
            })
        else {
            panic!()
        };
        assert_eq!(conversation.messages, [message]);
        assert_eq!(conversation.revision.session_seq, 1);
    }
}

#[test]
fn unmaterialized_create_survives_worker_restart_with_parent_identity() {
    for module in ["jsonl", "sqlite"] {
        let config = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let parent = new_session_id();
        let mut client = Client::new(module);
        let SessionStorageResponse::Session { descriptor } =
            client.execute(SessionStorageRequest::Create {
                config_root: config.path().into(),
                workspace_path: workspace.path().into(),
                session_id: new_session_id(),
                parent_session_id: Some(parent),
            })
        else {
            panic!()
        };
        assert!(!descriptor.local_path.as_ref().unwrap().exists());
        client.kill();
        let mut client = Client::new(module);
        let SessionStorageResponse::Writer { lease, revision } =
            client.execute(SessionStorageRequest::AcquireWriter {
                session: descriptor.session.clone(),
            })
        else {
            panic!()
        };
        assert_eq!(revision, StorageRevision::default());
        let SessionStorageResponse::Session { descriptor: opened } =
            client.execute(SessionStorageRequest::Open {
                path: descriptor.local_path.unwrap(),
            })
        else {
            panic!()
        };
        assert_eq!(opened.parent_session_id, Some(parent));
        assert_eq!(opened.session, descriptor.session);
        client.execute(SessionStorageRequest::ReleaseWriter { lease });
    }
}
