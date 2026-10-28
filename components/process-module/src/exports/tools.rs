use std::collections::HashMap;

use anyhow::{Result, anyhow};
use proteus_contracts::{
    contracts::ProcessToolDefinition, domain::ToolSpec, process_module::ToolModuleObject,
};

/// One immutable catalog per loaded tool export; never shared across bindings.
/// The ordered definitions remain authoritative for list, including duplicates.
/// Core still owns duplicate-name and schema validation before registration.
#[derive(Default)]
pub(super) struct ToolCatalog {
    definitions: Vec<std::result::Result<ProcessToolDefinition, String>>,
    first_by_name: HashMap<String, usize>,
}

impl ToolCatalog {
    pub(super) fn new(tools: &[ToolModuleObject]) -> Self {
        let mut definitions = Vec::with_capacity(tools.len());
        let mut first_by_name = HashMap::with_capacity(tools.len());
        for (index, tool) in tools.iter().enumerate() {
            let definition = serde_json::from_str::<ToolSpec>(&tool.spec_json())
                .map(|spec| ProcessToolDefinition {
                    spec,
                    model_visible: tool.model_visible(),
                    user_command: tool.user_command(),
                })
                .map_err(|error| error.to_string());
            if let Ok(definition) = &definition {
                // Match the previous linear find: exact name, first valid spec.
                first_by_name
                    .entry(definition.spec.name.clone())
                    .or_insert(index);
            }
            // Retain parse failures: list must not silently omit invalid tools,
            // while invocation lookup previously skipped their invalid specs.
            definitions.push(definition);
        }
        Self {
            definitions,
            first_by_name,
        }
    }

    pub(super) fn definitions(&self) -> Result<Vec<ProcessToolDefinition>> {
        self.definitions
            .iter()
            .map(|definition| match definition {
                Ok(definition) => Ok(definition.clone()),
                Err(error) => Err(anyhow!(error.clone())),
            })
            .collect()
    }

    pub(super) fn index(&self, name: &str) -> Option<usize> {
        self.first_by_name.get(name).copied()
    }
}

#[cfg(test)]
mod tests;
