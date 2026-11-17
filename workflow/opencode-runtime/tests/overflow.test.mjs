import test from "node:test";
import assert from "node:assert/strict";
import { definition } from "@opencode/core/tool/runtime";
import { Input as NativeReadInput } from "@opencode/core/tool/plugin/read";
import { canonical } from "../../../components/module-runtime-js/canonical.mjs";
import { createCompactionExport } from "../compaction.mjs";
import { workflow } from "../workflow.mjs";

const text = (role, value) => canonical(role, { Text: { text: value } }, role === "User" ? "user" : "model");
const ref = { provider: "opencode-go", model: "deepseek-v4.1-flash" };
const overflow = { kind: "context_window_exceeded", message: "provider context overflow", completed_messages: [] };
const summary = "## Objective\nFinish current work.\n\n## Work State\n### Completed\nEarlier inspection.\n\n## Next Move\nContinue the current task.";
const tool = { name: "read", description: "Read", input_schema: definition({ name: "read", input: NativeReadInput }).inputSchema };
const call = { id: "committed-once", name: "read", args: { path: "counter" }, surface: "function", raw_arguments: null };

async function run(mode = "success") {
    const controller = new AbortController();
    const history = [text("User", "Original task " + "historical context ".repeat(2500)),
        text("Assistant", "Earlier work " + "inspected ".repeat(1000)), text("User", "Current task")];
    const hook = createCompactionExport({ slot: "hook", module_id: "opencode_compact", contract_version: "v7",
        composition: "ordered_many", module_config: { keep: 0, ...(mode === "unchanged" ? { auto: false } : {}) } });
    const requests = [], checkpoints = [], summaries = [];
    let sample = 0, frame = 0, effects = 0, revision = 0, replacementAcked = false;
    const invocation = { signal: controller.signal, async hostCall(method, params) {
        if (method === "host.events.emit") return {};
        if (method === "host.context.build") return { chunks: [], token_estimate: null };
        if (method === "host.tools.select") return { tools: [tool] };
        if (method === "host.context.prepare") {
            assert.deepEqual(params.input.request.messages, params.input.messages,
                "preparation must receive canonical history, not request-only context projection");
            const output = { messages: params.input.messages, changed: false, user_message_replacements: [] };
            const result = await hook.invoke("hook.invoke", { event: { event: "before_context",
                input: params.input, output, target: null } }, invocation);
            return { output: result.result.action === "continue" ? output : result.result.output,
                target: { revision } };
        }
        if (method === "host.model.complete") {
            summaries.push(structuredClone(params.request));
            if (mode === "summary-error") throw new Error("summary rejected");
            if (mode === "cancel") controller.abort(new Error("canceled during summary"));
            return { messages: [text("Assistant", summary)], tool_calls: [], finish_reason: "Stop" };
        }
        if (method === "host.model.stream.start") {
            sample++; frame = 0;
            if (sample > 2) assert.ok(replacementAcked, "overflow cannot resample before replacement ACK");
            requests.push(structuredClone(params.request));
            return { stream_id: String(sample) };
        }
        if (method === "host.model.stream.next") {
            if (sample === 1) return { type: "response", response: {
                messages: [canonical("Assistant", { ToolCall: { call } }, "model")], tool_calls: [call],
                finish_reason: "ToolCalls", usage: { input_tokens: 100, output_tokens: 10 },
            } };
            if (sample === 2 || mode === "twice") {
                if (frame++ === 0) return { type: "message_completed", message: canonical("Assistant",
                    { ToolCall: { call: { ...call, id: "abandoned-partial" } } }, "model") };
                return { type: "error", failure: overflow };
            }
            return { type: "response", response: { messages: [text("Assistant", "finished")],
                tool_calls: [], finish_reason: "Stop", usage: { input_tokens: 50, output_tokens: 1 } } };
        }
        if (method === "host.history.checkpoint") {
            checkpoints.push(structuredClone(params));
            if (params.preparation_target) {
                if (mode === "ack-error") throw new Error("checkpoint refused");
                if (mode === "ack-null") return { revision: null };
                replacementAcked = true;
            }
            const messages = [...(params.history.history_replacement ?? history), ...params.history.new_messages];
            return { revision: ++revision, history: structuredClone(params.history), messages: structuredClone(messages) };
        }
        if (method === "host.tools.execute") {
            effects++;
            assert.equal(params.call.id, call.id);
            return { call_id: call.id, ok: true, output: "counter=1", content: [], error: null, metadata: null };
        }
        throw new Error(method);
    } };
    const result = await workflow({ task: { cwd: "/work", text: "Current task" }, history,
        runtime: { model_ref: ref, instructions: [], max_input_tokens: 128000, reasoning: {} } }, invocation);
    return { result, requests, checkpoints, summaries, effects, replacementAcked };
}

test("typed provider overflow uses native compaction and acknowledged history without repeating tools", async () => {
    const { result, requests, checkpoints, summaries, effects } = await run();
    assert.equal(result.status, "success", JSON.stringify(result.failure));
    assert.equal(result.result.output.text, "finished");
    assert.equal(effects, 1);
    assert.equal(requests.length, 3);
    assert.equal(summaries.length, 1);
    assert.deepEqual(summaries[0].tools, []);
    const compacted = checkpoints.find(p => p.preparation_target);
    assert.equal(compacted.history.compactions.at(-1).reason, "context_window_exceeded");
    assert.equal(compacted.history.compactions.at(-1).metadata.native_service, true);
    assert.ok(JSON.stringify(requests[2]).includes("counter=1"), "native recent transcript retains accepted results");
    assert.ok(!JSON.stringify(result).includes("abandoned-partial"));
    assert.equal(result.result.compactions.length, 1);
});

test("overflow recovery refuses no-change, failed/unconfirmed ACK, summary failure and cancellation", async () => {
    for (const [mode, message] of [["unchanged", "provider context overflow"],
        ["ack-null", "durable checkpoint acknowledgement"], ["ack-error", "checkpoint refused"],
        ["summary-error", "summary rejected"], ["cancel", "canceled during summary"]]) {
        const { result, requests, effects, replacementAcked } = await run(mode);
        assert.equal(result.status, "error", mode);
        assert.ok(result.failure.message.includes(message), `${mode}: ${result.failure.message}`);
        assert.equal(requests.length, 2, mode);
        assert.equal(effects, 1, mode);
        assert.equal(replacementAcked, false, mode);
        assert.ok(JSON.stringify(result.failure.history).includes("counter=1"), mode);
        assert.ok(!JSON.stringify(result.failure.history).includes("abandoned-partial"), mode);
    }
});

test("native one-overflow allowance rejects a second overflow before a completed model step", async () => {
    const { result, requests, summaries, effects } = await run("twice");
    assert.equal(result.status, "error");
    assert.deepEqual(result.failure.model_failure, overflow);
    assert.equal(requests.length, 3);
    assert.equal(summaries.length, 1);
    assert.equal(effects, 1);
    assert.equal(result.failure.history.compactions.length, 1);
});
