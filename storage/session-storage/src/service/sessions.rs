use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn milliseconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().try_into().unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn check_selected_format(kind: BackendKind, path: &Path) -> Result<()> {
    let other = match kind {
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
pub(super) fn open(kind: BackendKind, path: &Path) -> Result<StorageSessionDescriptor> {
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
    check_selected_format(kind, &path)?;
    let identity = identity::resolve_session_identity(&path)?;
    let workspace_path = workspace_dir::workspace_path_from_session_dir(&path)?;
    let meta = std::fs::metadata(&path)?;
    let journal = kind.backend().description_path(&path);
    let mut modified = std::fs::metadata(&journal)
        .and_then(|m| m.modified())
        .or_else(|_| meta.modified())?;
    // Read-only SQLite snapshots can create an empty WAL without any change
    // to the session. Like source stamps, recency ignores that empty WAL.
    if let Ok(wal) = std::fs::metadata(PathBuf::from(format!("{}-wal", journal.to_string_lossy())))
        && (!matches!(kind, BackendKind::Sqlite) || wal.len() != 0)
        && let Ok(wal_modified) = wal.modified()
    {
        modified = modified.max(wal_modified);
    }
    let observed = super::control::load(&path)?;
    ensure!(
        !observed.deleted
            && super::control::locator_incarnation(&observed)
                == super::control::locator_incarnation(&control),
        SessionStorageFailure::MissingSession
    );
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
        record_location: Some(journal),
        local_path: Some(path),
        created_at_ms: milliseconds(meta.created().unwrap_or(modified)),
        updated_at_ms: milliseconds(modified),
    })
}

/// Enumerate identities and metadata without reading canonical records.
pub(super) fn candidates(
    kind: BackendKind,
    root: &Path,
    workspace: Option<&Path>,
    audit: bool,
) -> Result<Vec<StorageSessionDescriptor>> {
    let mut result = Vec::new();
    for path in candidate_paths(root, workspace, audit)? {
        match open(kind, &path) {
            Ok(descriptor) => result.push(descriptor),
            Err(error) if audit => return Err(error),
            Err(_) => continue,
        }
    }
    result.sort_by(catalog_order);
    Ok(result)
}

fn catalog_order(a: &StorageSessionDescriptor, b: &StorageSessionDescriptor) -> std::cmp::Ordering {
    b.updated_at_ms
        .cmp(&a.updated_at_ms)
        .then_with(|| b.session.key.cmp(&a.session.key))
}

fn candidate_paths(root: &Path, workspace: Option<&Path>, audit: bool) -> Result<Vec<PathBuf>> {
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
            result.push(path);
        }
    }
    Ok(result)
}

impl StorageService {
    fn prioritize_catalog_paths(&self, paths: &mut [PathBuf], audit: bool) {
        if audit || paths.len() <= MAX_CATALOG_ENTRIES {
            return;
        }
        // Process possible hits before misses can evict them. This advisory
        // snapshot never skips a path or substitutes for catalog validation.
        let resident = self
            .state
            .try_lock()
            .map(|state| state.resident_catalog_paths())
            .unwrap_or_default();
        if resident.is_empty() {
            return;
        }
        let mut next = 0;
        for index in 0..paths.len() {
            if resident.contains(&paths[index]) {
                paths.swap(next, index);
                next += 1;
            }
        }
    }

    pub(super) fn list(
        &self,
        root: &Path,
        workspace: Option<&Path>,
        audit: bool,
    ) -> Result<SessionStorageResponse> {
        let _scan = self.lock_catalog()?;
        let mut paths = candidate_paths(root, workspace, audit)?;
        self.prioritize_catalog_paths(&mut paths, audit);
        let mut result = Vec::new();
        for path in paths {
            match self.catalog_summary(&path, audit) {
                Ok(summary) => result.push(summary),
                Err(error) if audit => return Err(error),
                Err(_) => continue,
            }
        }
        // A commit can reorder sessions while the list is being built. Sort the
        // descriptors we actually return, not timestamps from discovery.
        result.sort_by(|a, b| catalog_order(&a.descriptor, &b.descriptor));
        Ok(SessionStorageResponse::Sessions { sessions: result })
    }
}

impl State {
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
        check_selected_format(self.kind, &path)?;
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
        open(self.kind, path)
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
    pub(super) fn acquire(&mut self, session: SessionLocator) -> Result<SessionStorageResponse> {
        if let Some(writer) = self.writers.get_mut(&session) {
            // Do not publish a token until all fallible recovery has succeeded.
            let dir = checked_path(&session)?;
            let revision = writer.recovered_fold(&dir, self.kind)?.revision();
            let mut lease = writer.lease.clone();
            lease.token = Uuid::new_v4();
            writer.tokens.insert(lease.token);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::Fixture;

    #[test]
    fn oversized_catalog_schedules_resident_paths_without_dropping_candidates() {
        let f = Fixture::new(BackendKind::Jsonl);
        let cached = f.descriptor.local_path.clone().unwrap();
        f.service.prime_catalog(&cached).unwrap();
        let mut paths: Vec<_> = (0..MAX_CATALOG_ENTRIES)
            .map(|i| PathBuf::from(format!("unknown-{i}")))
            .collect();
        paths.push(cached.clone());
        let original = paths.clone();
        f.service.prioritize_catalog_paths(&mut paths, true);
        assert_eq!(paths, original, "audit preserves discovery order");
        f.service.prioritize_catalog_paths(&mut paths[1..], false);
        assert_eq!(paths, original, "in-capacity scan needs no scheduling");

        f.service.prioritize_catalog_paths(&mut paths, false);
        assert_eq!(paths[0], cached);
        paths.sort();
        let mut expected = original;
        expected.sort();
        assert_eq!(paths, expected);
    }
}
