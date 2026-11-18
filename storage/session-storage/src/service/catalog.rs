//! Metadata-invalidated bounded catalog cache; never used to authorize execution.
use super::*;
use std::{fs::Metadata, time::UNIX_EPOCH};
mod durable;

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
        stamp(&wal)?,
        stamp(&dir.join("session.json"))?,
    ])
}
pub(super) struct CachedSummary {
    stamps: Vec<Option<FileStamp>>,
    summary: StorageSessionSummary,
}
impl State {
    pub(super) fn catalog_summary(
        &mut self,
        dir: &Path,
        descriptor: StorageSessionDescriptor,
        audit: bool,
    ) -> Result<StorageSessionSummary> {
        let stamps = source_stamp(dir, self.kind)?;
        if !audit {
            if let Some(cached) = self.catalog.get(&descriptor.session) {
                if cached.stamps == stamps {
                    return Ok(cached.summary.clone());
                }
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
