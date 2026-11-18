use anyhow::{Result, bail};
use proteus_module_sdk::{
    contracts::contracts::{
        ProcessComponentExportInitialize, SessionStorageRequest, SessionStorageResponse,
    },
    session_storage::{SessionStorageExport, SessionStorageModule},
};
use session_storage::{BackendKind, StorageService};

struct Backend(StorageService);
impl SessionStorageModule for Backend {
    fn execute(&self, request: SessionStorageRequest) -> Result<SessionStorageResponse> {
        self.0.execute(request)
    }
}
fn load(binding: ProcessComponentExportInitialize) -> Result<Box<dyn proteus_module_sdk::Export>> {
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
    Ok(Box::new(SessionStorageExport::new(
        binding,
        Backend(StorageService::new(kind)),
    )?))
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
