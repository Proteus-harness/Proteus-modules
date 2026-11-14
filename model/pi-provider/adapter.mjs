// Keep Pi's transport, authentication, GPT tool conversion and usage accounting inside model/v13.
import { randomUUID } from "node:crypto";
import { createGoQuotaReader } from "./go-quota.mjs";
import { readFile } from "node:fs/promises";
import { ModelRuntime } from "../../tool/pi-tools/node_modules/@earendil-works/pi-coding-agent/dist/core/model-runtime.js";
import {
    normalizeContext,
    isRetryableAssistantError,
} from "../../tool/pi-tools/node_modules/@earendil-works/pi-ai/dist/index.js";
import { DEFAULT_AGENT_IDENTITY } from "../../tool/pi-tools/node_modules/@earendil-works/pi-agent-core/dist/agent-loop.js";
import { piMessages } from "../../components/pi-runtime-js/messages.mjs";

// Node's module entry point does not run the native CLI's proxy bootstrap.
import undici from "../../tool/pi-tools/node_modules/undici/index.js";
if (process.env.HTTPS_PROXY || process.env.https_proxy || process.env.HTTP_PROXY || process.env.http_proxy) {
    undici.setGlobalDispatcher(new undici.EnvHttpProxyAgent());
}

function descriptor(model) {
    return {
        adapter_id: "pi_codex",
        hosted_tools: [],
        capabilities: {
            supports_tools: true,
            supports_parallel_tool_calls: true,
            supports_freeform_tools: false,
            supports_streaming: true,
            supports_json_schema: false,
            supports_system_role: true,
            supports_developer_role: true,
            supports_cache_hints: true,
            supports_reasoning_config: model.reasoning,
            supports_image_input: model.input.includes("image"),
            supports_file_input: false,
            provider_hosted_tools: [],
            max_input_tokens: model.contextWindow,
            max_output_tokens: model.maxTokens,
        },
    };
}

async function context(request, model) {
    const history = [];
    const names = new Map();
    for (const m of request.messages) {
        for (const p of m.parts)
            if (p.payload.ToolCall) names.set(p.payload.ToolCall.call.id, p.payload.ToolCall.call.name);
        const native = piMessages([m], model)[0];
        if (native.role === "toolResult") native.toolName = names.get(native.toolCallId) ?? native.toolName;
        if (native.role === "user") {
            for (let i = 0; i < m.parts.length; i++) {
                const image = m.parts[i].payload.Image?.image;
                if (image?.path)
                    native.content[i] = {
                        type: "image",
                        data: (await readFile(image.path)).toString("base64"),
                        mimeType: image.mime_type,
                    };
            }
        }
        history.push(native);
    }
    const tools = request.tools.map((t) => ({
        name: t.name,
        description: t.description,
        parameters: t.input_schema,
        constrainedSampling: t.metadata?.["pi/constrained_sampling"] ?? {
            type: "json_schema",
            strict: t.surface.strict ? "require" : "prefer",
        },
    }));
    return normalizeContext({
        systemPrompt: request.instructions.map((i) => i.text).join("\n\n"),
        tools,
        messages: history,
    });
}

export function canonicalAnswer(message, id) {
    const calls = [],
        parts = [];
    const nativeParts = [];
    for (const block of message.content) {
        let payload;
        if (block.type === "text") payload = { Text: { text: block.text } };
        else if (block.type === "thinking")
            payload = { Reasoning: { text: block.thinking, signature: block.thinkingSignature ?? null } };
        else if (block.type === "toolCall") {
            const call = {
                id: block.id,
                name: block.name,
                args: block.arguments,
                surface: "function",
                raw_arguments: null,
            };
            payload = { ToolCall: { call } };
            calls.push(call);
        } else throw new Error("Unsupported native model content " + block.type);
        // Only retain native fields missing from the canonical block. Long
        // code edits and reasoning text otherwise occur twice in every journal entry.
        const {
            type,
            text,
            thinking,
            thinkingSignature,
            id: callID,
            name,
            arguments: args,
            ...providerFields
        } = block;
        nativeParts.push(providerFields);
        parts.push({ part_id: randomUUID(), provenance: "model", scope: "conversation", payload });
    }
    const { content, ...header } = message;
    const u = message.usage;
    // Preserve native item signatures and source IDs when this conversation resumes.
    const result = {
        id,
        role: "Assistant",
        phase: null,
        parts,
        name: null,
        tool_call_id: null,
        metadata: { pi_native: { header, parts: nativeParts } },
    };
    return {
        messages: [result],
        tool_calls: calls,
        finish_reason: { stop: "Stop", length: "Length", toolUse: "ToolCalls" }[message.stopReason],
        usage: {
            input_tokens: u.input + u.cacheRead + u.cacheWrite,
            output_tokens: u.output,
            cached_input_tokens: u.cacheRead,
            cache_creation_input_tokens: u.cacheWrite,
            reasoning_output_tokens: u.reasoning ?? null,
        },
        end_turn: message.endTurn ?? null,
        provider_metadata: { model: message.model, response_id: message.responseId ?? null },
    };
}

const transportCodes = new Set([
    "UND_ERR_SOCKET",
    "UND_ERR_CONNECT_TIMEOUT",
    "UND_ERR_HEADERS_TIMEOUT",
    "UND_ERR_BODY_TIMEOUT",
    "ECONNRESET",
    "ECONNREFUSED",
    "ETIMEDOUT",
    "EPIPE",
    "EAI_AGAIN",
    "ERR_STREAM_PREMATURE_CLOSE",
]);

export function modelFailure(error, { signal, started = false, nativeMessage } = {}) {
    const message = String(nativeMessage?.errorMessage ?? error?.message ?? error);
    let kind = "other";
    if (signal?.aborted || nativeMessage?.stopReason === "aborted" || error?.name === "AbortError") {
        kind = "interrupted";
    } else {
        // Pi preserves the deepest transport cause code in the native display
        // message. Its classifier first rejects known subscription/quota errors.
        const retryable = isRetryableAssistantError({ stopReason: "error", errorMessage: message });
        const terminal = /\b(?:401|403|authentication|unauthorized|forbidden|invalid[_ ]api[_ ]key)\b/i.test(
            message,
        );
        const codes = message.match(/\b(?:UND_ERR_[A-Z_]+|E[A-Z_]+)\b/g) ?? [];
        let cause = error;
        for (let depth = 0; cause && depth < 6; depth++, cause = cause.cause) {
            if (typeof cause.code === "string") codes.push(cause.code);
        }
        // A socket can also close cleanly at HTTP level, leaving the provider
        // parser without a protocol terminator and without an OS error code.
        const prematureEnd =
            nativeMessage?.stopReason === "error" &&
            /^(?:stream ended without finish_reason|OpenAI Responses stream ended without a stop reason|stream ended before a terminal response event|stream disconnected before completion(?::.*)?)$/i.test(message);
        if (retryable && !terminal && (prematureEnd || codes.some((code) => transportCodes.has(code)))) {
            kind = started ? "stream_disconnected" : { retryable: { retry_delay_ms: null } };
        }
    }
    return { kind, message, completed_messages: [] };
}

export function modelExport(binding) {
    if (
        binding.module_id !== "pi_codex" ||
        binding.contract_version !== "v13" ||
        Object.keys(binding.module_config).length !== 1 ||
        typeof binding.module_config.implementation !== "string"
    )
        throw new Error("Invalid Pi model configuration");
    const ready = ModelRuntime.create({ modelsPath: null, allowModelNetwork: false });
    const goQuota = binding.module_config.implementation === "opencode-go" ? createGoQuotaReader() : null;
    return {
        manifest: {
            slot: "model",
            module_id: "pi_codex",
            contract_version: "v13",
            composition: "select_one",
            module_features: goQuota ? ["model.quota.v1"] : [],
            config_schema: null,
        },
        async invoke(method, params, { signal, hostCall }) {
            const runtime = await ready;
            if (method === "quota") {
                if (!goQuota) throw new Error("Quota discovery is unsupported by this Pi provider");
                return goQuota(runtime, signal);
            }
            const ref = method === "describe" ? params.model : params.request?.model;
            const model = ref && runtime.getModel(binding.module_config.implementation, ref.model);
            if (!model) throw new Error("Pi model unavailable: " + ref?.model);
            if (method === "describe") return descriptor(model);
            if (method !== "stream") throw new Error("Unsupported Pi model operation " + method);
            const r = params.request,
                id = randomUUID();
            let sequence = 0;
            let started = false;
            let nativeMessage;
            try {
                if (!["None", "Auto"].includes(r.tool_choice)) throw new Error("Unsupported Pi tool choice");
                const stream = runtime.streamSimple(model, await context(r, model), {
                    signal,
                    reasoning: r.reasoning.effort,
                    toolChoice: r.tool_choice === "None" ? "none" : undefined,
                    temperature: r.sampling.temperature ?? undefined,
                    samplingParams: r.sampling.top_p == null ? undefined : { top_p: r.sampling.top_p },
                    maxTokens:
                        r.limits.max_output_tokens != null && r.limits.max_output_tokens < model.maxTokens
                            ? r.limits.max_output_tokens
                            : undefined,
                    transport: "auto",
                    identity: DEFAULT_AGENT_IDENTITY,
                    sessionId: r.cache.routing_key ?? undefined,
                    cacheRetention: r.cache.cache_context ? undefined : "none",
                });
                for await (const e of stream) {
                    if (e.type === "start") started = true;
                    if (params.stream && e.type === "text_delta")
                        await hostCall("host.model.emit", {
                            sequence: sequence++,
                            event: { TextDelta: { message_id: id, phase: null, text: e.delta } },
                        });
                }
                const message = await stream.result();
                nativeMessage = message;
                if (message.stopReason === "error" || message.stopReason === "aborted")
                    throw new Error(message.errorMessage ?? message.stopReason);
                return {
                    event_count: sequence,
                    terminal: { kind: "response", response: canonicalAnswer(message, id) },
                };
            } catch (error) {
                return {
                    event_count: sequence,
                    terminal: {
                        kind: "stream_error",
                        failure: modelFailure(error, { signal, started, nativeMessage }),
                    },
                };
            }
        },
    };
}
