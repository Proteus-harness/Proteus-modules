// OpenCode API mapping follows packages/core/src/usage-limits.ts at
// anomalyco/opencode 1fa50dd28cc860768fd89180cf40b2fb912b2813.
import { createHash } from "node:crypto";

const USAGE_URL = "https://opencode.ai/zen/go/v1/usage";
const BUDGETS_URL = "https://opencode.ai/console/api/policies/current-budgets";
const CACHE_MS = 30_000;
const TIMEOUT_MS = 15_000;
const BUDGET_TIMEOUT_MS = 5_000;

function object(value) {
    return value !== null && typeof value === "object" && !Array.isArray(value);
}

function nonnegative(value) {
    return typeof value === "number" && Number.isFinite(value) && value >= 0;
}

function timestamp(value) {
    const ms = typeof value === "string" ? Date.parse(value) : NaN;
    if (!Number.isFinite(ms) || ms <= 0) throw new Error("Invalid OpenCode quota reset time");
    return Math.floor(ms / 1000);
}

export function goUsageBucket(body) {
    if (!object(body?.usage)) throw new Error("Invalid OpenCode Go usage response");
    let limited = false;
    const windows = [["rolling", 18_000], ["weekly", 604_800], ["monthly", null]].map(([id, duration]) => {
        const meter = body.usage[id];
        if (!object(meter) || !["ok", "rate-limited"].includes(meter.status) || !nonnegative(meter.percent))
            throw new Error("Invalid OpenCode Go usage meter");
        limited ||= meter.status === "rate-limited";
        return { id, used_percent: meter.percent, duration_seconds: duration, resets_at: timestamp(meter.resetsAt) };
    });
    return { id: "opencode-go", name: "OpenCode Go · общий лимит", allowed: !limited, limit_reached: limited, windows };
}

export function budgetBuckets(body) {
    if (!Array.isArray(body)) throw new Error("Invalid OpenCode budgets response");
    const ids = new Set();
    const buckets = [];
    for (const rule of body) {
        if (!object(rule) || typeof rule.ruleID !== "string" || !rule.ruleID.trim() || ids.has(rule.ruleID)
            || !nonnegative(rule.limitDollars) || !nonnegative(rule.spentDollars)
            || !["each", "combined"].includes(rule.scope) || !["set", "model"].includes(rule.per)
            || !["all", "models"].includes(rule.coverage) || !Array.isArray(rule.models)
            || !Array.isArray(rule.vendors) || typeof rule.shared !== "boolean" || typeof rule.picked !== "boolean")
            throw new Error("Invalid OpenCode budget rule");
        ids.add(rule.ruleID);
        const reset = timestamp(rule.resetsAt);
        const modelIds = new Set();
        for (const model of rule.models) {
            if (!object(model) || typeof model.providerID !== "string" || !model.providerID
                || typeof model.modelID !== "string" || !model.modelID || typeof model.name !== "string" || !model.name
                || modelIds.has(model.providerID + "/" + model.modelID)
                || (model.spentDollars !== undefined && !nonnegative(model.spentDollars)))
                throw new Error("Invalid OpenCode budget model");
            modelIds.add(model.providerID + "/" + model.modelID);
        }
        for (const vendor of rule.vendors) {
            if (!object(vendor) || typeof vendor.id !== "string" || !vendor.id
                || typeof vendor.name !== "string" || !vendor.name) throw new Error("Invalid OpenCode budget vendor");
        }
        if (rule.inputPriceAtLeast !== undefined && !nonnegative(rule.inputPriceAtLeast))
            throw new Error("Invalid OpenCode budget price threshold");
        const scope = rule.scope === "combined" ? "Общий бюджет" : "Личный бюджет";
        const coverage = rule.coverage === "all" ? "все платные модели и сервисы"
            : [...rule.models.map(m => m.name), ...rule.vendors.map(v => v.name)].join(", ") || "выбранные модели";
        const label = `${scope} · ${coverage}` + (rule.inputPriceAtLeast === undefined ? "" : ` · вход от $${rule.inputPriceAtLeast}/млн`);
        if (rule.per === "model") {
            // A per-model rule has independent counters. Never use the rule's
            // aggregate spend as the spend of each individual model.
            for (const model of rule.models) {
                if (model.spentDollars === undefined) throw new Error("OpenCode omitted per-model budget spend");
                buckets.push(budgetBucket(`budget:${rule.ruleID}:${model.providerID}/${model.modelID}`,
                    `${scope} · ${model.name}`, model.spentDollars, rule.limitDollars, reset));
            }
        } else buckets.push(budgetBucket(`budget:${rule.ruleID}`, label, rule.spentDollars, rule.limitDollars, reset));
    }
    return buckets;
}

function budgetBucket(id, name, used, limit, reset) {
    const percent = limit === 0 ? 100 : used / limit * 100;
    if (!Number.isFinite(percent)) throw new Error("Invalid OpenCode budget percentage");
    return { id, name, informational: true, allowed: null, limit_reached: null, windows: [{
        id: "budget", used_percent: percent,
        duration_seconds: null, resets_at: reset, amount: { used, limit, unit: "USD" },
    }] };
}

async function jsonGet(url, apiKey, signal) {
    let response;
    try {
        response = await fetch(url, { headers: { Authorization: `Bearer ${apiKey}`, Accept: "application/json" }, signal });
    } catch {
        signal.throwIfAborted();
        throw new Error("OpenCode quota request failed");
    }
    if (!response.ok) {
        await response.body?.cancel().catch(() => {});
        throw new Error(`OpenCode quota returned HTTP ${response.status}`);
    }
    // Console metadata is small. Bound a malformed response before JSON parsing.
    try {
        const reader = response.body.getReader();
        const chunks = [];
        let size = 0;
        try {
            for (;;) {
                const { done, value } = await reader.read();
                if (done) break;
                size += value.byteLength;
                if (size > 1_048_576) throw new Error("Quota response too large");
                chunks.push(value);
            }
        } finally { await reader.cancel().catch(() => {}); }
        return JSON.parse(Buffer.concat(chunks).toString("utf8"));
    } catch { signal.throwIfAborted(); throw new Error("Invalid OpenCode quota JSON"); }
}

async function load(apiKey, signal) {
    const budgetTimeout = AbortSignal.timeout(BUDGET_TIMEOUT_MS);
    const budgetSignal = AbortSignal.any([signal, budgetTimeout]);
    const budgets = jsonGet(BUDGETS_URL, apiKey, budgetSignal).then(budgetBuckets).then(
        buckets => ({ buckets, notices: [] }),
        error => {
            signal.throwIfAborted();
            if (budgetTimeout.aborted) return {
                buckets: [], notices: ["Время ожидания денежных бюджетов OpenCode истекло. Лимиты Go доступны."],
            };
            const forbidden = error.message === "OpenCode quota returned HTTP 403";
            return { buckets: [], notices: [forbidden
                ? "Бюджеты недоступны (HTTP 403). Проверьте Budgets Read и права аккаунта."
                : "Не удалось получить денежные бюджеты OpenCode. " + error.message] };
        },
    );
    const results = await Promise.all([jsonGet(USAGE_URL, apiKey, signal).then(goUsageBucket), budgets]);
    return { observed_at: Math.floor(Date.now() / 1000), plan: "OpenCode Go",
        buckets: [results[0], ...results[1].buckets], credits: null, notices: results[1].notices };
}

/** One reader belongs to one export. A changed credential invalidates its cache. */
export function createGoQuotaReader() {
    let cache;
    let pending;
    return async (runtime, signal) => {
        signal.throwIfAborted();
        let credential;
        try { credential = await runtime.getAuth("opencode-go", { signal }); }
        catch { signal.throwIfAborted(); throw new Error("OpenCode Go credentials unavailable"); }
        signal.throwIfAborted();
        const apiKey = credential?.auth?.apiKey;
        if (typeof apiKey !== "string" || !apiKey) throw new Error("OpenCode Go API key unavailable");
        const key = createHash("sha256").update(apiKey).digest("hex");
        if (cache?.key === key && Date.now() - cache.at < CACHE_MS) return structuredClone(cache.snapshot);
        if (!pending || pending.key !== key || pending.controller.signal.aborted) {
            const controller = new AbortController();
            const flight = { key, controller, readers: 0, done: false };
            pending = flight;
            flight.promise = load(apiKey, AbortSignal.any([controller.signal, AbortSignal.timeout(TIMEOUT_MS)]))
                .then(snapshot => {
                    if (pending === flight && !controller.signal.aborted) cache = { key, at: Date.now(), snapshot };
                    return snapshot;
                }).catch(error => {
                    controller.abort();
                    throw error;
                }).finally(() => {
                    flight.done = true;
                    if (pending === flight) pending = undefined;
                });
        }
        const flight = pending;
        flight.readers++;
        let abort;
        try {
            signal.throwIfAborted();
            const canceled = new Promise((_, reject) => {
                abort = () => reject(signal.reason ?? new Error("Quota request canceled"));
                signal.addEventListener("abort", abort, { once: true });
            });
            return structuredClone(await Promise.race([flight.promise, canceled]));
        } finally {
            if (abort) signal.removeEventListener("abort", abort);
            if (--flight.readers === 0 && !flight.done) flight.controller.abort(new Error("Quota readers canceled"));
        }
    };
}
