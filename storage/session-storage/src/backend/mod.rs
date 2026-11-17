//! Independent durable representations. Semantic validation is shared, not a file format.
use crate::types::*;
use anyhow::Result;
use proteus_contracts::domain::SessionId;
use std::path::Path;

mod jsonl;
mod sqlite;
pub(crate) use jsonl::Jsonl;
pub(crate) use sqlite::Sqlite;

#[derive(Clone)]
pub(crate) struct PrefixHint {
    pub extent: u64,
    pub fingerprint: String,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SourceVersion {
    pub bytes: u64,
    pub modified: std::time::SystemTime,
    pub changed: (i64, i64),
}

pub(crate) trait RecordSnapshot: Send {
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
    fn next(&mut self) -> Result<Option<JournalRecord>>;
    fn fingerprint(&self) -> &str;
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
    fn append(&mut self, dir: &Path, record: &JournalRecord) -> Result<()>;
    fn description_path(&self, dir: &Path) -> std::path::PathBuf;
}
