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
        let mut next = self
            .session_seq
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("journal sequence overflow"))?;
        self.validation.apply_sequenced(id, record, &mut next)?;
        self.model_context.apply(record);
        self.usage.apply(record);
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
}

pub(crate) struct SnapshotState {
    pub identity: StorageSnapshot,
    pub source: Box<dyn RecordSnapshot>,
    verified: bool,
    verified_version: Option<crate::backend::SourceVersion>,
    page_after: Option<u64>,
    pending: Option<JournalRecord>,
}
impl SnapshotState {
    pub fn new(
        identity: StorageSnapshot,
        source: Box<dyn RecordSnapshot>,
        verified: bool,
    ) -> Result<Self> {
        let verified_version = source.source_version()?;
        Ok(Self {
            identity,
            source,
            verified,
            verified_version,
            page_after: None,
            pending: None,
        })
    }
    /// Reusing a resident semantic projection is safe only for a fully verified
    /// exact prefix. A cheap warm descriptor alone is never sufficient.
    pub fn is_verified_unchanged(&self) -> Result<bool> {
        Ok(self.verified && self.source.source_version()? == self.verified_version)
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
        while let Some(record) = self.source.next()? {
            if record.session_seq > prefix.revision.session_seq {
                continue;
            }
            state.apply(self.identity.session.session_id, &record)?;
        }
        Ok(state.revision() == prefix.revision)
    }
    pub fn fold(&mut self) -> Result<Fold> {
        self.reset_page_cursor();
        self.verified = false;
        let version = self.source.source_version()?;
        self.source.reset()?;
        let mut state = Fold::default();
        while let Some(record) = self.source.next()? {
            state.apply(self.identity.session.session_id, &record)?;
        }
        ensure!(
            state.revision() == self.identity.revision,
            "snapshot revision changed"
        );
        if self.source.source_version()? != version {
            self.source.verify_fixed_prefix()?;
        }
        self.verified = true;
        self.verified_version = self.source.source_version()?;
        Ok(state)
    }
    pub fn page(&mut self, after: u64, limit: u32) -> Result<StorageRecordPage> {
        ensure!(
            after <= self.identity.revision.session_seq,
            "record cursor is past the snapshot prefix"
        );
        // A warm snapshot is only a cheap provisional descriptor. Validate its
        // complete fixed prefix once before exposing any canonical records.
        if !self.verified || self.source.source_version()? != self.verified_version {
            self.fold()?;
        }
        if self.page_after != Some(after) {
            self.source.reset()?;
            self.pending = None;
        }
        // Failed pages leave no reusable cursor; a retry must restart safely.
        self.page_after = None;
        let page_version = self.source.source_version()?;
        let mut records = Vec::new();
        let mut bytes = 0_usize;
        while records.len() < limit as usize {
            let record = match self.pending.take() {
                Some(record) => record,
                None => match self.source.next()? {
                    Some(record) => record,
                    None => break,
                },
            };
            if record.session_seq <= after {
                continue;
            }
            let record_bytes = encoded_bytes(&record)?;
            ensure!(
                record_bytes <= SESSION_STORAGE_MAX_RECORD_BYTES,
                "canonical record exceeds maximum"
            );
            if !records.is_empty() && bytes + record_bytes > SESSION_STORAGE_PAGE_BYTE_BUDGET {
                self.pending = Some(record);
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
        self.page_after = Some(last);
        let next_after_seq = (last < self.identity.revision.session_seq).then_some(last);
        Ok(StorageRecordPage {
            snapshot: self.identity.clone(),
            records,
            next_after_seq,
        })
    }
    pub fn conversation_at(&mut self, point: StorageHistoryPoint) -> Result<StorageConversation> {
        if point == StorageHistoryPoint::Current {
            let state = self.fold()?;
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
        while let Some(record) = self.source.next()? {
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
            state.apply(self.identity.session.session_id, &record)?;
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
    let text = |message: &CanonicalMessage| {
        message.parts.iter().find_map(|part| match &part.payload {
            ContentPart::Text { text }
            | ContentPart::ReasoningSummary { text }
            | ContentPart::Reasoning { text, .. } => {
                let text = text.trim();
                (!text.is_empty()).then(|| text.to_owned())
            }
            ContentPart::ToolResult { result } => {
                let text = result.text_or_status();
                let text = text.trim();
                (!text.is_empty()).then(|| text.to_owned())
            }
            _ => None,
        })
    };
    let selected = messages
        .iter()
        .filter(|m| m.role == MessageRole::User)
        .find_map(text)
        .or_else(|| messages.iter().find_map(text))?;
    Some(if selected.chars().count() <= 160 {
        selected
    } else {
        format!("{}...", selected.chars().take(160).collect::<String>())
    })
}

fn encoded_bytes(value: &impl serde::Serialize) -> Result<usize> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("encoded record size overflow"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Counter(0);
    serde_json::to_writer(&mut count, value)?;
    Ok(count.0)
}
