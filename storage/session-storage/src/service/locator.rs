//! Opaque, stable reservation identity. No process-local pending table is required.
use super::*;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Key<P = PathBuf> {
    version: u32,
    pub path: P,
    pub workspace: P,
    pub parent_session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incarnation: Option<Uuid>,
}
pub(super) fn make(
    path: &Path,
    workspace: &Path,
    id: SessionId,
    parent_session_id: Option<SessionId>,
    incarnation: Option<Uuid>,
) -> Result<SessionLocator> {
    let key = serde_json::to_string(&Key {
        version: 1,
        path,
        workspace,
        parent_session_id,
        incarnation,
    })?;
    Ok(SessionLocator {
        session_id: id,
        key,
    })
}
pub(super) fn parse(session: &SessionLocator) -> Result<Key> {
    ensure!(
        session.key.len() <= 64 * 1024,
        "storage locator exceeds maximum"
    );
    let key: Key =
        serde_json::from_str(&session.key).context("invalid opaque storage session locator")?;
    ensure!(key.version == 1, "unsupported storage locator version");
    ensure!(
        key.parent_session_id != Some(session.session_id),
        "session cannot be its own parent"
    );
    ensure!(
        canonicalize_unmaterialized_path(&key.path)? == key.path,
        "noncanonical storage session path"
    );
    ensure!(
        std::fs::canonicalize(&key.workspace)? == key.workspace,
        "noncanonical storage workspace path"
    );
    let expected = identity::short_session_directory_name(session.session_id);
    ensure!(
        key.path
            .file_name()
            .is_some_and(|name| name == expected.as_str()),
        "storage locator id/path mismatch"
    );
    ensure!(
        workspace_dir::workspace_path_from_session_dir(&key.path)? == key.workspace,
        "storage locator workspace/path mismatch"
    );
    Ok(key)
}
