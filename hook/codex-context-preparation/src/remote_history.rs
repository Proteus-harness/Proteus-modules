//! Remote V2 retained history, adapted from Codex 67cc3c3
//! compact_remote_v2.rs and compact_remote_v2_images.rs (Apache-2.0).
use proteus_contracts::model_standard::input_estimates::ModelInputEstimate;
use proteus_contracts::{
    domain::{CompactionUserMessageReplacement, ToolContent, new_message_id},
    model_standard::{CanonicalMessage, CanonicalPart, ContentPart, PartProvenance, PartScope},
};

use crate::{
    budget::{estimate_text_tokens, truncate_to_tokens},
    history::{SelectedUserMessages, part_text},
};

// All current OpenAI ImageRef inputs are encoded with detail:auto. Pinned Codex
// estimates those as 7373 bytes, independently of the local file's byte length.
const AUTO_IMAGE_TOKENS: usize = 7373usize.div_ceil(4);

pub(crate) fn estimate_messages(messages: &[CanonicalMessage]) -> u32 {
    estimate_messages_with_estimates(messages, &ModelInputEstimate::default())
}

pub(crate) fn estimate_messages_with_estimates(
    messages: &[CanonicalMessage],
    estimates: &ModelInputEstimate,
) -> u32 {
    let total = messages.iter().fold(0usize, |total, message| {
        let tokens = message.parts.iter().fold(0usize, |sum, part| {
            sum.saturating_add(
                estimates
                    .tokens_for(part)
                    .map_or_else(|| part_tokens(&part.payload), |tokens| tokens as usize),
            )
        });
        let fully_estimated_opaque = !message.parts.is_empty()
            && message.parts.iter().all(|part| {
                matches!(part.payload, ContentPart::Opaque { .. })
                    && estimates.tokens_for(part).is_some()
            });
        total.saturating_add(if fully_estimated_opaque {
            tokens
        } else {
            tokens.max(1)
        })
    });
    u32::try_from(if messages.is_empty() { 1 } else { total }).unwrap_or(u32::MAX)
}

fn message_tokens(message: &CanonicalMessage) -> usize {
    message.parts.iter().fold(0usize, |total, part| {
        total.saturating_add(part_tokens(&part.payload))
    })
}

fn part_tokens(part: &ContentPart) -> usize {
    match part {
        ContentPart::Text { text } | ContentPart::ReasoningSummary { text } => {
            estimate_text_tokens(text)
        }
        ContentPart::Context { chunk } => estimate_text_tokens(&chunk.content),
        ContentPart::FileRef {
            path,
            content: Some(content),
        } => path
            .to_string_lossy()
            .len()
            .saturating_add(2)
            .saturating_add(content.len())
            .div_ceil(4),
        ContentPart::ToolResult { result } => {
            let text = result
                .error
                .as_deref()
                .filter(|text| !text.is_empty())
                .or_else(|| (!result.output.is_empty()).then_some(result.output.as_str()));
            let text_tokens = text.map(estimate_text_tokens).unwrap_or_else(|| {
                // Non-text placeholders are small; avoid copying a large
                // textual or JSON tool payload merely to count it.
                if result.content.is_empty() {
                    return estimate_text_tokens("(no output)");
                }
                result.content.iter().fold(0usize, |total, content| {
                    total.saturating_add(match content {
                        ToolContent::Text { text } => estimate_text_tokens(text),
                        ToolContent::Json { value } => serialized_tokens(value),
                        ToolContent::Image { mime_type, .. } => {
                            estimate_text_tokens(&format!("[image tool content: {mime_type}]"))
                        }
                        ToolContent::Binary { mime_type, .. } => {
                            estimate_text_tokens(&format!("[binary tool content: {mime_type}]"))
                        }
                        _ => 0,
                    })
                })
            });
            let images = result
                .content
                .iter()
                .filter(|content| matches!(content, ToolContent::Image { .. }))
                .count();
            text_tokens.saturating_add(images.saturating_mul(AUTO_IMAGE_TOKENS))
        }
        ContentPart::Image { .. } => AUTO_IMAGE_TOKENS,
        // The format is opaque to this hook. Count its serialized payload
        // conservatively rather than treating a checkpoint as zero context.
        ContentPart::Opaque { data, .. } => serialized_tokens(data),
        _ => part_text(part).map_or(0, |text| estimate_text_tokens(&text)),
    }
}

fn serialized_tokens(value: &serde_json::Value) -> usize {
    struct ByteCount(usize);
    impl std::io::Write for ByteCount {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = ByteCount(0);
    if serde_json::to_writer(&mut count, value).is_err() {
        return usize::MAX;
    }
    count.0.div_ceil(4)
}

pub(crate) fn select_recent_users(
    messages: &[CanonicalMessage],
    max_tokens: usize,
) -> SelectedUserMessages {
    let mut remaining = max_tokens;
    let mut selected = SelectedUserMessages::default();
    for message in messages.iter().rev() {
        if remaining == 0 {
            break;
        }
        let tokens = message_tokens(message).max(1);
        if tokens <= remaining {
            selected.messages.push(message.clone());
            remaining -= tokens;
            continue;
        }
        let has_images = message
            .parts
            .iter()
            .any(|part| matches!(part.payload, ContentPart::Image { .. }));
        let parts = if has_images {
            truncate_image_message(&message.parts, remaining)
        } else {
            truncate_text_message(&message.parts, remaining)
        };
        // The boundary consumes the rest even when an oversized image leaves
        // no content. Do not backfill with older user messages.
        remaining = 0;
        if parts.is_empty() {
            continue;
        }
        let mut replacement = message.clone();
        replacement.id = new_message_id();
        replacement.parts = parts;
        selected
            .replacements
            .push(CompactionUserMessageReplacement {
                source_message_id: message.id,
                replacement_message_id: replacement.id,
            });
        selected.messages.push(replacement);
    }
    selected.messages.reverse();
    selected.replacements.reverse();
    selected
}

fn fresh(payload: ContentPart) -> CanonicalPart {
    CanonicalPart::new(PartProvenance::Compactor, PartScope::Conversation, payload)
}

fn truncate_text_message(parts: &[CanonicalPart], mut remaining: usize) -> Vec<CanonicalPart> {
    let mut retained = Vec::new();
    for part in parts {
        if let Some(payload) = truncate_text_part(&part.payload, &mut remaining) {
            retained.push(fresh(payload));
        }
    }
    retained
}

fn truncate_text_part(payload: &ContentPart, remaining: &mut usize) -> Option<ContentPart> {
    let text = part_text(payload)?;
    if *remaining == 0 || text.is_empty() {
        return None;
    }
    let tokens = estimate_text_tokens(&text);
    if tokens <= *remaining {
        *remaining -= tokens;
        Some(payload.clone())
    } else {
        let text = truncate_to_tokens(&text, *remaining);
        *remaining = 0;
        Some(ContentPart::Text { text })
    }
}

fn truncate_image_message(parts: &[CanonicalPart], mut remaining: usize) -> Vec<CanonicalPart> {
    let mut end = parts.len();
    let mut reversed = Vec::with_capacity(parts.len());
    while end > 0 {
        let last = end - 1;
        let image = match &parts[last].payload {
            ContentPart::Image { .. } => Some(last),
            ContentPart::Text { text }
                if text == "</image>"
                    && last > 0
                    && matches!(parts[last - 1].payload, ContentPart::Image { .. }) =>
            {
                Some(last - 1)
            }
            _ => None,
        };
        if let Some(image) = image {
            let has_open = image > 0
                && matches!(&parts[image - 1].payload,
                ContentPart::Text { text } if text == "<image>" || (text.starts_with("<image name=") && text.ends_with('>')));
            let start = image - usize::from(has_open);
            let tokens = parts[start..end].iter().fold(0usize, |sum, part| {
                sum.saturating_add(part_tokens(&part.payload))
            });
            let fits = tokens <= remaining;
            remaining = if fits { remaining - tokens } else { 0 };
            if fits {
                reversed.extend(
                    parts[start..end]
                        .iter()
                        .rev()
                        .map(|part| fresh(part.payload.clone())),
                );
            }
            end = start;
        } else {
            if let Some(payload) = truncate_text_part(&parts[last].payload, &mut remaining) {
                reversed.push(fresh(payload));
            }
            end = last;
        }
    }
    reversed.reverse();
    reversed
}
