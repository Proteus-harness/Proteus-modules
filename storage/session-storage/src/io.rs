//! Durable, bounded local I/O shared by both independent journal backends.
use anyhow::{Context, Result, bail, ensure};
use ring::digest::{SHA256, digest};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

pub(crate) const MAX_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?
        .sync_all()
        .with_context(|| format!("sync directory {}", path.display()))
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("storage object has no parent")?;
    create_dir_all_durable(parent)?;
    let temp = parent.join(format!(".storage-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

/// Read-only discovery never repairs an interrupted final JSONL line.
pub(crate) fn complete_jsonl_offset(file: &mut File) -> Result<u64> {
    let len = file.metadata()?.len();
    let mut end = len;
    let mut block = [0_u8; 8192];
    while end != 0 {
        let start = end.saturating_sub(block.len() as u64);
        file.seek(SeekFrom::Start(start))?;
        let count = (end - start) as usize;
        file.read_exact(&mut block[..count])?;
        if let Some(index) = block[..count].iter().rposition(|b| *b == b'\n') {
            return Ok(start + index as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

pub(crate) fn restore_offset(path: &Path, committed: u64) -> Result<()> {
    match std::fs::metadata(path) {
        Ok(meta) => {
            ensure!(
                meta.len() >= committed,
                "journal shortened below committed offset"
            );
            if meta.len() != committed {
                let file = OpenOptions::new().write(true).open(path)?;
                file.set_len(committed)?;
                file.sync_data()?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && committed == 0 => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn bounded_read(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    let file = File::open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "storage object must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        bail!("storage object exceeds {maximum} bytes");
    }
    Ok(bytes)
}

/// Persist each newly created directory entry before its children are committed.
pub(crate) fn create_dir_all_durable(path: &Path) -> Result<()> {
    let mut missing = Vec::new();
    let mut current = path;
    while !current.try_exists()? {
        missing.push(current.to_path_buf());
        current = current
            .parent()
            .context("directory has no existing ancestor")?;
    }
    for dir in missing.iter().rev() {
        match std::fs::create_dir(dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        sync_directory(dir.parent().context("created directory has no parent")?)?;
    }
    Ok(())
}
