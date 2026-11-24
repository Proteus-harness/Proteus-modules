use anyhow::{Context, Result, ensure};
use proteus_contracts::contracts::{ModelCatalog, ModelCatalogEntry};
use proteus_contracts::model_standard::ModelCapabilities;
use serde::Deserialize;
use std::collections::HashSet;

#[derive(Debug, Deserialize)]
pub(super) struct Models {
    data: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
pub(super) struct Entry {
    id: String,
    name: Option<String>,
    description: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    status: Option<String>,
    #[serde(default)]
    capabilities: Capabilities,
    #[serde(default)]
    modalities: Modalities,
    #[serde(default)]
    limit: Limits,
}

#[derive(Default, Debug, Deserialize)]
struct Modalities {
    #[serde(default)]
    input: Vec<String>,
}
#[derive(Default, Debug, Deserialize)]
struct Limits {
    input: Option<u32>,
    context: Option<u32>,
    output: Option<u32>,
}
#[derive(Default, Debug, Deserialize)]
struct Capabilities {
    #[serde(default)]
    tools: Feature,
    #[serde(default)]
    reasoning: Feature,
    #[serde(default)]
    pub temperature: bool,
    #[serde(default)]
    pub top_p: bool,
}
#[derive(Default, Debug, Deserialize)]
struct Feature {
    #[serde(default)]
    supported: bool,
    #[serde(default)]
    effort: Vec<String>,
}

impl Models {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self> {
        let mut models: Self =
            serde_json::from_slice(bytes).context("invalid OpenCode Go model list")?;
        models.data.retain(|entry| entry.kind == "chat");
        let mut ids = HashSet::new();
        ensure!(
            !models.data.is_empty()
                && models
                    .data
                    .iter()
                    .all(|entry| !entry.id.is_empty() && ids.insert(&entry.id)),
            "invalid OpenCode Go chat model IDs"
        );
        models.catalog().validate().map_err(anyhow::Error::msg)?;
        Ok(models)
    }
    pub(super) fn get(&self, id: &str) -> Option<&Entry> {
        self.data.iter().find(|entry| entry.id == id)
    }
    pub(super) fn catalog(&self) -> ModelCatalog {
        ModelCatalog {
            models: self
                .data
                .iter()
                .map(|entry| ModelCatalogEntry {
                    id: entry.id.clone(),
                    display_name: entry.name.clone().unwrap_or_else(|| entry.id.clone()),
                    description: entry.description.clone(),
                    hidden: entry.status.as_deref() == Some("deprecated"),
                    reasoning_efforts: entry.capabilities.reasoning.effort.clone(),
                    default_reasoning_effort: None,
                })
                .collect(),
        }
    }
}

impl Entry {
    pub(super) fn descriptor(&self) -> ModelCapabilities {
        let tools = self.capabilities.tools.supported;
        let mut caps = ModelCapabilities::empty()
            .with_tools(tools)
            .with_parallel_tool_calls(tools)
            .with_image_input(
                self.modalities
                    .input
                    .iter()
                    .any(|m| m == "image" || m.starts_with("image/")),
            )
            .with_streaming(true)
            .with_system_role(true)
            .with_developer_role(true)
            .with_cache_hints(true)
            .with_reasoning_config(self.capabilities.reasoning.supported)
            .with_max_input_tokens(self.limit.input.or(self.limit.context));
        caps.max_output_tokens = self.limit.output;
        caps
    }
    pub(super) fn sampling(&self) -> (bool, bool) {
        (self.capabilities.temperature, self.capabilities.top_p)
    }
}
