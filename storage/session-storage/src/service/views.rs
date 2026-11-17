use super::*;
use crate::backend::PrefixHint;
#[derive(Clone)]
pub(super) struct PrefixCache {
    pub stamps: catalog::SourceStamp,
    pub revision: StorageRevision,
    pub hint: PrefixHint,
}
impl State {
    pub(super) fn remember_prefix(&mut self, session: SessionLocator, cache: PrefixCache) {
        self.prefix_order.retain(|s| s != &session);
        while self.prefixes.len() >= 1024 {
            if let Some(old) = self.prefix_order.pop_front() {
                self.prefixes.remove(&old);
            } else {
                break;
            }
        }
        self.prefix_order.push_back(session.clone());
        self.prefixes.insert(session, cache);
    }
    pub(super) fn snapshot(&mut self, session: SessionLocator) -> Result<StorageSnapshot> {
        let dir = self.checked_path(&session)?;
        let stamps = catalog::source_stamp(&dir, self.kind)?;
        let cached = self
            .prefixes
            .get(&session)
            .filter(|cache| cache.stamps == stamps)
            .cloned();
        let verified = cached.is_none();
        let (mut source, revision) = if let Some(cached) = cached {
            (
                self.kind
                    .backend()
                    .snapshot_warm(&dir, session.session_id, &cached.hint)?,
                cached.revision,
            )
        } else {
            let mut source = self.kind.backend().snapshot(&dir, session.session_id)?;
            let version = source.source_version()?;
            let mut state = Fold::default();
            while let Some(record) = source.next()? {
                state.apply(session.session_id, &record)?;
            }
            if source.source_version()? != version {
                source.verify_fixed_prefix()?;
            }
            let revision = state.revision();
            self.remember_prefix(
                session.clone(),
                PrefixCache {
                    stamps,
                    revision,
                    hint: PrefixHint {
                        extent: source.extent(),
                        fingerprint: source.fingerprint().to_owned(),
                    },
                },
            );
            (source, revision)
        };
        let identity = StorageSnapshot {
            session,
            snapshot_id: Uuid::new_v4().to_string(),
            revision,
            content_sha256: source.fingerprint().to_owned(),
        };
        source.reset()?;
        while self.snapshots.len() >= MAX_SNAPSHOTS {
            if let Some(id) = self.snapshot_order.pop_front() {
                self.snapshots.remove(&id);
            } else {
                break;
            }
        }
        self.snapshot_order.push_back(identity.snapshot_id.clone());
        self.snapshots.insert(
            identity.snapshot_id.clone(),
            SnapshotState::new(identity.clone(), source, verified)?,
        );
        Ok(identity)
    }
    pub(super) fn read_model_context_before_turn(
        &mut self,
        snapshot: StorageSnapshot,
        turn_id: proteus_contracts::domain::TurnId,
    ) -> Result<SessionStorageResponse> {
        // Validate the caller's exact live handle before considering resident
        // state. A writer can be newer than an older pinned snapshot.
        let verified = self.get_snapshot(&snapshot)?.is_verified_unchanged()?;
        let resident = verified
            .then(|| self.writers.get(&snapshot.session))
            .flatten()
            .and_then(|writer| writer.fold.as_ref())
            .filter(|fold| fold.revision() == snapshot.revision);
        let observations = if let Some(fold) = resident {
            fold.model_context.before_turn(turn_id)
        } else {
            self.get_snapshot(&snapshot)?
                .fold()?
                .model_context
                .before_turn(turn_id)
        }
        .context("model context requested outside the latest admitted turn")?;
        Ok(SessionStorageResponse::ModelContext { observations })
    }
    pub(super) fn get_snapshot(
        &mut self,
        snapshot: &StorageSnapshot,
    ) -> Result<&mut SnapshotState> {
        let source = self
            .snapshots
            .get_mut(&snapshot.snapshot_id)
            .context("snapshot handle expired or unknown")?;
        ensure!(
            source.identity == *snapshot,
            "snapshot identity/revision/fingerprint mismatch"
        );
        Ok(source)
    }
}
