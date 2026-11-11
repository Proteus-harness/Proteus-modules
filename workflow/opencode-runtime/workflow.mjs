import { createHash, randomUUID } from "node:crypto";
import { Effect, Stream } from "effect";
import { LLMClient } from "@opencode/ai";
import { make as makeStep } from "@opencode/core/session/runner/step";
import { Service as Bus } from "@opencode/core/bus";
import { Service as Snapshot } from "@opencode/core/snapshot";
import { Service as ToolOutput } from "@opencode/core/tool-output";
import { Error as ToolError } from "@opencode/core/tool";
import { estimateContext } from "@opencode/core/session/compaction";
import { toolBarrier } from "./dispatch.mjs";
import { selectNativeTools } from "./selection.mjs";
import { repairNativeCall } from "./repair.mjs";
import { nativeContext } from "./compaction.mjs";
import { canonical } from "../../components/module-runtime-js/canonical.mjs";

function withContext(messages, chunks) {
    if (!chunks.length) return messages;
    const anchor = messages.findIndex((m) => m.role === "User");
    if (anchor === -1) throw new Error("OpenCode conversation has no user message");
    // Context is projected only for sampling. Stable identifiers also allow
    // journal replay to compare the complete model request.
    const uuid = (key) => {
        const bytes = createHash("sha256").update(key).digest();
        bytes[6] = (bytes[6] & 15) | 80;
        bytes[8] = (bytes[8] & 63) | 128;
        return bytes
            .subarray(0, 16)
            .toString("hex")
            .replace(/^(........)(....)(....)(....)(............)$/, "$1-$2-$3-$4-$5");
    };
    const key = messages[anchor].id + "/opencode-context";
    const msg = {
        ...messages[anchor],
        id: uuid(key),
        metadata: null,
        parts: chunks.map((chunk, i) => ({
            part_id: uuid(key + i),
            provenance: "context_builder",
            scope: "request",
            payload: { Context: { chunk } },
        })),
    };
    return [...messages.slice(0, anchor), msg, ...messages.slice(anchor)];
}

function nativeEvents(reply) {
    const events = [{ type: "step-start" }];
    const finish = {
        Stop: "stop",
        ToolCalls: "tool-calls",
        Length: "length",
        ContentFilter: "content-filter",
        Unknown: "unknown",
    };
    if (!finish[reply.finish_reason]) throw new Error("Model stopped with " + reply.finish_reason);
    for (const message of reply.messages)
        for (const part of message.parts) {
            const p = part.payload;
            if (p.Text || p.Reasoning || p.ReasoningSummary) {
                const kind = p.Text ? "text" : "reasoning";
                const id = part.part_id;
                events.push(
                    { type: kind + "-start", id },
                    { type: kind + "-delta", id, text: (p.Text ?? p.Reasoning ?? p.ReasoningSummary).text },
                    { type: kind + "-end", id },
                );
            }
        }
    for (const { id, name, args } of reply.tool_calls)
        events.push({ type: "tool-call", id, name, input: args, providerExecuted: false });
    const u = reply.usage;
    const usage = u && {
        nonCachedInputTokens:
            u.input_tokens - (u.cached_input_tokens ?? 0) - (u.cache_creation_input_tokens ?? 0),
        cacheReadInputTokens: u.cached_input_tokens,
        cacheWriteInputTokens: u.cache_creation_input_tokens,
        visibleOutputTokens: u.output_tokens - (u.reasoning_output_tokens ?? 0),
        reasoningTokens: u.reasoning_output_tokens,
    };
    events.push(
        {
            type: "step-finish",
            reason: { normalized: finish[reply.finish_reason], raw: reply.finish_reason },
            usage,
        },
        { type: "finish" },
    );
    return events;
}

export async function workflow({ task, history, runtime }, { hostCall, signal }) {
    let messages = [...history];
    let progress = runtime.continuation?.history ?? {
        new_messages: [],
        history_replacement: null,
        compactions: [],
    };
    const emit = (event) => hostCall("host.events.emit", { event });
    async function checkpoint(tool_results = [], preparation_target = null) {
        const ack = await hostCall("host.history.checkpoint", {
            history: progress,
            tool_results,
            preparation_target,
        });
        if (ack.revision != null) {
            progress = ack.history;
            messages = ack.messages;
        }
    }
    try {
        if (runtime.intent) throw new Error("Unsupported OpenCode workflow intent " + runtime.intent);
        await emit({ TaskReceived: { task } });
        const context = await hostCall("host.context.build", { task });
        await emit({
            ContextBuilt: { chunks: context.chunks.length, token_estimate: context.token_estimate },
        });
        const ref = runtime.model_ref;
        let text = "";
        let measured = runtime.model_context?.at(-1);
        let lastUsage = measured?.kind === "usage" ? measured.last_tokens : null;
        for (;;) {
            signal.throwIfAborted();
            const selection = await hostCall("host.tools.select", {
                request: {
                    task,
                    cwd: task.cwd,
                    query: null,
                    max_tools: null,
                    reason: "before_model_request",
                    phase: "opencode",
                },
            });
            selection.tools = await selectNativeTools(ref, selection.tools, signal);
            const request = {
                model: ref,
                instructions: runtime.instructions,
                messages,
                tools: selection.tools,
                tool_choice: selection.tools.length ? "Auto" : "None",
                response_format: "Text",
                sampling: { temperature: null, top_p: null },
                reasoning: runtime.reasoning,
                limits: { max_input_tokens: runtime.max_input_tokens, max_output_tokens: null },
                cache: {
                    cache_instructions: true,
                    cache_context: true,
                    routing_key: runtime.conversation?.session_id ?? null,
                },
                client_metadata: {},
                metadata: null,
            };
            const input = {
                messages,
                request,
                window_tokens: runtime.max_input_tokens,
                token_estimate: null,
                reason: "opencode_before_sampling",
            };
            // Reuse the native estimator for unmeasured content; a provider
            // usage observation is an anchor for the next preparation.
            const projected = await nativeContext(input, messages);
            const estimate = estimateContext(projected);
            input.token_estimate = lastUsage == null ? estimate : Math.max(estimate, lastUsage);
            const prepared = await hostCall("host.context.prepare", { input });
            const p = prepared.output;
            if (p.changed) {
                const report = {
                    changed: true,
                    user_message_replacements: p.user_message_replacements,
                    reason: input.reason,
                    input_messages: messages.length,
                    output_messages: p.messages.length,
                    original_token_estimate: p.original_token_estimate,
                    output_token_estimate: p.token_estimate,
                    trigger_tokens: p.trigger_tokens,
                    summary_source: p.summary_source,
                    skipped_reason: p.skipped_reason,
                    summary: p.summary,
                    metadata: p.metadata,
                };
                progress = {
                    new_messages: [],
                    history_replacement: p.messages,
                    compactions: [...progress.compactions, report],
                };
                messages = p.messages;
                await checkpoint([], prepared.target);
            }
            request.messages = withContext(messages, context.chunks);
            await emit({ ModelRequestPrepared: { model: ref } });
            const cursor = await hostCall("host.model.stream.start", { request });
            let reply;
            while (!reply) {
                signal.throwIfAborted();
                const item = await hostCall("host.model.stream.next", cursor);
                if (item.type === "error") throw new Error(item.failure.message);
                if (item.type === "response") reply = item.response;
                else if (item.type !== "message_completed")
                    throw new Error("Unknown model frame " + item.type);
            }
            await emit({ ModelResponseReceived: { finish_reason: reply.finish_reason } });
            const events = nativeEvents(reply);
            const bindings = new Map(
                await Promise.all(reply.tool_calls.map(async (call) => [
                    call.id,
                    {
                        call_id: call.id,
                        execution_call: await repairNativeCall(call, selection.tools, signal),
                        message_id: randomUUID(),
                        part_id: randomUUID(),
                    },
                ])),
            );
            messages.push(...reply.messages);
            progress.new_messages.push(...reply.messages);
            await checkpoint([...bindings.values()]);
            text = reply.messages
                .flatMap((m) => m.parts.flatMap((p) => (p.payload.Text ? [p.payload.Text.text] : [])))
                .join("\n");
            lastUsage = reply.usage ? reply.usage.input_tokens + reply.usage.output_tokens : null;
            const results = new Map();
            const dispatch = toolBarrier(selection.tools, signal);
            const executeTool = ({ call }) =>
                Effect.tryPromise({
                    try: async () => {
                        const binding = bindings.get(call.id);
                        const result = await dispatch(binding.execution_call, () => hostCall("host.tools.execute", {
                            task,
                            call: binding.execution_call,
                        }));
                        results.set(call.id, result);
                        if (!result.ok) throw new ToolError({ message: result.error ?? result.output });
                        return {
                            content: [{ type: "text", text: result.output }],
                            metadata: result.metadata ?? undefined,
                        };
                    },
                    catch: (e) => e,
                });
            const step = await Effect.runPromise(
                makeStep.pipe(
                    Effect.provideService(Bus, { publish: () => Effect.void }),
                    Effect.provideService(Snapshot, { capture: () => Effect.succeed(undefined) }),
                    Effect.provideService(ToolOutput, { truncate: (r) => Effect.succeed(r) }),
                    Effect.provideService(LLMClient.Service, { stream: () => Stream.fromIterable(events) }),
                ),
                { signal },
            );
            const outcome = await Effect.runPromise(
                step.attempt({
                    sessionID: runtime.conversation?.session_id ?? runtime.execution_id,
                    assistantMessageID: reply.messages[0]?.id,
                    agent: "build",
                    model: {
                        ref: { providerID: ref.provider, id: ref.model },
                        model: { provider: ref.provider, route: {} },
                        cost: [],
                    },
                    isLocationClosed: () => signal.aborted,
                    prepared: {
                        request: { toolChoice: { type: selection.tools.length ? "auto" : "none" } },
                        options: {},
                        executeTool,
                    },
                    recoverContinuation: false,
                    recoverOverflow: Effect.succeed(false),
                    retry: () => Effect.succeed({ retry: false }),
                }),
                { signal },
            );
            // Native execution fibers may settle in a different order. Preserve
            // model call order and the pre-authorized Core result identities.
            for (const [id, binding] of bindings) {
                if (!results.has(id)) throw new Error("OpenCode step ended without a tool result for " + id);
                const message = canonical(
                    "Tool",
                    { ToolResult: { result: results.get(id) } },
                    "tool",
                    binding,
                );
                messages.push(message);
                progress.new_messages.push(message);
            }
            if (bindings.size) await checkpoint();
            if (outcome._tag !== "Completed")
                throw new Error("Unhandled OpenCode step outcome " + outcome._tag);
            if (!outcome.needsContinuation) break;
        }
        const output = { text, metadata: { workflow: "opencode_loop" } };
        await emit({ TurnFinished: { output } });
        return { status: "success", result: { output, ...progress } };
    } catch (e) {
        return {
            status: "error",
            failure: {
                message: String(e.message ?? e),
                history: progress.new_messages.length || progress.history_replacement ? progress : null,
                model_failure: null,
            },
        };
    }
}

export function createWorkflowExport(binding) {
    if (
        binding.module_id !== "opencode_loop" ||
        binding.slot !== "workflow" ||
        binding.contract_version !== "v20" ||
        binding.composition !== "select_one" ||
        Object.keys(binding.module_config).length
    )
        throw new Error("Invalid OpenCode workflow binding");
    return {
        manifest: {
            module_id: binding.module_id,
            slot: binding.slot,
            contract_version: "v20",
            composition: "select_one",
            module_features: [],
            config_schema: null,
        },
        invoke(method, params, invocation) {
            if (method !== "run") throw new Error("Unsupported workflow method " + method);
            return workflow(params, invocation);
        },
    };
}
