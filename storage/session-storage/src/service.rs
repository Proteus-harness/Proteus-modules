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
mod display;
mod locator;
mod objects;
mod sessions;
mod views;

const MAX_SNAPSHOTS: usize = 32;
const MAX_CATALOG_ENTRIES: usize = 1024;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    #[default]
    Jsonl,
    Sqlite,
}

pub struct StorageService {
    kind: BackendKind,
    state: Mutex<State>,
    // Bound cold catalog work independently of foreground writers.
    catalog_scan: Mutex<()>,
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
    backend: Box<dyn JournalBackend>,
    fold: Option<Fold>,
    head: Option<catalog::WriterHead>,
    _ownership: SessionWriteOwnership,
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
            kind,
            catalog_scan: Mutex::new(()),
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
    /// Durable operations finish under the service lock even if a caller stops
    /// waiting. Session identity and disposable cache reads only consult disk
    /// and can serve a pinned display while another request validates a journal.
    pub fn execute(&self, request: SessionStorageRequest) -> Result<SessionStorageResponse> {
        use SessionStorageRequest::*;
        request.validate()?;
        match request {
            Open { path } => Ok(SessionStorageResponse::Session {
                descriptor: sessions::open(self.kind, &path)?,
            }),
            ListCandidates {
                config_root,
                workspace_path,
            } => Ok(SessionStorageResponse::Candidates {
                sessions: sessions::candidates(
                    self.kind,
                    &config_root,
                    workspace_path.as_deref(),
                    false,
                )?,
            }),
            List {
                config_root,
                workspace_path,
                audit,
            } => self.list(&config_root, workspace_path.as_deref(), audit),
            ReadCache {
                session,
                key,
                source,
            } => cache::read_cache(session, key, source),
            request => self.lock_state()?.execute(request),
        }
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("storage service lock poisoned"))
    }

    fn lock_catalog(&self) -> Result<std::sync::MutexGuard<'_, ()>> {
        self.catalog_scan
            .lock()
            .map_err(|_| anyhow::anyhow!("catalog scan lock poisoned"))
    }

    /// Validate one session and persist its disposable catalog index.
    pub fn prime_catalog(&self, path: &Path) -> Result<()> {
        let _scan = self.lock_catalog()?;
        self.catalog_summary(path, false)?;
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
            Describe { session } => Ok(SessionStorageResponse::Session {
                descriptor: self.describe(&session)?,
            }),
            AcquireWriter { session } => self.acquire(session),
            ReleaseWriter { lease } => {
                let writer = self.writer(&lease)?;
                writer.tokens.remove(&lease.token);
                if writer.tokens.is_empty() {
                    let writer = self.writers.remove(&lease.session).expect("writer exists");
                    self.close_writer(&lease.session, writer);
                }
                Ok(SessionStorageResponse::Done)
            }
            HibernateWriter { lease } => {
                let writer = self.writer(&lease)?;
                writer.fold = None;
                writer.head = None;
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
            OpenDisplaySnapshot { session } => self.open_display_snapshot(session),
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
            } => {
                let sole_snapshot = self.snapshots.len() == 1;
                let page = self
                    .get_snapshot(&snapshot)?
                    .page(after_seq, limit, sole_snapshot)?;
                Ok(SessionStorageResponse::Records { page })
            }
            ReadDisplayRecords {
                snapshot,
                after_seq,
                limit,
            } => Ok(SessionStorageResponse::Records {
                page: self
                    .get_snapshot(&snapshot)?
                    .display_page(after_seq, limit)?,
            }),
            ReadConversation { snapshot, point } => Ok(SessionStorageResponse::Conversation {
                conversation: self.get_snapshot(&snapshot)?.conversation_at(point)?,
            }),
            ReadUsage { snapshot } => {
                let id = snapshot.session.session_id;
                Ok(SessionStorageResponse::Usage {
                    usage: self.get_snapshot(&snapshot)?.fold_usage()?.snapshot(id),
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
            Open { .. } | ReadCache { .. } | ListCandidates { .. } | List { .. } => {
                unreachable!("handled by StorageService")
            }
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
        checked_path(session)
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
        let kind = self.kind;
        let writer = self.writer(lease)?;
        let dir = checked_path(&lease.session)?;
        writer.recovered_fold(&dir, kind)
    }
}

impl Writer {
    fn recovered_fold(&mut self, dir: &Path, kind: BackendKind) -> Result<&mut Fold> {
        let id = self.lease.session.session_id;
        if self.fold.is_none() {
            self.backend.recover(dir, id)?;
            let stamps = catalog::source_stamp(dir, kind)?;
            let blobs = catalog::durable::blob_stamps(dir);
            let mut source = self.backend.snapshot(dir, id)?;
            let mut fold = Fold::default();
            while let Some(record) = source.next()? {
                fold.apply(id, &record)?;
            }
            self.head = blobs
                .filter(|blobs| {
                    catalog::source_stamp(dir, kind).ok().as_ref() == Some(&stamps)
                        && catalog::durable::blob_stamps(dir).as_ref() == Some(blobs)
                })
                .map(|blobs| catalog::WriterHead {
                    stamps,
                    blobs,
                    extent: source.extent(),
                    hash: source.read_hash(),
                });
            self.fold = Some(fold);
        }
        Ok(self.fold.as_mut().expect("writer fold initialized"))
    }
}

fn checked_path(session: &SessionLocator) -> Result<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};

    #[test]
    fn pinned_display_reads_do_not_wait_for_serialized_journal_work() {
        use SessionStorageRequest::*;
        use SessionStorageResponse as Response;

        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let service = StorageService::new(BackendKind::Jsonl);
        let Response::Session { descriptor } = service
            .execute(Create {
                config_root: root.path().to_owned(),
                workspace_path: workspace.path().to_owned(),
                session_id: proteus_contracts::domain::new_session_id(),
                parent_session_id: None,
            })
            .unwrap()
        else {
            panic!("expected session descriptor");
        };
        let session = descriptor.session.clone();
        let _writer = service
            .execute(AcquireWriter {
                session: session.clone(),
            })
            .unwrap();
        let Response::Snapshot { snapshot } = service
            .execute(OpenSnapshot {
                session: session.clone(),
            })
            .unwrap()
        else {
            panic!("expected snapshot");
        };
        let key = "pinned-display".to_owned();
        let value = serde_json::json!({"rows": [0, 1, 2]});
        let response = service
            .execute(WriteCache {
                session: session.clone(),
                key: key.clone(),
                source: snapshot.clone(),
                value: value.clone(),
            })
            .unwrap();
        assert!(matches!(response, Response::CacheWritten { etag: Some(_) }));

        let (open, read, candidates) = std::thread::scope(|scope| {
            let (tx, rx) = mpsc::channel();
            // A strict fold holds this lock until its complete snapshot is verified.
            let state = service.state.lock().unwrap();
            let path = descriptor.local_path.unwrap();
            let service = &service;
            let root = root.path().to_owned();
            scope.spawn(move || {
                let open = service.execute(Open { path });
                let read = service.execute(ReadCache {
                    session,
                    key,
                    source: Some(snapshot),
                });
                let candidates = service.execute(ListCandidates {
                    config_root: root,
                    workspace_path: None,
                });
                let _ = tx.send((open, read, candidates));
            });
            let result = rx.recv_timeout(Duration::from_secs(2));
            drop(state);
            result.expect("pinned display reads waited for the journal lock")
        });
        assert!(matches!(open.unwrap(), Response::Session { .. }));
        assert!(
            matches!(candidates.unwrap(), Response::Candidates { sessions } if sessions.len() == 1)
        );
        assert!(matches!(
            read.unwrap(),
            Response::Metadata { document: Some(document) } if document.value == value
        ));
    }
}
