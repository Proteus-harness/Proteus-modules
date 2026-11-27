//! Independent durable representations. Semantic validation is shared, not a file format.
use crate::types::*;
use anyhow::Result;
use proteus_contracts::domain::SessionId;
use std::path::{Path, PathBuf};

mod jsonl;
mod sqlite;
pub(crate) use jsonl::Jsonl;
pub(crate) use sqlite::{FILE as SQLITE_JOURNAL_FILE, Sqlite};

#[derive(Clone)]
pub(crate) struct PrefixHint {
    pub extent: u64,
    pub fingerprint: String,
}

pub(crate) struct Append {
    pub extent: u64,
    pub bytes: Vec<u8>,
    pub blobs: Vec<PathBuf>,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SourceVersion {
    pub bytes: u64,
    pub modified: std::time::SystemTime,
    pub changed: (i64, i64),
}

type DisplayRowVisitor<'a> = dyn FnMut(&JournalRecord, Option<String>) -> Result<()> + 'a;

/// Physical position and incremental digest within one pinned snapshot.
pub(crate) struct PageCursor {
    position: u64,
    hash: ring::digest::Context,
}

pub(crate) trait RecordSnapshot: Send {
    fn page_cursor(&self) -> PageCursor;
    /// Position in a pinned complete extent, not a substitute for EOF checks.
    fn at_pinned_end(&self) -> bool {
        false
    }
    fn restore_page_cursor(&mut self, cursor: PageCursor) -> Result<()>;
    /// None means the backend transaction already pins an immutable view.
    fn source_version(&self) -> Result<Option<SourceVersion>> {
        Ok(None)
    }
    /// Verify the pinned bytes after physical metadata changes. Appended suffixes
    /// are allowed; mutations of the fixed prefix are not.
    fn verify_fixed_prefix(&self) -> Result<()> {
        Ok(())
    }
    fn reset(&mut self) -> Result<()>;
    /// One boundary recorded during a full validation of this immutable source.
    fn remember_page_boundary(&mut self, _after: u64) {}
    fn reset_for_page(&mut self, _after: u64) -> Result<()> {
        self.reset()
    }
    /// Digest from the same validation stream used for the semantic fold.
    fn consumed_fingerprint(&self) -> String {
        crate::io::digest_hex(self.read_hash().finish())
    }
    /// Reconfirm all canonical data before reusing semantic validation.
    fn confirm_verified_data(&self) -> Result<bool> {
        Ok(false)
    }
    fn next(&mut self) -> Result<Option<JournalRecord>>;
    /// Full sequential replay may lend reusable raw-record storage. It is not
    /// part of the snapshot identity and is not needed by every backend.
    fn next_with_scratch(&mut self, _line: &mut Vec<u8>) -> Result<Option<JournalRecord>> {
        self.next()
    }
    fn next_capturing_external(
        &mut self,
        line: &mut Vec<u8>,
        _witness: &mut Option<crate::codec::ExternalWitness>,
    ) -> Result<Option<JournalRecord>> {
        self.next_with_scratch(line)
    }
    fn next_reauthenticating_external(
        &mut self,
        _witness: crate::codec::ExternalWitness,
        record: JournalRecord,
    ) -> Result<Option<JournalRecord>> {
        drop(record);
        self.next()
    }
    /// Opt in to a backend-specific validation stream. False means that
    /// no rows or cursor state have been touched and the caller must replay.
    fn try_visit_display_rows(
        &mut self,
        _cut: Option<u64>,
        _visit: &mut DisplayRowVisitor<'_>,
    ) -> Result<bool> {
        Ok(false)
    }
    fn fingerprint(&self) -> &str;
    /// Digest of bytes consumed by `next`. A complete validated replay seeds the writer head.
    fn read_hash(&self) -> ring::digest::Context;
    fn extent(&self) -> u64;
    fn prefix_fingerprint(&mut self, sequence: u64) -> Result<Option<String>>;
}

pub(crate) trait JournalBackend: Send {
    fn snapshot(&mut self, dir: &Path, id: SessionId) -> Result<Box<dyn RecordSnapshot>>;
    fn snapshot_warm(
        &mut self,
        dir: &Path,
        id: SessionId,
        hint: &PrefixHint,
    ) -> Result<Box<dyn RecordSnapshot>>;
    /// Caller holds the persistent writer lease. Only recovery may repair a torn tail.
    fn recover(&mut self, dir: &Path, id: SessionId) -> Result<()>;
    fn append(&mut self, dir: &Path, record: &JournalRecord) -> Result<Append>;
}

pub(crate) fn replay_records<T: Default>(
    source: &mut dyn RecordSnapshot,
    cut: Option<u64>,
    apply: &impl Fn(&mut T, &JournalRecord, Option<String>) -> Result<()>,
) -> Result<T> {
    replay_records_with(source, cut, T::default(), &|fold, record, hash, _| {
        apply(fold, record, hash)
    })
}

pub(crate) fn replay_records_with<T>(
    source: &mut dyn RecordSnapshot,
    cut: Option<u64>,
    mut fold: T,
    apply: &impl Fn(&mut T, &JournalRecord, Option<String>, &dyn RecordSnapshot) -> Result<()>,
) -> Result<T> {
    let mut line = Vec::new();
    while let Some(record) = source.next_with_scratch(&mut line)? {
        let boundary = (cut == Some(record.session_seq)).then(|| {
            source.remember_page_boundary(record.session_seq);
            source.consumed_fingerprint()
        });
        apply(&mut fold, &record, boundary, source)?;
    }
    Ok(fold)
}
