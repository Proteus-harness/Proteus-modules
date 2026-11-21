use crate::{
    domain::{ContextChunk, ContextRenderMode},
    model_standard::InstructionBlock,
};

/// Shared model-facing formatting; provider-specific JSON shaping stays in adapters.
pub(super) fn context_text(chunk: &ContextChunk) -> String {
    match chunk.render_mode {
        ContextRenderMode::Verbatim => chunk.content.clone(),
        ContextRenderMode::SourceAnnotated => format!(
            "Context from {}{}:\n{}",
            chunk.source,
            chunk
                .path
                .as_ref()
                .map(|path| format!(" ({})", path.display()))
                .unwrap_or_default(),
            chunk.content
        ),
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
