import { Effect } from "effect";
import { Plugin as InputRepair } from "@opencode/core/plugin/tool-input-repair";

// The native hook must run before Core validates a workflow's execution call.
// It has only the selected schemas; there are no tool/session host capabilities.
export async function repairNativeCall(call, specs, signal) {
    const input = call.raw_arguments == null ? call.args : JSON.parse(call.raw_arguments);
    const event = { tool: call.name, input };
    const hooks = [];
    const ctx = { tool: {
        hook: (name, callback) => Effect.sync(() => { if (name === "execute.before") hooks.push(callback); }),
        list: () => Effect.succeed(specs.map(spec => ({ id: spec.name, name: spec.name, description: spec.description, input: spec.input_schema }))),
    } };
    await Effect.runPromise(Effect.scoped(Effect.gen(function* () {
        yield* InputRepair.effect(ctx);
        for (const hook of hooks) yield* hook(event);
    })), { signal });
    return { ...call, args: event.input, raw_arguments: JSON.stringify(event.input) };
}
