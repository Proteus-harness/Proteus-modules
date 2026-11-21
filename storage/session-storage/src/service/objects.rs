use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

fn object_path(dir: &Path, blob: &StorageBlobRef) -> Result<PathBuf> {
    ensure!(
        blob.sha256.len() == 64
            && blob.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            && blob.sha256 == blob.sha256.to_ascii_lowercase(),
        "invalid blob hash"
    );
    ensure!(
        blob.bytes <= SESSION_STORAGE_MAX_BLOB_BYTES,
        "blob exceeds maximum"
    );
    Ok(dir.join("images").join(&blob.sha256))
}
impl State {
    pub(super) fn put_blob(
        &mut self,
        lease: StorageWriterLease,
        operation_id: Uuid,
        data: String,
        expected: StorageBlobRef,
    ) -> Result<SessionStorageResponse> {
        let dir = self.writer_path(&lease)?;
        let path = object_path(&dir, &expected)?;
        let bytes = STANDARD.decode(data)?;
        ensure!(
            bytes.len() as u64 == expected.bytes
                && crate::io::sha256_hex(&bytes) == expected.sha256,
            "blob size/hash mismatch"
        );
        if path.try_exists()? {
            ensure!(
                crate::io::bounded_file_eq(&path, &bytes, SESSION_STORAGE_MAX_BLOB_BYTES as usize)?,
                "content-addressed blob collision"
            );
        } else {
            crate::io::atomic_write(&path, &bytes)?;
        }
        crate::metadata::compare_exchange(
            &dir,
            operation_id,
            format!(".blob-operation-{operation_id}"),
            None,
            Some(serde_json::to_value(&expected)?),
        )?;
        Ok(SessionStorageResponse::Blob {
            blob: expected,
            local_path: Some(std::fs::canonicalize(path)?),
        })
    }
    pub(super) fn read_blob(
        &mut self,
        session: SessionLocator,
        blob: StorageBlobRef,
    ) -> Result<SessionStorageResponse> {
        let dir = self.checked_path(&session)?;
        let bytes = crate::io::bounded_read(
            &object_path(&dir, &blob)?,
            SESSION_STORAGE_MAX_BLOB_BYTES as usize,
        )?;
        ensure!(
            bytes.len() as u64 == blob.bytes && crate::io::sha256_hex(&bytes) == blob.sha256,
            "blob size/hash mismatch"
        );
        Ok(SessionStorageResponse::BlobData {
            blob,
            data_base64: STANDARD.encode(bytes),
        })
    }
    pub(super) fn read_blob_by_hash(
        &mut self,
        session: SessionLocator,
        sha256: String,
        max_bytes: u64,
    ) -> Result<SessionStorageResponse> {
        ensure!(
            max_bytes <= SESSION_STORAGE_MAX_BLOB_BYTES,
            "blob read exceeds maximum"
        );
        let dir = self.checked_path(&session)?;
        let path = object_path(
            &dir,
            &StorageBlobRef {
                sha256: sha256.clone(),
                bytes: 0,
            },
        )?;
        let bytes = crate::io::bounded_read(&path, max_bytes as usize)?;
        ensure!(
            crate::io::sha256_hex(&bytes) == sha256,
            "blob hash mismatch"
        );
        Ok(SessionStorageResponse::BlobData {
            blob: StorageBlobRef {
                sha256,
                bytes: bytes.len() as u64,
            },
            data_base64: STANDARD.encode(bytes),
        })
    }
    pub(super) fn delete(
        &mut self,
        lease: StorageWriterLease,
        operation_id: Uuid,
    ) -> Result<SessionStorageResponse> {
        let dir = super::locator::parse(&lease.session)?.path;
        let mut control = super::control::load(&dir)?;
        if let Some(receipt) = control.deletes.get(&operation_id) {
            ensure!(
                receipt.generation == lease.generation
                    && receipt.session_id == lease.session.session_id,
                "delete operation id conflicts with earlier identity"
            );
            if receipt.complete {
                return Ok(SessionStorageResponse::Done);
            }
        } else {
            self.writer_path(&lease)?;
            ensure!(
                self.writer(&lease)?.tokens.len() == 1,
                "session has other live writer handles; deletion requires exclusive ownership"
            );
            ensure!(
                control.generation == lease.generation && !control.deleted,
                "session generation is retired"
            );
            control.deletes.insert(
                operation_id,
                super::control::DeleteReceipt {
                    generation: lease.generation,
                    session_id: lease.session.session_id,
                    complete: false,
                },
            );
            super::control::save(&dir, &control)?;
        }
        // Keep ownership until both directory removal and durable receipt finish.
        let writer = self.writers.remove(&lease.session);
        let recovered_ownership = if writer.is_none() {
            Some(SessionWriteOwnership::acquire_slot(&dir)?)
        } else {
            None
        };
        if control.generation == lease.generation && dir.try_exists()? {
            ensure!(
                identity::resolve_session_identity(&dir)?.session_id == lease.session.session_id,
                "delete target identity changed"
            );
            std::fs::remove_dir_all(&dir)?;
            crate::io::sync_directory(dir.parent().context("session has no parent")?)?;
        }
        control.deleted = true;
        control
            .deletes
            .get_mut(&operation_id)
            .expect("receipt inserted")
            .complete = true;
        super::control::save(&dir, &control)?;
        self.prefixes.remove(&lease.session);
        self.prefix_order
            .retain(|session| session != &lease.session);
        self.catalog.remove(&lease.session);
        self.catalog_order
            .retain(|session| session != &lease.session);
        self.snapshots
            .retain(|_, snapshot| snapshot.identity.session != lease.session);
        self.snapshot_order
            .retain(|key| self.snapshots.contains_key(key));
        drop((writer, recovered_ownership));
        Ok(SessionStorageResponse::Done)
    }
}
