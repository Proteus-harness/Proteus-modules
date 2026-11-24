//! Move owned storage payloads into the SDK's existing Value wire shape.
use anyhow::{Context, Result, ensure};
use proteus_module_sdk::contracts::{
    contracts::SessionStorageResponse,
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
            let owned_text = match &mut part.payload {
                ContentPart::Text { text } => Some(mem::take(text)),
                _ => None,
            };
            let mut value = serde_json::to_value(part)?;
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
            serialized_parts.push(value);
        }
        serialized_messages.push(value);
    }

    Ok(())
}
