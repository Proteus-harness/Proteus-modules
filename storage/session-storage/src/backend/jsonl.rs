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
    fn append(&mut self, dir: &Path, record: &JournalRecord) -> Result<()> {
        let path = dir.join(codec::JOURNAL_FILE);
        let committed = self
            .committed_offset
            .context("JSONL writer must recover before append")?;
        restore_offset(&path, committed)?;
        let line = codec::encode(dir, record, self.blob_threshold)?;
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
        Ok(())
    }
    fn description_path(&self, dir: &Path) -> PathBuf {
        dir.join(codec::JOURNAL_FILE)
    }
}

struct JsonlSnapshot {
    file: Option<BufReader<std::io::Take<File>>>,
    dir: PathBuf,
    id: SessionId,
    length: u64,
    fingerprint: String,
    current_hash: HashContext,
    consumed: u64,
}
impl JsonlSnapshot {
    fn open(dir: &Path, id: SessionId, hint: Option<&PrefixHint>) -> Result<Self> {
        let path = dir.join(codec::JOURNAL_FILE);
        let mut file = match File::open(&path) {
            Ok(file) => Some(file),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let length = if let Some(file) = &mut file {
            ensure!(file.metadata()?.is_file(), "journal is not a regular file");
            if let Some(hint) = hint {
                ensure!(
                    file.metadata()?.len() >= hint.extent,
                    "journal shortened below cached prefix"
                );
                hint.extent
            } else {
                complete_jsonl_offset(file)?
            }
        } else {
            0
        };
        let mut hash = HashContext::new(&SHA256);
        if hint.is_none()
            && let Some(file) = &mut file
        {
            file.seek(SeekFrom::Start(0))?;
            let mut source = file.take(length);
            let mut block = [0_u8; 64 * 1024];
            loop {
                let read = source.read(&mut block)?;
                if read == 0 {
                    break;
                }
                crate::diagnostics::fingerprint(read);
                hash.update(&block[..read]);
            }
            ensure!(
                source.limit() == 0,
                "journal shortened during snapshot capture"
            );
            file.seek(SeekFrom::Start(0))?;
        }
        let fingerprint = if let Some(hint) = hint {
            hint.fingerprint.clone()
        } else {
            hash.finish()
                .as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        };
        if let Some(file) = &mut file {
            file.seek(SeekFrom::Start(0))?;
        }
        Ok(Self {
            file: file.map(|f| BufReader::with_capacity(64 * 1024, f.take(length))),
            dir: dir.to_owned(),
            id,
            length,
            fingerprint,
            current_hash: HashContext::new(&SHA256),
            consumed: 0,
        })
    }
}
impl RecordSnapshot for JsonlSnapshot {
    fn source_version(&self) -> Result<Option<SourceVersion>> {
        let Some(reader) = &self.file else {
            return Ok(None);
        };
        let metadata = reader.get_ref().get_ref().metadata()?;
        #[cfg(unix)]
        let changed = {
            use std::os::unix::fs::MetadataExt;
            (metadata.ctime(), metadata.ctime_nsec())
        };
        #[cfg(not(unix))]
        let changed = (0, 0);
        Ok(Some(SourceVersion {
            bytes: metadata.len(),
            modified: metadata.modified()?,
            changed,
        }))
    }
    fn verify_fixed_prefix(&self) -> Result<()> {
        let Some(reader) = &self.file else {
            return Ok(());
        };
        let file = reader.get_ref().get_ref();
        ensure!(
            file.metadata()?.len() >= self.length,
            "journal shortened below snapshot prefix"
        );
        let mut hash = HashContext::new(&SHA256);
        let mut offset = 0;
        let mut block = [0_u8; 64 * 1024];
        #[cfg(not(unix))]
        let mut fallback = file.try_clone()?;
        #[cfg(not(unix))]
        let saved_position = fallback.stream_position()?;
        let verification = (|| {
            while offset < self.length {
                let count = ((self.length - offset) as usize).min(block.len());
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
            let actual: String = hash
                .finish()
                .as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            ensure!(
                actual == self.fingerprint,
                "journal complete prefix changed during snapshot read"
            );
            Ok(())
        })();
        #[cfg(not(unix))]
        fallback.seek(SeekFrom::Start(saved_position))?;
        verification
    }
    fn reset(&mut self) -> Result<()> {
        if let Some(reader) = self.file.take() {
            let mut file = reader.into_inner().into_inner();
            file.seek(SeekFrom::Start(0))?;
            self.file = Some(BufReader::with_capacity(64 * 1024, file.take(self.length)));
        }
        self.consumed = 0;
        self.current_hash = HashContext::new(&SHA256);
        Ok(())
    }
    fn next(&mut self) -> Result<Option<JournalRecord>> {
        let Some(reader) = &mut self.file else {
            return Ok(None);
        };
        let mut line = Vec::new();
        // Bound even a malicious unterminated line before allocation reaches file size.
        let count = reader
            .by_ref()
            .take((MAX_PAYLOAD_BYTES * 2 + 1) as u64)
            .read_until(b'\n', &mut line)?;
        if count == 0 {
            ensure!(
                self.consumed == self.length && reader.get_ref().limit() == 0,
                "journal shortened during snapshot read"
            );
            let actual: String = self
                .current_hash
                .clone()
                .finish()
                .as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            ensure!(
                actual == self.fingerprint,
                "journal complete prefix changed during snapshot read"
            );
            return Ok(None);
        }
        ensure!(
            line.len() <= MAX_PAYLOAD_BYTES * 2,
            "journal line exceeds maximum"
        );
        if !line.ends_with(b"\n") {
            bail!("journal snapshot lost a complete line");
        }
        self.consumed += line.len() as u64;
        self.current_hash.update(&line);
        line.pop();
        ensure!(!line.is_empty(), "journal contains an empty complete line");
        codec::decode(&self.dir, self.id, &line).map(Some)
    }
    fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    fn extent(&self) -> u64 {
        self.length
    }
    fn prefix_fingerprint(&mut self, sequence: u64) -> Result<Option<String>> {
        self.reset()?;
        let mut found = (sequence == 0).then(|| crate::io::sha256_hex(b""));
        while let Some(record) = self.next()? {
            if record.session_seq == sequence {
                found = Some(
                    self.current_hash
                        .clone()
                        .finish()
                        .as_ref()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect(),
                );
            }
        }
        Ok(found)
    }
}
