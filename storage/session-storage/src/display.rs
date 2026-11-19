//! Snapshot-local display records captured during the complete semantic fold.
use crate::types::*;
use anyhow::{Result, ensure};
use std::{
    fs::File,
    io::{self, BufRead, BufReader, BufWriter, Seek, Write},
};

pub(crate) const MAX_STAGING_BYTES: u64 = 512 * 1024 * 1024;

pub(crate) struct DisplayWriter {
    writer: BufWriter<File>,
    budget: u64,
    remaining: u64,
}

impl DisplayWriter {
    pub fn new(budget: u64) -> Option<Self> {
        Some(Self {
            writer: BufWriter::new(tempfile::tempfile().ok()?),
            budget,
            remaining: budget,
        })
    }

    pub fn push(&mut self, record: &JournalRecord) -> Result<()> {
        let before = self.remaining;
        serde_json::to_writer(&mut *self, record)?;
        ensure!(
            before - self.remaining <= SESSION_STORAGE_MAX_RECORD_BYTES as u64,
            "display record exceeds maximum"
        );
        self.write_all(b"\n")?;
        Ok(())
    }

    pub fn finish(self) -> Option<DisplayRecords> {
        Some(DisplayRecords {
            reader: BufReader::new(self.writer.into_inner().ok()?),
            bytes: self.budget - self.remaining,
            after: None,
            pending: None,
        })
    }
}

impl Write for DisplayWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(io::Error::other("display staging exceeds byte budget"));
        }
        let written = self.writer.write(bytes)?;
        self.remaining -= written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

pub(crate) struct DisplayRecords {
    reader: BufReader<File>,
    pub(crate) bytes: u64,
    after: Option<u64>,
    pending: Option<(JournalRecord, usize)>,
}

impl DisplayRecords {
    fn next(&mut self) -> Result<Option<(JournalRecord, usize)>> {
        let mut line = Vec::new();
        if self.reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(None);
        }
        ensure!(line.ends_with(b"\n"), "incomplete display staging record");
        let bytes = line.len() - 1;
        Ok(Some((serde_json::from_slice(&line)?, bytes)))
    }

    pub fn page(
        &mut self,
        snapshot: &StorageSnapshot,
        after: u64,
        limit: u32,
    ) -> Result<StorageRecordPage> {
        ensure!(
            after <= snapshot.revision.session_seq,
            "display cursor is past the snapshot prefix"
        );
        if self.after != Some(after) {
            self.reader.rewind()?;
            self.pending = None;
        }
        self.after = None;
        let mut records = Vec::new();
        let mut bytes = 0;
        let mut finished = false;
        while records.len() < limit as usize {
            let next = match self.pending.take() {
                Some(record) => Some(record),
                None => self.next()?,
            };
            let Some((record, record_bytes)) = next else {
                finished = true;
                break;
            };
            if record.session_seq <= after {
                continue;
            }
            if !records.is_empty() && bytes + record_bytes > SESSION_STORAGE_PAGE_BYTE_BUDGET {
                self.pending = Some((record, record_bytes));
                break;
            }
            bytes += record_bytes;
            records.push(record);
        }
        let last = records.last().map_or(after, |record| record.session_seq);
        self.after = Some(last);
        Ok(StorageRecordPage {
            snapshot: snapshot.clone(),
            records,
            next_after_seq: (!finished && last < snapshot.revision.session_seq).then_some(last),
        })
    }
}

pub(crate) fn state_fits(state: &StorageResumeState) -> bool {
    struct Limit(usize);
    impl Write for Limit {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.0 {
                return Err(io::Error::other("display state exceeds byte budget"));
            }
            self.0 -= bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Limit(SESSION_STORAGE_MAX_DISPLAY_STATE_BYTES), state).is_ok()
}
