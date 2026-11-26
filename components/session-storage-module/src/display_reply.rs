//! Move owned storage payloads into the SDK's existing Value wire shape.
use anyhow::{Context, Result, ensure};
use proteus_module_sdk::contracts::{
    contracts::SessionStorageResponse,
    domain::ToolResult,
    model_standard::{CanonicalMessage, ContentPart},
    storage::journal::JournalEntry,
};
use serde_json::Value;
use std::mem;

pub(super) fn into_value(mut response: SessionStorageResponse) -> Result<Value> {
    let messages = match &mut response {
        SessionStorageResponse::Metadata {
            document: Some(document),
        } => {
            // ReadCache already hydrated this Value and checked its source.
            // Serialize the small envelope, not a second copy of the tree.
            let payload = mem::take(&mut document.value);
            let mut result = serde_json::to_value(response)?;
            let slot = result
                .get_mut("document")
                .and_then(|value| value.get_mut("value"))
                .context("serialized cache document omitted its value slot")?;
            ensure!(slot.is_null(), "serialized cache value slot changed");
            *slot = payload;
            return Ok(result);
        }
        SessionStorageResponse::Conversation { conversation } => {
            let messages = mem::take(&mut conversation.messages);
            let mut result = serde_json::to_value(response)?;
            let output = result
                .get_mut("conversation")
                .and_then(|value| value.get_mut("messages"))
                .and_then(Value::as_array_mut)
                .context("serialized storage conversation omitted its messages array")?;
            move_messages(output, messages)?;
            return Ok(result);
        }
        SessionStorageResponse::DisplaySnapshot {
            projection: Some(projection),
        } => mem::take(&mut projection.state.conversation.messages),
        SessionStorageResponse::Records { page } => {
            let records = mem::take(&mut page.records);
            let mut result = serde_json::to_value(response)?;
            let output = result
                .get_mut("page")
                .and_then(|value| value.get_mut("records"))
                .and_then(Value::as_array_mut)
                .context("serialized display page omitted its records array")?;
            output.reserve_exact(records.len());
            for mut record in records {
                if let JournalEntry::HistoryMutated(mutation) = &mut record.entry {
                    let messages = mem::take(&mut mutation.messages);
                    let mut value = serde_json::to_value(record)?;
                    let output_messages = value
                        .get_mut("entry")
                        .and_then(|value| value.get_mut("payload"))
                        .and_then(|value| value.get_mut("messages"))
                        .and_then(Value::as_array_mut)
                        .context(
                            "serialized display history mutation omitted its messages array",
                        )?;
                    move_messages(output_messages, messages)?;
                    output.push(value);
                } else if let JournalEntry::ToolResultRecorded(recorded)
                | JournalEntry::ToolEffectRecorded(recorded) = &mut record.entry
                {
                    let strings = ToolResultStrings::take(&mut recorded.result);
                    let mut value = serde_json::to_value(record)?;
                    strings.restore(&mut value, &["entry", "payload", "result"])?;
                    output.push(value);
                } else {
                    output.push(serde_json::to_value(record)?);
                }
            }
            return Ok(result);
        }
        _ => return Ok(serde_json::to_value(response)?),
    };
    let mut result = serde_json::to_value(response)?;
    let serialized_messages = result
        .get_mut("projection")
        .and_then(|value| value.get_mut("state"))
        .and_then(|value| value.get_mut("conversation"))
        .and_then(|value| value.get_mut("messages"))
        .and_then(Value::as_array_mut)
        .context("serialized display snapshot omitted its messages array")?;
    move_messages(serialized_messages, messages)?;
    Ok(result)
}

const MIN_COMPACT_LEAF_BYTES: usize = 4 * 1024;

fn take_compact_string(text: &mut String) -> Option<String> {
    (text.len() >= MIN_COMPACT_LEAF_BYTES && text.capacity() == text.len()).then(|| mem::take(text))
}

fn take_reasoning_strings(
    payload: &mut ContentPart,
) -> Option<(&'static str, [Option<String>; 2])> {
    let (variant, strings) = match payload {
        ContentPart::ReasoningSummary { text } => {
            ("ReasoningSummary", [take_compact_string(text), None])
        }
        ContentPart::Reasoning { text, signature } => (
            "Reasoning",
            // Keep the Option discriminant, including Some(empty).
            [
                take_compact_string(text),
                signature.as_mut().and_then(take_compact_string),
            ],
        ),
        _ => return None,
    };
    strings
        .iter()
        .any(Option::is_some)
        .then_some((variant, strings))
}

// Opaque data can retain spare capacity after decode. Only lift a known,
// large string that is already as compact as the serializer's copy. Keep
// rebuilding all surrounding objects and arrays in their original order.
fn take_compact_opaque_leaf(format: &str, data: &mut Value) -> Option<(&'static str, String)> {
    let item_type = data.get("type").and_then(Value::as_str);
    let field = match (format, item_type) {
        ("openai.responses.compaction.v1", Some("compaction")) => "encrypted_content",
        ("anthropic.messages.redacted_thinking", Some("redacted_thinking")) => "data",
        _ => return None,
    };
    let Value::String(text) = data.get_mut(field)? else {
        return None;
    };
    take_compact_string(text).map(|text| (field, text))
}

struct ToolResultStrings([Option<String>; 2]);

impl ToolResultStrings {
    fn take(result: &mut ToolResult) -> Self {
        Self([
            take_compact_string(&mut result.output),
            result.error.as_mut().and_then(take_compact_string),
        ])
    }

    fn restore(self, envelope: &mut Value, path: &[&str]) -> Result<()> {
        if self.0.iter().all(Option::is_none) {
            return Ok(());
        }
        let mut result = envelope;
        for segment in path {
            result = result
                .get_mut(*segment)
                .context("serialized display tool result omitted its payload")?;
        }
        for (field, text) in ["output", "error"].into_iter().zip(self.0) {
            if let Some(text) = text {
                let slot = result
                    .get_mut(field)
                    .context("serialized display tool result omitted its string slot")?;
                ensure!(
                    slot.as_str() == Some(""),
                    "serialized display tool result string slot changed"
                );
                *slot = Value::String(text);
            }
        }
        Ok(())
    }
}

fn move_messages(
    serialized_messages: &mut Vec<Value>,
    messages: Vec<CanonicalMessage>,
) -> Result<()> {
    serialized_messages.reserve_exact(messages.len());

    // Use the pinned DTO serializer to emit every key and its position. Only
    // replace existing content slots; insertion could change preserve_order.
    for mut message in messages {
        let parts = mem::take(&mut message.parts);
        let mut value = serde_json::to_value(message)?;
        let serialized_parts = value
            .get_mut("parts")
            .and_then(Value::as_array_mut)
            .context("serialized display message omitted its parts array")?;
        serialized_parts.reserve_exact(parts.len());
        for mut part in parts {
            let tool_result = match &mut part.payload {
                ContentPart::ToolResult { result } => Some(ToolResultStrings::take(result)),
                _ => None,
            };
            let owned_text = match &mut part.payload {
                ContentPart::Text { text } => Some(mem::take(text)),
                _ => None,
            };
            let opaque_leaf = match &mut part.payload {
                ContentPart::Opaque { format, data } => take_compact_opaque_leaf(format, data),
                _ => None,
            };
            let reasoning = take_reasoning_strings(&mut part.payload);
            let mut value = serde_json::to_value(part)?;
            if let Some((variant, strings)) = reasoning {
                let payload = value
                    .get_mut("payload")
                    .and_then(|value| value.get_mut(variant))
                    .context("serialized display reasoning omitted its payload")?;
                for (field, text) in ["text", "signature"].into_iter().zip(strings) {
                    if let Some(text) = text {
                        let slot = payload
                            .get_mut(field)
                            .context("serialized display reasoning omitted its string slot")?;
                        ensure!(
                            slot.as_str() == Some(""),
                            "serialized display reasoning string slot changed"
                        );
                        *slot = Value::String(text);
                    }
                }
            }
            if let Some(text) = owned_text {
                let slot = value
                    .get_mut("payload")
                    .and_then(|value| value.get_mut("Text"))
                    .and_then(|value| value.get_mut("text"))
                    .context("serialized display text omitted its string slot")?;
                ensure!(
                    slot.as_str() == Some(""),
                    "serialized display text slot changed"
                );
                *slot = Value::String(text);
            }
            if let Some((field, text)) = opaque_leaf {
                let slot = value
                    .get_mut("payload")
                    .and_then(|value| value.get_mut("Opaque"))
                    .and_then(|value| value.get_mut("data"))
                    .and_then(|value| value.get_mut(field))
                    .context("serialized opaque display part omitted its string slot")?;
                ensure!(
                    slot.as_str() == Some(""),
                    "serialized opaque display string slot changed"
                );
                *slot = Value::String(text);
            }
            if let Some(strings) = tool_result {
                strings.restore(&mut value, &["payload", "ToolResult", "result"])?;
            }
            serialized_parts.push(value);
        }
        serialized_messages.push(value);
    }

    Ok(())
}
