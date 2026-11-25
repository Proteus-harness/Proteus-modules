import { createHash } from "node:crypto";
import { toLLMMessages } from "@opencode/core/session/runner/to-llm-message";

const KEY = "opencode/synthetic";
const PROJECTED = "opencode/instruction_projection";

function collectInstructionEvents(message, events) {
    if (message.metadata?.[PROJECTED]) {
        events.push(message.metadata[PROJECTED]);
        return;
    }
    for (const event of message.metadata?.[KEY] ?? []) events.push(event);
    message.parts.forEach(part => {
        const records = part.payload.ToolResult?.result.metadata?.[KEY];
        // Match the one-level flatMap of ToolResult records, including sparse arrays.
        if (Array.isArray(records)) {
            for (let i = 0, length = records.length; i < length; i++)
                if (i in records) events.push(records[i]);
        } else if (records != null) events.push(records);
    });
}

// Canonical metadata is the only persisted instruction state. Native
// SessionInstructions uses these same synthetic records for path deduplication.
export function instructionEvents(messages = []) {
    const events = [];
    messages.forEach(message => collectInstructionEvents(message, events));
    return events;
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
    let insertions;
    const events = [];
    for (let i = 0; i < source.length; i++) {
        const message = source[i];
        events.length = 0;
        collectInstructionEvents(message, events);
        for (const [index, event] of events.entries()) {
            const paths = event.metadata?.instruction?.paths ?? [];
            if (paths.every(path => seen.has(path))) continue;
            for (const path of paths) seen.add(path);
            const id = identity(`${message.id}:${index}:opencode-instructions`);
            const [native] = toLLMMessages([{ ...event, id }], { providerID: model.provider, id: model.model });
            (insertions ??= []).push([i + 1, {
                id, role: "User", phase: null, name: null, tool_call_id: null,
                parts: native.content.map((part, partIndex) => {
                    if (part.type !== "text") throw new Error("Unexpected native synthetic instruction content");
                    return { part_id: identity(`${id}:${partIndex}`), provenance: "context_builder", scope: "request", payload: { Text: { text: part.text } } };
                }),
                metadata: { [PROJECTED]: event },
            }]);
        }
    }
    if (!insertions) return source;

    // Expand the fresh filtered array from the tail so each range moves at most once.
    let end = source.length;
    let write = end + insertions.length;
    source.length = write;
    for (let i = insertions.length - 1; i >= 0; i--) {
        const [after, projected] = insertions[i];
        if (after < end) {
            write -= end - after;
            source.copyWithin(write, after, end);
        }
        source[--write] = projected;
        end = after;
    }
    return source;
}
