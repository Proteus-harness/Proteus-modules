import { canonical } from "../../components/module-runtime-js/canonical.mjs";
import { piMessage, piMessages, assistant, finishedStream } from "../../components/pi-runtime-js/messages.mjs";
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

export function preparation(params, invocation) {
    return prepare(params, invocation, false);
}

export function preparationFromJsonLine(params, invocation) {
    return prepare(params, invocation, true);
}

async function prepare(params, { hostCall, signal, catalogProvider }, jsonLineInputs) {
    const { event } = params;
    const unchanged = { result: { action: "continue" } };
    if (event.event !== "before_context" || !event.input.request) return unchanged;
    const request = event.input.request;
    const model = resolveModel(request.model, catalogProvider);
    const messages = event.output.messages;
    const hint = jsonLineInputs ? event.input.token_estimate : undefined;
    // Only the worker's parsed JSON-line requests can take this path. The
    // pinned Pi predicate uses scalar settings and does not inspect the rows.
    const decision = jsonLineInputs && event.input.reason === "pi_before_sampling"
        && event.output.changed === false && Array.isArray(messages)
        && Number.isInteger(hint) && hint >= 0 && hint <= 0xffff_ffff
        && Number.isFinite(model.contextWindow)
        ? shouldCompact(hint, model.contextWindow, DEFAULT_COMPACTION_SETTINGS)
        : undefined;
    if (decision === false) {
        const names = new Map();
        // Keep the same chronological conversion, including its validation
        // errors and cross-row tool names, without retaining the results.
        messages.forEach(message => { piMessage(message, model, names); });
        return unchanged;
    }
    const pi = piMessages(messages, model);
    const tokens = (jsonLineInputs ? hint : event.input.token_estimate) ?? estimateContextTokens(pi).tokens;
    if (!(decision ?? shouldCompact(tokens, model.contextWindow, DEFAULT_COMPACTION_SETTINGS))) return unchanged;
    // Reuse the private projection now that no estimator reads it.
    const entries = pi;
    messages.forEach((m, i) => {
        const common = {
            id: m.id,
            parentId: i ? messages[i - 1].id : null,
            timestamp: "1970-01-01T00:00:00.000Z",
        };
        const prior = m.metadata?.pi_compaction;
        entries[i] = prior
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
                operation: "Complete",
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
    const userIndex = messages.findLastIndex((m) => m.role === "User");
    const currentUser = userIndex === -1 ? undefined : messages[userIndex];
    const replacements =
        currentUser && (userIndex < cut || (cut > 0 && messages.lastIndexOf(currentUser, cut - 1) !== -1))
            ? [{ source_message_id: currentUser.id, replacement_message_id: compacted.id }]
            : [];
    return {
        result: {
            action: "context_preparation",
            output: {
                ...event.output,
                messages: messages.toSpliced(0, cut, compacted),
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
