use serde_json::{Map, Value};

use crate::{
    domain::{ContextChunk, ContextRenderMode},
    model_standard::InstructionBlock,
};

/// Move the rendered context into its final block rather than copying it through json!.
pub(super) fn context_text_block(kind: &str, chunk: &ContextChunk) -> Value {
    let mut block = Map::with_capacity(2);
    block.insert("type".to_owned(), Value::String(kind.to_owned()));
    block.insert("text".to_owned(), Value::String(context_text(chunk)));
    Value::Object(block)
}

/// Shared model-facing formatting; provider-specific JSON shaping stays in adapters.
pub(super) fn context_text(chunk: &ContextChunk) -> String {
    match chunk.render_mode {
        ContextRenderMode::Verbatim => chunk.content.clone(),
        ContextRenderMode::SourceAnnotated => {
            let path = chunk
                .path
                .as_ref()
                .map(|path| format!(" ({})", path.display()))
                .unwrap_or_default();
            let fields = [
                "Context from ",
                chunk.source.as_str(),
                path.as_str(),
                ":\n",
                chunk.content.as_str(),
            ];
            let bytes = fields
                .iter()
                .try_fold(0usize, |sum, field| sum.checked_add(field.len()))
                .expect("context text capacity overflow");
            let mut text = String::with_capacity(bytes);
            for field in fields {
                text.push_str(field);
            }
            text
        }
    }
}

/// Canonical instruction prefix only; legacy system messages and wire shaping
/// remain provider-specific. Preserve equal-priority order and nonblank bytes.
pub(super) fn instruction_text(instructions: &[InstructionBlock]) -> String {
    let mut instructions: Vec<_> = instructions.iter().collect();
    instructions.sort_by_key(|instruction| std::cmp::Reverse(instruction.priority));
    instructions
        .into_iter()
        .map(|instruction| instruction.text.as_str())
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}
