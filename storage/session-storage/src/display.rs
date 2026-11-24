//! Snapshot-local display records captured during the complete semantic fold.
use crate::types::*;
use anyhow::{Result, ensure};
use std::{
    fs::File,
    io::{self, BufRead, BufReader, BufWriter, Seek, Write},
};

pub(crate) const MAX_STAGING_BYTES: u64 = 512 * 1024 * 1024;
// Keep at most one small scratch buffer per request, not per open snapshot.
const MAX_REUSABLE_LINE_CAPACITY: usize = 8 * 1024;

pub(crate) struct DisplayWriter {
    writer: BufWriter<File>,
    budget: u64,
    remaining: u64,
    // These are capacity hints from bytes successfully staged, not an index
    // of record identities or trusted read sizes.
    large_lines: Vec<(u64, usize)>,
}

const LARGE_JSONL_STAGE_MIN_SOURCE_BYTES: u64 = 1024 * 1024;
const LARGE_JSONL_STAGE_BUFFER_BYTES: usize = 64 * 1024;

impl DisplayWriter {
    pub fn new(budget: u64) -> Option<Self> {
        Self::open(budget, None)
    }

    pub fn for_jsonl(budget: u64, source_bytes: u64) -> Option<Self> {
        // Do not reserve a bigger transient buffer for short sources, or when
        // the remaining staging budget offers too little space to batch.
        if source_bytes >= LARGE_JSONL_STAGE_MIN_SOURCE_BYTES
            && budget >= LARGE_JSONL_STAGE_BUFFER_BYTES as u64
        {
            Self::open(budget, Some(LARGE_JSONL_STAGE_BUFFER_BYTES))
        } else {
            Self::new(budget)
        }
    }

    fn open(budget: u64, capacity: Option<usize>) -> Option<Self> {
        let file = tempfile::tempfile().ok()?;
        let writer = match capacity {
            Some(bytes) => BufWriter::with_capacity(bytes, file),
            None => BufWriter::new(file),
        };
        Some(Self {
            writer,
            budget,
            remaining: budget,
            large_lines: Vec::new(),
        })
    }

    pub fn push(&mut self, record: &JournalRecord) -> Result<()> {
        let before = self.remaining;
        serde_json::to_writer(&mut *self, record)?;
        let record_bytes = before - self.remaining;
        ensure!(
            record_bytes <= SESSION_STORAGE_MAX_RECORD_BYTES as u64,
            "display record exceeds maximum"
        );
        self.write_all(b"\n")?;
        // The aggregate staging budget caps the number of these hints.
        if record_bytes > SESSION_STORAGE_PAGE_BYTE_BUDGET as u64 {
            self.large_lines
                .push((self.budget - before, record_bytes as usize + 1));
        }
        Ok(())
    }

    pub fn finish(self) -> Option<DisplayRecords> {
        Some(DisplayRecords {
            reader: BufReader::new(self.writer.into_inner().ok()?),
            bytes: self.budget - self.remaining,
            after: None,
            pending: None,
            large_lines: (!self.large_lines.is_empty()).then(|| {
                Box::new(LargeStageLines {
                    lengths: self.large_lines,
                    offset: Some(0),
                })
            }),
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

#[cfg(test)]
#[test]
fn display_writer_batches_large_jsonl_and_keeps_small_default() {
    let small = DisplayWriter::new(MAX_STAGING_BYTES)
        .unwrap()
        .writer
        .capacity();
    assert_eq!(
        DisplayWriter::for_jsonl(MAX_STAGING_BYTES, LARGE_JSONL_STAGE_MIN_SOURCE_BYTES - 1)
            .unwrap()
            .writer
            .capacity(),
        small
    );
    assert_eq!(
        DisplayWriter::for_jsonl(
            LARGE_JSONL_STAGE_BUFFER_BYTES as u64 - 1,
            LARGE_JSONL_STAGE_MIN_SOURCE_BYTES
        )
        .unwrap()
        .writer
        .capacity(),
        small
    );
    assert_eq!(
        DisplayWriter::for_jsonl(MAX_STAGING_BYTES, LARGE_JSONL_STAGE_MIN_SOURCE_BYTES)
            .unwrap()
            .writer
            .capacity(),
        LARGE_JSONL_STAGE_BUFFER_BYTES
    );
}

#[cfg(test)]
#[test]
fn display_writer_keeps_stage_bytes_budget_and_failed_flush_fallback() {
    use proteus_contracts::{
        domain::*,
        model_standard::{CanonicalMessage, MessageRole},
    };
    let record = JournalRecord {
        schema_version: JOURNAL_SCHEMA_VERSION,
        session_id: new_session_id(),
        record_id: new_record_id(),
        session_seq: 1,
        timestamp_ms: 1,
        execution_id: None,
        thread_id: Some(new_thread_id()),
        turn_id: None,
        entry: JournalEntry::HistoryMutated(HistoryMutated {
            previous_revision: 0,
            new_revision: 1,
            mutation: HistoryMutationKind::Append,
            messages: vec![CanonicalMessage::text(
                MessageRole::User,
                "x".repeat(24 * 1024),
            )],
            compaction: None,
            tool_results: vec![],
        }),
    };
    let mut expected_line = serde_json::to_vec(&record).unwrap();
    expected_line.push(b'\n');
    for mut writer in [
        DisplayWriter::new(MAX_STAGING_BYTES).unwrap(),
        DisplayWriter::for_jsonl(MAX_STAGING_BYTES, LARGE_JSONL_STAGE_MIN_SOURCE_BYTES).unwrap(),
    ] {
        writer.push(&record).unwrap();
        let mut stage = writer.finish().unwrap();
        stage.reader.rewind().unwrap();
        let mut bytes = Vec::new();
        stage.reader.read_until(b'\n', &mut bytes).unwrap();
        assert!(
            bytes == expected_line,
            "stage bytes or final rewind changed"
        );
        assert_eq!(stage.bytes, expected_line.len() as u64);
        assert!(stage.reader.fill_buf().unwrap().is_empty());
    }
    let budget = LARGE_JSONL_STAGE_BUFFER_BYTES as u64;
    for mut writer in [
        DisplayWriter::new(budget).unwrap(),
        DisplayWriter::for_jsonl(budget, LARGE_JSONL_STAGE_MIN_SOURCE_BYTES).unwrap(),
    ] {
        for _ in 0..budget / expected_line.len() as u64 {
            writer.push(&record).unwrap();
        }
        assert!(writer.push(&record).is_err());
    }

    // Controlled local I/O failure on an ordinary file opened read-only.
    // An earlier error from push or a later error from finish are both a
    // private staging failure, never a published display snapshot.
    let file = tempfile::NamedTempFile::new().unwrap();
    let cap = DisplayWriter::new(MAX_STAGING_BYTES)
        .unwrap()
        .writer
        .capacity();
    for capacity in [cap, LARGE_JSONL_STAGE_BUFFER_BYTES] {
        let mut failing = DisplayWriter {
            writer: BufWriter::with_capacity(capacity, File::open(file.path()).unwrap()),
            budget: MAX_STAGING_BYTES,
            remaining: MAX_STAGING_BYTES,
            large_lines: Vec::new(),
        };
        let projection = failing.push(&record).ok().and_then(|()| failing.finish());
        assert!(projection.is_none());
    }
}

struct LargeStageLines {
    lengths: Vec<(u64, usize)>,
    // Reset on rewind and disabled after an ambiguous partial read error.
    offset: Option<u64>,
}

pub(crate) struct DisplayRecords {
    reader: BufReader<File>,
    pub(crate) bytes: u64,
    after: Option<u64>,
    pending: Option<(JournalRecord, usize)>,
    large_lines: Option<Box<LargeStageLines>>,
}

impl DisplayRecords {
    fn advance_large_line_offset(&mut self, bytes: usize) {
        if let Some(lines) = &mut self.large_lines {
            lines.offset = lines.offset.and_then(|start| start.checked_add(bytes as u64));
        }
    }

    fn next(&mut self, line: &mut Vec<u8>) -> Result<Option<(JournalRecord, usize)>> {
        fn decode(line: &[u8]) -> Result<(JournalRecord, usize)> {
            ensure!(line.ends_with(b"\n"), "incomplete display staging record");
            let bytes = line.len() - 1;
            Ok((serde_json::from_slice(line)?, bytes))
        }

        line.clear();
        // Complete records already in the private stage buffer need no copy.
        match self.reader.fill_buf() {
            Ok([]) => return Ok(None),
            Ok(buffered) => {
                if let Some(end) = memchr::memchr(b'\n', buffered) {
                    let count = end + 1;
                    let result = decode(&buffered[..count]);
                    self.reader.consume(count);
                    self.advance_large_line_offset(count);
                    return result.map(Some);
                }
            }
            // Keep the existing read_until retry behavior for interruptions.
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
        if let Some(lines) = &self.large_lines
            && let Some(start) = lines.offset
            && let Ok(index) = lines.lengths.binary_search_by_key(&start, |(offset, _)| *offset)
        {
            let bytes = lines.lengths[index].1;
            // Skip the hint if allocation fails or the staged file is now
            // shorter. Neither condition changes the ordinary read/error path.
            if self.reader.get_ref().metadata().is_ok_and(|meta| {
                start.checked_add(bytes as u64).is_some_and(|end| end <= meta.len())
            }) {
                let _ = line.try_reserve_exact(bytes);
            }
        }
        let read = match self.reader.read_until(b'\n', line) {
            Ok(read) => read,
            Err(error) => {
                if let Some(lines) = &mut self.large_lines {
                    lines.offset = None;
                }
                return Err(error.into());
            }
        };
        self.advance_large_line_offset(read);
        if read == 0 {
            return Ok(None);
        }
        let decoded = decode(line)?;
        if line.capacity() > MAX_REUSABLE_LINE_CAPACITY {
            // Large rows still release their temporary bytes before page assembly.
            *line = Vec::new();
        }
        Ok(Some(decoded))
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
            if let Some(lines) = &mut self.large_lines {
                lines.offset = Some(0);
            }
        }
        self.after = None;
        let mut records = Vec::new();
        let mut line = Vec::new();
        let mut bytes = 0;
        let mut finished = false;
        while records.len() < limit as usize {
            let next = match self.pending.take() {
                Some(record) => Some(record),
                None => self.next(&mut line)?,
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

#[cfg(test)]
#[test]
fn display_line_scratch_reuses_small_capacity_but_releases_a_large_row() {
    use proteus_contracts::{
        domain::*,
        model_standard::{CanonicalMessage, MessageRole},
    };

    let id = new_session_id();
    let mut writer = DisplayWriter::new(MAX_STAGING_BYTES).unwrap();
    for (index, size) in [100, 200, 64 * 1024, 300].into_iter().enumerate() {
        writer
            .push(&JournalRecord {
                schema_version: JOURNAL_SCHEMA_VERSION,
                record_id: new_record_id(),
                session_id: id,
                session_seq: index as u64 + 1,
                timestamp_ms: 23,
                execution_id: None,
                thread_id: Some(new_thread_id()),
                turn_id: None,
                entry: JournalEntry::HistoryMutated(HistoryMutated {
                    previous_revision: index as u64,
                    new_revision: index as u64 + 1,
                    mutation: HistoryMutationKind::Append,
                    messages: vec![CanonicalMessage::text(MessageRole::User, "x".repeat(size))],
                    compaction: None,
                    tool_results: vec![],
                }),
            })
            .unwrap();
    }
    let mut reader = writer.finish().unwrap();
    reader.reader.rewind().unwrap();
    let mut scratch = Vec::new();
    for expected in 1..=2 {
        let (record, bytes) = reader.next(&mut scratch).unwrap().unwrap();
        assert_eq!(record.session_seq, expected);
        assert_eq!(bytes, serde_json::to_vec(&record).unwrap().len());
        assert_eq!(scratch.capacity(), 0, "complete buffered row needs no copy");
    }
    // Force each row across a read boundary to exercise the unchanged fallback.
    reader.reader = BufReader::with_capacity(64, reader.reader.into_inner());
    reader.reader.rewind().unwrap();
    let mut small_capacity = None;
    for expected in 1..=4 {
        let (record, bytes) = reader.next(&mut scratch).unwrap().unwrap();
        assert_eq!(record.session_seq, expected);
        assert_eq!(bytes, serde_json::to_vec(&record).unwrap().len());
        if expected == 3 {
            assert_eq!(scratch.capacity(), 0, "do not retain a large scratch");
        } else {
            assert_eq!(scratch.len(), bytes + 1);
            assert!(scratch.capacity() <= MAX_REUSABLE_LINE_CAPACITY);
            if expected == 1 {
                // Ensure the second test row already fits the same allocation.
                scratch.reserve(1024);
                small_capacity = Some((scratch.as_ptr(), scratch.capacity()));
            }
            if expected == 2 {
                assert_eq!(Some((scratch.as_ptr(), scratch.capacity())), small_capacity);
            }
        }
    }
    assert!(reader.next(&mut scratch).unwrap().is_none());
}
