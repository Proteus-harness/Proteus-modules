//! Подтверждённое хранение сессий: JSONL и самостоятельный SQLite journal.
//! Реализация не зависит от proteus-core и не исполняет модель или tools.
pub mod archive;
pub mod backend;
pub mod blob_codec;
mod codec;
mod diagnostics;
mod identity;
mod io;
mod lease;
mod metadata;
mod path_identity;
mod service;
mod snapshot;
mod types;
mod workspace_dir;

pub use service::{BackendKind, StorageService};

#[cfg(test)]
mod tests;
