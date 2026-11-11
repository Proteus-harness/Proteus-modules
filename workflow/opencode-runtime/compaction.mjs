import { randomUUID } from "node:crypto";
import { Effect, Layer, Stream } from "effect";
import { AIError, InvalidRequestError, LanguageModel, LLMClient, LLMEvent, LLMRequest } from "@opencode/ai";
import { Service as Compaction, layer as compactionLayer, estimateContext, estimatePrompt } from "@opencode/core/session/compaction";
import { Service as Bus } from "@opencode/core/bus";
import { Service as Database } from "@opencode/core/database/database";
import { Service as ModelRequest } from "@opencode/core/session/model-request";
import { Service as Config } from "@opencode/core/config";
import { Plugin as CompactionConfig } from "@opencode/core/config/plugin/compaction";
import { toLLMMessages } from "@opencode/core/session/runner/to-llm-message";
import { canonical } from "../../components/module-runtime-js/canonical.mjs";
import { nativeSystemPrompt } from "./prompt.mjs";

const field = (key, value) => ({ key, title: key, description: `OpenCode 2.0.22 ${key}`, value,
    default: null, required: false, advanced: true, unit: null });
export const configSchema = { fields: [
    field("auto", { type: "boolean" }),
    field("buffer", { type: "integer", minimum: 0, maximum: null }),
    field("keep", { type: "integer", minimum: 0, maximum: null }),
    field("models", { type: "json" }),
] };

function validateConfig(config) {
    if (!config || typeof config !== "object" || Array.isArray(config)) throw new Error("Invalid OpenCode compaction config");
    for (const key of Object.keys(config)) if (!["auto", "buffer", "keep", "models"].includes(key)) throw new Error(`Unknown OpenCode compaction config ${key}`);
    if (config.auto !== undefined && typeof config.auto !== "boolean") throw new Error("auto must be boolean");
    for (const key of ["buffer", "keep"]) if (config[key] !== undefined && (!Number.isSafeInteger(config[key]) || config[key] < 0)) throw new Error(`${key} must be a nonnegative integer`);
    if (config.models !== undefined) {
        if (!config.models || typeof config.models !== "object" || Array.isArray(config.models)) throw new Error("models must be a map");
        for (const value of Object.values(config.models)) {
            if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("model limits must be an object");
            for (const key of Object.keys(value)) if (!["context", "input", "output", "native_compaction"].includes(key)) throw new Error(`Unknown model compaction field ${key}`);
            for (const key of ["context", "input", "output"]) if (value[key] !== undefined && (!Number.isSafeInteger(value[key]) || value[key] < 0)) throw new Error(`model ${key} must be a nonnegative integer`);
            if (value.native_compaction !== undefined && typeof value.native_compaction !== "boolean") throw new Error("native_compaction must be boolean");
        }
    }
    return config;
}

function textParts(message) {
    return message.parts.flatMap(({ payload }) => {
        if (payload.Text) return [payload.Text.text];
        if (payload.Context) return [payload.Context.chunk.content ?? ""];
        if (payload.FileRef) return [payload.FileRef.content ?? `[File: ${payload.FileRef.path}]`];
        if (payload.Reasoning || payload.ReasoningSummary) return [(payload.Reasoning ?? payload.ReasoningSummary).text];
        if (payload.Opaque || payload.ToolCall || payload.ToolResult || payload.Image) return [];
        throw new Error(`Unsupported OpenCode compaction part ${Object.keys(payload).join(",")}`);
    }).join("\n");
}

/** Canonical -> native durable message shapes. No private database or HTTP. */
export async function nativeContext(input, messages, config = {}) {
    const request = input.request;
    const providerID = request.model.provider;
    const modelID = request.model.model;
    const limits = config.models?.[`${providerID}/${modelID}`] ?? {};
    const ref = { providerID, id: modelID, variant: request.reasoning?.effort };
    const model = LanguageModel.make({ id: modelID, provider: providerID,
        route: { id: modelID, providerMetadataKey: providerID } });
    const resolved = { ref, model, capabilities: { input: ["text", "image", "pdf"] },
        limit: { context: limits.context ?? input.window_tokens ?? 0,
            input: limits.input ?? request.limits?.max_input_tokens ?? 0,
            output: limits.output ?? request.limits?.max_output_tokens ?? 0 },
        cost: { input: 0, output: 0, cache: { read: 0, write: 0 } },
        ...((limits.native_compaction ?? ((providerID === "openai" || providerID === "openai-codex") && /^gpt-/.test(modelID))) ? { compaction: { type: "native" } } : {}),
    };
    const results = new Map(messages.flatMap(message => message.parts.flatMap(({ payload }) =>
        payload.ToolResult ? [[payload.ToolResult.result.call_id, payload.ToolResult.result]] : [])));
    const native = [];
    for (const message of messages) {
        const previous = message.metadata?.opencode_compaction;
        if (previous) { native.push({ ...previous, id: message.id, type: "compaction", status: "completed" }); continue; }
        const common = { id: message.id, metadata: message.metadata ?? undefined };
        if (message.role === "User") {
            const files = message.parts.flatMap(({ payload }) => {
                if (!payload.Image) return [];
                const image = payload.Image.image;
                if (typeof image.data !== "string") throw new Error("OpenCode compaction cannot read a non-inline image through a model callback");
                return [{ mime: image.mime_type, data: image.data, source: { type: "inline" } }];
            });
            native.push({ ...common, type: "user", text: textParts(message), files });
        } else if (message.role === "System" || message.role === "Developer") {
            native.push({ ...common, type: "system", text: textParts(message) });
        } else if (message.role === "Assistant") {
            const content = message.parts.flatMap(({ payload }) => {
                if (payload.Text) return [{ type: "text", text: payload.Text.text }];
                if (payload.Reasoning || payload.ReasoningSummary) return [{ type: "reasoning", text: (payload.Reasoning ?? payload.ReasoningSummary).text }];
                if (payload.Opaque) return [];
                if (!payload.ToolCall) throw new Error("Unsupported assistant part in OpenCode compaction");
                const call = payload.ToolCall.call, result = results.get(call.id);
                const content = result ? [{ type: "text", text: result.output }, ...(result.content ?? []).map(item => {
                    if (item.type !== "text") throw new Error("OpenCode tool-result media needs an explicit canonical media bridge");
                    return { type: "text", text: item.text };
                })] : [];
                return [{ type: "tool", id: call.id, name: call.name,
                    state: result ? result.ok ? { status: "completed", input: call.args, content } :
                        { status: "error", input: call.args, content, error: { message: result.error ?? result.output } } :
                        { status: "pending", input: call.args } }];
            });
            native.push({ ...common, type: "assistant", model: message.metadata?.opencode_model ?? ref, content,
                ...(message.metadata?.opencode_tokens ? { tokens: message.metadata.opencode_tokens } : {}) });
        } else if (message.role !== "Tool") throw new Error(`Unsupported canonical role ${message.role}`);
    }
    const tools = { definitions: (request.tools ?? []).map(tool => ({ type: "tool", name: tool.name,
        description: tool.description, inputSchema: tool.input_schema })) };
    const system = await nativeSystemPrompt(request.model, tools.definitions.map(tool => tool.name));
    const context = { session: { id: "proteus-compaction" }, agent: { id: "build", info: { system } },
        model: resolved, initial: (request.instructions ?? []).map(part => part.text).join("\n\n"),
        messages: native, tools };
    // Core supplies a complete active-prompt estimate independently of native
    // durable assistant usage. Represent that measurement as an empty native
    // anchor: the real estimator and due/previous-checkpoint rules still run.
    if (input.token_estimate != null && native.at(-1)?.type !== "compaction") {
        native.push({ id: "proteus-prompt-measurement", type: "assistant", model: ref, content: [],
            tokens: { input: input.token_estimate, output: 0, reasoning: 0, cache: { read: 0, write: 0 } } });
    }
    return context;
}

function canonicalPrompt(message) {
    const parts = message.content.map(part => {
        if (part.type === "text" || part.type === "reasoning") return { Text: { text: part.text } };
        if (part.type === "tool-call") return { ToolCall: { call: { id: part.id, name: part.name,
            args: part.input, surface: "function", raw_arguments: null } } };
        if (part.type === "tool-result") return { ToolResult: { result: { call_id: part.id,
            ok: part.result.type !== "error", output: typeof part.result.value === "string" ? part.result.value : JSON.stringify(part.result.value),
            content: [], error: part.result.type === "error" ? JSON.stringify(part.result.value) : null, metadata: null } } };
        if (part.type === "media" && part.media.data !== undefined) return { Image: { image: {
            data: part.media.data, mime_type: part.media.mediaType } } };
        throw new Error(`Unsupported upstream compaction prompt content ${part.type}`);
    });
    const canonicalMessage = canonical(({ user: "User", assistant: "Assistant", tool: "Tool", system: "System" })[message.role], parts[0] ?? { Text: { text: "" } }, "runtime");
    canonicalMessage.parts = parts.map(payload => ({ part_id: randomUUID(), provenance: "runtime", scope: "conversation", payload }));
    return canonicalMessage;
}

/** Run the pinned complete service, redirecting every external effect to Core. */
export async function runNativeCompaction(context, template, config, { hostCall, signal }, reason = "auto") {
    const events = [];
    let hostFailure;
    const llm = {
        stream: request => Stream.unwrap(Effect.tryPromise({ try: async () => {
            signal.throwIfAborted();
            const response = await hostCall("host.model.complete", { request: {
                ...template, instructions: request.system.map(part => ({ kind: "System", text: part.text, priority: 100 })),
                messages: request.messages.map(canonicalPrompt), tools: [], tool_choice: "None", response_format: "Text",
                metadata: { ...(template.metadata ?? {}), suppress_stream_deltas: true },
            } });
            signal.throwIfAborted();
            if (response.tool_calls?.length) throw new Error("OpenCode compaction model attempted tool calls");
            const text = response.messages.filter(message => message.role === "Assistant").map(textParts).join("");
            const finish = { Stop: "stop", Length: "length", ContentFilter: "content-filter" }[response.finish_reason];
            if (!finish) throw new Error(`OpenCode compaction unsupported model finish ${response.finish_reason}`);
            return Stream.fromIterable([LLMEvent.textDelta({ id: "summary", text }), LLMEvent.finish({ reason: { normalized: finish } })]);
        }, catch: error => {
            hostFailure = error;
            return error?.data?.kind === "context_window_exceeded" ?
                new AIError({ reason: new InvalidRequestError({ message: error.message, classification: "context-overflow" }) }) :
                { error: { type: "compaction.model-failed", message: error.message ?? String(error) } };
        } })),
        compact: () => Effect.fail({ error: { type: "provider.unsupported-operation",
            message: "Native provider compaction is unavailable through hook/v6 host.model.complete" } }),
    };
    const services = Layer.mergeAll(
        Layer.succeed(Bus, { publish: (event, data) => Effect.sync(() => events.push({ type: event.type, ...data })) }),
        Layer.succeed(Database, { db: {} }), Layer.succeed(LLMClient.Service, llm),
        Layer.succeed(ModelRequest, { compaction: input => {
            if (context.model.compaction?.type === "native") return Effect.fail({ error: {
                type: "provider.unsupported-operation",
                message: "Native provider remote compaction is unsupported by hook/v6 host.model.complete; no local summary was substituted",
            } });
            events.push({ type: "proteus.compaction.prepared", budget: input.inputTokens.measured });
            return Effect.succeed({ event: {}, options: {}, retry: () => Effect.void,
                request: new LLMRequest({ model: context.model.model, system: input.system, messages: input.messages, tools: [] }) });
        } }),
    );
    const configService = { entries: () => Effect.succeed([{ type: "document", info: { compaction: {
        ...(config.auto === undefined ? {} : { auto: config.auto }), ...(config.buffer === undefined ? {} : { buffer: config.buffer }),
        ...(config.keep === undefined ? {} : { keep: { tokens: config.keep } }),
    } } }]) };
    const program = Effect.scoped(Effect.gen(function* () {
        const service = yield* Compaction;
        yield* CompactionConfig.effect({ event: { subscribe: () => Stream.empty } }).pipe(Effect.provideService(Config, configService));
        return yield* service.compact({ reason, context, ...(reason === "manual" ? { inputID: "compact" } : {}) });
    })).pipe(Effect.provide(compactionLayer.pipe(Layer.provide(services))));
    let outcome;
    try { outcome = await Effect.runPromise(program, { signal }); }
    catch (error) { signal.throwIfAborted(); throw error; }
    signal.throwIfAborted();
    if (outcome.status === "failed") {
        const error = new Error(`OpenCode compaction ${outcome.error.type}: ${outcome.error.message}`);
        if (hostFailure?.data) error.data = hostFailure.data;
        throw error;
    }
    return { outcome, events };
}

export function createCompactionExport(binding) {
    if (binding.slot !== "hook" || binding.module_id !== "opencode_compact" || binding.contract_version !== "v6" || binding.composition !== "ordered_many") throw new Error("Expected hook/opencode_compact v6 ordered_many");
    const config = validateConfig(binding.module_config);
    return { manifest: { slot: "hook", module_id: "opencode_compact", contract_version: "v6", composition: "ordered_many", module_features: [], config_schema: configSchema },
        async invoke(method, params, invocation) {
            if (method !== "hook.invoke") throw new Error(`Unsupported compaction hook method ${method}`);
            const event = params.event;
            if (event.event !== "before_context" || !event.input.request) return { result: { action: "continue" } };
            invocation.signal.throwIfAborted();
            const input = { ...event.input, messages: event.output.messages,
                token_estimate: event.output.changed ? event.output.token_estimate : event.input.token_estimate };
            const context = await nativeContext(input, input.messages, config);
            const original = estimateContext(context);
            const reason = input.reason === "manual" ? "manual" : ["overflow", "context_window_exceeded"].includes(input.reason) ? "overflow" : "auto";
            const { outcome, events } = await runNativeCompaction(context, input.request, config, invocation, reason);
            if (outcome.status === "skipped") return { result: { action: "continue" } };
            const completed = events.findLast(event => event.type.includes("compaction") && typeof event.text === "string" && "recent" in event);
            if (!completed || !completed.text.trim()) throw new Error("Native provider compaction requires a provider-context operation; no local summary was substituted");
            const native = { type: "compaction", status: "completed", summary: completed.text, recent: completed.recent };
            const serialized = toLLMMessages([{ ...native, id: "checkpoint" }], context.model.ref)[0].content[0].text;
            const checkpoint = canonical("User", { Text: { text: serialized } }, "compactor");
            const instructions = input.messages.flatMap(message => [
                ...(message.metadata?.["opencode/synthetic"] ?? []),
                ...message.parts.flatMap(part => part.payload.ToolResult?.result.metadata?.["opencode/synthetic"] ?? []),
            ]);
            checkpoint.metadata = { opencode_compaction: native, ...(instructions.length ? { "opencode/synthetic": instructions } : {}) };
            const currentUser = input.messages.findLast(message => message.role === "User");
            return { result: { action: "context_preparation", output: { ...event.output, messages: [checkpoint], changed: true,
                summary: completed.text, user_message_replacements: currentUser ? [{ source_message_id: currentUser.id, replacement_message_id: checkpoint.id }] : [],
                original_token_estimate: original, token_estimate: null,
                trigger_tokens: reason === "auto" ? events.find(event => event.type === "proteus.compaction.prepared")?.budget ?? null : null,
                summary_source: "model", skipped_reason: null,
                metadata: { upstream: "@opencode/core@2.0.22/session/compaction", native_service: true,
                    prompt_estimate: estimatePrompt(context), provider_compaction: false } } } };
        } };
}
