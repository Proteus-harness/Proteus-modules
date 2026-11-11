import { validateToolArguments } from "./upstream.mjs";

// The Core checkpoint binds the executable call before upstream runs it.
// Use the same native preparation/coercion while keeping the sampled call intact.
export function executionCall(call, tool) {
    if (!tool) return call;
    try {
        const input = structuredClone(call.args);
        const prepared = tool.prepareArguments ? tool.prepareArguments(input) : input;
        const args = validateToolArguments(tool, { name: call.name, arguments: prepared });
        return { ...call, args };
    } catch {
        // Upstream runAgentLoop remains responsible for its validation error and
        // tool-result message. It will not execute this invalid binding.
        return call;
    }
}
