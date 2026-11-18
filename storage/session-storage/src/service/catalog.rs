//! Metadata-invalidated bounded catalog cache; never used to authorize execution.
use super::*;
use std::{fs::Metadata, time::UNIX_EPOCH};
pub(super) mod durable;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct FileStamp {
    len: u64,
    modified: u128,
    device: u64,
    inode: u64,
    changed: (i64, i64),
}
fn stamp(path: &Path) -> Result<Option<FileStamp>> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(Some(file_stamp(&meta)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn file_stamp(meta: &Metadata) -> Result<FileStamp> {
    #[cfg(unix)]
    let (device, inode, changed) = {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino(), (meta.ctime(), meta.ctime_nsec()))
    };
    #[cfg(not(unix))]
    let (device, inode, changed) = (0, 0, (0, 0));
    Ok(FileStamp {
        len: meta.len(),
        modified: meta
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        device,
        inode,
        changed,
    })
}
pub(super) type SourceStamp = Vec<Option<FileStamp>>;
pub(super) fn source_stamp(dir: &Path, kind: BackendKind) -> Result<SourceStamp> {
    let journal = kind.backend().description_path(dir);
    let wal = PathBuf::from(format!("{}-wal", journal.to_string_lossy()));
    Ok(vec![
        stamp(&journal)?,
        // Opening a read-only SQLite snapshot may create an empty WAL. It
        // carries no committed data and is equivalent to a missing WAL.
        stamp(&wal)?.filter(|stamp| stamp.len != 0),
        stamp(&dir.join("session.json"))?,
    ])
}
pub(super) struct CachedSummary {
    stamps: Vec<Option<FileStamp>>,
    summary: StorageSessionSummary,
}

/// Authority for a resident fold starts at a verified replay and advances
/// only across this writer's own successful appends.
pub(super) struct WriterHead {
    pub stamps: SourceStamp,
    pub blobs: durable::BlobStamps,
    pub extent: u64,
    pub hash: ring::digest::Context,
}

impl WriterHead {
    pub fn advance(
        &mut self,
        dir: &Path,
        kind: BackendKind,
        append: crate::backend::Append,
    ) -> Option<()> {
        self.stamps = source_stamp(dir, kind).ok()?;
        durable::update_blobs(dir, &mut self.blobs, append.blobs)?;
        self.extent = append.extent;
        self.hash.update(&append.bytes);
        Some(())
    }
}

impl State {
    pub(super) fn close_writer(&mut self, session: &SessionLocator, writer: Writer) {
        let Writer {
            backend,
            _ownership,
            ..
        } = writer;
        // SQLite may checkpoint and remove its WAL when the last writer
        // connection closes. Rebind an already verified head while the
        // session lease still excludes another writer.
        let head = self.catalog_before_sqlite_close(session);
        drop(backend);
        if let Some((dir, mut prefix, summary)) = head
            && let Ok(stamps) = source_stamp(&dir, self.kind)
            && stamps != prefix.stamps
            && stamps[2] == prefix.stamps[2]
            && stamps[0]
                .as_ref()
                .zip(prefix.stamps[0].as_ref())
                .is_some_and(|(new, old)| new.device == old.device && new.inode == old.inode)
            && let Some(blobs) = durable::blob_stamps(&dir)
            && Some(durable::blob_stamp_digest(&blobs)) == prefix.blob_digest
        {
            prefix.stamps = stamps;
            self.save_catalog_head(&dir, summary, &prefix, blobs);
        }
    }

    fn catalog_before_sqlite_close(
        &self,
        session: &SessionLocator,
    ) -> Option<(PathBuf, super::views::PrefixCache, StorageSessionSummary)> {
        if !matches!(self.kind, BackendKind::Sqlite) {
            return None;
        }
        let cached = self.catalog.get(session)?;
        let prefix = self.prefixes.get(session)?;
        let dir = self.checked_path(session).ok()?;
        let stamps = source_stamp(&dir, self.kind).ok()?;
        if cached.stamps != stamps || !prefix.valid_at(&dir, &stamps) {
            return None;
        }
        Some((dir, prefix.clone(), cached.summary.clone()))
    }

    pub(super) fn load_catalog_head(
        &mut self,
        dir: &Path,
        descriptor: &StorageSessionDescriptor,
        stamps: &SourceStamp,
    ) -> Option<super::views::PrefixCache> {
        let (summary, prefix) = durable::load(dir, descriptor, stamps)?;
        self.remember_prefix(descriptor.session.clone(), prefix.clone());
        self.remember_summary(stamps.clone(), summary);
        Some(prefix)
    }

    pub(super) fn save_catalog_head(
        &mut self,
        dir: &Path,
        summary: StorageSessionSummary,
        prefix: &super::views::PrefixCache,
        blobs: durable::BlobStamps,
    ) {
        durable::save(dir, &summary, prefix, blobs);
        self.remember_prefix(summary.descriptor.session.clone(), prefix.clone());
        self.remember_summary(prefix.stamps.clone(), summary);
    }

    pub(super) fn catalog_summary(
        &mut self,
        dir: &Path,
        descriptor: StorageSessionDescriptor,
        audit: bool,
    ) -> Result<StorageSessionSummary> {
        let stamps = source_stamp(dir, self.kind)?;
        if !audit {
            if let Some(cached) = self.catalog.get(&descriptor.session)
                && cached.stamps == stamps
                && self
                    .prefixes
                    .get(&descriptor.session)
                    .is_some_and(|prefix| prefix.valid_at(dir, &stamps))
            {
                return Ok(cached.summary.clone());
            }
            if let Some((summary, prefix)) = durable::load(dir, &descriptor, &stamps) {
                self.remember_prefix(descriptor.session.clone(), prefix);
                self.remember_summary(stamps, summary.clone());
                return Ok(summary);
            }
        }
        let blobs = durable::blob_stamps(dir);
        let mut source = self
            .kind
            .backend()
            .snapshot(dir, descriptor.session.session_id)?;
        let mut fold = Fold::default();
        while let Some(record) = source.next()? {
            fold.apply(descriptor.session.session_id, &record)?;
        }
        let prefix = super::views::PrefixCache {
            stamps: stamps.clone(),
            revision: fold.revision(),
            hint: crate::backend::PrefixHint {
                extent: source.extent(),
                fingerprint: source.fingerprint().to_owned(),
            },
            blob_digest: blobs.as_ref().map(durable::blob_stamp_digest),
        };
        self.remember_prefix(descriptor.session.clone(), prefix.clone());
        let summary = StorageSessionSummary {
            descriptor: descriptor.clone(),
            message_count: fold.validation.history().len(),
            preview: crate::snapshot::preview(fold.validation.history()),
        };
        if !audit {
            if let Some(blobs) = blobs {
                durable::save(dir, &summary, &prefix, blobs);
            }
            self.remember_summary(stamps, summary.clone());
        }
        Ok(summary)
    }

    fn remember_summary(&mut self, stamps: SourceStamp, summary: StorageSessionSummary) {
        let session = summary.descriptor.session.clone();
        self.catalog_order.retain(|old| old != &session);
        while self.catalog.len() >= 1024 {
            if let Some(old) = self.catalog_order.pop_front() {
                self.catalog.remove(&old);
            } else {
                break;
            }
        }
        self.catalog_order.push_back(session.clone());
        self.catalog
            .insert(session, CachedSummary { stamps, summary });
    }
}
