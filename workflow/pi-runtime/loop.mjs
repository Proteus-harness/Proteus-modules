import { randomUUID } from "node:crypto";
import { runAgentLoop, convertToLlm, buildSystemPrompt, estimateContextTokens, resolveModel, createCodingToolDefinitions, SETTINGS_DEFAULTS, retryDelayMs } from "./upstream.mjs";
import { executionCall } from "./tool-arguments.mjs";
import { canonical } from "../../components/module-runtime-js/canonical.mjs";
import { assistant, finishedStream, piContent, piMessages } from "../../components/pi-runtime-js/messages.mjs";

import { receiveModelResponse } from "../../components/module-runtime-js/model-stream.mjs";

function piRetryDelay(failure, attempt) {
    const policy = SETTINGS_DEFAULTS.retry;
    if (!policy.enabled || attempt > policy.maxRetries) return undefined;
    const advice = failure.kind?.retryable?.retry_delay_ms ?? 0;
    return Math.min(policy.maxAgentDelayMs, Math.max(retryDelayMs(policy, attempt), advice));
}

export async function workflow({ task, runtime, history }, { hostCall, signal, catalogProvider }) {
    let progress = runtime.continuation?.history ?? {
        new_messages: [],
        history_replacement: null,
        compactions: [],
    };
    let messages = [...history];
    const pending = new Map();
    const results = new Map();
    const emit = (event) => hostCall("host.events.emit", { event });
    async function checkpoint(bindings = [], target = null) {
        const ack = await hostCall("host.history.checkpoint", {
            history: progress,
            tool_results: bindings,
            preparation_target: target,
        });
        if (ack.revision !== null) {
            progress = ack.history;
            messages = ack.messages;
        }
    }
    try {
        if (runtime.intent) throw new Error("Unsupported Pi workflow intent " + runtime.intent);
        const model = resolveModel(runtime.model_ref, catalogProvider);
        await emit({ TaskReceived: { task } });
        const context = await hostCall("host.context.build", { task });
        await emit({
            ContextBuilt: { chunks: context.chunks.length, token_estimate: context.token_estimate },
        });
        const selection = await hostCall("host.tools.select", {
            request: {
                task,
                cwd: task.cwd,
                query: null,
                max_tools: null,
                reason: "before_model_request",
                phase: "pi",
            },
        });
        const order = ["read", "bash", "edit", "write"];
        const specs = selection.tools.toSorted((a, b) => order.indexOf(a.name) - order.indexOf(b.name));
        const prompt = buildSystemPrompt({
            cwd: task.cwd,
            selectedTools: specs.map((t) => t.name),
            toolSnippets: Object.fromEntries(specs.map((t) => [t.name, t.metadata?.["pi/prompt_snippet"]])),
            toolGuidelines: Object.fromEntries(
                specs.map((t) => [t.name, t.metadata?.["pi/prompt_guidelines"] ?? []]),
            ),
            contextFiles: context.chunks.map((c) => ({ path: c.path, content: c.content })),
            skills: [],
        });
        const definitions = new Map(createCodingToolDefinitions(task.cwd).map(t => [t.name, t]));
        const tools = specs.map((spec) => ({
            name: spec.name,
            label: spec.name,
            description: spec.description,
            parameters: spec.input_schema,
            executionMode: spec.metadata?.["pi/execution_mode"] ?? undefined,
            prepareArguments: spec.metadata?.["pi/package"] === "@earendil-works/pi-coding-agent@1.5.2"
                ? definitions.get(spec.name)?.prepareArguments : undefined,
            execute: async (id, args) => {
                const binding = pending.get(id);
                if (!binding) throw new Error("Unbound Pi invocation " + id);
                const result = await hostCall("host.tools.execute", { task, call: { ...binding.execution_call, args } });
                results.set(id, result);
                if (!result.ok) throw new Error(result.error ?? result.output);
                return {
                    content: piContent(result),
                    details: result.metadata?.["pi/details"],
                    structuredContent: result.metadata?.["pi/structured_content"] ?? undefined,
                };
            },
        }));
        let finalText = "";
        const previousUsage = runtime.model_context?.at(-1);
        let usageAnchor =
            previousUsage?.kind === "usage"
                ? {
                      id: history.findLast((m) => m.role === "Assistant")?.id,
                      tokens: previousUsage.last_tokens,
                  }
                : null;
        const streamFn = async () => {
            signal.throwIfAborted();
            const request = {
                model: runtime.model_ref,
                instructions: [...runtime.instructions, { kind: "System", text: prompt, priority: 100 }],
                messages,
                tools: specs,
                tool_choice: tools.length ? "Auto" : "None",
                response_format: "Text",
                sampling: { temperature: null, top_p: null },
                reasoning: runtime.reasoning,
                limits: { max_input_tokens: model.contextWindow, max_output_tokens: model.maxTokens },
                cache: {
                    cache_instructions: true,
                    cache_context: true,
                    routing_key: runtime.conversation?.session_id ?? null,
                },
                client_metadata: {},
                metadata: null,
            };
            const projected = piMessages(messages, model);
            const anchor = usageAnchor ? messages.findIndex((m) => m.id === usageAnchor.id) : -1;
            if (anchor >= 0) projected[anchor].usage.totalTokens = usageAnchor.tokens;
            const tokenEstimate = estimateContextTokens(projected).tokens;
            const prepared = await hostCall("host.context.prepare", {
                input: {
                    messages,
                    request,
                    window_tokens: model.contextWindow,
                    token_estimate: tokenEstimate,
                    reason: "pi_before_sampling",
                },
            });
            const p = prepared.output;
            if (p.changed) {
                const rep = {
                    changed: true,
                    user_message_replacements: p.user_message_replacements,
                    reason: "pi_before_sampling",
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
                    compactions: [...progress.compactions, rep],
                };
                messages = p.messages;
                await checkpoint([], prepared.target);
            }
            request.messages = messages;
            const reply = await receiveModelResponse(request, { hostCall, signal }, piRetryDelay);
            await emit({ ModelResponseReceived: { finish_reason: reply.finish_reason } });
            const answer = assistant(reply, model);
            if (answer.stopReason === "error")
                throw new Error("Pi model stopped with " + reply.finish_reason);
            usageAnchor = { id: reply.messages.at(-1)?.id, tokens: answer.usage.totalTokens };
            finalText = answer.content
                .filter((c) => c.type === "text")
                .map((c) => c.text)
                .join("\n");
            messages.push(...reply.messages);
            progress.new_messages.push(...reply.messages);
            for (const call of reply.tool_calls)
                pending.set(call.id, {
                    call_id: call.id,
                    execution_call: executionCall(call, tools.find(t => t.name === call.name)),
                    message_id: randomUUID(),
                    part_id: randomUUID(),
                });
            await checkpoint([...pending.values()]);
            return finishedStream(answer);
        };
        const trace = async (event) => {
            if (event.type !== "message_end" || event.message.role !== "toolResult") return;
            const msg = event.message,
                binding = pending.get(msg.toolCallId);
            const result = results.get(msg.toolCallId) ?? {
                call_id: msg.toolCallId,
                ok: !msg.isError,
                output: "",
                error: msg.content.map((c) => c.text).join("\n"),
                content: [],
                metadata: null,
            };
            const m = canonical("Tool", { ToolResult: { result } }, "tool", binding);
            messages.push(m);
            progress.new_messages.push(m);
            pending.delete(msg.toolCallId);
            results.delete(msg.toolCallId);
        };
        const piHistory = piMessages(history, model);
        await runAgentLoop(
            piHistory.slice(-1),
            {
                messages: [{ role: "system", content: prompt, timestamp: 0 }, ...piHistory.slice(0, -1)],
                tools,
            },
            {
                model,
                reasoning: runtime.reasoning.effort,
                convertToLlm,
                finishTurn: async () => {
                    await checkpoint();
                },
            },
            trace,
            signal,
            streamFn,
        );
        const output = { text: finalText, metadata: { workflow: "pi_loop" } };
        await emit({ TurnFinished: { output } });
        return { status: "success", result: { output, ...progress } };
    } catch (error) {
        return {
            status: "error",
            failure: { message: String(error.message ?? error), history: progress.new_messages.length || progress.history_replacement ? progress : null, model_failure: error.modelFailure ?? null },
        };
    }
}
