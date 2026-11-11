use std::time::Duration;

use proteus_contracts::domain::{ConfigField, ConfigValueSchema, ModuleConfigSchema};
use serde::Deserialize;
use serde_json::{Value, json};

pub(crate) const MAX_CODE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_TRANSFER_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Limits {
    pub timeout_ms: u64,
    pub memory_limit_bytes: usize,
    pub max_calls: usize,
    pub max_concurrency: usize,
    pub max_output_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout_ms: 30_000,
            memory_limit_bytes: 32 * 1024 * 1024,
            max_calls: 256,
            max_concurrency: 4,
            max_output_bytes: 32 * 1024,
        }
    }
}

impl Limits {
    pub fn parse(value: Value) -> Result<Self, String> {
        let limits: Self = serde_json::from_value(value)
            .map_err(|error| format!("invalid code_mode config: {error}"))?;
        for (name, value, min, max) in [
            ("timeout_ms", limits.timeout_ms, 1, 60_000),
            (
                "memory_limit_bytes",
                limits.memory_limit_bytes as u64,
                1024 * 1024,
                128 * 1024 * 1024,
            ),
            ("max_calls", limits.max_calls as u64, 1, 1024),
            ("max_concurrency", limits.max_concurrency as u64, 1, 16),
            (
                "max_output_bytes",
                limits.max_output_bytes as u64,
                1,
                1024 * 1024,
            ),
        ] {
            if !(min..=max).contains(&value) {
                return Err(format!(
                    "code_mode {name} must be an integer in {min}..={max}"
                ));
            }
        }
        Ok(limits)
    }

    pub fn validate_script(&self, code: &str, timeout_ms: Option<u64>) -> Result<Duration, String> {
        if code.len() > MAX_CODE_BYTES {
            return Err(format!("code_mode code exceeds {MAX_CODE_BYTES} bytes"));
        }
        let timeout_ms = timeout_ms.unwrap_or(self.timeout_ms);
        if timeout_ms == 0 || timeout_ms > self.timeout_ms {
            return Err(format!(
                "code_mode timeout_ms must be in 1..={}",
                self.timeout_ms
            ));
        }
        Ok(Duration::from_millis(timeout_ms))
    }
}

pub fn config_schema() -> ModuleConfigSchema {
    let integer = |id, label, description, default, min, max| {
        ConfigField::new(
            id,
            label,
            description,
            ConfigValueSchema::Integer {
                minimum: Some(min),
                maximum: Some(max),
            },
        )
        .with_default(json!(default))
    };
    ModuleConfigSchema {
        fields: vec![
            integer(
                "timeout_ms",
                "Бюджет скрипта, мс",
                "До 60 секунд. Уже запущенные host callbacks завершаются по родительской отмене/deadline; JS не получает дополнительные OS-права.",
                30_000,
                1,
                60_000,
            ),
            integer(
                "memory_limit_bytes",
                "JS heap, байт",
                "Бюджет QuickJS; память native bridge и процессов инструментов учитывается отдельно.",
                32 * 1024 * 1024,
                1024 * 1024,
                128 * 1024 * 1024,
            ),
            integer(
                "max_calls",
                "Вложенных вызовов",
                "Максимальное число запрошенных tools.call за скрипт.",
                256,
                1,
                1024,
            ),
            integer(
                "max_concurrency",
                "Одновременных callbacks",
                "Верхняя граница host callbacks; Core сохраняет собственные execution gates каждого инструмента.",
                4,
                1,
                16,
            ),
            integer(
                "max_output_bytes",
                "Печатного вывода, байт",
                "Общий UTF-8 бюджет text и console.log; переполнение завершает скрипт ошибкой с сохранением префикса.",
                32 * 1024,
                1,
                1024 * 1024,
            ),
        ],
    }
}
