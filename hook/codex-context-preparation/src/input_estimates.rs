//! Provider-owned local sizing. No checkpoint schema or ciphertext parsing here.
use proteus_contracts::{
    domain::ModelRef,
    model_standard::{
        CanonicalMessage, ContentPart,
        input_estimates::{ModelInputEstimate, ModelInputEstimateRequest},
    },
    process_module::{HookModuleHost, ProcessModuleError},
};

pub(crate) fn for_messages(
    model: &ModelRef,
    messages: &[CanonicalMessage],
    host: &mut dyn HookModuleHost,
) -> Result<ModelInputEstimate, ProcessModuleError> {
    let parts = messages
        .iter()
        .flat_map(|message| &message.parts)
        .filter(|part| matches!(part.payload, ContentPart::Opaque { .. }))
        .cloned()
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return Ok(ModelInputEstimate::default());
    }
    crate::summary::ensure_not_cancelled(host)?;
    let request = ModelInputEstimateRequest {
        model: model.clone(),
        parts,
    };
    request.validate().map_err(super::error)?;
    let result =
        host.estimate_model_input_json(serde_json::to_string(&request).map_err(super::error)?)?;
    crate::summary::ensure_not_cancelled(host)?;
    let estimate: Option<ModelInputEstimate> =
        serde_json::from_str(&result).map_err(super::error)?;
    let estimate = estimate.unwrap_or_default();
    estimate.validate(&request).map_err(super::error)?;
    Ok(estimate)
}
