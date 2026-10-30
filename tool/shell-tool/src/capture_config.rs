//! Explicit, binding-owned ephemeral output retention. Absence means no capture.
use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use proteus_contracts::domain::{ConfigField, ConfigValueSchema, ModuleConfigSchema};
use serde_json::Value;

const MIB: u64 = 1024 * 1024;
pub(crate) const MAX_PATH_BYTES: usize = 4096;

pub(crate) struct CaptureConfig {
    pub(crate) directory: PathBuf,
    pub(crate) cap: u64,
    pub(crate) slots: usize,
    pub(crate) retention: Duration,
}

impl CaptureConfig {
    pub(crate) fn parse(config: &Value) -> Result<Option<Self>> {
        let Some(value) = config.get("source_capture") else {
            return Ok(None);
        };
        let object = value
            .as_object()
            .context("source_capture must be an object")?;
        const KEYS: [&str; 5] = [
            "mode",
            "directory",
            "max_capture_bytes",
            "max_captures",
            "retention_seconds",
        ];
        if object.len() != KEYS.len() || object.keys().any(|key| !KEYS.contains(&key.as_str())) {
            bail!(
                "source_capture requires exactly mode, directory, max_capture_bytes, max_captures, retention_seconds"
            );
        }
        if object["mode"].as_str() != Some("ephemeral_file") {
            bail!("source_capture mode must be ephemeral_file");
        }
        let directory = object["directory"]
            .as_str()
            .context("capture directory must be a string")?;
        if directory.len() > MAX_PATH_BYTES || !std::path::Path::new(directory).is_absolute() {
            bail!("capture directory must be an absolute path of at most 4096 bytes");
        }
        let directory = PathBuf::from(directory)
            .canonicalize()
            .context("capture directory is unavailable")?;
        if !directory.is_dir() || directory.as_os_str().as_encoded_bytes().len() > MAX_PATH_BYTES {
            bail!("capture directory must be an existing directory of at most 4096 encoded bytes");
        }
        let cap = integer(
            &object["max_capture_bytes"],
            1,
            64 * MIB,
            "max_capture_bytes",
        )?;
        let slots = integer(&object["max_captures"], 1, 16, "max_captures")?;
        if cap * slots > 256 * MIB {
            bail!("capture per-binding reservation must not exceed 256 MiB");
        }
        let seconds = integer(&object["retention_seconds"], 1, 1800, "retention_seconds")?;
        Ok(Some(Self {
            directory,
            cap,
            slots: slots as usize,
            retention: Duration::from_secs(seconds),
        }))
    }
}

fn integer(value: &Value, min: u64, max: u64, name: &str) -> Result<u64> {
    value
        .as_u64()
        .filter(|n| (min..=max).contains(n))
        .with_context(|| format!("source_capture {name} must be an integer in {min}..={max}"))
}

pub fn config_schema() -> ModuleConfigSchema {
    let bounded = |min, max| ConfigValueSchema::Integer {
        minimum: Some(min),
        maximum: Some(max),
    };
    ModuleConfigSchema { fields: vec![ConfigField::new(
        "source_capture", "Временное сохранение вывода exec",
        "Необязательный объект. Сохраняет raw prefix локально до TTL/выхода worker; вывод может содержать secrets. Не durable artifact и не срок физического удаления. Отсутствует — ничего не сохраняется.",
        ConfigValueSchema::Object { fields: vec![
            ConfigField::new("mode", "Режим", "Только ephemeral anonymous files.", ConfigValueSchema::choices(&[("ephemeral_file", "Временный файл")])).required(),
            ConfigField::new("directory", "Каталог", "Существующий абсолютный private directory, выбранный оператором; до 4096 encoded bytes.", ConfigValueSchema::text()).required(),
            ConfigField::new("max_capture_bytes", "Байт на capture", "Лимит raw prefix. Произведение на max_captures не больше 256 MiB.", bounded(1, 64 * MIB as i64)).required(),
            ConfigField::new("max_captures", "Captures на binding", "Включает active/pending/retired handles; не process-wide quota.", bounded(1, 16)).required(),
            ConfigField::new("retention_seconds", "Логический TTL", "После завершения writer; cleanup lazy, не deletion SLA.", bounded(1, 1800)).required(),
        ] },
    ).advanced()] }
}

#[cfg(test)]
mod tests;
