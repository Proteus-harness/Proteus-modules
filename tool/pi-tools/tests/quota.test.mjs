import { test } from "node:test";
import assert from "node:assert/strict";
import { modelExport } from "../../../model/pi-provider/adapter.mjs";
import { createGoQuotaReader, goUsageBucket, budgetBuckets } from "../../../model/pi-provider/go-quota.mjs";
import { ModelRuntime } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/model-runtime.js";

const resetsAt = "2026-11-15T00:00:00.000Z";
const meter = percent => ({ status: "ok", percent, resetsAt });
const usage = { usage: { rolling: meter(10), weekly: meter(25), monthly: meter(40) } };
const budget = { ruleID: "rule", limitDollars: 20, spentDollars: 7.5,
    scope: "each", per: "set", coverage: "all", models: [], vendors: [], picked: true, shared: false, resetsAt };
const controller = () => new AbortController();
const runtime = { getAuth: async () => ({ auth: { apiKey: "synthetic-test-key" } }) };
const response = (body, status = 200) => new Response(JSON.stringify(body), { status });
async function withFetch(fetch, fn) {
    const original = globalThis.fetch;
    globalThis.fetch = fetch;
    try { return await fn(); } finally { globalThis.fetch = original; }
}

test("Go maps one shared pool with three reset windows and rejects malformed meters", () => {
    const bucket = goUsageBucket(usage);
    assert.equal(bucket.windows.length, 3);
    assert.deepEqual(bucket.windows.map(w => w.duration_seconds), [18000, 604800, null]);
    assert.equal(bucket.windows[0].resets_at, Date.parse(resetsAt) / 1000);
    const limited = structuredClone(usage); limited.usage.rolling.status = "rate-limited";
    assert.equal(goUsageBucket(limited).allowed, false);
    for (const percent of [-1, null, Infinity, "1"]) {
        const bad = structuredClone(usage); bad.usage.rolling.percent = percent;
        assert.throws(() => goUsageBucket(bad), /Invalid/);
    }
    assert.throws(() => goUsageBucket({ usage: {} }), /Invalid/);
    const bad = structuredClone(usage); bad.usage.weekly.resetsAt = "invalid";
    assert.throws(() => goUsageBucket(bad), /reset time/);
});

test("USD budgets preserve shared and independent per-model counters without influencing allowance", () => {
    const shared = budgetBuckets([{ ...budget, scope: "combined" }])[0];
    assert.match(shared.name, /Общий бюджет/);
    assert.deepEqual(shared.windows[0].amount, { used: 7.5, limit: 20, unit: "USD" });
    assert.equal(shared.informational, true);
    assert.equal(shared.allowed, null);
    const perModel = { ...budget, per: "model", coverage: "models", models: [
        { providerID: "one", modelID: "a", name: "A", spentDollars: 2 },
        { providerID: "one", modelID: "b", name: "B", spentDollars: 4 },
    ] };
    assert.deepEqual(budgetBuckets([perModel]).map(b => b.windows[0].amount.used), [2, 4]);
    delete perModel.models[0].spentDollars;
    assert.throws(() => budgetBuckets([perModel]), /omitted/);
    assert.throws(() => budgetBuckets([budget, budget]), /Invalid/);
    assert.throws(() => budgetBuckets([{ ...budget, spentDollars: -1 }]), /Invalid/);
});

test("actual model export advertises Go quota and processes null params without model lookup", async () => {
    const original = ModelRuntime.create;
    ModelRuntime.create = async () => runtime;
    try {
        await withFetch(async url => url.includes("/usage") ? response(usage) : response([budget]), async () => {
            const provider = modelExport({ module_id: "pi_codex", contract_version: "v13", module_config: { implementation: "opencode-go" } });
            assert.deepEqual(provider.manifest.module_features, ["model.quota.v1"]);
            const snapshot = await provider.invoke("quota", null, { signal: controller().signal });
            assert.equal(snapshot.buckets.length, 2);
            assert.deepEqual(snapshot.notices, []);
            const other = modelExport({ module_id: "pi_codex", contract_version: "v13", module_config: { implementation: "openai-codex" } });
            assert.deepEqual(other.manifest.module_features, []);
        });
    } finally { ModelRuntime.create = original; }
});

test("budgets 403 keeps Go limits and names the missing permission without response body leaks", async () => {
    await withFetch(async url => url.includes("/usage") ? response(usage) : response({ secret: "never-publish-this-body" }, 403), async () => {
        const snapshot = await createGoQuotaReader()(runtime, controller().signal);
        assert.equal(snapshot.buckets.length, 1);
        assert.match(snapshot.notices[0], /Budgets Read/);
        assert.doesNotMatch(JSON.stringify(snapshot), /never-publish/);
    });
});

test("concurrent callers share requests, cache copies, and rotated credentials invalidate the cache", async () => {
    let requests = 0;
    await withFetch(async (url, options) => {
        requests++;
        assert.match(options.headers.Authorization, /^Bearer synthetic/);
        assert.equal(options.headers.Accept, "application/json");
        await new Promise(resolve => setTimeout(resolve, 5));
        return response(url.includes("/usage") ? usage : []);
    }, async () => {
        const read = createGoQuotaReader();
        const [first, second] = await Promise.all([read(runtime, controller().signal), read(runtime, controller().signal)]);
        assert.equal(requests, 2);
        first.buckets.length = 0;
        assert.equal(second.buckets.length, 1);
        assert.equal((await read(runtime, controller().signal)).buckets.length, 1);
        assert.equal(requests, 2);
        await read({ getAuth: async () => ({ auth: { apiKey: "synthetic-rotated" } }) }, controller().signal);
        assert.equal(requests, 4);
    });
});

test("canceling one reader preserves the shared request, canceling all aborts transport", async () => {
    let release;
    let aborted = 0;
    const gate = new Promise(resolve => { release = resolve; });
    await withFetch(async (url, { signal }) => {
        const stopped = new Promise((_, reject) => signal.addEventListener("abort", () => { aborted++; reject(signal.reason); }, { once: true }));
        await Promise.race([gate, stopped]);
        return response(url.includes("/usage") ? usage : []);
    }, async () => {
        const read = createGoQuotaReader();
        const a = controller(), b = controller();
        const first = read(runtime, a.signal), second = read(runtime, b.signal);
        const rejection = assert.rejects(first, /cancel-a/);
        await new Promise(resolve => setImmediate(resolve));
        a.abort(new Error("cancel-a")); await rejection;
        assert.equal(aborted, 0);
        release(); assert.equal((await second).buckets.length, 1);
    });
    await withFetch(async (_, { signal }) => new Promise((_, reject) => {
        signal.addEventListener("abort", () => { aborted++; reject(signal.reason); }, { once: true });
    }), async () => {
        const c = controller();const request = createGoQuotaReader()(runtime, c.signal);
        const rejection = assert.rejects(request, /cancel-all/);
        await new Promise(resolve => setImmediate(resolve));
        c.abort(new Error("cancel-all")); await rejection;
        await new Promise(resolve => setImmediate(resolve));
        assert.equal(aborted, 2);
    });
});

test("expired cache never hides a later authentication failure", async () => {
    const originalNow = Date.now;let now = originalNow();let failure = false;
    Date.now = () => now;
    try {
        await withFetch(async url => failure ? response({ sensitive: "not-shown" }, 401) : response(url.includes("/usage") ? usage : []), async () => {
            const read = createGoQuotaReader();await read(runtime, controller().signal);
            now += 31_000;failure = true;
            await assert.rejects(read(runtime, controller().signal), /HTTP 401/);
        });
    } finally { Date.now = originalNow; }
});

test("malformed optional budgets and provider transport failures remain safe and distinct", async () => {
    await withFetch(async url => response(url.includes("/usage") ? usage : { invalid: true }), async () => {
        const snapshot = await createGoQuotaReader()(runtime, controller().signal);
        assert.match(snapshot.notices[0], /бюджеты/);
        assert.equal(snapshot.buckets.length, 1);
    });
    await withFetch(async () => { throw new Error("secret-key-and-server-body"); }, async () => {
        await assert.rejects(createGoQuotaReader()(runtime, controller().signal), error => !error.message.includes("secret") && /request failed/.test(error.message));
    });
});

test("metadata deadline bounds stalled readers", async () => {
    const originalTimeout = AbortSignal.timeout;
    AbortSignal.timeout = () => originalTimeout(10);
    try {
        await withFetch(async (_, { signal }) => new Promise((_, reject) => {
            signal.addEventListener("abort", () => reject(signal.reason), { once: true });
        }), async () => {
            const keepAlive = setTimeout(() => {}, 100);
            try { await assert.rejects(createGoQuotaReader()(runtime, controller().signal), /timeout/i); }
            finally { clearTimeout(keepAlive); }
        });
    } finally { AbortSignal.timeout = originalTimeout; }
});

test("metadata response size and credential error messages are bounded and sanitized", async () => {
    await withFetch(async () => new Response("x".repeat(1_048_577)), async () => {
        await assert.rejects(createGoQuotaReader()(runtime, controller().signal), /Invalid OpenCode quota JSON/);
    });
    const broken = { getAuth: async () => { throw new Error("do-not-expose-credential-details"); } };
    await assert.rejects(createGoQuotaReader()(broken, controller().signal), error => error.message === "OpenCode Go credentials unavailable");
});

test("a stalled optional budget request preserves successful Go meters", async () => {
    const originalTimeout = AbortSignal.timeout;
    AbortSignal.timeout = milliseconds => originalTimeout(milliseconds === 5_000 ? 10 : 200);
    let budgetAborted = false;
    try {
        await withFetch(async (url, { signal }) => {
            if (url.includes("/usage")) return response(usage);
            return new Promise((_, reject) => signal.addEventListener("abort", () => {
                budgetAborted = true;
                reject(signal.reason);
            }, { once: true }));
        }, async () => {
            const keepAlive = setTimeout(() => {}, 250);
            try {
                const snapshot = await createGoQuotaReader()(runtime, controller().signal);
                assert.equal(snapshot.buckets.length, 1);
                assert.equal(snapshot.buckets[0].id, "opencode-go");
                assert.match(snapshot.notices[0], /Время ожидания денежных бюджетов/);
                assert.equal(budgetAborted, true);
            } finally { clearTimeout(keepAlive); }
        });
    } finally { AbortSignal.timeout = originalTimeout; }
});
