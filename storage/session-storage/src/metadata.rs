//! Metadata CAS and its operation receipt commit in one atomic document.
use crate::{
    io::{atomic_write, bounded_read, sha256_hex},
    types::*,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};
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
fn hydrate(dir: &Path, stored: Option<&StoredDocument>) -> Result<Option<StorageMetadataDocument>> {
    stored
        .map(|stored| {
            ensure!(
                stored.object_sha256.len() == 64
                    && stored.object_sha256.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid metadata object hash"
            );
            let bytes = bounded_read(
                &dir.join(".metadata-objects")
                    .join(format!("{}.json", stored.object_sha256)),
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
fn store(dir: &Path, document: &StorageMetadataDocument) -> Result<StoredDocument> {
    let bytes = serde_json::to_vec(&document.value)?;
    ensure!(
        bytes.len() <= SESSION_STORAGE_MAX_METADATA_BYTES,
        "metadata document exceeds maximum"
    );
    let object_sha256 = sha256_hex(&bytes);
    let path = dir
        .join(".metadata-objects")
        .join(format!("{object_sha256}.json"));
    if path.try_exists()? {
        ensure!(
            bounded_read(&path, SESSION_STORAGE_MAX_METADATA_BYTES)? == bytes,
            "metadata object collision"
        );
    } else {
        atomic_write(&path, &bytes)?;
    }
    Ok(StoredDocument {
        etag: document.etag.clone(),
        object_sha256,
    })
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
pub(crate) fn compare_exchange(
    dir: &Path,
    operation_id: Uuid,
    key: String,
    expected: Option<String>,
    value: Option<serde_json::Value>,
) -> Result<Option<StorageMetadataDocument>> {
    let request_hash = sha256_hex(&serde_json::to_vec(&(&key, &expected, &value))?);
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
    let document = value.map(|value| {
        let mut etag_input = operation_id.as_bytes().to_vec();
        etag_input.extend(serde_json::to_vec(&value).expect("JSON value encoding is infallible"));
        StorageMetadataDocument {
            etag: sha256_hex(&etag_input),
            value,
        }
    });
    let stored = document
        .as_ref()
        .map(|document| store(dir, document))
        .transpose()?;
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
