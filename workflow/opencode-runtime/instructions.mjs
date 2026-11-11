import { createHash } from "node:crypto";
import { toLLMMessages } from "@opencode/core/session/runner/to-llm-message";

const KEY = "opencode/synthetic";
const PROJECTED = "opencode/instruction_projection";

// Canonical metadata is the only persisted instruction state. Native
// SessionInstructions uses these same synthetic records for path deduplication.
export function instructionEvents(messages = []) {
    return messages.flatMap(message => {
        if (message.metadata?.[PROJECTED]) return [message.metadata[PROJECTED]];
        return [...(message.metadata?.[KEY] ?? []), ...message.parts.flatMap(part => part.payload.ToolResult?.result.metadata?.[KEY] ?? [])];
    });
}

function identity(value) {
    const hex = createHash("sha256").update(value).digest("hex").slice(0, 32).split("");
    hex[12] = "5";
    hex[16] = "8";
    return `${hex.slice(0, 8).join("")}-${hex.slice(8, 12).join("")}-${hex.slice(12, 16).join("")}-${hex.slice(16, 20).join("")}-${hex.slice(20).join("")}`;
}

export function projectInstructions(messages, model) {
    // Rebuild the derived projection so a repeated BeforeModel is idempotent.
    const source = messages.filter(message => !message.metadata?.[PROJECTED]);
    const seen = new Set();
    const result = [];
    for (const message of source) {
        result.push(message);
        for (const [index, event] of instructionEvents([message]).entries()) {
            const paths = event.metadata?.instruction?.paths ?? [];
            if (paths.every(path => seen.has(path))) continue;
            for (const path of paths) seen.add(path);
            const id = identity(`${message.id}:${index}:opencode-instructions`);
            const [native] = toLLMMessages([{ ...event, id }], { providerID: model.provider, id: model.model });
            result.push({
                id, role: "User", phase: null, name: null, tool_call_id: null,
                parts: native.content.map((part, partIndex) => {
                    if (part.type !== "text") throw new Error("Unexpected native synthetic instruction content");
                    return { part_id: identity(`${id}:${partIndex}`), provenance: "context_builder", scope: "request", payload: { Text: { text: part.text } } };
                }),
                metadata: { [PROJECTED]: event },
            });
        }
    }
    return result;
}
