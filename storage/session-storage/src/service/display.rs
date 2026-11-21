//! Capture display state in the same strict pass that validates its source.
use super::*;
use crate::display::{DisplayWriter, MAX_STAGING_BYTES, state_fits};

impl State {
    pub(super) fn open_display_snapshot(
        &mut self,
        session: SessionLocator,
    ) -> Result<SessionStorageResponse> {
        let dir = self.checked_path(&session)?;
        let descriptor = self.describe(&session)?;
        let stamps = catalog::source_stamp(&dir, self.kind)?;
        let blobs = catalog::durable::blob_stamps(&dir);
        let mut source = self.kind.backend().snapshot(&dir, session.session_id)?;
        let version = source.source_version()?;
        let cut = self
            .prefixes
            .get(&session)
            .map(|cache| cache.revision.session_seq);
        let mut state = Fold::default();
        let mut previous_boundary =
            (cut == Some(0)).then(|| (state.revision(), source.consumed_fingerprint()));
        let staged: u64 = self
            .snapshots
            .values()
            .filter_map(|snapshot| snapshot.display.as_ref())
            .map(|display| display.bytes)
            .sum();
        let budget = MAX_STAGING_BYTES.saturating_sub(staged);
        let mut display = match self.kind {
            BackendKind::Jsonl => DisplayWriter::for_jsonl(budget, source.extent()),
            BackendKind::Sqlite => DisplayWriter::new(budget),
        };
        let mut line = Vec::new();
        while let Some(record) = source.next_with_scratch(&mut line)? {
            state.apply(session.session_id, &record)?;
            if cut == Some(record.session_seq) {
                source.remember_page_boundary(record.session_seq);
                previous_boundary = Some((state.revision(), source.consumed_fingerprint()));
            }
            if record.entry.kind().is_display_record()
                && let Some(writer) = &mut display
                && writer.push(&record).is_err()
            {
                display = None;
            }
        }
        drop(line);
        if source.source_version()? != version {
            source.verify_fixed_prefix()?;
        }
        let identity = StorageSnapshot {
            session: session.clone(),
            snapshot_id: Uuid::new_v4().to_string(),
            revision: state.revision(),
            content_sha256: source.fingerprint().to_owned(),
        };
        if let Some(blobs) = blobs
            && catalog::durable::blob_stamps(&dir).as_ref() == Some(&blobs)
            && catalog::source_stamp(&dir, self.kind)? == stamps
        {
            let prefix = views::PrefixCache {
                stamps,
                revision: identity.revision,
                hint: crate::backend::PrefixHint {
                    extent: source.extent(),
                    fingerprint: identity.content_sha256.clone(),
                },
                blob_digest: Some(catalog::durable::blob_stamp_digest(&blobs)),
            };
            let messages = state.validation.history();
            let summary = StorageSessionSummary {
                descriptor,
                message_count: messages.len(),
                preview: crate::snapshot::preview(messages),
            };
            self.save_catalog_head(&dir, summary, &prefix, blobs);
        }
        let Some(display) = display.and_then(DisplayWriter::finish) else {
            return Ok(SessionStorageResponse::DisplaySnapshot { projection: None });
        };
        let state = state.into_resume(session.session_id);
        if !state_fits(&state) {
            return Ok(SessionStorageResponse::DisplaySnapshot { projection: None });
        }
        let mut snapshot = SnapshotState::new(identity.clone(), source, true, previous_boundary)?;
        snapshot.display = Some(display);
        self.remember_snapshot(snapshot);
        Ok(SessionStorageResponse::DisplaySnapshot {
            projection: Some(StorageDisplaySnapshot {
                snapshot: identity,
                state,
            }),
        })
    }
}
