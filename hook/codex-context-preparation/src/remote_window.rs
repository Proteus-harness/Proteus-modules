//! Pinned remote preflight only rewrites a trailing run of tool outputs when
//! the request is already over the known context window. It never drops calls.
//! Adapted from OpenAI Codex 67cc3c3 compact_remote.rs (Apache-2.0).
use proteus_contracts::{
    domain::{new_message_id, new_part_id},
    model_standard::{CanonicalMessage, ContentPart, InstructionBlock},
};

use crate::{budget::estimate_text_tokens, remote_history::estimate_messages};

const TRUNCATED_OUTPUT: &str = "Output exceeded the available model context and was truncated";

pub(crate) fn trim_trailing_tool_outputs(
    messages: &mut [CanonicalMessage],
    instructions: &[InstructionBlock],
    window_tokens: Option<u32>,
) {
    let Some(window) = window_tokens else { return };
    let base_tokens = instructions.iter().fold(0usize, |total, block| {
        total.saturating_add(estimate_text_tokens(&block.text))
    });
    let mut estimate = usize::try_from(estimate_messages(messages))
        .unwrap_or(usize::MAX)
        .saturating_add(base_tokens);
    for message in messages.iter_mut().rev() {
        if estimate <= window as usize {
            break;
        }
        if message.parts.is_empty()
            || !message
                .parts
                .iter()
                .all(|part| matches!(part.payload, ContentPart::ToolResult { .. }))
        {
            break;
        }
        let before = estimate_messages(std::slice::from_ref(message)) as usize;
        message.id = new_message_id();
        for part in &mut message.parts {
            if let ContentPart::ToolResult { result } = &mut part.payload {
                result.output = TRUNCATED_OUTPUT.to_owned();
                result.content.clear();
                if result.error.is_some() {
                    result.error = Some(TRUNCATED_OUTPUT.to_owned());
                }
                part.part_id = new_part_id();
            }
        }
        let after = estimate_messages(std::slice::from_ref(message)) as usize;
        estimate = estimate.saturating_sub(before).saturating_add(after);
    }
}
