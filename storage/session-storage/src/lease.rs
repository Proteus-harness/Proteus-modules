use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::Write,
    path::Path,
};

use anyhow::{Context, Result, bail};
use proteus_contracts::domain::SessionId;

/// The inode is outside the removable session directory and is never unlinked.
pub(crate) fn ownership_path(session_dir: &Path) -> Result<std::path::PathBuf> {
    let parent = session_dir
        .parent()
        .context("session writer slot has no parent")?;
    let parent = fs::canonicalize(parent).with_context(|| {
        format!(
            "failed to resolve session writer parent {}",
            parent.display()
        )
    })?;
    let name = session_dir
        .file_name()
        .context("session writer slot has no basename")?;
    let mut lock_name = std::ffi::OsString::from(".");
    lock_name.push(name);
    lock_name.push(".write.lock");
    Ok(parent.join(lock_name))
}

#[derive(Debug)]
pub(crate) struct SessionWriteOwnership {
    _file: File,
    owner_pid: u32,
}

impl Drop for SessionWriteOwnership {
    fn drop(&mut self) {
        // A forked child must only close its inherited descriptor, not unlock
        // the still-live parent's shared open-file-description lease.
        if self.owner_pid == std::process::id() {
            // Close alone can leave a lock held by a transient fork/dup copy.
            // Explicitly end this owner's lease before retirement is signaled;
            // File drop still closes the descriptor if unlock reports an error.
            let _ = self._file.unlock();
        }
    }
}

impl SessionWriteOwnership {
    pub(crate) fn acquire(session_dir: &Path, _session_id: SessionId) -> Result<Self> {
        Self::acquire_slot(session_dir)
    }

    pub(crate) fn acquire_slot(session_dir: &Path) -> Result<Self> {
        let path = ownership_path(session_dir)?;
        reject_symlink(&path)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("failed to open session writer lock {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let owner = active_owner_description(&path);
                bail!(
                    "session already has an active writer in another process{owner}; lock: {}",
                    path.display()
                );
            }
            Err(TryLockError::Error(error)) => {
                return Err(error).with_context(|| {
                    format!("failed to acquire session writer lock {}", path.display())
                });
            }
        }

        // Install rollback ownership immediately after acquiring the lock so
        // errors while writing diagnostic metadata also release this lease.
        let ownership = Self {
            _file: file,
            owner_pid: std::process::id(),
        };
        let mut file = &ownership._file;
        file.set_len(0)
            .with_context(|| format!("failed to reset session writer lock {}", path.display()))?;
        writeln!(file, "pid={}", ownership.owner_pid)
            .with_context(|| format!("failed to identify session writer in {}", path.display()))?;
        file.sync_data()
            .with_context(|| format!("failed to sync session writer lock {}", path.display()))?;
        Ok(ownership)
    }
}

fn reject_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!(
                "session writer lock must not be a symlink: {}",
                path.display()
            )
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to inspect writer lock {}", path.display()))
        }
    }
}

fn active_owner_description(path: &Path) -> String {
    fs::read_to_string(path)
        .ok()
        .and_then(|owner| {
            let owner = owner.trim();
            (!owner.is_empty()).then(|| format!(" ({owner})"))
        })
        .unwrap_or_default()
}
