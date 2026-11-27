use std::collections::HashSet;

use proteus_contracts::{
    domain::{MessageId, ToolResult},
    model_standard::{CanonicalMessage, ContentPart, MessageRole, PartScope},
    process_module::ProcessModuleError,
};
use serde_json::Value;

pub(crate) fn tool_result_message(result: ToolResult) -> CanonicalMessage {
    let call_id = result.call_id.clone();
    CanonicalMessage::new(MessageRole::Tool, vec![ContentPart::ToolResult { result }])
        .with_tool_call_id(call_id)
}

/// Pinned Codex context_manager/normalize.rs: missing call outputs become
/// prompt-only "aborted" outputs. This does not assert whether an effect ran.
/// The journal and durable history retain the original unresolved call.
pub(crate) fn normalize_missing_tool_outputs(messages: &mut Vec<CanonicalMessage>) {
    let mut has_tool_calls = false;
    let completed = messages
        .iter()
        .flat_map(|message| &message.parts)
        .filter_map(|part| match &part.payload {
            ContentPart::ToolResult { result } => Some(&result.call_id),
            ContentPart::ToolCall { .. } => {
                has_tool_calls = true;
                None
            }
            _ => None,
        })
        .collect::<HashSet<_>>();
    if !has_tool_calls {
        return;
    }
    let mut inserts = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        for part in &message.parts {
            if let ContentPart::ToolCall { call } = &part.payload
                && !completed.contains(&call.id)
            {
                let mut output = tool_result_message(ToolResult::error(call.id.clone(), "aborted"));
                output.parts[0].scope = PartScope::Request;
                inserts.push((index + 1, output));
            }
        }
    }
    for (index, output) in inserts.into_iter().rev() {
        messages.insert(index, output);
    }
}

pub(crate) fn current_user_index(
    messages: &[CanonicalMessage],
    current_user_message_id: MessageId,
) -> Option<usize> {
    messages
        .iter()
        .position(|message| message.id == current_user_message_id)
}

pub(crate) fn persistent_messages_from_model_messages(
    messages: &[CanonicalMessage],
) -> Vec<CanonicalMessage> {
    persistent_messages_from_model_messages_excluding_phases(messages, &[])
}

fn persistent_messages_from_model_messages_excluding_phases(
    messages: &[CanonicalMessage],
    excluded_phases: &[&str],
) -> Vec<CanonicalMessage> {
    messages
        .iter()
        .filter(|message| !is_ephemeral_context_message(message))
        .filter(|message| {
            !message
                .metadata
                .get("workflow_phase")
                .and_then(Value::as_str)
                .is_some_and(|phase| excluded_phases.contains(&phase))
        })
        .cloned()
        .collect()
}

pub(crate) fn replace_after_compaction(
    compacted_messages: &[CanonicalMessage],
    model_messages: &mut Vec<CanonicalMessage>,
    persistent_messages: &mut Vec<CanonicalMessage>,
    current_user_message_id: MessageId,
    excluded_persistent_phases: &[&str],
) -> Result<usize, ProcessModuleError> {
    let current_user_position = current_user_index(compacted_messages, current_user_message_id)
        .ok_or_else(|| {
            ProcessModuleError::new(
                "compaction changed history but dropped the current user message",
            )
        })?;
    *model_messages = compacted_messages.to_vec();
    *persistent_messages = persistent_messages_from_model_messages_excluding_phases(
        model_messages,
        excluded_persistent_phases,
    );
    if current_user_index(persistent_messages, current_user_message_id).is_none() {
        return Err(ProcessModuleError::new(
            "compaction changed persistent history but dropped the current user message",
        ));
    }
    Ok(current_user_position)
}

fn is_ephemeral_context_message(message: &CanonicalMessage) -> bool {
    !message.parts.is_empty()
        && message
            .parts
            .iter()
            .all(|part| part.scope == PartScope::Request)
}
