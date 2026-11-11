import test from "node:test";
import assert from "node:assert/strict";
import { receiveModelResponse } from "../../../components/module-runtime-js/model-stream.mjs";

const request = {model: {provider:"lab", model:"flash"}, messages: [{tool_result:"append already accepted"}]};
function transport(failure, signal) {
    const attempts = [];
    const hostCall = async (method, params) => {
        if (method === "host.events.emit") return {};
        if (method === "host.model.stream.start") {
            attempts.push(structuredClone(params.request));
            return {stream_id:String(attempts.length)};
        }
        assert.equal(method, "host.model.stream.next");
        return {type:"error", failure};
    };
    return {ctx:{hostCall, signal}, attempts};
}

test("exhaustion preserves the typed failure and retries only the same sampling request", async () => {
    const failure = {kind:"stream_disconnected", message:"lost", completed_messages:[]};
    const {ctx, attempts} = transport(failure, new AbortController().signal);
    await assert.rejects(receiveModelResponse(request, ctx, (_, n) => n <= 2 ? 0 : undefined),
        error => error.modelFailure === failure);
    assert.deepEqual(attempts, [request, request, request]);
});

test("other failures bypass the reconnect schedule", async () => {
    for (const kind of ["other", "interrupted", "session_budget_exceeded", "context_window_exceeded"]) {
        const failure = {kind, message:"terminal", completed_messages:[]};
        const {ctx, attempts} = transport(failure, new AbortController().signal);
        await assert.rejects(receiveModelResponse(request, ctx, () => assert.fail("unexpected retry")),
            error => error.modelFailure === failure);
        assert.deepEqual(attempts, [request]);
    }
});

test("user cancellation interrupts backoff without a new model call", async () => {
    const controller = new AbortController();
    const failure = {kind:{retryable:{retry_delay_ms:null}}, message:"reset", completed_messages:[]};
    const {ctx, attempts} = transport(failure, controller.signal);
    await assert.rejects(receiveModelResponse(request, ctx, () => {
        setImmediate(() => controller.abort());
        return 60000;
    }), {name:"AbortError"});
    assert.deepEqual(attempts, [request]);
});
