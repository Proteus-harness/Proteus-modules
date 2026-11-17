//! The integration fixtures select a real independent storage worker explicitly.
use std::path::{Path, PathBuf};

use proteus_core::core::{
    AppConfig, EvalReport, SessionStore, StorageService, configured_session_storage,
    read_eval_report,
};
use serde_json::json;

pub fn configure(config: &mut AppConfig) {
    // Production profiles may already export storage. Preserve every other slot
    // while replacing that export with the executable prepared by the runner.
    config.components.retain(|_, component| {
        let mut value = serde_json::to_value(&*component).expect("component config");
        if let Some(exports) = value["exports"].as_object_mut() {
            exports.remove("session_storage");
            if exports.is_empty() {
                return false;
            }
        }
        *component = serde_json::from_value(value).expect("component without storage export");
        true
    });
    let command = std::env::var_os("PROTEUS_TEST_SESSION_STORAGE_MODULE")
        .expect("run scripts/test.py with the prepared session storage worker");
    config.modules.session_storage = Some("jsonl".into());
    config.components.insert(
        "fixture-session-storage".into(),
        serde_json::from_value(json!({
            "command": PathBuf::from(command),
            "transport": {"max_frame_bytes": 134217728},
            "exports": {"session_storage": {"jsonl": {}}}
        }))
        .expect("storage component config"),
    );
}

fn service() -> StorageService {
    let mut config = AppConfig::default();
    configure(&mut config);
    configured_session_storage(
        &config,
        &std::env::current_dir().expect("current directory"),
    )
    .expect("configured fixture session storage")
}

pub fn open(path: PathBuf) -> anyhow::Result<SessionStore> {
    SessionStore::open(service(), path)
}

#[allow(dead_code)]
pub fn eval(path: impl AsRef<Path>) -> anyhow::Result<EvalReport> {
    read_eval_report(service(), path)
}
