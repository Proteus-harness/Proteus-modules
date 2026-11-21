use proteus_contracts::{
    domain::ToolResult,
    model_standard::{CanonicalMessage, ContentPart},
};

pub(crate) fn message_text(message: &CanonicalMessage) -> String {
    let text = message
        .parts
        .iter()
        .filter_map(|part| match &part.payload {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        "<empty model response>".to_owned()
    } else {
        text
    }
}

pub(crate) fn output_text(message: &CanonicalMessage, messages: &[CanonicalMessage]) -> String {
    let text = message_text(message);
    if text != "<empty model response>" {
        return text;
    }

    let Some(result) = latest_tool_result(messages) else {
        return text;
    };
    let output = result.output.trim();
    let error = result.error.as_deref().unwrap_or_default().trim();
    if output.is_empty() && error.is_empty() {
        return text;
    }

    // The separator counts toward the same scalar limit as the tool text.
    // Borrow both fields so an arbitrarily large result is never joined first.
    let separator = if output.is_empty() || error.is_empty() {
        ""
    } else {
        "\n"
    };
    let mut fallback = String::from(
        "Model returned an empty final response after the last tool call.\n\nLast tool result:\n",
    );
    let mut remaining = 2_000;
    for field in [output, separator, error] {
        if field.len() > remaining
            && let Some((end, _)) = field.char_indices().nth(remaining)
        {
            fallback.push_str(&field[..end]);
            fallback.push_str("\n[truncated]");
            break;
        }
        fallback.push_str(field);
        remaining -= field.chars().count();
    }
    fallback
}

fn latest_tool_result(messages: &[CanonicalMessage]) -> Option<&ToolResult> {
    messages.iter().rev().find_map(|message| {
        message
            .parts
            .iter()
            .rev()
            .find_map(|part| match &part.payload {
                ContentPart::ToolResult { result } => Some(result),
                _ => None,
            })
    })
}
