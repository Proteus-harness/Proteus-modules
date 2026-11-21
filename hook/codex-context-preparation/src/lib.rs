//! Pinned Codex automatic compaction as an ordinary preparation hook.
mod budget;
mod compaction;
mod config;
mod config_schema;
pub use config_schema::config_schema;
mod base_instructions;
mod history;
mod input_estimates;
mod recovery;
mod remote;
mod remote_history;
mod remote_window;
mod summary;

use proteus_contracts::{
    contracts::{HookEvent, HookInput, HookResponse},
    model_standard::input_estimates::{ContextTokenEstimateSource, ModelInputEstimate},
    process_module::{HookModule, HookModuleHost, ModuleRegistry, ProcessModuleError},
};

pub(crate) const MODULE_ID: &str = "codex_compact";

pub struct CodexPreparationHook(config::CompactorConfig);

impl HookModule for CodexPreparationHook {
    fn invoke_json(
        &self,
        input_json: String,
        host: &mut dyn HookModuleHost,
    ) -> Result<String, ProcessModuleError> {
        let hook: HookInput = serde_json::from_str(&input_json).map_err(error)?;
        let response = match hook.event {
            HookEvent::BeforeContext {
                mut input,
                mut output,
                model_capabilities,
                ..
            } => {
                // Every participant transforms the validated preceding candidate.
                input.messages = std::mem::take(&mut output.messages);
                if output.changed {
                    input.token_estimate = output.token_estimate;
                    if let Some(request) = input.request.as_mut() {
                        ContextTokenEstimateSource::Estimated.write_metadata(&mut request.metadata);
                    }
                }
                let remote = model_capabilities.is_some_and(|value| value.supports_compaction);
                let prepared = compaction::compact(input, &self.0, remote, host)?;
                if output.changed && !prepared.changed {
                    if let Some(estimates) = ModelInputEstimate::from_metadata(&prepared.metadata) {
                        estimates.write_metadata(&mut output.metadata);
                        output.token_estimate = prepared.token_estimate;
                        HookResponse::ContextPreparationDiagnostics {
                            diagnostics: output.diagnostics(),
                        }
                    } else {
                        HookResponse::Continue
                    }
                } else if !prepared.changed {
                    HookResponse::ContextPreparationDiagnostics {
                        diagnostics: prepared.diagnostics(),
                    }
                } else {
                    HookResponse::ContextPreparation { output: prepared }
                }
            }
            _ => HookResponse::Continue,
        };
        serde_json::to_string(&response).map_err(error)
    }
}

fn error(error: impl std::fmt::Display) -> ProcessModuleError {
    ProcessModuleError::new(error.to_string())
}

pub fn register_modules(registry: &mut dyn ModuleRegistry) -> Result<(), ProcessModuleError> {
    let config = config::CompactorConfig::parse(registry.module_config()).map_err(error)?;
    registry.register_hook(MODULE_ID.to_owned(), Box::new(CodexPreparationHook(config)))
}

pub fn register_base_instructions(
    registry: &mut dyn ModuleRegistry,
) -> Result<(), ProcessModuleError> {
    registry.register_hook(
        "codex_base_instructions".to_owned(),
        Box::new(base_instructions::CodexBaseInstructions),
    )
}

#[cfg(test)]
mod tests;
