import { Effect } from "effect";
import { make } from "@opencode/core/session/system-prompt";
import { projectInstructions } from "./instructions.mjs";
import { Plugins } from "@opencode/core/plugin/optimize";

export async function nativeSystemPrompt(model, toolNames, agentSystem) {
    const callbacks = [];
    const info = { providerID: model.provider, id: model.model };
    const event = {
        agent: "build", model: info,
        tools: Object.fromEntries(toolNames.map(name => [name, {}])),
        system: [{ text: agentSystem ?? make(toolNames) }],
    };
    const ctx = {
        model: { list: () => Effect.succeed({ data: [] }) },
        agent: { get: () => Effect.succeed({ data: { system: agentSystem } }) },
        session: { hook: (name, callback) => Effect.sync(() => { if (name === "context") callbacks.push(callback); }) },
    };
    return Effect.runPromise(Effect.scoped(Effect.gen(function* () {
        for (const plugin of Plugins) yield* plugin.effect(ctx);
        for (const callback of callbacks) yield* callback(event);
        return event.system.map(part => part.text).join("\n\n");
    })));
}

export function createPromptExport(binding) {
    if (binding.slot !== "hook" || binding.module_id !== "opencode_prompt" || binding.contract_version !== "v6" || binding.composition !== "ordered_many") {
        throw new Error("Expected hook/opencode_prompt v6 ordered_many");
    }
    if (Object.keys(binding.module_config).some(key => key !== "agent_system")) throw new Error("Unknown OpenCode prompt config field");
    if (binding.module_config.agent_system !== undefined && typeof binding.module_config.agent_system !== "string") throw new Error("agent_system must be a string");
    return {
        manifest: { slot: "hook", module_id: "opencode_prompt", contract_version: "v6", composition: "ordered_many", module_features: [], config_schema: { fields: [{ key: "agent_system", title: "Agent system", description: "Explicit native agent.system override", value: { type: "string", multiline: true, secret: false }, default: null, required: false, advanced: true, unit: null }] } },
        async invoke(method, input, { signal }) {
            if (method !== "hook.invoke") throw new Error(`Unsupported prompt hook method ${method}`);
            if (input.event.event !== "before_model") return { result: { action: "continue" } };
            const request = input.event.request;
            signal.throwIfAborted();
            const text = await nativeSystemPrompt(request.model, request.tools.map(tool => tool.name), binding.module_config.agent_system);
            signal.throwIfAborted();
            return { result: { action: "model_context", messages: projectInstructions(request.messages, request.model),
                instructions: [...request.instructions, { kind: "System", text, priority: 100 }] } };
        },
    };
}
