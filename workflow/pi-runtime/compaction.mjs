import { canonical } from "../../components/module-runtime-js/canonical.mjs";
import { piMessages, assistant, finishedStream } from "../../components/pi-runtime-js/messages.mjs";
import {
    prepareCompaction,
    compact,
    shouldCompact,
    estimateContextTokens,
    DEFAULT_COMPACTION_SETTINGS,
    convertToLlm,
    createCompactionSummaryMessage,
    resolveModel,
} from "./upstream.mjs";

export async function preparation(params, { hostCall, signal, catalogProvider }) {
    const { event } = params;
    const unchanged = { result: { action: "continue" } };
    if (event.event !== "before_context" || !event.input.request) return unchanged;
    const request = event.input.request;
    const model = resolveModel(request.model, catalogProvider);
    const messages = event.output.messages;
    const pi = piMessages(messages, model);
    const tokens = event.input.token_estimate ?? estimateContextTokens(pi).tokens;
    if (!shouldCompact(tokens, model.contextWindow, DEFAULT_COMPACTION_SETTINGS)) return unchanged;
    const entries = messages.map((m, i) => {
        const common = {
            id: m.id,
            parentId: i ? messages[i - 1].id : null,
            timestamp: new Date(0).toISOString(),
        };
        const prior = m.metadata?.pi_compaction;
        return prior
            ? { ...common, ...prior, type: "compaction" }
            : { ...common, type: "message", message: pi[i] };
    });
    const plan = prepareCompaction(entries, DEFAULT_COMPACTION_SETTINGS);
    if (!plan) return unchanged;
    const streamFn = async (_, ctx, options) => {
        const instructions = [];
        const history = [];
        for (const m of ctx.messages) {
            const text =
                typeof m.content === "string" ? m.content : (m.content?.map((c) => c.text).join("\n") ?? "");
            if (m.role === "system") instructions.push({ kind: "System", priority: 100, text });
            else history.push(canonical("User", { Text: { text } }, "user"));
        }
        const reply = await hostCall("host.model.complete", {
            request: {
                ...request,
                instructions,
                messages: history,
                tools: [],
                tool_choice: "None",
                cache: { cache_context: false, cache_instructions: false, routing_key: options.sessionId },
                metadata: { suppress_stream_deltas: true },
            },
        });
        return finishedStream(assistant(reply, model));
    };
    const summary = await compact(
        plan,
        model,
        undefined,
        undefined,
        undefined,
        signal,
        request.reasoning.effort,
        streamFn,
    );
    const text = convertToLlm([createCompactionSummaryMessage(summary.summary, summary.tokensBefore, 0)])[0]
        .content[0].text;
    const compacted = canonical("User", { Text: { text } }, "compactor");
    compacted.metadata = { pi_compaction: summary };
    const cut = messages.findIndex((m) => m.id === summary.firstKeptEntryId);
    if (cut < 0) throw new Error("Pi compaction retained an unknown entry");
    const currentUser = messages.findLast((m) => m.role === "User");
    const replacements =
        currentUser && messages.indexOf(currentUser) < cut
            ? [{ source_message_id: currentUser.id, replacement_message_id: compacted.id }]
            : [];
    return {
        result: {
            action: "context_preparation",
            output: {
                ...event.output,
                messages: [compacted, ...messages.slice(cut)],
                changed: true,
                user_message_replacements: replacements,
                summary: summary.summary,
                original_token_estimate: tokens,
                token_estimate: null,
                trigger_tokens: model.contextWindow - DEFAULT_COMPACTION_SETTINGS.reserveTokens,
                summary_source: "model",
                skipped_reason: null,
                metadata: { first_kept_entry_id: summary.firstKeptEntryId },
            },
        },
    };
}
