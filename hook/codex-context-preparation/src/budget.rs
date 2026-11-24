use crate::history::message_text_len;
use proteus_contracts::{contracts::ContextPreparationInput, model_standard::CanonicalMessage};

const DEFAULT_USER_MESSAGE_BUDGET_TOKENS: usize = 20_000;

pub(crate) fn estimate_messages_tokens(messages: &[CanonicalMessage]) -> u32 {
    estimate_message_text_lengths(messages.iter().filter_map(message_text_len))
}

fn estimate_message_text_lengths(lengths: impl Iterator<Item = usize>) -> u32 {
    let tokens = lengths.map(estimate_text_bytes).sum::<usize>();
    u32::try_from(tokens.max(1)).unwrap_or(u32::MAX)
}

fn estimate_text_bytes(bytes: usize) -> usize {
    bytes.saturating_add(3) / 4
}

pub(crate) fn estimate_text_tokens(text: &str) -> usize {
    estimate_text_bytes(text.len())
}

pub(crate) fn truncate_to_tokens(text: &str, max_tokens: usize) -> String {
    let max_bytes = max_tokens.saturating_mul(4);
    if text.is_empty() || (max_tokens > 0 && text.len() <= max_bytes) {
        return text.to_owned();
    }
    truncate_middle_with_token_budget(text, max_tokens)
}

fn truncate_middle_with_token_budget(text: &str, max_tokens: usize) -> String {
    let max_bytes = max_tokens.saturating_mul(4);
    if max_bytes == 0 {
        return format!("…{} tokens truncated…", estimate_text_tokens(text));
    }
    let left_budget = max_bytes / 2;
    let right_budget = max_bytes - left_budget;
    let tail_start = text.len().saturating_sub(right_budget);
    let prefix_end = text.floor_char_boundary(left_budget);
    let suffix_start = text.ceil_char_boundary(tail_start.max(prefix_end));
    let removed_bytes = text.len().saturating_sub(max_bytes);
    let marker_tokens = removed_bytes.saturating_add(3) / 4;
    format!(
        "{}…{marker_tokens} tokens truncated…{}",
        &text[..prefix_end],
        &text[suffix_start..]
    )
}

/// Mirrors the pinned Codex local auto-compact limit: 90% of the raw context
/// window, with an explicitly configured limit capped at that value. The
/// caller must not invent a default when the provider did not expose a window.
pub(crate) fn resolve_trigger_tokens(
    input: &ContextPreparationInput,
    config: &crate::config::CompactorConfig,
) -> Option<u32> {
    let configured = config.trigger_tokens;
    let context_limit = input
        .window_tokens
        .map(|window| (u64::from(window) * 9 / 10) as u32);
    match (configured, context_limit) {
        (Some(configured), Some(context_limit)) => Some(configured.min(context_limit)),
        (Some(configured), None) => Some(configured),
        (None, Some(context_limit)) => Some(context_limit),
        (None, None) => None,
    }
}

pub(crate) fn user_message_budget_tokens() -> usize {
    DEFAULT_USER_MESSAGE_BUDGET_TOKENS
}
