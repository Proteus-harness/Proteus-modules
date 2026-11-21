// Canonical ContextChunk projection shared by JavaScript model adapters.
// Core validates render_mode; provider metadata never selects the envelope.
export function contextText(chunk) {
    if (chunk.render_mode === "verbatim") return chunk.content;
    const path = chunk.path == null ? "" : ` (${chunk.path})`;
    return `Context from ${chunk.source}${path}:\n${chunk.content}`;
}
