import { test } from "node:test";
import assert from "node:assert/strict";
import { buildPrompt, estimateContext, estimatePrompt, recentUserMessages } from "@opencode/core/session/compaction";
import { canonical } from "../../../components/module-runtime-js/canonical.mjs";
import { createCompactionExport, nativeContext, runNativeCompaction } from "../compaction.mjs";

const binding = config => ({ slot: "hook", module_id: "opencode_compact", contract_version: "v6", composition: "ordered_many", module_config: config });
const text = message => message.parts.flatMap(part => part.payload.Text ? [part.payload.Text.text] : []).join("\n");
const summary = "## Objective\n- Finish the task.\n\n## Work State\n### Completed\n- Inspected a file.\n\n## Next Move\n1. Implement the fix.";
function fixture(tokens = 180000, window = 200000) {
    const messages = [canonical("User", { Text: { text: "Original task" } }, "user"),
        canonical("Assistant", { Text: { text: "I inspected a file." } }, "model"),
        canonical("User", { Text: { text: "Current exact anchor: finish it." } }, "user")];
    const request = { model: { provider: "opencode-go", model: "deepseek-v4.1-flash" },
        instructions: [{ kind: "Developer", text: "Preserve current instructions.", priority: 90 }],
        messages, tools: [{ name: "read", description: "Read files", input_schema: { type: "object" } }],
        limits: {}, reasoning: { effort: "high" }, sampling: { temperature: 0.3 },
        cache: { routing_key: "active-session" }, client_metadata: { task: "same" }, metadata: { custom: true } };
    return { event: { event: "before_context", target: { revision: 7 },
        input: { messages, request, token_estimate: tokens, window_tokens: window, reason: null },
        output: { messages, changed: false, user_message_replacements: [], summary: null,
            token_estimate: tokens, original_token_estimate: null, trigger_tokens: null,
            summary_source: null, skipped_reason: null, metadata: null } } };
}
function fakeHost(replies = [summary]) {
    const requests = [];
    const controller = new AbortController();
    return { requests, controller, invocation: { signal: controller.signal,
        async hostCall(method, params) {
            assert.equal(method, "host.model.complete"); requests.push(params.request);
            const next = replies[Math.min(requests.length - 1, replies.length - 1)];
            if (next instanceof Error) throw next;
            return { finish_reason: "Stop", tool_calls: [], messages: [canonical("Assistant", { Text: { text: next } }, "model")] };
        } } };
}

test("actual upstream service threshold boundaries and input-window precedence", async () => {
    for (const [window, threshold] of [[100, 90], [31999, 28800], [32000, 16000], [50000, 34000], [200000, 180000]]) {
        for (const offset of [-1, 0]) {
            const input = fixture(threshold + offset, window);
            const host = fakeHost();
            const result = await createCompactionExport(binding({})).invoke("hook.invoke", input, host.invocation);
            assert.equal(host.requests.length, offset === -1 ? 0 : 1, `${window}/${threshold + offset}`);
            assert.equal(result.result.action, offset === -1 ? "continue" : "context_preparation");
        }
    }
    const input = fixture(34000, 200000); input.event.input.request.limits.max_input_tokens = 50000;
    const host = fakeHost();
    await createCompactionExport(binding({})).invoke("hook.invoke", input, host.invocation);
    assert.equal(host.requests.length, 1);
});

test("summary uses actual native service prompt/split and Core model callback controls", async () => {
    const input = fixture(), before = structuredClone(input), host = fakeHost();
    const result = await createCompactionExport(binding({})).invoke("hook.invoke", input, host.invocation);
    assert.deepEqual(input, before, "preparation candidates and target must remain immutable");
    const request = host.requests[0];
    assert.equal(text(request.messages.at(-1)), buildPrompt(false));
    assert.ok(request.messages.some(message => text(message).includes("Original task")));
    assert.ok(!request.messages.some(message => text(message).includes("Current exact anchor")), "latest exchange is retained verbatim by native split, not summarized");
    assert.deepEqual(request.tools, []); assert.equal(request.tool_choice, "None");
    assert.deepEqual(request.model, input.event.input.request.model);
    assert.deepEqual(request.reasoning, input.event.input.request.reasoning);
    assert.deepEqual(request.cache, input.event.input.request.cache);
    assert.deepEqual(request.client_metadata, input.event.input.request.client_metadata);
    assert.deepEqual(request.metadata, { custom: true, suppress_stream_deltas: true });
    const output = result.result.output;
    assert.equal(output.messages.length, 1); assert.equal(output.messages[0].parts[0].provenance, "compactor");
    assert.ok(text(output.messages[0]).includes("Current exact anchor: finish it."));
    assert.ok(text(output.messages[0]).includes("<conversation-checkpoint>"));
    assert.deepEqual(output.user_message_replacements, [{ source_message_id: input.event.input.messages.at(-1).id, replacement_message_id: output.messages[0].id }]);
    assert.equal(output.metadata.native_service, true);
    assert.equal(output.original_token_estimate, 180000);
});

test("native config plugin controls auto, explicit buffer, and manual override", async () => {
    const input = fixture(199999), host = fakeHost();
    await createCompactionExport(binding({ auto: false })).invoke("hook.invoke", input, host.invocation);
    assert.equal(host.requests.length, 0);
    input.event.input.reason = "manual";
    await createCompactionExport(binding({ auto: false })).invoke("hook.invoke", input, host.invocation);
    assert.equal(host.requests.length, 1);
    const buffered = fixture(190000), second = fakeHost();
    await createCompactionExport(binding({ buffer: 10000, keep: 0 })).invoke("hook.invoke", buffered, second.invocation);
    assert.equal(second.requests.length, 1);
});

test("actual native template retry nudges once and rejects invalid summaries", async () => {
    const host = fakeHost(["unstructured response", summary]);
    await createCompactionExport(binding({})).invoke("hook.invoke", fixture(), host.invocation);
    assert.equal(host.requests.length, 2);
    assert.match(text(host.requests[1].messages.at(-1)), /previous response did not fill in the required summary template/);
    await assert.rejects(createCompactionExport(binding({})).invoke("hook.invoke", fixture(), fakeHost(["bad"]).invocation), /required template/);
});

test("a native checkpoint is reused for updates and is not immediately compacted again", async () => {
    const hook = createCompactionExport(binding({})), host = fakeHost();
    const first = await hook.invoke("hook.invoke", fixture(), host.invocation);
    const input = fixture(); input.event.output.messages = first.result.output.messages;
    const skipped = await hook.invoke("hook.invoke", input, host.invocation);
    assert.equal(skipped.result.action, "continue"); assert.equal(host.requests.length, 1);
    input.event.output.messages = [...first.result.output.messages, canonical("User", { Text: { text: "New task" } }, "user")];
    await hook.invoke("hook.invoke", input, host.invocation);
    assert.equal(text(host.requests.at(-1).messages.at(-1)), buildPrompt(true));
});

test("native-provider remote compaction reports its gap, never substitutes a local summary", async () => {
    const input = fixture(), host = fakeHost();
    const key = "opencode-go/deepseek-v4.1-flash";
    await assert.rejects(createCompactionExport(binding({ models: { [key]: { native_compaction: true } } })).invoke("hook.invoke", input, host.invocation), /Provider compaction|Native compaction|provider.*compaction/);
    assert.equal(host.requests.length, 0);
});

test("native estimation and recent whole user-message selection stay directly upstream", async () => {
    const input = fixture();
    const context = await nativeContext(input.event.input, input.event.output.messages);
    assert.equal(estimateContext(context), 180000);
    assert.deepEqual(estimatePrompt(context), { measured: 180000, estimated: 0 });
    const recent = recentUserMessages(context.messages, context.model, 1000);
    assert.equal(recent.length, 2);
    assert.ok(recent.at(-1).content[0].text.includes("Current exact anchor"));
    const direct = await runNativeCompaction(context, input.event.input.request, {}, fakeHost().invocation);
    assert.equal(direct.outcome.status, "completed");
});

test("cancel and non-preparation events cannot produce a model call", async () => {
    const hook = createCompactionExport(binding({})), host = fakeHost();
    assert.deepEqual(await hook.invoke("hook.invoke", { event: { event: "turn_started" } }, host.invocation), { result: { action: "continue" } });
    host.controller.abort(new Error("turn canceled"));
    await assert.rejects(hook.invoke("hook.invoke", fixture(), host.invocation), /turn canceled/);
    assert.equal(host.requests.length, 0);
});

test("strict configuration rejects unknown fields and malformed model limits", () => {
    assert.throws(() => createCompactionExport(binding({ fake: true })), /Unknown/);
    assert.throws(() => createCompactionExport(binding({ keep: -1 })), /nonnegative/);
    assert.throws(() => createCompactionExport(binding({ models: { "p/m": { input: "large" } } })), /nonnegative/);
    assert.ok(createCompactionExport(binding({})).manifest.config_schema.fields.some(field => field.key === "models"));
});

test("tool calls/results reach the native transcript, and instruction state survives checkpoint", async () => {
    const input = fixture(), host = fakeHost();
    const call = { id: "read-1", name: "read", args: { filePath: "a" }, surface: "function", raw_arguments: null };
    input.event.output.messages.splice(2, 0,
        canonical("Assistant", { ToolCall: { call } }, "model"),
        canonical("Tool", { ToolResult: { result: { call_id: call.id, ok: true, output: "File content", content: [], error: null,
            metadata: { "opencode/synthetic": [{ type: "synthetic", text: "Scoped instruction", metadata: { instruction: { paths: ["/workspace/AGENTS.md"] } } }] } } } }, "tool"));
    const output = await createCompactionExport(binding({})).invoke("hook.invoke", input, host.invocation);
    assert.ok(host.requests[0].messages.some(message => message.parts.some(part => part.payload.ToolCall?.call.name === "read")));
    assert.ok(host.requests[0].messages.some(message => message.parts.some(part => part.payload.ToolResult?.result.output === "File content")));
    assert.equal(output.result.output.messages[0].metadata["opencode/synthetic"][0].text, "Scoped instruction");
});

test("model budget failure retains Core typed data and never emits a summary", async () => {
    const failure = Object.assign(new Error("budget exhausted"), { data: { kind: "session_budget_exceeded", message: "budget exhausted", completed_messages: [] } });
    const host = fakeHost([failure]);
    await assert.rejects(createCompactionExport(binding({})).invoke("hook.invoke", fixture(), host.invocation), error => {
        assert.deepEqual(error.data, failure.data); return true;
    });
    assert.equal(host.requests.length, 1);
});

test("native context overflow recovery shrinks the actual summary request", async () => {
    const failure = Object.assign(new Error("context full"), { data: { kind: "context_window_exceeded", message: "context full", completed_messages: [] } });
    const host = fakeHost([failure, summary]);
    const input = fixture();
    input.event.output.messages.unshift(...Array.from({ length: 10 }, (_, index) =>
        canonical("User", { Text: { text: `Historical exchange ${index}: ${"x".repeat(3000)}` } }, "user")));
    const result = await createCompactionExport(binding({})).invoke("hook.invoke", input, host.invocation);
    assert.equal(result.result.action, "context_preparation");
    assert.equal(host.requests.length, 2);
});

test("cancellation after a summary callback rejects instead of returning changed history", async () => {
    const host = fakeHost();
    const complete = host.invocation.hostCall;
    host.invocation.hostCall = async (...args) => {
        const result = await complete(...args); host.controller.abort(new Error("turn canceled during summary")); return result;
    };
    await assert.rejects(createCompactionExport(binding({})).invoke("hook.invoke", fixture(), host.invocation), /turn canceled/);
});
