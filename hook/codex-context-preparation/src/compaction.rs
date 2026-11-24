use proteus_contracts::model_standard::{
    ContentPart,
    input_estimates::{ContextTokenEstimateSource, ModelInputEstimate},
};
use proteus_contracts::{
    contracts::{ContextPreparationInput, ContextPreparationOutput},
    process_module::{HookModuleHost, ProcessModuleError},
};
use serde_json::json;

use crate::{
    budget::{estimate_messages_tokens, resolve_trigger_tokens, user_message_budget_tokens},
    history::{replacement_messages, select_recent_user_messages, split_history},
    recovery::complete_summary_with_recovery,
};

pub(crate) fn compact(
    input: ContextPreparationInput,
    config: &crate::config::CompactorConfig,
    remote: bool,
    host: &mut dyn HookModuleHost,
) -> Result<ContextPreparationOutput, ProcessModuleError> {
    let estimates = if remote {
        match input.request.as_ref() {
            Some(request) => {
                crate::input_estimates::for_messages(&request.model, &input.messages, host)?
            }
            None => ModelInputEstimate::default(),
        }
    } else {
        ModelInputEstimate::default()
    };
    let has_opaque = input
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .any(|part| matches!(part.payload, ContentPart::Opaque { .. }));
    let token_estimate = if remote && has_opaque {
        let fallback =
            crate::remote_history::estimate_messages_with_estimates(&input.messages, &estimates);
        match input
            .request
            .as_ref()
            .and_then(|request| ContextTokenEstimateSource::from_metadata(&request.metadata))
        {
            Some(ContextTokenEstimateSource::Observed) => input.token_estimate.unwrap_or(fallback),
            Some(ContextTokenEstimateSource::Estimated) => fallback,
            None => input.token_estimate.unwrap_or(0).max(fallback),
        }
    } else {
        input
            .token_estimate
            .unwrap_or_else(|| estimate_messages_tokens(&input.messages))
    };
    let unchanged = |mut output: ContextPreparationOutput| {
        if remote {
            estimates.write_metadata(&mut output.metadata);
        }
        output
    };
    let Some(trigger_tokens) = resolve_trigger_tokens(&input, config) else {
        return Ok(unchanged(unchanged_without_trigger(
            input.messages,
            token_estimate,
        )));
    };
    if input.messages.is_empty() {
        return Ok(unchanged(unchanged_with_diagnostics(
            input.messages,
            token_estimate,
            trigger_tokens,
            "no_persistent_history_to_compact",
        )));
    }
    if token_estimate < trigger_tokens {
        return Ok(unchanged(unchanged_with_diagnostics(
            input.messages,
            token_estimate,
            trigger_tokens,
            "below_trigger_threshold",
        )));
    }

    let history = split_history(&input.messages);
    if history.compactable_message_count == 0 {
        return Ok(unchanged(unchanged_with_diagnostics(
            input.messages,
            token_estimate,
            trigger_tokens,
            "no_persistent_history_to_compact",
        )));
    }

    if remote {
        return crate::remote::compact(
            input,
            config,
            history,
            token_estimate,
            trigger_tokens,
            &estimates,
            host,
        );
    }

    // Filter newest-first so exhausted budgets never classify or clone older users.
    let preserved_user_messages =
        select_recent_user_messages(&input.messages, user_message_budget_tokens());
    let summary = complete_summary_with_recovery(&input, config, history.summary_history, host)?;
    let retained_count = preserved_user_messages.messages.len();
    let replacement = replacement_messages(
        &history.ephemeral_context,
        preserved_user_messages.messages,
        &summary,
    );
    let output_token_estimate = estimate_messages_tokens(&replacement);

    let mut output = ContextPreparationOutput::changed(replacement, Some(summary));
    output.user_message_replacements = preserved_user_messages.replacements;
    output.token_estimate = Some(output_token_estimate);
    output.original_token_estimate = Some(token_estimate);
    output.trigger_tokens = Some(trigger_tokens);
    output.summary_source = Some("model".to_owned());
    output.metadata = json!({
        "compacted_messages": history.compactable_message_count,
        "preserved_user_messages": retained_count,
        "ephemeral_context_messages": history.ephemeral_context.len(),
    });
    Ok(output)
}

fn unchanged_without_trigger(
    messages: Vec<proteus_contracts::model_standard::CanonicalMessage>,
    token_estimate: u32,
) -> ContextPreparationOutput {
    let mut output = ContextPreparationOutput::unchanged(messages);
    output.token_estimate = Some(token_estimate);
    output.original_token_estimate = Some(token_estimate);
    output.skipped_reason = Some("no_trigger_threshold".to_owned());
    output
}

fn unchanged_with_diagnostics(
    messages: Vec<proteus_contracts::model_standard::CanonicalMessage>,
    token_estimate: u32,
    trigger_tokens: u32,
    reason: &str,
) -> ContextPreparationOutput {
    let mut output = ContextPreparationOutput::unchanged(messages);
    output.token_estimate = Some(token_estimate);
    output.original_token_estimate = Some(token_estimate);
    output.trigger_tokens = Some(trigger_tokens);
    output.skipped_reason = Some(reason.to_owned());
    output.metadata = serde_json::Value::Null;
    output
}
