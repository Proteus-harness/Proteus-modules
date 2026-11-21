//! JSONL memory reference process module.
//!
//! Provides `jsonl_memory` tools and an automatic recall context provider.

use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow};
use proteus_contracts::{
    domain::{MemoryItem, MemoryQuery},
    process_module::{ModuleRegistry, ProcessModuleError},
};
use serde::Deserialize;
mod provider;
mod tools;
#[cfg(test)]
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonlMemoryConfig {
    #[serde(default = "JsonlMemoryStoreModule::default_path")]
    path: PathBuf,
}

pub fn config_schema() -> proteus_contracts::domain::ModuleConfigSchema {
    use proteus_contracts::domain::{ConfigField, ConfigValueSchema, ModuleConfigSchema};
    ModuleConfigSchema {
        fields: vec![
            ConfigField::new(
                "path",
                "Файл памяти",
                "Путь относительно рабочей папки модуля.",
                ConfigValueSchema::text(),
            )
            .with_default(
                JsonlMemoryStoreModule::default_path()
                    .to_string_lossy()
                    .into_owned(),
            ),
        ],
    }
}

impl Default for JsonlMemoryConfig {
    fn default() -> Self {
        Self {
            path: JsonlMemoryStoreModule::default_path(),
        }
    }
}

pub struct JsonlMemoryStoreModule {
    path: PathBuf,
    lock: Mutex<()>,
}

impl JsonlMemoryStoreModule {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn default_path() -> PathBuf {
        PathBuf::from(".proteus/memory.jsonl")
    }
}

impl Default for JsonlMemoryStoreModule {
    fn default() -> Self {
        Self::new(Self::default_path())
    }
}

impl JsonlMemoryStoreModule {
    fn remember(&self, item: &MemoryItem) -> Result<(), ProcessModuleError> {
        let payload = serde_json::to_string(item)
            .map_err(|error| ProcessModuleError::new(error.to_string()))?;
        remember_impl(&self.path, &self.lock, &payload)
            .map_err(|error| ProcessModuleError::new(format!("{error:#}")))
    }
    fn recall(&self, query: &MemoryQuery) -> Result<Vec<MemoryItem>, ProcessModuleError> {
        recall_impl(&self.path, query)
            .map_err(|error| ProcessModuleError::new(format!("{error:#}")))
    }
}

fn remember_impl(path: &PathBuf, lock: &Mutex<()>, item_json: &str) -> Result<()> {
    let item: MemoryItem =
        serde_json::from_str(item_json).with_context(|| "invalid MemoryItem JSON")?;
    let _guard = lock
        .lock()
        .map_err(|_| anyhow!("jsonl memory mutex poisoned"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open memory {}", path.display()))?;
    let mut line = serde_json::to_vec(&item)?;
    line.push(b'\n');
    file.write_all(&line)?;
    file.flush()?;
    Ok(())
}

fn recall_impl(path: &PathBuf, query: &MemoryQuery) -> Result<Vec<MemoryItem>> {
    if query.limit == 0 {
        return Ok(Vec::new());
    }
    let file = match OpenOptions::new().read(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut items = Vec::new();
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    for line_number in 1usize.. {
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        // Match BufRead::lines: an unpaired CR at EOF is not a delimiter.
        let text = match line.strip_suffix('\n') {
            Some(text) => text.strip_suffix('\r').unwrap_or(text),
            None => &line,
        };
        let item: MemoryItem = serde_json::from_str(text).with_context(|| {
            format!(
                "invalid MemoryItem JSON in {} line {line_number}",
                path.display()
            )
        })?;
        if query.text.is_empty() || item.content.contains(&query.text) {
            items.push(item);
        }
        if items.len() >= query.limit {
            break;
        }
        // Never retain a large outlier as scratch for all following records.
        if line.capacity() > reader.capacity() {
            line = String::new();
        } else {
            line.clear();
        }
    }
    Ok(items)
}

pub fn register_modules(registry: &mut dyn ModuleRegistry) -> Result<(), ProcessModuleError> {
    let config: JsonlMemoryConfig = serde_json::from_value(registry.module_config().clone())
        .map_err(|error| {
            ProcessModuleError::new(format!("invalid jsonl memory config: {error}"))
        })?;
    let store = Arc::new(JsonlMemoryStoreModule::new(config.path));
    registry.register_tool(Box::new(tools::MemoryTool {
        store: store.clone(),
        remember: true,
    }))?;
    registry.register_tool(Box::new(tools::MemoryTool {
        store: store.clone(),
        remember: false,
    }))?;
    registry.register_context_provider(
        "jsonl_memory".into(),
        Box::new(provider::MemoryProvider { store }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonl_recall_preserves_line_boundaries_and_stops_at_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.jsonl");
        let facts = [
            MemoryItem::new("fact", "не подходит".repeat(7000), Value::Null),
            MemoryItem::new("fact", "Match 🙂", Value::Null),
            MemoryItem::new("fact", "Match é", Value::Null),
        ];
        let rows = facts
            .each_ref()
            .map(|fact| serde_json::to_string(fact).unwrap());
        let store = JsonlMemoryStoreModule::new(path.clone());
        fs::write(&path, format!("{}\r\n{}\n{}\r", rows[0], rows[1], rows[2])).unwrap();
        assert_eq!(
            store.recall(&MemoryQuery::new("Match", 10)).unwrap(),
            facts[1..]
        );

        let mut data = format!("{}\r\n{}\n", rows[0], rows[1]).into_bytes();
        data.extend_from_slice(b"\xff\r\n");
        fs::write(&path, data).unwrap();
        assert_eq!(
            store.recall(&MemoryQuery::new("Match", 1)).unwrap(),
            facts[1..2]
        );
        let error = store.recall(&MemoryQuery::new("Match", 2)).unwrap_err();
        assert!(error.to_string().contains("valid UTF-8"), "{error}");
    }

    #[test]
    fn jsonl_recall_preserves_json_error_columns_for_crlf_and_bare_cr() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.jsonl");
        let store = JsonlMemoryStoreModule::new(path.clone());
        for (row, raw) in [("{", "{\r\n"), ("{\r", "{\r")] {
            fs::write(&path, raw).unwrap();
            let expected = serde_json::from_str::<MemoryItem>(row).unwrap_err();
            let actual = store.recall(&MemoryQuery::new("", 10)).unwrap_err();
            assert!(actual.to_string().contains("line 1:"), "{actual}");
            assert!(
                actual.to_string().ends_with(&expected.to_string()),
                "{actual}"
            );
        }
    }

    #[test]
    fn configured_jsonl_backend_remembers_across_reopened_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.jsonl");
        let fact = MemoryItem::new(
            "fact",
            "durable architecture fact",
            serde_json::json!({"scope":"owner"}),
        );
        JsonlMemoryStoreModule::new(path.clone())
            .remember(&fact)
            .unwrap();
        let result = JsonlMemoryStoreModule::new(path)
            .recall(&MemoryQuery::new("architecture", 10))
            .unwrap();
        let recalled = result;
        assert_eq!(recalled.len(), 1);
        assert_eq!(recalled[0].kind, fact.kind);
        assert_eq!(recalled[0].content, fact.content);
        assert_eq!(recalled[0].metadata["scope"], "owner");
    }

    #[test]
    fn jsonl_recall_rejects_malformed_lines() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("memory.jsonl");
        let first = MemoryItem::new("decision", "keep this", Value::Null);
        let second = MemoryItem::new("preference", "keep that", Value::Null);
        let contents = format!(
            "{}\nnot-json\n{}\n",
            serde_json::to_string(&first).expect("first item"),
            serde_json::to_string(&second).expect("second item")
        );
        fs::write(&path, contents).expect("memory file");

        let store = JsonlMemoryStoreModule::new(path.clone());
        assert_eq!(store.recall(&MemoryQuery::new("keep", 1)).unwrap(), [first]);
        let error = store
            .recall(&MemoryQuery::new("keep", 10))
            .expect_err("malformed memory line must fail");
        assert!(
            error.message.starts_with(&format!(
                "invalid MemoryItem JSON in {} line 2:",
                path.display()
            )),
            "{error}"
        );
    }

    #[test]
    fn jsonl_recall_limit_zero_returns_no_items() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.jsonl");
        fs::write(&path, "not-json\n").unwrap();
        let store = JsonlMemoryStoreModule::new(path);
        for text in ["", "remembered"] {
            assert!(store.recall(&MemoryQuery::new(text, 0)).unwrap().is_empty());
        }
    }
}
