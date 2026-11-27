//! Capture display state in the same strict pass that validates its source.
use super::*;
use crate::backend::RecordSnapshot;
use crate::display::{DisplayWriter, MAX_STAGING_BYTES, state_fits};
use proteus_contracts::model_standard::ContentPart;

fn text_reaches_page_budget(record: &JournalRecord) -> bool {
    let JournalEntry::HistoryMutated(change) = &record.entry else {
        return false;
    };
    let mut remaining = SESSION_STORAGE_PAGE_BYTE_BUDGET;
    // Every Text value here is serialized in the staged row. JSON escaping
    // can only increase its byte count, so this is a sufficient lower bound.
    for part in change.messages.iter().flat_map(|message| &message.parts) {
        if let ContentPart::Text { text } = &part.payload {
            remaining = remaining.saturating_sub(text.len());
            if remaining == 0 {
                return true;
            }
        }
    }
    false
}

impl State {
    pub(super) fn open_display_snapshot(
        &mut self,
        session: SessionLocator,
    ) -> Result<SessionStorageResponse> {
        let dir = self.checked_path(&session)?;
        let descriptor = self.describe(&session)?;
        let stamps = catalog::source_stamp(&dir, self.kind)?;
        let blobs = catalog::durable::blob_stamps(&dir);
        let cut = self
            .prefixes
            .get(&session)
            .map(|cache| cache.revision.session_seq);
        let staged: u64 = self
            .snapshots
            .values()
            .filter_map(|snapshot| snapshot.display.as_ref())
            .map(|display| display.bytes)
            .sum();
        let budget = MAX_STAGING_BYTES.saturating_sub(staged);
        let apply = |(state, previous_boundary, display): &mut (Fold, Option<_>, Option<DisplayWriter>),
                     record: &JournalRecord,
                     boundary,
                     replay_source: Option<&dyn RecordSnapshot>|
         -> Result<()> {
            state.apply(session.session_id, record)?;
            if let Some(hash) = boundary {
                *previous_boundary = Some((state.revision(), hash));
            }
            if record.entry.kind().is_display_record()
                && let Some(writer) = display
                && ((record.session_seq == 1
                    && state.revision().history_revision == 1
                    && replay_source.is_some_and(|source| source.at_pinned_end())
                    && text_reaches_page_budget(record))
                    || writer.push(record).is_err())
            {
                *display = None;
            }
            Ok(())
        };
        let (source, (state, previous_boundary, display)) = match self.kind {
            BackendKind::Jsonl if cfg!(unix) => Jsonl::replay_snapshot_with(
                &dir,
                session.session_id,
                cut,
                |extent| (Fold::default(), None, DisplayWriter::for_jsonl(budget, extent)),
                |fold, record, hash, source| apply(fold, record, hash, Some(source)),
            )?,
            BackendKind::Sqlite => Sqlite::replay_display(
                &dir,
                session.session_id,
                cut,
                || (Fold::default(), None, DisplayWriter::new(budget)),
                |fold, record, hash| apply(fold, record, hash, None),
            )?,
            BackendKind::Jsonl => {
                let mut source = self.kind.backend().snapshot(&dir, session.session_id)?;
                let version = source.source_version()?;
                let writer = DisplayWriter::for_jsonl(budget, source.extent());
                let mut staged = (Fold::default(), None, writer);
                if !source.try_visit_display_rows(cut, &mut |record, hash| {
                    apply(&mut staged, record, hash, None)
                })? {
                    let mut line = Vec::new();
                    while let Some(record) = source.next_with_scratch(&mut line)? {
                        let boundary = (cut == Some(record.session_seq)).then(|| {
                            source.remember_page_boundary(record.session_seq);
                            source.consumed_fingerprint()
                        });
                        apply(&mut staged, &record, boundary, None)?;
                    }
                }
                if source.source_version()? != version {
                    source.verify_fixed_prefix()?;
                }
                (source, staged)
            }
        };
        let previous_boundary = previous_boundary.or_else(|| {
            (cut == Some(0)).then(|| (StorageRevision::default(), crate::io::sha256_hex(b"")))
        });
        let identity = StorageSnapshot {
            session: session.clone(),
            snapshot_id: Uuid::new_v4().to_string(),
            revision: state.revision(),
            content_sha256: source.fingerprint().to_owned(),
        };
        if let Some(blobs) = blobs
            && catalog::durable::matches_blob_stamps(&dir, &blobs) == Some(true)
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
        let Some(display) = display.and_then(|writer| writer.finish_projection(identity.revision))
        else {
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
