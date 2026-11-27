use crate::{backend::RecordSnapshot, types::*};
use anyhow::{Result, ensure};
use proteus_contracts::{domain::SessionId, model_standard::CanonicalMessage};
use proteus_session_semantics::{JournalValidationState, ModelContextProjection, UsageProjection};

#[derive(Default)]
pub(crate) struct Fold {
    pub validation: JournalValidationState,
    pub model_context: ModelContextProjection,
    pub usage: UsageProjection,
    pub session_seq: u64,
}
impl Fold {
    pub fn apply(&mut self, id: SessionId, record: &JournalRecord) -> Result<()> {
        self.apply_projected::<true, true>(id, record)
    }

    /// Validate every journal fact without constructing unused query projections.
    pub fn apply_validated(&mut self, id: SessionId, record: &JournalRecord) -> Result<()> {
        self.apply_projected::<false, false>(id, record)
    }

    fn apply_projected<const MODEL_CONTEXT: bool, const USAGE: bool>(
        &mut self,
        id: SessionId,
        record: &JournalRecord,
    ) -> Result<()> {
        let mut next = self
            .session_seq
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("journal sequence overflow"))?;
        self.validation.apply_sequenced(id, record, &mut next)?;
        if MODEL_CONTEXT {
            self.model_context.apply(record);
        }
        if USAGE {
            self.usage.apply(record);
        }
        self.session_seq = record.session_seq;
        Ok(())
    }
    pub fn revision(&self) -> StorageRevision {
        StorageRevision {
            session_seq: self.session_seq,
            history_revision: self.validation.history_revision(),
        }
    }
    pub fn conversation(&self) -> StorageConversation {
        StorageConversation {
            messages: self.validation.history().to_vec(),
            revision: self.revision(),
            model_context: self.model_context.snapshot(),
        }
    }
    pub fn resume(&self, id: SessionId) -> StorageResumeState {
        StorageResumeState {
            conversation: self.conversation(),
            usage: self.usage.snapshot(id),
            last_history_turn: self.validation.last_history_turn(),
            interrupted_turns: self.validation.interrupted_turns(),
        }
    }
    pub fn into_resume(self, id: SessionId) -> StorageResumeState {
        let revision = self.revision();
        let last_history_turn = self.validation.last_history_turn();
        let interrupted_turns = self.validation.interrupted_turns();
        StorageResumeState {
            conversation: StorageConversation {
                messages: self.validation.into_history(),
                revision,
                model_context: self.model_context.snapshot(),
            },
            usage: self.usage.snapshot(id),
            last_history_turn,
            interrupted_turns,
        }
    }
}

pub(crate) struct SnapshotState {
    pub identity: StorageSnapshot,
    pub source: Box<dyn RecordSnapshot>,
    // At most one historical boundary, derived inside the same validation
    // pass as identity. No caller-supplied descriptor or Fold is retained.
    previous_boundary: Option<(StorageRevision, String)>,
    // One successful historical comparison, not proof of full semantic replay.
    compared_boundary: Option<(StorageRevision, String)>,
    verified: bool,
    verified_version: Option<crate::backend::SourceVersion>,
    page_after: Option<u64>,
    pending: Option<PendingPageRecord>,
    pub(crate) display: Option<crate::display::DisplayRecords>,
    served_record_bytes: usize,
}
// A lone sequential reader reuses its look-ahead without another disk read.
// Once other snapshots are open, cap the encoded size retained by each
// additional reader. At most one previously lone reader can still own a large row.
const SHARED_BOUNDARY_BYTES: usize = 64 * 1024;

enum PendingPageRecord {
    Buffered {
        record: JournalRecord,
        encoded_bytes: Option<usize>,
    },
    OnSource {
        encoded_bytes: usize,
    },
}

impl SnapshotState {
    pub fn new(
        identity: StorageSnapshot,
        source: Box<dyn RecordSnapshot>,
        verified: bool,
        previous_boundary: Option<(StorageRevision, String)>,
    ) -> Result<Self> {
        let verified_version = source.source_version()?;
        Ok(Self {
            identity,
            source,
            verified,
            previous_boundary,
            compared_boundary: None,
            verified_version,
            page_after: None,
            pending: None,
            display: None,
            served_record_bytes: 0,
        })
    }
    /// Reusing a resident semantic projection is safe only for a fully verified
    /// exact prefix. A cheap warm descriptor alone is never sufficient.
    pub fn is_verified_unchanged(&self) -> Result<bool> {
        Ok(self.verified && self.source.source_version()? == self.verified_version)
    }
    pub(crate) fn released_large_payload(&self) -> bool {
        self.served_record_bytes >= SESSION_STORAGE_PAGE_BYTE_BUDGET
            || self
                .display
                .as_ref()
                .is_some_and(|d| d.bytes >= SESSION_STORAGE_PAGE_BYTE_BUDGET as u64)
    }

    fn reset_page_cursor(&mut self) {
        self.page_after = None;
        self.pending = None;
    }
    pub fn matches_prefix(&mut self, prefix: &StorageSnapshot) -> Result<bool> {
        self.reset_page_cursor();
        if prefix.session != self.identity.session
            || prefix.revision.session_seq > self.identity.revision.session_seq
        {
            return Ok(false);
        }
        if self.verified {
            let known = if prefix.revision.session_seq == self.identity.revision.session_seq {
                Some((&self.identity.revision, &self.identity.content_sha256))
            } else {
                self.previous_boundary
                    .as_ref()
                    .filter(|(revision, _)| revision.session_seq == prefix.revision.session_seq)
                    .map(|(revision, hash)| (revision, hash))
            };
            if let Some((revision, hash)) = known
                && self.source.confirm_verified_data()?
            {
                return Ok(*revision == prefix.revision && *hash == prefix.content_sha256);
            }
        }
        if self.compared_boundary.as_ref().is_some_and(|(revision, hash)| {
            *revision == prefix.revision && *hash == prefix.content_sha256
        }) && matches!(self.source.confirm_verified_data(), Ok(true))
        {
            return Ok(true);
        }
        // If reauthentication could not prove equality, keep the original
        // replay (including its precise corruption and I/O errors).
        let Some(hash) = self
            .source
            .prefix_fingerprint(prefix.revision.session_seq)?
        else {
            return Ok(false);
        };
        if hash != prefix.content_sha256 {
            return Ok(false);
        }
        self.source.reset()?;
        let mut state = Fold::default();
        let mut line = Vec::new();
        while let Some(record) = self.source.next_with_scratch(&mut line)? {
            if record.session_seq > prefix.revision.session_seq {
                continue;
            }
            // The comparison needs the verified revision, not model context or usage.
            state.apply_validated(self.identity.session.session_id, &record)?;
        }
        let revision = state.revision();
        let matches = revision == prefix.revision;
        if matches && revision.session_seq < self.identity.revision.session_seq {
            self.compared_boundary = Some((revision, hash));
        }
        Ok(matches)
    }
    pub fn fold_model_context(&mut self) -> Result<ModelContextProjection> {
        Ok(self.fold_with_terminal::<true, false>(false)?.0.model_context)
    }

    /// Usage has no dependency on the separate model-context projection.
    /// Both paths still validate the same complete fixed canonical prefix.
    pub fn fold_usage(&mut self) -> Result<UsageProjection> {
        Ok(self.fold_with_terminal::<false, true>(false)?.0.usage)
    }

    fn fold_with_terminal<const MODEL_CONTEXT: bool, const USAGE: bool>(
        &mut self,
        capture: bool,
    ) -> Result<(Fold, Option<JournalRecord>)> {
        self.reset_page_cursor();
        self.verified = false;
        let version = self.source.source_version()?;
        self.source.reset()?;
        let mut state = Fold::default();
        let mut terminal = None;
        let mut line = Vec::new();
        while let Some(record) = self.source.next_with_scratch(&mut line)? {
            state.apply_projected::<MODEL_CONTEXT, USAGE>(
                self.identity.session.session_id,
                &record,
            )?;
            if capture && record.session_seq == self.identity.revision.session_seq {
                terminal = Some(record);
            }
        }
        drop(line);
        ensure!(
            state.revision() == self.identity.revision,
            "snapshot revision changed"
        );
        if self.source.source_version()? != version {
            self.source.verify_fixed_prefix()?;
        }
        self.verified = true;
        self.verified_version = self.source.source_version()?;
        Ok((state, terminal))
    }
    pub fn page(
        &mut self,
        after: u64,
        limit: u32,
        sole_snapshot: bool,
    ) -> Result<(Vec<JournalRecord>, Option<u64>)> {
        ensure!(
            after <= self.identity.revision.session_seq,
            "record cursor is past the snapshot prefix"
        );
        // A warm snapshot is only a cheap provisional descriptor. Validate its
        // complete fixed prefix once before exposing any canonical records.
        let before = self.source.source_version()?;
        let mut terminal = None;
        if !self.verified || before != self.verified_version {
            let capture =
                limit > 0 && self.identity.revision.session_seq.checked_sub(1) == Some(after);
            // Canonical pages need full verification, not the derived views.
            terminal = self.fold_with_terminal::<false, false>(capture)?.1;
        }
        // Failed pages leave no reusable cursor. Cover repositioning as well
        // as streaming with the same source-version check.
        let continuing = self.page_after.take() == Some(after);
        let page_version = self.source.source_version()?;
        // For an exact terminal page, the complete fold already decoded the
        // one result and left the reader at EOF. Only hand it to the existing
        // page checks after the backend confirms the pinned data (by a
        // read transaction or by authenticating the complete prefix).
        // Blob-bearing JSONL, changed or unknown sources keep the normal scan.
        if terminal.is_some() && before == page_version && self.source.confirm_verified_data()? {
            self.pending = terminal.map(|record| PendingPageRecord::Buffered {
                record,
                encoded_bytes: None,
            });
        } else if !continuing {
            if terminal.is_some() && before != page_version {
                self.source.verify_fixed_prefix()?;
            }
            drop(terminal);
            self.pending = None;
            self.source.reset_for_page(after)?;
        }
        let mut records = Vec::new();
        let mut bytes = 0_usize;
        let mut line = Vec::new();
        while records.len() < limit as usize {
            let cursor = (!sole_snapshot && !records.is_empty()).then(|| self.source.page_cursor());
            let (record, known_bytes) = match self.pending.take() {
                Some(PendingPageRecord::Buffered {
                    record,
                    encoded_bytes,
                }) => (record, encoded_bytes),
                Some(PendingPageRecord::OnSource { encoded_bytes }) => {
                    let record = self
                        .source
                        .next_with_scratch(&mut line)?
                        .ok_or_else(|| anyhow::anyhow!("missing deferred journal record"))?;
                    (record, Some(encoded_bytes))
                }
                None => match self.source.next_with_scratch(&mut line)? {
                    Some(record) => (record, None),
                    None => break,
                },
            };
            if record.session_seq <= after {
                continue;
            }
            let record_bytes = match known_bytes {
                Some(bytes) => bytes,
                None => crate::codec::encoded_bytes(&record)?,
            };
            ensure!(
                record_bytes <= SESSION_STORAGE_MAX_RECORD_BYTES,
                "canonical record exceeds maximum"
            );
            if !records.is_empty() && bytes + record_bytes > SESSION_STORAGE_PAGE_BYTE_BUDGET {
                self.pending = Some(if sole_snapshot || record_bytes <= SHARED_BOUNDARY_BYTES {
                    PendingPageRecord::Buffered {
                        record,
                        encoded_bytes: Some(record_bytes),
                    }
                } else {
                    self.source
                        .restore_page_cursor(cursor.expect("nonempty page has a cursor"))?;
                    PendingPageRecord::OnSource {
                        encoded_bytes: record_bytes,
                    }
                });
                break;
            }
            bytes += record_bytes;
            records.push(record);
        }
        if self.source.source_version()? != page_version {
            self.source.verify_fixed_prefix()?;
            self.verified_version = self.source.source_version()?;
        }
        let last = records.last().map_or(after, |record| record.session_seq);
        self.served_record_bytes = self.served_record_bytes.saturating_add(bytes);
        self.page_after = Some(last);
        let next_after_seq = (last < self.identity.revision.session_seq).then_some(last);
        Ok((records, next_after_seq))
    }
    pub(crate) fn record_page(
        &self,
        snapshot: StorageSnapshot,
        records: Vec<JournalRecord>,
        next_after_seq: Option<u64>,
    ) -> StorageRecordPage {
        // The service already checked this live handle exactly. Do not keep
        // spare capacity carried by an in-process caller in the response.
        let compact = [
            &snapshot.snapshot_id,
            &snapshot.session.key,
            &snapshot.content_sha256,
        ]
        .into_iter()
        .all(|text| text.capacity() == text.len());
        StorageRecordPage {
            snapshot: if compact { snapshot } else { self.identity.clone() },
            records,
            next_after_seq,
        }
    }
    pub fn display_page(
        &mut self,
        after: u64,
        limit: u32,
    ) -> Result<(Vec<JournalRecord>, Option<u64>)> {
        let version = self.source.source_version()?;
        if version != self.verified_version {
            self.source.verify_fixed_prefix()?;
            self.verified_version = self.source.source_version()?;
        }
        let page = self
            .display
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("snapshot has no display projection"))?
            .page(self.identity.revision.session_seq, after, limit)?;
        if self.source.source_version()? != self.verified_version {
            self.source.verify_fixed_prefix()?;
            self.verified_version = self.source.source_version()?;
        }
        Ok(page)
    }
    pub fn conversation_at(&mut self, point: StorageHistoryPoint) -> Result<StorageConversation> {
        if point == StorageHistoryPoint::Current {
            // The pinned conversation response needs model context, not usage.
            let state = self.fold_with_terminal::<true, false>(false)?.0;
            return Ok(StorageConversation {
                revision: state.revision(),
                model_context: state.model_context.snapshot(),
                messages: state.validation.into_history(),
            });
        }
        self.reset_page_cursor();
        self.source.reset()?;
        let mut state = Fold::default();
        let mut selected = matches!(
            point,
            StorageHistoryPoint::Revision {
                history_revision: 0
            }
        )
        .then(|| state.conversation());
        let mut line = Vec::new();
        while let Some(record) = self.source.next_with_scratch(&mut line)? {
            if let JournalEntry::HistoryMutated(mutation) = &record.entry {
                let compacted = mutation
                    .compaction
                    .as_ref()
                    .is_some_and(|report| report.changed);
                if mutation.mutation == HistoryMutationKind::Replace && !compacted {
                    // A deliberate reset must not resurrect the discarded branch.
                    selected = None;
                }
                if let StorageHistoryPoint::BeforeReplacement { part_id } = point {
                    if compacted
                        && mutation
                            .messages
                            .iter()
                            .any(|m| m.parts.iter().any(|p| p.part_id == part_id))
                        && !state
                            .validation
                            .history()
                            .iter()
                            .any(|m| m.parts.iter().any(|p| p.part_id == part_id))
                    {
                        selected = Some(state.conversation());
                    }
                }
            }
            state.apply_projected::<true, false>(self.identity.session.session_id, &record)?;
            if let StorageHistoryPoint::Revision { history_revision } = point {
                if selected.is_none() && state.validation.history_revision() == history_revision {
                    selected = Some(state.conversation());
                }
            }
        }
        // Finish the immutable prefix verification before exposing a prior state.
        selected.ok_or_else(|| {
            anyhow::anyhow!(
                "requested history revision/replacement is unavailable after the latest reset"
            )
        })
    }
}

pub(crate) fn preview(messages: &[CanonicalMessage]) -> Option<String> {
    use proteus_contracts::model_standard::{ContentPart, MessageRole};
    fn excerpt(text: &str) -> Option<String> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        const LIMIT: usize = 160;
        if text.len() > LIMIT
            && let Some((end, _)) = text.char_indices().nth(LIMIT)
        {
            let mut short = String::with_capacity(end + 3);
            short.push_str(&text[..end]);
            short.push_str("...");
            return Some(short);
        }
        Some(text.to_owned())
    }

    let text = |message: &CanonicalMessage| {
        message.parts.iter().find_map(|part| match &part.payload {
            ContentPart::Text { text }
            | ContentPart::ReasoningSummary { text }
            | ContentPart::Reasoning { text, .. } => excerpt(text),
            ContentPart::ToolResult { result } => {
                let text = result
                    .error
                    .as_deref()
                    .filter(|text| !text.is_empty())
                    .or_else(|| (!result.output.is_empty()).then_some(result.output.as_str()));
                match text {
                    Some(text) => excerpt(text),
                    None => excerpt(&result.text_or_status()),
                }
            }
            _ => None,
        })
    };
    messages
        .iter()
        .filter(|m| m.role == MessageRole::User)
        .find_map(text)
        .or_else(|| messages.iter().find_map(text))
}
