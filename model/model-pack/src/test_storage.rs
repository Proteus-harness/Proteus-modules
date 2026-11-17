//! Persistence fixtures use the independently built storage process, never a fake.
use std::path::{Path, PathBuf};

use proteus_core::core::{AppConfig, StorageService, configured_session_storage};
use serde_json::json;

pub(crate) fn service(workspace: &Path) -> StorageService {
    let command = PathBuf::from(
        std::env::var_os("PROTEUS_TEST_SESSION_STORAGE_MODULE")
            .expect("run scripts/test.py full with the prepared storage worker"),
    );
    assert!(
        command.is_absolute(),
        "storage fixture executable must be absolute"
    );
    let mut config = AppConfig::default();
    config.modules.session_storage = Some("jsonl".into());
    config.components.insert(
        "fixture-session-storage".into(),
        serde_json::from_value(json!({
            "command": command,
            "transport": {"max_frame_bytes": 134217728},
            "exports": {"session_storage": {"jsonl": {}}},
        }))
        .expect("storage fixture component"),
    );
    configured_session_storage(&config, workspace).expect("storage fixture service")
}
