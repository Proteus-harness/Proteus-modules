//! Metadata CAS and its operation receipt commit in one atomic document.
use crate::{
    io::{atomic_write, bounded_file_eq, bounded_read, digest_hex, sha256_hex},
    types::*,
};
use anyhow::{Result, ensure};
use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use uuid::Uuid;
const FILE: &str = ".storage-metadata.json";
const MAX_ENVELOPE: usize = 16 * 1024 * 1024;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    // None is a durable tombstone: an old standalone file may not reappear.
    documents: BTreeMap<String, Option<StoredDocument>>,
    receipts: BTreeMap<Uuid, Receipt>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredDocument {
    etag: String,
    object_sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    request_sha256: String,
    document: Option<StoredDocument>,
}
fn missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}
fn load(dir: &Path) -> Result<Envelope> {
    match bounded_read(&dir.join(FILE), MAX_ENVELOPE) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if missing(&error) => Ok(Envelope::default()),
        Err(error) => Err(error),
    }
}
// Both callers supply a single ASCII hex component.
fn metadata_object_path(dir: &Path, hash: &str) -> PathBuf {
    const OBJECTS: &str = ".metadata-objects";
    const EXTENSION: &str = ".json";
    let mut path = PathBuf::with_capacity(
        dir.as_os_str().len() + OBJECTS.len() + hash.len() + EXTENSION.len() + 2,
    );
    path.as_mut_os_string().push(dir.as_os_str());
    path.push(OBJECTS);
    path.push(hash);
    path.as_mut_os_string().push(EXTENSION);
    path
}
fn hydrate(dir: &Path, stored: Option<&StoredDocument>) -> Result<Option<StorageMetadataDocument>> {
    stored
        .map(|stored| {
            ensure!(
                stored.object_sha256.len() == 64
                    && stored.object_sha256.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid metadata object hash"
            );
            let bytes = bounded_read(
                &metadata_object_path(dir, &stored.object_sha256),
                SESSION_STORAGE_MAX_METADATA_BYTES,
            )?;
            ensure!(
                sha256_hex(&bytes) == stored.object_sha256,
                "metadata object hash mismatch"
            );
            Ok(StorageMetadataDocument {
                etag: stored.etag.clone(),
                value: serde_json::from_slice(&bytes)?,
            })
        })
        .transpose()
}
fn store(
    dir: &Path,
    operation_id: Uuid,
    value: serde_json::Value,
) -> Result<(StorageMetadataDocument, StoredDocument)> {
    let bytes = serde_json::to_vec(&value)?;
    ensure!(
        bytes.len() <= SESSION_STORAGE_MAX_METADATA_BYTES,
        "metadata document exceeds maximum"
    );
    let object_sha256 = sha256_hex(&bytes);
    let path = metadata_object_path(dir, &object_sha256);
    if path.try_exists()? {
        ensure!(
            bounded_file_eq(&path, &bytes, SESSION_STORAGE_MAX_METADATA_BYTES)?,
            "metadata object collision"
        );
    } else {
        atomic_write(&path, &bytes)?;
    }
    // Persisted etags hash the raw UUID followed by these same JSON bytes.
    let mut etag = Context::new(&SHA256);
    etag.update(operation_id.as_bytes());
    etag.update(&bytes);
    let etag = digest_hex(etag.finish());
    Ok((
        StorageMetadataDocument {
            etag: etag.clone(),
            value,
        },
        StoredDocument {
            etag,
            object_sha256,
        },
    ))
}
/// Import compatibility stays inside storage. No consumer guesses local files.
fn legacy(dir: &Path, key: &str) -> Result<Option<StorageMetadataDocument>> {
    if !key.ends_with(".json") || key.starts_with(".storage-") {
        return Ok(None);
    }
    match bounded_read(&dir.join(key), SESSION_STORAGE_MAX_METADATA_BYTES) {
        Ok(bytes) => Ok(Some(StorageMetadataDocument {
            etag: sha256_hex(&bytes),
            value: serde_json::from_slice(&bytes)?,
        })),
        Err(error) if missing(&error) => Ok(None),
        Err(error) => Err(error),
    }
}
pub(crate) fn read(dir: &Path, key: &str) -> Result<Option<StorageMetadataDocument>> {
    let envelope = load(dir)?;
    if let Some(document) = envelope.documents.get(key) {
        hydrate(dir, document.as_ref())
    } else {
        legacy(dir, key)
    }
}
// Keep hashing the original compact JSON tuple, without retaining its bytes.
fn hash_request(request: &impl Serialize) -> Result<String> {
    struct HashWriter(ring::digest::Context);
    impl std::io::Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut writer = HashWriter(ring::digest::Context::new(&ring::digest::SHA256));
    serde_json::to_writer(&mut writer, request)?;
    Ok(crate::io::digest_hex(writer.0.finish()))
}

pub(crate) fn compare_exchange(
    dir: &Path,
    operation_id: Uuid,
    key: String,
    expected: Option<String>,
    value: Option<serde_json::Value>,
) -> Result<Option<StorageMetadataDocument>> {
    let request_hash = hash_request(&(&key, &expected, &value))?;
    let mut envelope = load(dir)?;
    if let Some(receipt) = envelope.receipts.get(&operation_id) {
        ensure!(
            receipt.request_sha256 == request_hash,
            "metadata operation id conflicts with earlier content"
        );
        return hydrate(dir, receipt.document.as_ref());
    }
    let actual = if let Some(document) = envelope.documents.get(&key) {
        hydrate(dir, document.as_ref())?
    } else {
        legacy(dir, &key)?
    };
    ensure!(
        actual.as_ref().map(|d| &d.etag) == expected.as_ref(),
        "metadata etag conflict"
    );
    let (document, stored) = value
        .map(|value| store(dir, operation_id, value))
        .transpose()?
        .unzip();
    envelope.documents.insert(key, stored.clone());
    envelope.receipts.insert(
        operation_id,
        Receipt {
            request_sha256: request_hash,
            document: stored,
        },
    );
    let bytes = serde_json::to_vec(&envelope)?;
    ensure!(
        bytes.len() <= MAX_ENVELOPE,
        "metadata document/operation ledger exceeds maximum"
    );
    atomic_write(&dir.join(FILE), &bytes)?;
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_etag_binds_raw_operation_uuid_but_object_path_only_binds_value() {
        let dir = tempfile::tempdir().unwrap();
        let value = serde_json::json!(["line\n\"two\"", null, true, "тест"]);
        let mut expected = None;
        for (operation, etag) in [
            (
                1,
                "af44957d64d7c62688b51c760f666c0c27b69f42141a4791fa04a5ba815b9e65",
            ),
            (
                2,
                "b0acc94e67d1c6cc291fe196cdda09d1a29f0b83089c7a7a17ca339b8e7add29",
            ),
        ] {
            let doc = compare_exchange(
                dir.path(),
                Uuid::from_u128(operation),
                "settings".into(),
                expected.take(),
                Some(value.clone()),
            )
            .unwrap()
            .unwrap();
            assert_eq!(doc.value, value);
            assert_eq!(doc.etag, etag);
            expected = Some(doc.etag);
        }
        let object = dir
            .path()
            .join(".metadata-objects")
            .join("26edbce3163db56a25fc6d027d9f2a831f6340e3836c57e3c3ed33061cd71e7d.json");
        assert_eq!(
            std::fs::read(object).unwrap(),
            serde_json::to_vec(&value).unwrap()
        );
    }
}
