use super::*;
use crate::backend::{PrefixHint, replay_records};

#[derive(Clone)]
pub(super) struct PrefixCache {
    pub stamps: catalog::SourceStamp,
    pub revision: StorageRevision,
    pub hint: PrefixHint,
    pub blob_digest: Option<catalog::durable::BlobStampDigest>,
}

impl PrefixCache {
    pub fn valid_at(&self, dir: &Path, stamps: &catalog::SourceStamp) -> bool {
        &self.stamps == stamps
            && self.blob_digest.is_some_and(|digest| {
                catalog::durable::blob_stamps(dir)
                    .as_ref()
                    .map(catalog::durable::blob_stamp_digest)
                    == Some(digest)
            })
    }
}
impl State {
    pub(super) fn remember_prefix(&mut self, session: SessionLocator, cache: PrefixCache) {
        catalog::remember_catalog_entry(&mut self.prefixes, &mut self.prefix_order, session, cache);
    }
    pub(super) fn prepare_snapshot(&mut self, session: SessionLocator) -> Result<SnapshotState> {
        let dir = self.checked_path(&session)?;
        let stamps = catalog::source_stamp(&dir, self.kind)?;
        let cached = self
            .prefixes
            .get(&session)
            .filter(|cache| cache.valid_at(&dir, &stamps));
        let descriptor = self.describe(&session)?;
        let kind = self.kind;
        let session_id = session.session_id;
        let warm = |cached: &PrefixCache| -> Result<_> {
            let source = kind
                .backend()
                .snapshot_warm(&dir, session_id, &cached.hint)?;
            ensure!(
                catalog::source_matches(&dir, kind, &cached.stamps)?,
                "journal changed during snapshot capture"
            );
            Ok((source, cached.revision))
        };
        let mut verified = false;
        let mut previous_boundary = None;
        let (mut source, revision) = if let Some(cached) = cached {
            warm(cached)?
        } else if let Some(writer) = self.writers.get(&session)
            && let (Some(fold), Some(head)) = (&writer.fold, &writer.head)
            && head.stamps == stamps
            && let Some(blobs) = catalog::durable::blob_stamps(&dir)
            && blobs == head.blobs
        {
            let messages = fold.validation.history();
            let prefix = PrefixCache {
                stamps,
                revision: fold.revision(),
                hint: PrefixHint {
                    extent: head.extent,
                    fingerprint: crate::io::digest_hex(head.hash.clone().finish()),
                },
                blob_digest: Some(catalog::durable::blob_stamp_digest(&blobs)),
            };
            let summary = StorageSessionSummary {
                descriptor,
                message_count: messages.len(),
                preview: crate::snapshot::preview(messages),
            };
            self.save_catalog_head(&dir, summary, &prefix, blobs);
            verified = true;
            warm(&prefix)?
        } else if let Some(cached) = self.load_catalog_head(&dir, &descriptor, &stamps) {
            warm(&cached)?
        } else {
            let blobs = catalog::durable::blob_stamps(&dir);
            let cut = self
                .prefixes
                .get(&session)
                .map(|cache| cache.revision.session_seq);
            let apply =
                |(state, boundary): &mut (Fold, Option<_>), record: &JournalRecord, hash| {
                    state.apply_validated(session.session_id, record)?;
                    if let Some(hash) = hash {
                        *boundary = Some((state.revision(), hash));
                    }
                    Ok(())
                };
            let (source, (state, boundary)) = match self.kind {
                BackendKind::Jsonl => Jsonl::replay_snapshot(&dir, session.session_id, cut, apply)?,
                BackendKind::Sqlite => {
                    let mut source = self.kind.backend().snapshot(&dir, session.session_id)?;
                    let fold = replay_records(source.as_mut(), cut, &apply)?;
                    (source, fold)
                }
            };
            previous_boundary = boundary.or_else(|| {
                (cut == Some(0)).then(|| (StorageRevision::default(), crate::io::sha256_hex(b"")))
            });
            let blobs = blobs
                .filter(|blobs| catalog::durable::matches_blob_stamps(&dir, blobs) == Some(true));
            let prefix = PrefixCache {
                stamps,
                revision: state.revision(),
                hint: PrefixHint {
                    extent: source.extent(),
                    fingerprint: source.fingerprint().to_owned(),
                },
                blob_digest: blobs.as_ref().map(catalog::durable::blob_stamp_digest),
            };
            let revision = prefix.revision;
            if let Some(blobs) = blobs
                && catalog::source_matches(&dir, self.kind, &prefix.stamps)?
            {
                let messages = state.validation.history();
                let summary = StorageSessionSummary {
                    descriptor,
                    message_count: messages.len(),
                    preview: crate::snapshot::preview(messages),
                };
                self.save_catalog_head(&dir, summary, &prefix, blobs);
            }
            verified = true;
            (source, revision)
        };
        let identity = StorageSnapshot {
            session,
            snapshot_id: Uuid::new_v4().to_string(),
            revision,
            content_sha256: source.fingerprint().to_owned(),
        };
        source.reset()?;
        SnapshotState::new(identity, source, verified, previous_boundary)
    }
    pub(super) fn snapshot(&mut self, session: SessionLocator) -> Result<StorageSnapshot> {
        let prepared = self.prepare_snapshot(session)?;
        let identity = prepared.identity.clone();
        self.remember_snapshot(prepared);
        Ok(identity)
    }
    pub(super) fn remember_snapshot(&mut self, prepared: SnapshotState) {
        let identity = &prepared.identity;
        while self.snapshots.len() >= MAX_SNAPSHOTS {
            if let Some(id) = self.snapshot_order.pop_front() {
                self.snapshots.remove(&id);
            } else {
                break;
            }
        }
        self.snapshot_order.push_back(identity.snapshot_id.clone());
        self.snapshots
            .insert(identity.snapshot_id.clone(), prepared);
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
            .and_then(|writer| {
                let head = writer.head.as_ref()?;
                (crate::io::digest_hex(head.hash.clone().finish()) == snapshot.content_sha256)
                    .then_some(writer.fold.as_ref())?
            })
            .filter(|fold| fold.revision() == snapshot.revision);
        let observations = if let Some(fold) = resident {
            fold.model_context.before_turn(turn_id)
        } else {
            self.get_snapshot(&snapshot)?
                .fold_model_context()?
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
