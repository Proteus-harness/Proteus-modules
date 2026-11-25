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
    digest_hex(digest(&SHA256, bytes))
}

pub(crate) fn digest_hex(digest: ring::digest::Digest) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = digest.as_ref();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(char::from(HEX[(byte >> 4) as usize]));
        encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    encoded
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
    if len == 0 {
        return Ok(0);
    }
    // A completed append ends at a newline. Only a torn tail needs a scan.
    file.seek(SeekFrom::Start(len - 1))?;
    let mut last = [0];
    file.read_exact(&mut last)?;
    if last == [b'\n'] {
        return Ok(len);
    }
    let mut end = len - 1;
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

fn open_regular(path: &Path) -> Result<File> {
    open_regular_with_metadata(path).map(|(file, _)| file)
}

fn open_regular_with_metadata(path: &Path) -> Result<(File, std::fs::Metadata)> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "storage object must be a regular file");
    Ok((file, metadata))
}

/// Compare an existing object without retaining a second copy. Read through to
/// EOF (or maximum + 1) even after a mismatch so size and I/O errors still win.
pub(crate) fn bounded_file_eq(path: &Path, expected: &[u8], maximum: usize) -> Result<bool> {
    let mut file = open_regular(path)?.take(maximum as u64 + 1);
    let mut buffer = [0; 16 * 1024];
    let mut remaining = Some(expected);
    let mut total = 0;
    loop {
        let count = match file.read(&mut buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            break;
        }
        total += count;
        remaining = remaining.and_then(|rest| rest.strip_prefix(&buffer[..count]));
    }
    ensure!(total <= maximum, "storage object exceeds {maximum} bytes");
    Ok(remaining == Some(&[]))
}

/// Fill the caller's buffer, then count any trailing bytes through EOF or
/// maximum + 1. Preserve late I/O and limit errors before the caller checks size.
pub(crate) fn bounded_read_into(path: &Path, maximum: usize, output: &mut [u8]) -> Result<usize> {
    read_bounded_into(open_regular(path)?, maximum, output)
}

fn read_bounded_into(reader: impl Read, maximum: usize, output: &mut [u8]) -> Result<usize> {
    let mut reader = reader.take(maximum as u64 + 1);
    let mut read = |buffer: &mut [u8]| loop {
        match reader.read(buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            result => break result,
        }
    };
    let mut total = 0;
    while total < output.len() {
        let count = read(&mut output[total..])?;
        if count == 0 {
            break;
        }
        total += count;
    }
    // On an exact fill, check for EOF without allocating a discard buffer.
    if total == output.len() && read(&mut [0])? != 0 {
        total += 1;
        let mut trailing = [0; 8 * 1024];
        loop {
            let count = read(&mut trailing)?;
            if count == 0 {
                break;
            }
            total += count;
        }
    }
    ensure!(total <= maximum, "storage object exceeds {maximum} bytes");
    Ok(total)
}

pub(crate) fn bounded_read(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    open_bounded(path, maximum, None)
}

pub(crate) fn bounded_read_with_size_hint(
    path: &Path,
    maximum: usize,
    max_hint: usize,
) -> Result<Vec<u8>> {
    open_bounded(path, maximum, Some(max_hint))
}

fn open_bounded(path: &Path, maximum: usize, max_hint: Option<usize>) -> Result<Vec<u8>> {
    let (file, metadata) = open_regular_with_metadata(path)?;
    // The existing length is an allocation hint from the same file, not an
    // admission check. Large or uncertain reads keep the original growth path.
    let capacity = match max_hint {
        Some(hint) if metadata.len() <= hint.min(maximum) as u64 => metadata.len() as usize,
        _ => 0,
    };
    read_bounded(file, maximum, capacity)
}

fn read_bounded(file: File, maximum: usize, capacity: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(capacity);
    file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        bail!("storage object exceeds {maximum} bytes");
    }
    Ok(bytes)
}

#[cfg(test)]
#[test]
fn bounded_read_small_hint_preserves_empty_limits_and_object_checks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("record");
    for data in [b"".as_slice(), b"small value", &[b'x'; 512]] {
        std::fs::write(&path, data).unwrap();
        assert_eq!(bounded_read(&path, data.len()).unwrap(), data);
        assert_eq!(
            bounded_read_with_size_hint(&path, data.len(), 64 * 1024).unwrap(),
            data
        );
        if !data.is_empty() {
            let maximum = data.len() - 1;
            assert_eq!(
                bounded_read_with_size_hint(&path, maximum, 64 * 1024)
                    .unwrap_err()
                    .to_string(),
                bounded_read(&path, maximum).unwrap_err().to_string()
            );
        }
    }
    for other in [dir.path().join("absent"), dir.path().to_path_buf()] {
        assert!(bounded_read(&other, 128).is_err());
        assert!(bounded_read_with_size_hint(&other, 128, 64 * 1024).is_err());
    }
}

#[cfg(test)]
#[test]
fn bounded_read_checks_actual_bytes_after_an_ordinary_size_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("record");
    std::fs::write(&path, b"initial bytes").unwrap();
    let source = File::open(&path).unwrap();
    let size = source.metadata().unwrap().len() as usize;
    OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(3)
        .unwrap();
    assert_eq!(read_bounded(source, 64, size).unwrap(), b"ini");

    let source = File::open(&path).unwrap();
    let size = source.metadata().unwrap().len() as usize;
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b" larger")
        .unwrap();
    assert_eq!(read_bounded(source, 64, size).unwrap(), b"ini larger");
    assert!(read_bounded(File::open(&path).unwrap(), 4, size).is_err());
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

#[cfg(test)]
#[test]
fn sha256_hex_matches_persisted_lowercase_format() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
