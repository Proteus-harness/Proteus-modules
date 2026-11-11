import { randomUUID } from "node:crypto";

export function canonical(role, payload, provenance, binding) {
    return {
        id: binding?.message_id ?? randomUUID(),
        role,
        phase: null,
        parts: [{ part_id: binding?.part_id ?? randomUUID(), provenance, scope: "conversation", payload }],
        name: null,
        tool_call_id: payload.ToolResult?.result.call_id ?? null,
        metadata: null,
    };
}
