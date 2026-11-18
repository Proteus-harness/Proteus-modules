//! Typed storage service. No provider-, workflow- or tool-specific dispatch.
use crate::{
    backend::{JournalBackend, Jsonl, Sqlite},
    identity,
    lease::SessionWriteOwnership,
    path_identity::canonicalize_unmaterialized_path,
    snapshot::{Fold, SnapshotState},
    types::*,
    workspace_dir,
};
use anyhow::{Context, Result, bail, ensure};
use proteus_contracts::domain::SessionId;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::Mutex,
};
use uuid::Uuid;

mod cache;
mod catalog;
mod commits;
mod control;
mod locator;
mod objects;
mod sessions;
mod views;

const MAX_SNAPSHOTS: usize = 32;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    #[default]
    Jsonl,
    Sqlite,
}

pub struct StorageService {
    state: Mutex<State>,
}
struct State {
    kind: BackendKind,
    writers: HashMap<SessionLocator, Writer>,
    snapshots: HashMap<String, SnapshotState>,
    snapshot_order: VecDeque<String>,
    catalog: HashMap<SessionLocator, catalog::CachedSummary>,
    catalog_order: VecDeque<SessionLocator>,
    prefixes: HashMap<SessionLocator, views::PrefixCache>,
    prefix_order: VecDeque<SessionLocator>,
}
struct Writer {
    lease: StorageWriterLease,
    tokens: HashSet<Uuid>,
    _ownership: SessionWriteOwnership,
    backend: Box<dyn JournalBackend>,
    fold: Option<Fold>,
}
impl BackendKind {
    fn backend(self) -> Box<dyn JournalBackend> {
        match self {
            Self::Jsonl => Box::new(Jsonl::new()),
            Self::Sqlite => Box::new(Sqlite::default()),
        }
    }
}
impl StorageService {
    pub fn new(kind: BackendKind) -> Self {
        Self {
            state: Mutex::new(State {
                kind,
                writers: HashMap::new(),
                snapshots: HashMap::new(),
                snapshot_order: VecDeque::new(),
                catalog: HashMap::new(),
                catalog_order: VecDeque::new(),
                prefixes: HashMap::new(),
                prefix_order: VecDeque::new(),
            }),
        }
    }
    /// Each admitted transaction completes under this service lock even if an
    /// RPC caller stops waiting. Stable operation ids reconcile its durable result.
    pub fn execute(&self, request: SessionStorageRequest) -> Result<SessionStorageResponse> {
        request.validate()?;
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("storage service lock poisoned"))?
            .execute(request)
    }

    /// Validate one session and persist its disposable catalog index.
    pub fn prime_catalog(&self, path: &Path) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("storage service lock poisoned"))?;
        let descriptor = state.open(path)?;
        let dir = descriptor
            .local_path
            .clone()
            .context("session has no local path")?;
        state.catalog_summary(&dir, descriptor, false)?;
        Ok(())
    }
}
impl State {
    fn execute(&mut self, request: SessionStorageRequest) -> Result<SessionStorageResponse> {
        use SessionStorageRequest::*;
        match request {
            Create {
                config_root,
                workspace_path,
                session_id,
                parent_session_id,
            } => self.create(config_root, workspace_path, session_id, parent_session_id),
            Open { path } => Ok(SessionStorageResponse::Session {
                descriptor: self.open(&path)?,
            }),
            Describe { session } => Ok(SessionStorageResponse::Session {
                descriptor: self.describe(&session)?,
            }),
            List {
                config_root,
                workspace_path,
                audit,
            } => self.list(&config_root, workspace_path.as_deref(), audit),
            AcquireWriter { session } => self.acquire(session),
            ReleaseWriter { lease } => {
                let writer = self.writer(&lease)?;
                writer.tokens.remove(&lease.token);
                if writer.tokens.is_empty() {
                    self.writers.remove(&lease.session);
                }
                Ok(SessionStorageResponse::Done)
            }
            HibernateWriter { lease } => {
                self.writer(&lease)?.fold = None;
                Ok(SessionStorageResponse::Done)
            }
            Commit { commit } => self.commit(commit),
            CommitStatus {
                session,
                operation_id,
            } => Ok(SessionStorageResponse::CommitStatus {
                ack: self.commit_status(&session, operation_id)?,
            }),
            OpenSnapshot { session } => {
                let snapshot = self.snapshot(session)?;
                Ok(SessionStorageResponse::Snapshot { snapshot })
            }
            ReleaseSnapshot { snapshot } => {
                self.get_snapshot(&snapshot)?;
                self.snapshots.remove(&snapshot.snapshot_id);
                self.snapshot_order.retain(|s| s != &snapshot.snapshot_id);
                Ok(SessionStorageResponse::Done)
            }
            CompareSnapshots { snapshot, prefix } => {
                let matches = self.get_snapshot(&snapshot)?.matches_prefix(&prefix)?;
                Ok(SessionStorageResponse::SnapshotPrefix { matches })
            }
            ReadRecords {
                snapshot,
                after_seq,
                limit,
            } => Ok(SessionStorageResponse::Records {
                page: self.get_snapshot(&snapshot)?.page(after_seq, limit)?,
            }),
            ReadConversation { snapshot, point } => Ok(SessionStorageResponse::Conversation {
                conversation: self.get_snapshot(&snapshot)?.conversation_at(point)?,
            }),
            ReadUsage { snapshot } => {
                let id = snapshot.session.session_id;
                Ok(SessionStorageResponse::Usage {
                    usage: self.get_snapshot(&snapshot)?.fold()?.usage.snapshot(id),
                })
            }
            Resume { lease } => {
                let id = lease.session.session_id;
                Ok(SessionStorageResponse::Resumed {
                    state: self.writer_fold(&lease)?.resume(id),
                })
            }
            ReadExecutionState { lease } => {
                let fold = self.writer_fold(&lease)?;
                Ok(SessionStorageResponse::ExecutionState {
                    revision: fold.revision(),
                    interrupted_turns: fold.validation.interrupted_turns(),
                })
            }
            ReadCommittedTurn {
                lease,
                thread_id,
                turn_id,
            } => {
                let fold = self.writer_fold(&lease)?;
                let (messages, changed_compactions) =
                    fold.validation.committed_history(thread_id, turn_id);
                Ok(SessionStorageResponse::CommittedTurn {
                    state: StorageCommittedTurn {
                        messages,
                        changed_compactions,
                        revision: fold.revision(),
                    },
                })
            }
            ReadModelContextBeforeTurn { snapshot, turn_id } => {
                self.read_model_context_before_turn(snapshot, turn_id)
            }
            ReadWriterModelContextBeforeTurn { lease, turn_id } => {
                let observations = self
                    .writer_fold(&lease)?
                    .model_context
                    .before_turn(turn_id)
                    .context("model context requested outside the latest admitted turn")?;
                Ok(SessionStorageResponse::ModelContext { observations })
            }
            ReadCache {
                session,
                key,
                source,
            } => self.read_cache(session, key, source),
            WriteCache {
                session,
                key,
                source,
                value,
            } => self.write_cache(session, key, source, value),
            ReadMetadata { session, key } => {
                let dir = self.checked_path(&session)?;
                Ok(SessionStorageResponse::Metadata {
                    document: crate::metadata::read(&dir, &key)?,
                })
            }
            CompareExchangeMetadata {
                lease,
                operation_id,
                key,
                expected_etag,
                value,
            } => {
                let dir = self.writer_path(&lease)?;
                Ok(SessionStorageResponse::Metadata {
                    document: crate::metadata::compare_exchange(
                        &dir,
                        operation_id,
                        key,
                        expected_etag,
                        value,
                    )?,
                })
            }
            PutBlob {
                lease,
                operation_id,
                data_base64,
                expected,
            } => self.put_blob(lease, operation_id, data_base64, expected),
            ReadBlob { session, blob } => self.read_blob(session, blob),
            ReadBlobByHash {
                session,
                sha256,
                max_bytes,
            } => self.read_blob_by_hash(session, sha256, max_bytes),
            Delete {
                lease,
                operation_id,
            } => self.delete(lease, operation_id),
        }
    }
    fn checked_path(&self, session: &SessionLocator) -> Result<PathBuf> {
        let key = locator::parse(session)?;
        let control = control::load(&key.path)?;
        ensure!(
            key.incarnation == control::locator_incarnation(&control),
            "session locator incarnation is retired"
        );
        if !key.path.try_exists()? {
            return Ok(key.path);
        }
        let identity = identity::resolve_session_identity(&key.path)?;
        ensure!(
            identity.session_id == session.session_id
                && identity.parent_session_id == key.parent_session_id,
            "session locator identity changed"
        );
        Ok(key.path)
    }
    fn writer(&mut self, lease: &StorageWriterLease) -> Result<&mut Writer> {
        let writer = self
            .writers
            .get_mut(&lease.session)
            .ok_or(SessionStorageFailure::InvalidWriterLease)?;
        ensure!(
            writer.lease.session == lease.session
                && writer.lease.generation == lease.generation
                && writer.tokens.contains(&lease.token),
            SessionStorageFailure::InvalidWriterLease
        );
        Ok(writer)
    }
    fn writer_path(&mut self, lease: &StorageWriterLease) -> Result<PathBuf> {
        self.writer(lease)?;
        self.checked_path(&lease.session)
    }
    fn writer_fold(&mut self, lease: &StorageWriterLease) -> Result<&mut Fold> {
        let dir = self.writer_path(lease)?;
        let writer = self.writer(lease)?;
        if writer.fold.is_none() {
            writer.backend.recover(&dir, lease.session.session_id)?;
            let mut source = writer.backend.snapshot(&dir, lease.session.session_id)?;
            let mut fold = Fold::default();
            while let Some(record) = source.next()? {
                fold.apply(lease.session.session_id, &record)?;
            }
            writer.fold = Some(fold);
        }
        Ok(writer.fold.as_mut().expect("writer fold initialized"))
    }
}
