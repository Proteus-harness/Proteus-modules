use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn milliseconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().try_into().unwrap_or(i64::MAX))
        .unwrap_or(0)
}

impl State {
    fn check_selected_format(&self, path: &Path) -> Result<()> {
        let other = match self.kind {
            BackendKind::Jsonl => BackendKind::Sqlite,
            BackendKind::Sqlite => BackendKind::Jsonl,
        }
        .backend()
        .description_path(path);
        ensure!(
            !other.try_exists()?,
            "session contains another canonical journal format; select its storage implementation or explicitly migrate it"
        );
        Ok(())
    }
    pub(super) fn create(
        &mut self,
        root: PathBuf,
        workspace: PathBuf,
        id: SessionId,
        parent: Option<SessionId>,
    ) -> Result<SessionStorageResponse> {
        ensure!(parent != Some(id), "session cannot be its own parent");
        let workspace = std::fs::canonicalize(workspace)?;
        let encoded = workspace_dir::encode_workspace_path(&workspace)?;
        let path = canonicalize_unmaterialized_path(
            &root
                .join("sessions")
                .join(encoded)
                .join(identity::short_session_directory_name(id)),
        )?;
        self.check_selected_format(&path)?;
        let may_create = identity::validate_new_session_target(&path, id)?;
        let control = super::control::load(&path)?;
        let session = super::locator::make(
            path.clone(),
            workspace.clone(),
            id,
            parent,
            super::control::locator_incarnation(&control),
        )?;
        let descriptor = if may_create || control.deleted {
            StorageSessionDescriptor {
                session: session.clone(),
                parent_session_id: parent,
                workspace_path: workspace,
                record_location: Some(self.kind.backend().description_path(&path)),
                local_path: Some(path),
                created_at_ms: 0,
                updated_at_ms: 0,
            }
        } else {
            let descriptor = self.open(&path)?;
            ensure!(
                descriptor.parent_session_id == parent,
                "session parent identity changed"
            );
            descriptor
        };
        Ok(SessionStorageResponse::Session { descriptor })
    }
    pub(super) fn open(&self, path: &Path) -> Result<StorageSessionDescriptor> {
        let path = if path
            .file_name()
            .is_some_and(|name| name == "journal.jsonl" || name == "journal.sqlite")
        {
            path.parent()
                .context("record location has no session parent")?
        } else {
            path
        };
        let path = canonicalize_unmaterialized_path(path)?;
        let control = super::control::load(&path)?;
        if control.deleted || !path.try_exists()? {
            return Err(SessionStorageFailure::MissingSession.into());
        }
        self.check_selected_format(&path)?;
        let identity = identity::resolve_session_identity(&path)?;
        let workspace_path = workspace_dir::workspace_path_from_session_dir(&path)?;
        let meta = std::fs::metadata(&path)?;
        let journal = self.kind.backend().description_path(&path);
        let mut modified = std::fs::metadata(&journal)
            .and_then(|m| m.modified())
            .or_else(|_| meta.modified())?;
        if let Ok(wal_modified) =
            std::fs::metadata(PathBuf::from(format!("{}-wal", journal.to_string_lossy())))
                .and_then(|m| m.modified())
        {
            modified = modified.max(wal_modified);
        }
        Ok(StorageSessionDescriptor {
            session: super::locator::make(
                path.clone(),
                workspace_path.clone(),
                identity.session_id,
                identity.parent_session_id,
                control.incarnation,
            )?,
            parent_session_id: identity.parent_session_id,
            workspace_path,
            record_location: Some(self.kind.backend().description_path(&path)),
            local_path: Some(path),
            created_at_ms: milliseconds(meta.created().unwrap_or(modified)),
            updated_at_ms: milliseconds(modified),
        })
    }
    pub(super) fn describe(&self, session: &SessionLocator) -> Result<StorageSessionDescriptor> {
        let path = self.checked_path(session)?;
        if !path.try_exists()? || super::control::load(&path)?.deleted {
            let key = super::locator::parse(session)?;
            return Ok(StorageSessionDescriptor {
                session: session.clone(),
                parent_session_id: key.parent_session_id,
                workspace_path: key.workspace,
                record_location: Some(self.kind.backend().description_path(&path)),
                local_path: Some(path),
                created_at_ms: 0,
                updated_at_ms: 0,
            });
        }
        let descriptor = self.open(&path)?;
        ensure!(
            descriptor.session == *session,
            "session locator identity mismatch"
        );
        Ok(descriptor)
    }
    pub(super) fn list(
        &mut self,
        root: &Path,
        workspace: Option<&Path>,
        audit: bool,
    ) -> Result<SessionStorageResponse> {
        let sessions = root.join("sessions");
        let workspace_dirs = if let Some(workspace) = workspace {
            vec![sessions.join(workspace_dir::encode_workspace_path(workspace)?)]
        } else {
            match std::fs::read_dir(&sessions) {
                Ok(entries) => entries
                    .filter_map(|entry| entry.ok())
                    .filter(|e| e.path().is_dir())
                    .map(|e| e.path())
                    .collect(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(error) => return Err(error.into()),
            }
        };
        let mut result = Vec::new();
        for dir in workspace_dirs {
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            for entry in entries {
                let path = match entry {
                    Ok(entry) => entry.path(),
                    Err(error) if audit => return Err(error.into()),
                    Err(_) => continue,
                };
                if !path.is_dir() {
                    continue;
                }
                let descriptor = match self.open(&path) {
                    Ok(d) => d,
                    Err(error) if audit => return Err(error),
                    Err(_) => continue,
                };
                match self.catalog_summary(&path, descriptor, audit) {
                    Ok(summary) => result.push(summary),
                    Err(error) if audit => return Err(error),
                    Err(_) => continue,
                }
            }
        }
        result.sort_by(|a, b| {
            b.descriptor
                .updated_at_ms
                .cmp(&a.descriptor.updated_at_ms)
                .then_with(|| b.descriptor.session.key.cmp(&a.descriptor.session.key))
        });
        Ok(SessionStorageResponse::Sessions { sessions: result })
    }
    pub(super) fn acquire(&mut self, session: SessionLocator) -> Result<SessionStorageResponse> {
        if let Some(writer) = self.writers.get_mut(&session) {
            let mut lease = writer.lease.clone();
            lease.token = Uuid::new_v4();
            writer.tokens.insert(lease.token);
            let revision = self.writer_fold(&lease)?.revision();
            return Ok(SessionStorageResponse::Writer { lease, revision });
        }
        let descriptor = self.describe(&session)?;
        let path = self.checked_path(&session)?;
        crate::io::create_dir_all_durable(path.parent().context("session has no parent")?)?;
        let ownership = SessionWriteOwnership::acquire(&path, session.session_id)?;
        let mut control = super::control::load(&path)?;
        let key = super::locator::parse(&session)?;
        ensure!(
            key.incarnation == super::control::locator_incarnation(&control),
            "session locator incarnation is retired"
        );
        ensure!(
            !control.deletes.values().any(|d| !d.complete),
            "session has an unfinished deletion; reconcile it before writing"
        );
        let created = match std::fs::create_dir(&path) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(error) => return Err(error.into()),
        };
        identity::ensure_writable_identity(
            &path,
            session.session_id,
            identity::SessionDirectoryKind::ShortNumeric,
            descriptor.parent_session_id,
            &descriptor.workspace_path,
            created,
        )?;
        std::fs::File::open(path.join("session.json"))?.sync_all()?;
        crate::io::sync_directory(&path)?;
        if created {
            crate::io::sync_directory(path.parent().expect("parent validated"))?;
        }
        if created || control.deleted {
            control.generation = Uuid::new_v4();
            control.incarnation = key.incarnation;
            control.deleted = false;
        }
        ensure!(!control.deleted, "session generation is retired");
        super::control::save(&path, &control)?;
        let lease = StorageWriterLease {
            session: session.clone(),
            token: Uuid::new_v4(),
            generation: control.generation,
        };
        self.writers.insert(
            session.clone(),
            Writer {
                lease: lease.clone(),
                tokens: HashSet::from([lease.token]),
                _ownership: ownership,
                backend: self.kind.backend(),
                fold: None,
                head: None,
            },
        );
        let result = self.writer_fold(&lease).map(|fold| fold.revision());
        match result {
            Ok(revision) => Ok(SessionStorageResponse::Writer { lease, revision }),
            Err(error) => {
                self.writers.remove(&session);
                Err(error)
            }
        }
    }
}
