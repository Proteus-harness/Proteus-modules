import test from "node:test";
import assert from "node:assert/strict";
import { createRetryDelay } from "../retry.mjs";

const runtime = {
    execution_id: "session",
    model_ref: { provider: "opencode-go", model: "deepseek-v4.1-flash" },
};

test("native schedule retains ten bounded reconnect decisions and caller delay advice", async () => {
    const delay = await createRetryDelay(runtime, new AbortController().signal);
    const failure = { kind: "stream_disconnected", message: "closed", completed_messages: [] };
    for (let attempt = 1; attempt <= 10; attempt++) {
        const value = await delay(failure);
        const base = Math.min(2000 * 2 ** (attempt - 1), 10000);
        assert.ok(value >= base * 0.8 && value <= base * 1.2 + 1, String(value));
    }
    assert.equal(await delay(failure), undefined);
    const advised = await createRetryDelay(runtime, new AbortController().signal);
    assert.ok((await advised({ ...failure, kind: { retryable: { retry_delay_ms: 25000 } } })) >= 25000);
});

test("terminal causes do not consume the native schedule and cancellation stops decisions", async () => {
    const controller = new AbortController();
    const delay = await createRetryDelay(runtime, controller.signal);
    for (const kind of ["other", "interrupted", "session_budget_exceeded", "context_window_exceeded"]) {
        assert.equal(await delay({ kind, message: "terminal", completed_messages: [] }), undefined);
    }
    assert.equal(
        typeof (await delay({ kind: "stream_disconnected", message: "closed", completed_messages: [] })),
        "number",
    );
    controller.abort(new Error("stop"));
    await assert.rejects(
        delay({ kind: "stream_disconnected", message: "closed", completed_messages: [] }),
        /stop/,
    );
});
