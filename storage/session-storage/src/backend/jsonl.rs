use super::*;
use crate::{
    codec,
    io::{MAX_PAYLOAD_BYTES, complete_jsonl_offset, restore_offset, sync_directory},
};
use anyhow::{Context, bail, ensure};
use ring::digest::{Context as HashContext, SHA256};
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

#[derive(Default)]
pub(crate) struct Jsonl {
    committed_offset: Option<u64>,
    blob_threshold: usize,
}
impl Jsonl {
    pub(crate) fn new() -> Self {
        Self {
            committed_offset: None,
            blob_threshold: codec::DEFAULT_BLOB_THRESHOLD,
        }
    }

    pub(crate) fn replay_snapshot<T: Default>(
        dir: &Path,
        id: SessionId,
        cut: Option<u64>,
        apply: impl Fn(&mut T, &JournalRecord, Option<String>) -> Result<()>,
    ) -> Result<(Box<dyn RecordSnapshot>, T)> {
        Self::replay_snapshot_with(dir, id, cut, |_| T::default(), apply)
    }

    pub(crate) fn replay_snapshot_with<T>(
        dir: &Path,
        id: SessionId,
        cut: Option<u64>,
        initialize: impl Fn(u64) -> T,
        apply: impl Fn(&mut T, &JournalRecord, Option<String>) -> Result<()>,
    ) -> Result<(Box<dyn RecordSnapshot>, T)> {
        ensure!(
            !dir.join("journal.sqlite").try_exists()?,
            "session contains a SQLite journal; select its storage implementation or explicitly migrate it"
        );
        let (mut source, version) = JsonlSnapshot::pin(dir, id, None)?;
        let extent = source.extent();
        let observed = if cfg!(unix) {
            let fold = replay_records_with(&mut source, cut, initialize(extent), &apply)?;
            let fingerprint = source.consumed_fingerprint();
            if source.source_version()? == version {
                source.fingerprint = Some(fingerprint);
                return Ok((Box::new(source), fold));
            }
            Some(fingerprint)
        } else {
            None
        };

        // Discard the first attempt if the inode changed. Re-authenticate
        // the same pinned extent before replaying it, excluding any appends.
        let version = source.source_version()?;
        let fingerprint = source.hash_fixed_prefix()?;
        ensure!(
            observed.is_none_or(|observed| observed == fingerprint),
            "journal complete prefix changed during snapshot capture"
        );
        source.fingerprint = Some(fingerprint);
        source.reset()?;
        source.fully_read = false;
        source.has_external_payloads = false;
        source.page_boundary = None;
        let fold = replay_records_with(&mut source, cut, initialize(extent), &apply)?;
        let replayed_version = source.source_version()?;
        if replayed_version != version {
            source.verify_fixed_prefix()?;
            ensure!(
                source.source_version()? == replayed_version,
                "journal changed during snapshot capture"
            );
        }
        Ok((Box::new(source), fold))
    }
}

impl JournalBackend for Jsonl {
    fn snapshot(&mut self, dir: &Path, id: SessionId) -> Result<Box<dyn RecordSnapshot>> {
        ensure!(
            !dir.join("journal.sqlite").try_exists()?,
            "session contains a SQLite journal; select its storage implementation or explicitly migrate it"
        );
        Ok(Box::new(JsonlSnapshot::open(dir, id, None)?))
    }
    fn snapshot_warm(
        &mut self,
        dir: &Path,
        id: SessionId,
        hint: &PrefixHint,
    ) -> Result<Box<dyn RecordSnapshot>> {
        ensure!(
            !dir.join("journal.sqlite").try_exists()?,
            "session contains a SQLite journal"
        );
        Ok(Box::new(JsonlSnapshot::open(dir, id, Some(hint))?))
    }
    fn recover(&mut self, dir: &Path, _id: SessionId) -> Result<()> {
        ensure!(
            !dir.join("journal.sqlite").try_exists()?,
            "session contains a SQLite journal; storage implementations cannot silently replace it"
        );
        let path = dir.join(codec::JOURNAL_FILE);
        let length = match File::open(&path) {
            Ok(mut file) => complete_jsonl_offset(&mut file)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e.into()),
        };
        restore_offset(&path, length)?;
        self.committed_offset = Some(length);
        Ok(())
    }
    fn append(&mut self, dir: &Path, record: &JournalRecord) -> Result<Append> {
        let path = dir.join(codec::JOURNAL_FILE);
        let committed = self
            .committed_offset
            .context("JSONL writer must recover before append")?;
        restore_offset(&path, committed)?;
        let (line, blobs) = codec::encode(dir, record, self.blob_threshold)?;
        let next = committed
            .checked_add(line.len() as u64)
            .context("journal offset overflow")?;
        let existed = path.try_exists()?;
        let result = (|| {
            let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
            file.write_all(&line)?;
            file.sync_data()?;
            if !existed {
                sync_directory(dir)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            restore_offset(&path, committed).context("journal rollback failed")?;
            return Err(error);
        }
        self.committed_offset = Some(next);
        Ok(Append {
            extent: next,
            bytes: line,
            blobs,
        })
    }
    fn description_path(&self, dir: &Path) -> PathBuf {
        dir.join(codec::JOURNAL_FILE)
    }
}

struct PageBoundary {
    after: u64,
    offset: u64,
    hash: HashContext,
}

// This is a retention cap, not a journal record limit. Ordinary buffered
// fallback lines may require two allocations before settling at this size.
const MAX_RETAINED_LINE_CAPACITY: usize = 128 * 1024;

struct JsonlSnapshot {
    file: Option<BufReader<std::io::Take<File>>>,
    dir: PathBuf,
    id: SessionId,
    length: u64,
    fingerprint: Option<String>,
    current_hash: HashContext,
    consumed: u64,
    has_external_payloads: bool,
    // A resident writer can attest semantics before this reader has scanned blobs.
    fully_read: bool,
    page_boundary: Option<PageBoundary>,
}
impl JsonlSnapshot {
    fn open(dir: &Path, id: SessionId, hint: Option<&PrefixHint>) -> Result<Self> {
        let (mut source, _) = Self::pin(dir, id, hint)?;
        if hint.is_none() {
            source.fingerprint = Some(source.hash_fixed_prefix()?);
        }
        Ok(source)
    }

    fn pin(
        dir: &Path,
        id: SessionId,
        hint: Option<&PrefixHint>,
    ) -> Result<(Self, Option<SourceVersion>)> {
        let path = dir.join(codec::JOURNAL_FILE);
        let mut file = match File::open(&path) {
            Ok(file) => Some(file),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let mut version = None;
        let length = if let Some(file) = &mut file {
            let metadata = file.metadata()?;
            ensure!(metadata.is_file(), "journal is not a regular file");
            version = Some(version_from_metadata(&metadata)?);
            if let Some(hint) = hint {
                ensure!(
                    metadata.len() >= hint.extent,
                    "journal shortened below cached prefix"
                );
                hint.extent
            } else {
                complete_jsonl_offset(file)?
            }
        } else {
            0
        };
        if let Some(file) = &mut file {
            file.seek(SeekFrom::Start(0))?;
        }
        let source = Self {
            file: file.map(|f| BufReader::with_capacity(64 * 1024, f.take(length))),
            dir: dir.to_owned(),
            id,
            length,
            fingerprint: hint.map(|hint| hint.fingerprint.clone()),
            current_hash: HashContext::new(&SHA256),
            consumed: 0,
            has_external_payloads: false,
            fully_read: false,
            page_boundary: None,
        };
        Ok((source, version))
    }
    fn hash_fixed_prefix(&self) -> Result<String> {
        let mut hash = HashContext::new(&SHA256);
        let Some(reader) = &self.file else {
            return Ok(crate::io::digest_hex(hash.finish()));
        };
        let file = reader.get_ref().get_ref();
        ensure!(
            file.metadata()?.len() >= self.length,
            "journal shortened below snapshot prefix"
        );
        let mut offset = 0;
        let mut block = [0_u8; 64 * 1024];
        #[cfg(not(unix))]
        let mut fallback = file.try_clone()?;
        #[cfg(not(unix))]
        let saved_position = fallback.stream_position()?;
        let verification = (|| {
            while offset < self.length {
                let count = (self.length - offset).min(block.len() as u64) as usize;
                #[cfg(unix)]
                let read = {
                    use std::os::unix::fs::FileExt;
                    file.read_at(&mut block[..count], offset)?
                };
                #[cfg(not(unix))]
                let read = {
                    fallback.seek(SeekFrom::Start(offset))?;
                    fallback.read(&mut block[..count])?
                };
                ensure!(read > 0, "journal shortened during prefix verification");
                crate::diagnostics::fingerprint(read);
                hash.update(&block[..read]);
                offset += read as u64;
            }
            Ok(crate::io::digest_hex(hash.finish()))
        })();
        #[cfg(not(unix))]
        fallback.seek(SeekFrom::Start(saved_position))?;
        verification
    }
    fn position(&mut self, offset: u64, hash: HashContext) -> Result<()> {
        if let Some(reader) = &mut self.file {
            reader.get_mut().get_mut().seek(SeekFrom::Start(offset))?;
            reader.consume(reader.buffer().len());
            reader.get_mut().set_limit(self.length - offset);
        }
        self.consumed = offset;
        self.current_hash = hash;
        Ok(())
    }
}

fn version_from_metadata(metadata: &std::fs::Metadata) -> Result<SourceVersion> {
    #[cfg(unix)]
    let changed = {
        use std::os::unix::fs::MetadataExt;
        (metadata.ctime(), metadata.ctime_nsec())
    };
    #[cfg(not(unix))]
    let changed = (0, 0);
    Ok(SourceVersion {
        bytes: metadata.len(),
        modified: metadata.modified()?,
        changed,
    })
}

impl RecordSnapshot for JsonlSnapshot {
    fn page_cursor(&self) -> super::PageCursor {
        super::PageCursor {
            position: self.consumed,
            hash: self.current_hash.clone(),
        }
    }
    fn restore_page_cursor(&mut self, cursor: super::PageCursor) -> Result<()> {
        self.position(cursor.position, cursor.hash)
    }

    fn source_version(&self) -> Result<Option<SourceVersion>> {
        let Some(reader) = &self.file else {
            return Ok(None);
        };
        Ok(Some(version_from_metadata(
            &reader.get_ref().get_ref().metadata()?,
        )?))
    }
    fn verify_fixed_prefix(&self) -> Result<()> {
        ensure!(
            self.hash_fixed_prefix()? == self.fingerprint(),
            "journal complete prefix changed during snapshot read"
        );
        Ok(())
    }
    fn reset(&mut self) -> Result<()> {
        self.position(0, HashContext::new(&SHA256))
    }
    fn remember_page_boundary(&mut self, after: u64) {
        self.page_boundary = Some(PageBoundary {
            after,
            offset: self.consumed,
            hash: self.current_hash.clone(),
        });
    }
    fn reset_for_page(&mut self, after: u64) -> Result<()> {
        if let Some(cut) = &self.page_boundary
            && cut.after == after
            && cut.offset >= self.length - cut.offset
            && !self.has_external_payloads
        {
            // An offset is not authority: authenticate all pinned bytes before
            // skipping older records. Resume hashing at the actual line boundary.
            self.verify_fixed_prefix()?;
            return self.position(cut.offset, cut.hash.clone());
        }
        self.reset()
    }
    fn next(&mut self) -> Result<Option<JournalRecord>> {
        self.next_with_scratch(&mut Vec::new())
    }
    fn next_with_scratch(&mut self, line: &mut Vec<u8>) -> Result<Option<JournalRecord>> {
        let Some(reader) = &mut self.file else {
            return Ok(None);
        };
        let mut decode = |line: &[u8]| -> Result<Option<JournalRecord>> {
            ensure!(
                line.len() <= MAX_PAYLOAD_BYTES * 2,
                "journal line exceeds maximum"
            );
            let Some(payload) = line.strip_suffix(b"\n") else {
                bail!("journal snapshot lost a complete line");
            };
            self.consumed += line.len() as u64;
            crate::diagnostics::replayed_jsonl(line.len());
            self.current_hash.update(line);
            ensure!(
                !payload.is_empty(),
                "journal contains an empty complete line"
            );
            let (record, external) = codec::decode(&self.dir, self.id, payload)?;
            self.has_external_payloads |= external;
            Ok(Some(record))
        };

        // Borrow only complete lines. The buffered file remains the same pinned
        // source used by read_until, and no extra storage lives on the snapshot.
        match reader.fill_buf() {
            Ok(bytes) => {
                if let Some(end) = memchr::memchr(b'\n', bytes)
                    && end < MAX_PAYLOAD_BYTES * 2
                {
                    let count = end + 1;
                    let result = decode(&bytes[..count]);
                    reader.consume(count);
                    return result;
                }
            }
            // read_until already retries interrupted reads.
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err.into()),
        }
        line.clear();
        // Bound even a malicious unterminated line before allocation reaches file size.
        let count = reader
            .by_ref()
            .take((MAX_PAYLOAD_BYTES * 2 + 1) as u64)
            .read_until(b'\n', line)?;
        if count == 0 {
            ensure!(
                self.consumed == self.length && reader.get_ref().limit() == 0,
                "journal shortened during snapshot read"
            );
            if let Some(expected) = &self.fingerprint {
                ensure!(
                    self.consumed_fingerprint() == *expected,
                    "journal complete prefix changed during snapshot read"
                );
            }
            self.fully_read = true;
            return Ok(None);
        }
        let result = decode(line);
        if line.capacity() > MAX_RETAINED_LINE_CAPACITY {
            *line = Vec::new();
        }
        result
    }
    fn consumed_fingerprint(&self) -> String {
        crate::io::digest_hex(self.current_hash.clone().finish())
    }
    fn confirm_verified_data(&self) -> Result<bool> {
        if !self.fully_read || self.has_external_payloads {
            // Even an unchanged journal cannot prove blobs still exist.
            return Ok(false);
        }
        self.verify_fixed_prefix()?;
        Ok(true)
    }
    fn fingerprint(&self) -> &str {
        self.fingerprint.as_deref().expect("sealed JSONL snapshot")
    }
    fn read_hash(&self) -> HashContext {
        self.current_hash.clone()
    }
    fn extent(&self) -> u64 {
        self.length
    }
    fn prefix_fingerprint(&mut self, sequence: u64) -> Result<Option<String>> {
        self.reset()?;
        let mut found = (sequence == 0).then(|| crate::io::sha256_hex(b""));
        while let Some(record) = self.next()? {
            if record.session_seq == sequence {
                found = Some(self.consumed_fingerprint());
            }
        }
        Ok(found)
    }
}
