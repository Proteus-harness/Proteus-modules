use super::*;
use proteus_contracts::domain::RecordId;
impl State {
    pub(super) fn commit(&mut self, commit: StorageCommit) -> Result<SessionStorageResponse> {
        let mut record = JournalRecord {
            schema_version: JOURNAL_SCHEMA_VERSION,
            record_id: commit.operation_id,
            session_seq: commit
                .expected
                .session_seq
                .checked_add(1)
                .context("journal sequence overflow")?,
            timestamp_ms: commit.timestamp_ms,
            session_id: commit.lease.session.session_id,
            execution_id: commit.attribution.execution_id,
            thread_id: commit.attribution.thread_id,
            turn_id: commit.attribution.turn_id,
            entry: commit.entry,
        };
        if let JournalEntry::HookInvoked(trace) = &record.entry {
            proteus_session_semantics::journal::hooks::validate_raw_trace(&record, trace)?;
        }
        record.entry = crate::codec::canonicalize_entry(record.entry)?;
        let fold = self.writer_fold(&commit.lease)?;
        let actual = fold.revision();
        if actual != commit.expected || fold.validation.contains_record_id(&record.record_id) {
            if let Some(ack) = self.commit_status(&commit.lease.session, commit.operation_id)? {
                let previous_history = match &ack.record.entry {
                    JournalEntry::HistoryMutated(m) => m.previous_revision,
                    _ => ack.revision.history_revision,
                };
                ensure!(
                    ack.record.session_seq == record.session_seq
                        && previous_history == commit.expected.history_revision,
                    "operation retry changed its original expected revision"
                );
                ensure!(
                    ack.record.entry == record.entry
                        && ack.record.timestamp_ms == commit.timestamp_ms
                        && ack.record.execution_id == commit.attribution.execution_id
                        && ack.record.thread_id == commit.attribution.thread_id
                        && ack.record.turn_id == commit.attribution.turn_id,
                    "storage operation id conflicts with earlier content"
                );
                return Ok(SessionStorageResponse::Committed { ack });
            }
            bail!(
                "storage revision conflict: expected {:?}, actual {:?}",
                commit.expected,
                actual
            );
        }
        let dir = self.writer_path(&commit.lease)?;
        let kind = self.kind;
        let writer = self.writer(&commit.lease)?;
        if let Some(head) = writer.head.as_ref()
            && catalog::source_stamp(&dir, kind).ok().as_ref() != Some(&head.stamps)
        {
            writer.head = None;
        }
        // This state is private until ACK. On any failure discard it and replay
        // the durable prefix before the next operation, instead of cloning the
        // entire conversation for every non-history journal fact.
        let result = (|| {
            writer
                .fold
                .as_mut()
                .expect("writer initialized")
                .apply(record.session_id, &record)?;
            writer.backend.append(&dir, &record)
        })();
        match result {
            Ok(append) => {
                writer.head = writer.head.take().and_then(|mut head| {
                    head.advance(&dir, kind, append)?;
                    Some(head)
                });
            }
            Err(error) => {
                writer.fold = None;
                writer.head = None;
                return Err(error);
            }
        };
        let fold = writer.fold.as_ref().expect("writer initialized");
        Ok(SessionStorageResponse::Committed {
            ack: StorageCommitAck {
                operation_id: commit.operation_id,
                revision: fold.revision(),
                record,
            },
        })
    }
    pub(super) fn commit_status(
        &mut self,
        session: &SessionLocator,
        id: RecordId,
    ) -> Result<Option<StorageCommitAck>> {
        let dir = self.checked_path(session)?;
        let mut source = self.kind.backend().snapshot(&dir, session.session_id)?;
        let mut fold = Fold::default();
        let mut found = None;
        while let Some(record) = source.next()? {
            // An ACK needs the validated revision and original record, not the
            // model-context or usage views. Still validate the entire suffix.
            fold.apply_validated(session.session_id, &record)?;
            if record.record_id == id {
                found = Some(StorageCommitAck {
                    operation_id: id,
                    revision: fold.revision(),
                    record,
                });
            }
        }
        Ok(found)
    }
}
