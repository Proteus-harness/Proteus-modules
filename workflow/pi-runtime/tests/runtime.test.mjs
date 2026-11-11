import test from "node:test";
import assert from "node:assert/strict";
import { canonical } from "../../../components/module-runtime-js/canonical.mjs";
import { preparation } from "../compaction.mjs";
import { workflow } from "../loop.mjs";
const request = {
    model: { provider: "openai_codex", model: "gpt-6.1-sol" },
    reasoning: { effort: "high", summary: true },
};
const message = (role, text) => canonical(role, { Text: { text } }, role === "User" ? "user" : "model");
const reply = (text) => ({
    messages: [message("Assistant", text)],
    tool_calls: [],
    finish_reason: "Stop",
    usage: { input_tokens: 100, output_tokens: 8 },
});
test("Pi compact uses the bound model and retains the latest user turn", async () => {
    const messages = Array.from({ length: 24 }, (_, i) =>
        message(i % 2 ? "Assistant" : "User", "file /tmp/example\n".repeat(3000)),
    );
    messages.push(message("User", "keep this current input"));
    let samples = 0;
    const result = await preparation(
        {
            event: {
                event: "before_context",
                input: { request, token_estimate: 510000 },
                output: { messages, user_message_replacements: [] },
            },
        },
        {
            signal: new AbortController().signal,
            hostCall: async (method, { request: r }) => {
                assert.equal(method, "host.model.complete");
                assert.equal(r.tools.length, 0);
                assert.ok(
                    r.messages.some((m) =>
                        m.parts.some((p) => p.payload.Text?.text.includes("file /tmp/example")),
                    ),
                );
                assert.ok(r.instructions[0].text.length > 10);
                samples++;
                return reply("Earlier work summary.");
            },
        },
    );
    assert.ok(samples >= 1);
    assert.equal(result.result.action, "context_preparation");
    assert.ok(result.result.output.messages.length < messages.length);
    assert.equal(result.result.output.messages.at(-1).id, messages.at(-1).id);
    assert.match(result.result.output.messages[0].parts[0].payload.Text.text, /Earlier work summary/);
});
test("upstream loop dispatches the model call once through the bound registry and checkpoints before it", async () => {
    const history = [message("User", "read the file")],
        trace = [];
    const call = { id: "r1", name: "read", args: { path: "note" }, surface: "function", raw_arguments: null };
    const tool = {
        name: "read",
        description: "Read a file",
        input_schema: { type: "object", properties: { path: { type: "string" } }, required: ["path"] },
    };
    let steps = 0,
        tracked = [],
        sampled;
    const result = await workflow(
        {
            task: { cwd: "/work", text: "read the file" },
            history,
            runtime: { instructions: [], model_ref: request.model, reasoning: request.reasoning },
        },
        {
            signal: new AbortController().signal,
            async hostCall(method, p) {
                trace.push(method);
                if (method === "host.context.build") return { chunks: [] };
                if (method === "host.tools.select") return { tools: [tool] };
                if (method === "host.context.prepare")
                    return { output: { messages: p.input.messages, changed: false }, target: null };
                if (method === "host.model.stream.start") {
                    assert.match(p.request.instructions[0].text, /coding assistant running inside pi/);
                    sampled =
                        ++steps === 1
                            ? {
                                  ...reply(""),
                                  messages: [canonical("Assistant", { ToolCall: { call } }, "model")],
                                  tool_calls: [call],
                                  finish_reason: "ToolCalls",
                              }
                            : reply("done");
                    if (steps === 2)
                        assert.equal(
                            p.request.messages.at(-1).parts[0].payload.ToolResult.result.output,
                            "test result",
                        );
                    return { stream_id: "stream" };
                }
                if (method === "host.model.stream.next") {
                    return { type: "response", response: sampled };
                }
                if (method === "host.history.checkpoint") {
                    tracked = p.tool_results;
                    return { revision: null };
                }
                if (method === "host.tools.execute") {
                    assert.equal(tracked[0].execution_call.id, p.call.id);
                    return {
                        call_id: p.call.id,
                        ok: true,
                        output: "test result",
                        error: null,
                        content: [],
                        metadata: null,
                    };
                }
                if (method === "host.events.emit") return {};
                throw Error(method);
            },
        },
    );
    assert.equal(result.status, "success", JSON.stringify(result.failure));
    assert.equal(result.result.output.text, "done");
    assert.equal(steps, 2);
    assert.equal(trace.filter((m) => m === "host.tools.execute").length, 1);
});
