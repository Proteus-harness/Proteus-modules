//! Preserve the SDK's Value wire shape while moving owned display text into it.
use anyhow::{Context, Result, ensure};
use proteus_module_sdk::contracts::{
    contracts::SessionStorageResponse, model_standard::ContentPart,
};
use serde_json::Value;
use std::mem;

pub(super) fn into_value(mut response: SessionStorageResponse) -> Result<Value> {
    let SessionStorageResponse::DisplaySnapshot {
        projection: Some(projection),
    } = &mut response
    else {
        return Ok(serde_json::to_value(response)?);
    };
    let messages = mem::take(&mut projection.state.conversation.messages);
    let mut result = serde_json::to_value(response)?;
    let serialized_messages = result
        .get_mut("projection")
        .and_then(|value| value.get_mut("state"))
        .and_then(|value| value.get_mut("conversation"))
        .and_then(|value| value.get_mut("messages"))
        .and_then(Value::as_array_mut)
        .context("serialized display snapshot omitted its messages array")?;
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

    Ok(result)
}
