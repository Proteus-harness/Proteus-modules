//! Remote V2 compaction, adapted from Codex 67cc3c3 compact_remote_v2.rs.
//! Transport, OAuth and opaque encoding belong to the selected model module.
use std::time::Duration;

use proteus_contracts::{
    contracts::{ContextPreparationInput, ContextPreparationOutput},
    domain::{ResponseFormat, ToolChoice},
    model_standard::{
        CanonicalMessage, CanonicalModelRequest, CanonicalModelResponse, ContentPart, FinishReason,
        MessageRole, ModelFailureKind, ModelOperation, PartProvenance, PartScope,
        input_estimates::ModelInputEstimate,
    },
    process_module::{HookModuleHost, ProcessModuleError},
};
use serde_json::json;

use crate::{
    config::CompactorConfig,
    history::{HistoryParts, collect_user_messages},
    recovery::{retry_delay, wait_for_retry},
    remote_history::{estimate_messages_with_estimates, select_recent_users},
    summary::{ensure_not_cancelled, suppress_stream_deltas},
};

pub(crate) fn compact(
    input: ContextPreparationInput,
    config: &CompactorConfig,
    history: HistoryParts,
    token_estimate: u32,
    trigger_tokens: u32,
    estimates: &ModelInputEstimate,
    host: &mut dyn HookModuleHost,
) -> Result<ContextPreparationOutput, ProcessModuleError> {
    let users = collect_user_messages(&history.compactable_history);
    let retained = select_recent_users(&users, 64_000);
    let request = request(&input, history.summary_history, estimates)?;
    let checkpoint = complete_checkpoint(request, config, host)?;
    ensure_not_cancelled(host)?;

    let retained_count = retained.messages.len();
    let mut messages = retained.messages;
    let insertion = messages.len().saturating_sub(1);
    messages.splice(
        insertion..insertion,
        history.ephemeral_context.iter().cloned(),
    );
    messages.push(checkpoint);
    let estimates = crate::input_estimates::for_messages(
        &input
            .request
            .as_ref()
            .expect("remote request template")
            .model,
        &messages,
        host,
    )?;
    let output_estimate = estimate_messages_with_estimates(&messages, &estimates);
    let mut output = ContextPreparationOutput::changed(messages, None);
    output.user_message_replacements = retained.replacements;
    output.token_estimate = Some(output_estimate);
    output.original_token_estimate = Some(token_estimate);
    output.trigger_tokens = Some(trigger_tokens);
    output.summary_source = Some("model".to_owned());
    output.metadata = json!({
        "compacted_messages": history.compactable_history.len(),
        "preserved_user_messages": retained_count,
        "ephemeral_context_messages": history.ephemeral_context.len(),
        "remote_compaction": true,
    });
    estimates.write_metadata(&mut output.metadata);
    Ok(output)
}

fn request(
    input: &ContextPreparationInput,
    mut messages: Vec<CanonicalMessage>,
    estimates: &ModelInputEstimate,
) -> Result<CanonicalModelRequest, ProcessModuleError> {
    let template = input.request.as_ref().ok_or_else(|| {
        ProcessModuleError::new("remote compaction requires a model request template")
    })?;
    crate::remote_window::trim_trailing_tool_outputs_with_estimates(
        &mut messages,
        &template.instructions,
        input.window_tokens,
        estimates,
    );
    // No summary prompt and no loss of model-visible tools. The adapter owns
    // encoding the operation as its native compaction request/trigger.
    let mut request = CanonicalModelRequest::new(template.model.clone(), messages)
        .with_operation(ModelOperation::Compact)
        .with_instructions(template.instructions.clone())
        .with_tools(template.tools.clone())
        .with_tool_choice(ToolChoice::Auto)
        .with_response_format(ResponseFormat::Text)
        .with_sampling(template.sampling.clone())
        .with_reasoning(template.reasoning.clone())
        .with_limits(template.limits.clone())
        .with_cache(template.cache.clone())
        .with_client_metadata(template.client_metadata.clone())
        .with_metadata(template.metadata.clone());
    request.limits.max_output_tokens = None;
    suppress_stream_deltas(&mut request);
    Ok(request)
}

fn complete_checkpoint(
    request: CanonicalModelRequest,
    config: &CompactorConfig,
    host: &mut dyn HookModuleHost,
) -> Result<CanonicalMessage, ProcessModuleError> {
    let max_retries = config.stream_max_retries.min(2);
    let request = serde_json::to_string(&request).map_err(super::error)?;
    let mut retries = 0;
    loop {
        ensure_not_cancelled(host)?;
        let response = host
            .complete_model_json(request.clone())
            .and_then(|response| {
                ensure_not_cancelled(host)?;
                let response: CanonicalModelResponse =
                    serde_json::from_str(&response).map_err(super::error)?;
                checkpoint(response)
            });
        match response {
            Ok(checkpoint) => return Ok(checkpoint),
            Err(error) => {
                let delay = match error.model_failure.as_ref().map(|failure| failure.kind) {
                    Some(ModelFailureKind::Retryable { retry_delay_ms }) => {
                        retry_delay_ms.map(Duration::from_millis)
                    }
                    Some(ModelFailureKind::StreamDisconnected) => None,
                    // Remote protocol errors, context overflow, budget and
                    // cancellation are terminal. Never fall back to local.
                    _ => return Err(error),
                };
                if retries >= max_retries {
                    return Err(error);
                }
                retries += 1;
                wait_for_retry(host, delay.unwrap_or_else(|| retry_delay(retries)))?;
            }
        }
    }
}

fn checkpoint(response: CanonicalModelResponse) -> Result<CanonicalMessage, ProcessModuleError> {
    if response.finish_reason != FinishReason::Stop || !response.tool_calls.is_empty() {
        return Err(ProcessModuleError::new(
            "remote compaction must finish with Stop and cannot request tools",
        ));
    }
    let mut checkpoints = response.messages.into_iter().filter(|message| {
        message
            .parts
            .iter()
            .any(|part| matches!(part.payload, ContentPart::Opaque { .. }))
    });
    let Some(mut message) = checkpoints.next() else {
        return Err(ProcessModuleError::new(
            "remote compaction returned no opaque checkpoint",
        ));
    };
    if checkpoints.next().is_some()
        || message.role != MessageRole::Assistant
        || message.parts.len() != 1
        || !matches!(&message.parts[0].payload, ContentPart::Opaque { format, data }
            if !format.is_empty() && !data.is_null())
    {
        return Err(ProcessModuleError::new(
            "remote compaction must return exactly one assistant opaque checkpoint",
        ));
    }
    // Same bytes, compactor ownership. New part identity is required when its
    // provenance changes; the provider's response remains unchanged in journal.
    message.parts = message
        .parts
        .into_iter()
        .map(|part| {
            proteus_contracts::model_standard::CanonicalPart::new(
                PartProvenance::Compactor,
                PartScope::Conversation,
                part.payload,
            )
        })
        .collect();
    Ok(message)
}
