import { test } from "node:test";
import assert from "node:assert/strict";
import { modelExport, modelFailure } from "../../../model/pi-provider/adapter.mjs";
import { ModelRuntime } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/model-runtime.js";
import { streamSimple } from "../node_modules/@earendil-works/pi-ai/dist/api/openai-completions.js";
import { resolveModel } from "../../../workflow/pi-runtime/upstream.mjs";

const model = resolveModel({ model: "deepseek-v4.1-flash" }, "opencode-go");
const socketMessage = "terminated: other side closed (UND_ERR_SOCKET)";
const request = {
    model: { provider: "pi_codex", model: model.id },
    messages: [],
    tools: [],
    instructions: [],
    tool_choice: "None",
    reasoning: { effort: "high" },
    sampling: { temperature: null, top_p: null },
    limits: { max_output_tokens: null },
    cache: { routing_key: "session", cache_context: true },
};
const binding = {
    module_id: "pi_codex",
    contract_version: "v13",
    module_config: { implementation: "opencode-go" },
};
const usage = { input: 10, output: 2, cacheRead: 0, cacheWrite: 0, totalTokens: 12 };
const failed = {
    role: "assistant",
    content: [{ type: "text", text: "unaccepted" }],
    stopReason: "error",
    errorMessage: socketMessage,
    usage,
};

function runtimeWithStream(produce) {
    return { getModel: () => model, streamSimple: produce };
}

async function withRuntime(runtime, invoke) {
    const create = ModelRuntime.create;
    ModelRuntime.create = async () => runtime;
    try {
        return await invoke(modelExport(binding));
    } finally {
        ModelRuntime.create = create;
    }
}

test("real pinned Pi preserves a mid-stream socket cause and never retries inside the adapter", async () => {
    let fetches = 0;
    let sent = false;
    const encoder = new TextEncoder();
    const cause = Object.assign(new Error("other side closed"), { code: "UND_ERR_SOCKET" });
    const output = streamSimple(
        model,
        { systemPrompt: "test", messages: [] },
        {
            apiKey: "synthetic",
            reasoning: "high",
            fetch: async () => {
                fetches++;
                return new Response(
                    new ReadableStream({
                        async pull(controller) {
                            if (!sent) {
                                sent = true;
                                controller.enqueue(
                                    encoder.encode(
                                        'data: {"id":"one","choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":null}]}\n\n',
                                    ),
                                );
                                return;
                            }
                            await new Promise((resolve) => setTimeout(resolve, 5));
                            controller.error(new TypeError("terminated", { cause }));
                        },
                    }),
                    { headers: { "content-type": "text/event-stream" } },
                );
            },
        },
    );
    const events = [];
    for await (const event of output) events.push(event.type);
    const message = await output.result();
    assert.equal(fetches, 1);
    assert.ok(events.includes("start"));
    assert.ok(events.includes("text_delta"));
    assert.equal(message.stopReason, "error");
    assert.match(message.errorMessage, /UND_ERR_SOCKET/);
    assert.equal(modelFailure(null, { started: true, nativeMessage: message }).kind, "stream_disconnected");
});

test("known transport causes use typed failures and terminal causes remain terminal", () => {
    assert.deepEqual(modelFailure(new Error(socketMessage), { started: false }).kind, {
        retryable: { retry_delay_ms: null },
    });
    assert.equal(modelFailure(new Error(socketMessage), { started: true }).kind, "stream_disconnected");
    const controller = new AbortController();
    controller.abort();
    assert.equal(
        modelFailure(new Error(socketMessage), { signal: controller.signal, started: true }).kind,
        "interrupted",
    );
    assert.equal(
        modelFailure(Object.assign(new Error(socketMessage), { name: "AbortError" }), { started: true }).kind,
        "interrupted",
    );
    assert.equal(
        modelFailure(null, { nativeMessage: { ...failed, stopReason: "aborted" }, started: true }).kind,
        "interrupted",
    );
    for (const text of [
        "GoUsageLimitError: available balance exhausted (UND_ERR_SOCKET)",
        "insufficient_quota (UND_ERR_SOCKET)",
        "401 authentication rejected (UND_ERR_SOCKET)",
        "403 forbidden (UND_ERR_SOCKET)",
        "generation reached length limit",
        "unrecognized stream error",
        "ECONNRESETTING",
        "UND_ERR_SOCKET_EXTRA",
    ])
        assert.equal(modelFailure(new Error(text), { started: true }).kind, "other", text);
});

test("a native protocol EOF with no OS cause remains reconnectable", () => {
    for (const errorMessage of ["Stream ended without finish_reason", "OpenAI Responses stream ended without a stop reason"]) {
        const nativeMessage = { ...failed, errorMessage };
        assert.equal(modelFailure(null, { started: true, nativeMessage }).kind, "stream_disconnected");
    }
});

test("failed native partial text and tools cannot become accepted messages or effects", async () => {
    let produced = 0;
    const partial = {
        ...failed,
        content: [{ type: "toolCall", id: "unsafe", name: "write", arguments: { path: "state" } }],
    };
    const emitted = [];
    await withRuntime(
        runtimeWithStream(() => {
            produced++;
            return {
                async *[Symbol.asyncIterator]() {
                    yield { type: "start" };
                    yield { type: "text_delta", delta: "unfinished" };
                },
                async result() {
                    return partial;
                },
            };
        }),
        async (adapter) => {
            const result = await adapter.invoke(
                "stream",
                { request, stream: true },
                {
                    signal: new AbortController().signal,
                    hostCall: async (method, params) => {
                        emitted.push({ method, ...params });
                    },
                },
            );
            assert.equal(result.event_count, 1);
            assert.equal(result.terminal.kind, "stream_error");
            assert.equal(result.terminal.failure.kind, "stream_disconnected");
            assert.deepEqual(result.terminal.failure.completed_messages, []);
            assert.equal(emitted.length, 1);
            assert.equal(emitted[0].method, "host.model.emit");
            assert.deepEqual(Object.keys(emitted[0].event), ["TextDelta"]);
        },
    );
    assert.equal(produced, 1, "workflow owns the retry budget");
});

test("a caller reconnect allocates a fresh assistant identity and preserves accepted tool history", async () => {
    const accepted = {
        id: "accepted",
        role: "Tool",
        phase: null,
        name: "write",
        tool_call_id: "already-written",
        metadata: null,
        parts: [
            {
                part_id: "part",
                provenance: "tool",
                scope: "conversation",
                payload: {
                    ToolResult: {
                        result: {
                            call_id: "already-written",
                            ok: true,
                            output: "committed once",
                            error: null,
                            content: [],
                            metadata: null,
                        },
                    },
                },
            },
        ],
    };
    const r = { ...request, messages: [accepted] };
    const seen = [],
        emitted = [];
    let attempts = 0;
    await withRuntime(
        runtimeWithStream((_, context) => {
            seen.push(structuredClone(context));
            const error = attempts++ === 0;
            const message = error
                ? failed
                : {
                      ...failed,
                      errorMessage: undefined,
                      stopReason: "stop",
                      provider: model.provider,
                      api: model.api,
                      model: model.id,
                      timestamp: 0,
                      content: [{ type: "text", text: "done" }],
                  };
            return {
                async *[Symbol.asyncIterator]() {
                    yield { type: "start" };
                    yield { type: "text_delta", delta: error ? "unaccepted" : "done" };
                },
                async result() {
                    return message;
                },
            };
        }),
        async (adapter) => {
            const invoke = () =>
                adapter.invoke(
                    "stream",
                    { request: r, stream: true },
                    {
                        signal: new AbortController().signal,
                        hostCall: async (_, frame) => emitted.push(frame),
                    },
                );
            const first = await invoke(),
                second = await invoke();
            assert.equal(first.terminal.kind, "stream_error");
            assert.equal(second.terminal.kind, "response");
            assert.notEqual(emitted[0].event.TextDelta.message_id, emitted[1].event.TextDelta.message_id);
            assert.equal(second.terminal.response.messages[0].id, emitted[1].event.TextDelta.message_id);
            assert.deepEqual(
                seen[0],
                seen[1],
                "accepted tool results survive the caller reconnect unchanged",
            );
            assert.deepEqual(r.messages, [accepted]);
            assert.equal(seen[1].messages[0].toolCallId, "already-written");
        },
    );
});
