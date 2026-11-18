use super::*;
use crate::types::*;
use proteus_contracts::{
    domain::{HistoryCompactionReport, new_record_id, new_session_id, new_thread_id},
    model_standard::{
        CanonicalMessage, CanonicalPart, ContentPart, MessageRole, PartProvenance, PartScope,
    },
};

struct Fixture {
    service: StorageService,
    root: tempfile::TempDir,
    _workspace: tempfile::TempDir,
    descriptor: StorageSessionDescriptor,
    lease: StorageWriterLease,
    revision: StorageRevision,
    thread: proteus_contracts::domain::ThreadId,
}
impl Fixture {
    fn new(kind: BackendKind) -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let service = StorageService::new(kind);
        let SessionStorageResponse::Session { descriptor } = service
            .execute(SessionStorageRequest::Create {
                config_root: root.path().to_owned(),
                workspace_path: workspace.path().to_owned(),
                session_id: new_session_id(),
                parent_session_id: None,
            })
            .unwrap()
        else {
            panic!()
        };
        assert!(
            !descriptor.local_path.clone().unwrap().exists(),
            "create is only a read-only reservation"
        );
        let SessionStorageResponse::Writer { lease, revision } = service
            .execute(SessionStorageRequest::AcquireWriter {
                session: descriptor.session.clone(),
            })
            .unwrap()
        else {
            panic!()
        };
        Self {
            service,
            root,
            _workspace: workspace,
            descriptor,
            lease,
            revision,
            thread: new_thread_id(),
        }
    }
    fn append(&mut self, messages: Vec<CanonicalMessage>) -> StorageCommit {
        let commit = StorageCommit {
            lease: self.lease.clone(),
            operation_id: new_record_id(),
            expected: self.revision,
            timestamp_ms: 1234,
            attribution: StorageRecordAttribution {
                execution_id: None,
                thread_id: Some(self.thread),
                turn_id: None,
            },
            entry: JournalEntry::HistoryMutated(HistoryMutated {
                previous_revision: self.revision.history_revision,
                new_revision: self.revision.history_revision + 1,
                mutation: HistoryMutationKind::Append,
                messages,
                compaction: None,
                tool_results: vec![],
            }),
        };
        let SessionStorageResponse::Committed { ack } = self
            .service
            .execute(SessionStorageRequest::Commit {
                commit: commit.clone(),
            })
            .unwrap()
        else {
            panic!()
        };
        self.revision = ack.revision;
        commit
    }
    fn snapshot(&self) -> StorageSnapshot {
        let SessionStorageResponse::Snapshot { snapshot } = self
            .service
            .execute(SessionStorageRequest::OpenSnapshot {
                session: self.descriptor.session.clone(),
            })
            .unwrap()
        else {
            panic!()
        };
        snapshot
    }
    fn conversation(
        &self,
        snapshot: StorageSnapshot,
        point: StorageHistoryPoint,
    ) -> StorageConversation {
        let SessionStorageResponse::Conversation { conversation } = self
            .service
            .execute(SessionStorageRequest::ReadConversation { snapshot, point })
            .unwrap()
        else {
            panic!()
        };
        conversation
    }
}
fn backends() -> [BackendKind; 2] {
    [BackendKind::Jsonl, BackendKind::Sqlite]
}
fn text(value: &str) -> CanonicalMessage {
    CanonicalMessage::text(MessageRole::User, value)
}

mod journal;
mod metadata;
mod ownership;

mod performance;

mod paging;

mod model_context;
mod persisted_hooks;
