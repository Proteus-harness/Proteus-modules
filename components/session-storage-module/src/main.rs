use std::sync::Arc;

use anyhow::{Result, bail};
use proteus_module_sdk::{
    Export,
    contracts::contracts::{
        PROCESS_SESSION_STORAGE_EXECUTE_METHOD, ProcessComponentExportInitialize,
        ProcessComponentExportManifest, SESSION_STORAGE_DISPLAY_PROJECTION_FEATURE,
        SessionStorageRequest, SessionStorageResponse,
    },
    hosts::HostBridge,
    session_storage::{SessionStorageExport, SessionStorageModule},
};
use serde_json::Value;
use session_storage::{BackendKind, StorageService};

mod display_reply;

#[derive(Clone)]
struct Backend(Arc<StorageService>);
impl SessionStorageModule for Backend {
    fn supports_display_projection(&self) -> bool {
        true
    }

    fn execute(&self, request: SessionStorageRequest) -> Result<SessionStorageResponse> {
        self.0.execute(request)
    }
}
struct DisplayExport {
    standard: SessionStorageExport<Backend>,
    backend: Backend,
    projection_negotiated: bool,
}

impl Export for DisplayExport {
    fn manifest(&self) -> ProcessComponentExportManifest {
        self.standard.manifest()
    }

    fn invoke(&self, method: &str, params: Value, host: &HostBridge) -> Result<Value> {
        // Leave the existing SDK contract and errors in charge for every
        // other request, including unnegotiated display requests.
        if !self.projection_negotiated
            || method != PROCESS_SESSION_STORAGE_EXECUTE_METHOD
            || params.get("operation").and_then(Value::as_str) != Some("open_display_snapshot")
        {
            return self.standard.invoke(method, params, host);
        }
        let request: SessionStorageRequest = serde_json::from_value(params)?;
        request.validate()?;
        display_reply::into_value(self.backend.execute(request)?)
    }
}

fn load(binding: ProcessComponentExportInitialize) -> Result<Box<dyn Export>> {
    if binding.slot != "session_storage" {
        bail!("unsupported storage slot {}", binding.slot);
    }
    let kind = match binding.module_id.as_str() {
        "jsonl" => BackendKind::Jsonl,
        "sqlite" => BackendKind::Sqlite,
        other => bail!("unsupported storage implementation {other}"),
    };
    let config = if binding.module_config.is_null() {
        None
    } else {
        Some(
            binding
                .module_config
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("storage config must be an object"))?,
        )
    };
    if config.is_some_and(|config| !config.is_empty()) {
        bail!("unknown session storage configuration fields");
    }
    let backend = Backend(Arc::new(StorageService::new(kind)));
    let standard = SessionStorageExport::new(binding, backend.clone())?;
    let projection_negotiated = standard
        .manifest()
        .module_features
        .iter()
        .any(|feature| feature == SESSION_STORAGE_DISPLAY_PROJECTION_FEATURE);
    Ok(Box::new(DisplayExport {
        standard,
        backend,
        projection_negotiated,
    }))
}
fn main() {
    let arguments: Vec<_> = std::env::args_os().collect();
    if arguments.len() == 2 && arguments[1] == "--version" {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        return;
    }
    if arguments
        .get(1)
        .is_some_and(|arg| arg == "--compact-archive")
    {
        let result = (|| -> Result<()> {
            anyhow::ensure!(
                arguments.len() == 3,
                "usage: --compact-archive SESSION_DIRECTORY"
            );
            let report = session_storage::archive::compact_jsonl_archive(std::path::Path::new(
                &arguments[2],
            ))?;
            println!("{}", serde_json::to_string(&report)?);
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("{}: {error:#}", env!("CARGO_PKG_NAME"));
            std::process::exit(1);
        }
        return;
    }
    if let Err(error) = proteus_module_sdk::run(load) {
        eprintln!("{}: {error:#}", env!("CARGO_PKG_NAME"));
        std::process::exit(1);
    }
}
