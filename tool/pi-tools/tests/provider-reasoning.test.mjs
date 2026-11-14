import { test } from "node:test";
import assert from "node:assert/strict";
import { modelExport } from "../../../model/pi-provider/adapter.mjs";
import { ModelRuntime } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/model-runtime.js";
import { streamSimple } from "../node_modules/@earendil-works/pi-ai/dist/api/openai-completions.js";
import { resolveModel } from "../../../workflow/pi-runtime/upstream.mjs";
import { piMessages } from "../../../components/pi-runtime-js/messages.mjs";

const model = resolveModel({ model: "deepseek-v4.1-flash" }, "opencode-go");
const binding = {
    module_id: "pi_codex",
    contract_version: "v13",
    module_config: { implementation: "opencode-go" },
};
const request = {
    model: { provider: "pi_codex", model: model.id },
    messages: [], tools: [], instructions: [], tool_choice: "None",
    reasoning: { effort: "high" },
    sampling: { temperature: null, top_p: null },
    limits: { max_output_tokens: null },
    cache: { routing_key: "fixture", cache_context: true },
};

async function invoke(produce, stream = true) {
    const create = ModelRuntime.create;
    ModelRuntime.create = async () => ({ getModel: () => model, streamSimple: produce });
    try {
        const frames = [];
        const result = await modelExport(binding).invoke("stream", { request, stream }, {
            signal: new AbortController().signal,
            hostCall: async (method, frame) => {
                assert.equal(method, "host.model.emit");
                frames.push(frame);
            },
        });
        return { frames, result };
    } finally {
        ModelRuntime.create = create;
    }
}

test("pinned Pi reasoning_content reaches the model stream once and survives the terminal response", async () => {
    let fetches = 0;
    const chunks = [
        { choices: [{ delta: { reasoning_content: "Проверю " }, finish_reason: null }] },
        { choices: [{ delta: { reasoning_content: "условие.", reasoning: "duplicate", content: "Ответ." }, finish_reason: null }] },
        { choices: [{ delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 8, completion_tokens: 12, total_tokens: 20 } },
    ];
    const produce = (model, context, options) => streamSimple(model, context, {
        ...options,
        apiKey: "synthetic",
        fetch: async () => {
            fetches++;
            return new Response(chunks.map(chunk => "data: " + JSON.stringify({ id: "fixture", ...chunk }) + "\n\n").join("") + "data: [DONE]\n\n", {
                headers: { "content-type": "text/event-stream" },
            });
        },
    });
    for (const streamed of [true, false]) {
        const { frames, result } = await invoke(produce, streamed);
        assert.equal(result.terminal.kind, "response");
        const response = result.terminal.response;
        assert.deepEqual(response.messages[0].parts.map(part => part.payload), [
            { Reasoning: { text: "Проверю условие.", signature: "reasoning_content" } },
            { Text: { text: "Ответ." } },
        ]);
        assert.equal(response.usage.output_tokens, 12);
        if (streamed) {
            assert.deepEqual(frames.map(frame => frame.sequence), [0, 1, 2]);
            assert.deepEqual(frames.map(frame => frame.event), [
                { ReasoningDelta: { message_id: response.messages[0].id, text: "Проверю " } },
                { ReasoningDelta: { message_id: response.messages[0].id, text: "условие." } },
                { TextDelta: { message_id: response.messages[0].id, phase: null, text: "Ответ." } },
            ]);
            assert.equal(result.event_count, 3);
        } else {
            assert.deepEqual(frames, []);
            assert.equal(result.event_count, 0);
        }
    }
    assert.equal(fetches, 2, "each invocation uses only its synthetic provider response");
});

test("redacted thinking and its opaque signature never become visible stream text", async () => {
    const message = {
        role: "assistant", provider: model.provider, api: model.api, model: model.id,
        timestamp: 0, stopReason: "stop",
        usage: { input: 8, output: 12, cacheRead: 0, cacheWrite: 0, totalTokens: 20 },
        content: [
            { type: "thinking", thinking: "[redacted]", thinkingSignature: "opaque-encrypted-payload", redacted: true },
            { type: "text", text: "Ответ." },
        ],
    };
    const { frames, result } = await invoke(() => ({
        async *[Symbol.asyncIterator]() {
            yield { type: "start" };
            yield { type: "thinking_delta", contentIndex: 0, delta: "[redacted]", partial: message };
            yield { type: "thinking_delta", contentIndex: 1, delta: "not thinking", partial: message };
            yield { type: "text_delta", delta: "Ответ." };
        },
        async result() { return message; },
    }));
    assert.equal(result.terminal.kind, "response");
    assert.equal(result.event_count, 1);
    assert.deepEqual(frames.map(frame => Object.keys(frame.event)), [["TextDelta"]]);
    assert.ok(!JSON.stringify(frames).includes("opaque-encrypted-payload"));
    const canonical = result.terminal.response.messages[0];
    assert.deepEqual(canonical.parts[0].payload, {
        Opaque: { format: "pi/thinking", data: message.content[0] },
    });
    assert.ok(canonical.parts.every(part => !part.payload.Reasoning));
    assert.deepEqual(piMessages([canonical], model)[0], message);
    canonical.parts.push({
        part_id: "foreign", provenance: "model", scope: "conversation",
        payload: { Opaque: { format: "unknown/provider", data: { type: "thinking", thinking: "foreign" } } },
    });
    assert.deepEqual(piMessages([canonical], model)[0], message, "unknown opaque data is not Pi content");
});

test("multiple readable thinking blocks stream the same text as the canonical reasoning projection", async () => {
    const message = {
        role: "assistant", provider: model.provider, api: model.api, model: model.id,
        timestamp: 0, stopReason: "stop",
        usage: { input: 8, output: 12, cacheRead: 0, cacheWrite: 0, totalTokens: 20 },
        content: [
            { type: "thinking", thinking: " \tПервое " },
            { type: "thinking", thinking: "[redacted]", thinkingSignature: "opaque", redacted: true },
            { type: "thinking", thinking: "\n\t\u0085" },
            { type: "thinking", thinking: "\nВторое." },
        ],
    };
    const { frames, result } = await invoke(() => ({
        async *[Symbol.asyncIterator]() {
            yield { type: "start" };
            for (const [contentIndex, deltas] of [[0, [" \t", "Первое", " "]], [1, ["[redacted]"]],
                [2, ["\n", "\t\u0085"]], [3, ["\n", "Второе."]]]) {
                for (const delta of deltas)
                    yield { type: "thinking_delta", contentIndex, delta, partial: message };
                yield { type: "thinking_end", contentIndex, partial: message };
            }
        },
        async result() { return message; },
    }));
    assert.equal(result.terminal.kind, "response");
    const canonical = result.terminal.response.messages[0];
    const expected = " \tПервое \n\nВторое.";
    assert.equal([canonical.parts[0].payload.Reasoning.text,
        canonical.parts[3].payload.Reasoning.text].join("\n"), expected);
    assert.equal(frames.map(frame => frame.event.ReasoningDelta.text).join(""), expected);
    assert.deepEqual(frames.map(frame => frame.sequence), [0, 1, 2]);
    assert.equal(result.event_count, 3);
    assert.ok(frames.every(frame => frame.event.ReasoningDelta.message_id === canonical.id));
});
