//! Metadata-invalidated bounded catalog cache; never used to authorize execution.
use super::*;
use std::{fs::Metadata, time::UNIX_EPOCH};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct FileStamp {
    len: u64,
    modified: u128,
    device: u64,
    inode: u64,
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
    let (device, inode) = {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino())
    };
    #[cfg(not(unix))]
    let (device, inode) = (0, 0);
    Ok(FileStamp {
        len: meta.len(),
        modified: meta
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        device,
        inode,
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
        }
        let mut source = self
            .kind
            .backend()
            .snapshot(dir, descriptor.session.session_id)?;
        let mut fold = Fold::default();
        while let Some(record) = source.next()? {
            fold.apply(descriptor.session.session_id, &record)?;
        }
        self.remember_prefix(
            descriptor.session.clone(),
            super::views::PrefixCache {
                stamps: stamps.clone(),
                revision: fold.revision(),
                hint: crate::backend::PrefixHint {
                    extent: source.extent(),
                    fingerprint: source.fingerprint().to_owned(),
                },
            },
        );
        let summary = StorageSessionSummary {
            descriptor: descriptor.clone(),
            message_count: fold.validation.history().len(),
            preview: crate::snapshot::preview(fold.validation.history()),
        };
        if !audit {
            self.catalog_order
                .retain(|session| session != &descriptor.session);
            while self.catalog.len() >= 1024 {
                if let Some(old) = self.catalog_order.pop_front() {
                    self.catalog.remove(&old);
                } else {
                    break;
                }
            }
            self.catalog_order.push_back(descriptor.session.clone());
            self.catalog.insert(
                descriptor.session,
                CachedSummary {
                    stamps,
                    summary: summary.clone(),
                },
            );
        }
        Ok(summary)
    }
}
