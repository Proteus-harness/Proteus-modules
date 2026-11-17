//! Persistent generation and deletion receipts live beside the never-unlinked lock.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Control {
    pub generation: Uuid,
    #[serde(default)]
    pub incarnation: Option<Uuid>,
    pub deleted: bool,
    pub deletes: BTreeMap<Uuid, DeleteReceipt>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteReceipt {
    pub generation: Uuid,
    pub session_id: SessionId,
    pub complete: bool,
}
impl Default for Control {
    fn default() -> Self {
        Self {
            generation: Uuid::new_v4(),
            incarnation: None,
            deleted: false,
            deletes: BTreeMap::new(),
        }
    }
}
fn path(dir: &Path) -> Result<PathBuf> {
    let name = dir
        .file_name()
        .context("session has no basename")?
        .to_string_lossy();
    Ok(dir
        .parent()
        .context("session has no parent")?
        .join(format!(".{name}.storage-control.json")))
}
pub(super) fn load(dir: &Path) -> Result<Control> {
    let path = path(dir)?;
    match crate::io::bounded_read(&path, 1024 * 1024) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(Control::default())
        }
        Err(error) => Err(error),
    }
}
pub(super) fn save(dir: &Path, control: &Control) -> Result<()> {
    let bytes = serde_json::to_vec(control)?;
    ensure!(
        bytes.len() <= 1024 * 1024,
        "session generation/deletion ledger exceeds maximum"
    );
    crate::io::atomic_write(&path(dir)?, &bytes)
}

/// A fresh Create after retirement names the next incarnation; stale locators
/// continue to name the old one and cannot materialize another directory.
pub(super) fn locator_incarnation(control: &Control) -> Option<Uuid> {
    if control.deleted {
        Some(control.generation)
    } else {
        control.incarnation
    }
}
