import { createInterface } from "node:readline";
import { Console } from "node:console";

function object(value, required, optional = []) {
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Expected an object");
    if (required.some(key => !(key in value)) || Object.keys(value).some(key => !required.includes(key) && !optional.includes(key))) {
        throw new Error("Unexpected or missing protocol fields");
    }
    return value;
}

export async function runComponent(factory) {
    globalThis.console = new Console({ stdout: process.stderr, stderr: process.stderr });
    const send = frame => process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", ...frame })}\n`);
    const failure = (id, error, code = -32000) => send({ id, error: { code, message: String(error.message ?? error), ...(error.data === undefined ? {} : { data: error.data }) } });
    const exports = new Map();
    const active = new Map();
    const pending = new Map();
    const canceledCallbacks = new Set();
    let generation;
    let sequence = 0;
    let ready = false;
    const hostId = id => {
        if (typeof id !== "string" || !new RegExp(`^h:${generation}:[1-9]\\d*$`).test(id)) throw new Error("Invalid invocation id");
    };
    const keyOf = ref => `${ref.slot}/${ref.module_id}`;
    function hostCall(invocationId, signal, method, params) {
        signal.throwIfAborted();
        const id = `m:${generation}:${++sequence}`;
        return new Promise((resolve, reject) => {
            const canceled = () => { pending.delete(id); canceledCallbacks.add(id); reject(signal.reason); };
            signal.addEventListener("abort", canceled, { once: true });
            pending.set(id, { resolve, reject, cleanup: () => signal.removeEventListener("abort", canceled) });
            send({ id, method, params: { invocation_id: invocationId, params } });
        });
    }
    async function initialize(request) {
        const params = object(request.params, ["protocol_version", "component_id", "exports"], ["_meta"]);
        if (params.protocol_version !== "v3") throw new Error("Only component protocol v3 is supported");
        generation = /^h:(0|[1-9]\d*):0$/.exec(request.id)?.[1];
        if (generation === undefined || typeof params.component_id !== "string" || !params.component_id.trim()) throw new Error("Invalid component identity");
        if (!Array.isArray(params.exports) || !params.exports.length) throw new Error("Explicit exports are required");
        for (const binding of params.exports) {
            object(binding, ["slot", "module_id", "contract_version", "composition", "module_config", "host_features"]);
            if (!Array.isArray(binding.host_features) || new Set(binding.host_features).size !== binding.host_features.length) throw new Error("Invalid host features");
            const key = keyOf(binding);
            if (exports.has(key)) throw new Error("Duplicate export");
            const handler = await factory(binding);
            if (keyOf(handler.manifest) !== key || handler.manifest.contract_version !== binding.contract_version || handler.manifest.composition !== binding.composition) throw new Error("Export manifest mismatch");
            exports.set(key, handler);
        }
        ready = true;
        send({ id: request.id, result: { protocol_version: "v3", component_id: params.component_id, exports: [...exports.values()].map(handler => handler.manifest) } });
    }
    function invoke(request) {
        hostId(request.id);
        if (active.has(request.id)) throw new Error("Duplicate active invocation");
        const params = object(request.params, ["export", "lineage", "params"]);
        object(params.export, ["slot", "module_id"]);
        const handler = exports.get(keyOf(params.export));
        if (!handler) throw new Error("Unconfigured export");
        const lineage = object(params.lineage, ["root_invocation_id", "parent_invocation_id", "depth"]);
        hostId(lineage.root_invocation_id);
        if (!Number.isSafeInteger(lineage.depth) || lineage.depth < 0) throw new Error("Invalid lineage depth");
        if (lineage.depth === 0) {
            if (lineage.root_invocation_id !== request.id || lineage.parent_invocation_id !== null) throw new Error("Invalid root lineage");
        } else hostId(lineage.parent_invocation_id);
        const controller = new AbortController();
        active.set(request.id, controller);
        const context = { signal: controller.signal, invocationId: request.id,
            hostCall: (method, args) => hostCall(request.id, controller.signal, method, args) };
        Promise.resolve().then(() => handler.invoke(request.method, params.params, context)).then(
            result => send({ id: request.id, result }),
            error => failure(request.id, error, controller.signal.aborted ? -32800 : -32000),
        ).finally(() => active.delete(request.id));
    }
    const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
    for await (const line of lines) {
        let request;
        try {
            request = JSON.parse(line);
            if (request.jsonrpc !== "2.0") throw new Error("jsonrpc must be 2.0");
            if (ready && ("result" in request || "error" in request)) {
                object(request, ["jsonrpc", "id", "result" in request ? "result" : "error"]);
                if (canceledCallbacks.delete(request.id)) continue;
                const waiter = pending.get(request.id);
                if (!waiter) throw new Error("Unmatched callback response");
                pending.delete(request.id);
                waiter.cleanup();
                if (request.error) {
                    const error = new Error(request.error.message);
                    error.data = request.error.data;
                    waiter.reject(error);
                } else waiter.resolve(request.result);
                continue;
            }
            if (ready && request.method === "$/cancelRequest") {
                object(request.params, ["invocation_id", "cause"]);
                hostId(request.params.invocation_id);
                if (!["user", "timeout", "shutdown"].includes(request.params.cause)) throw new Error("Invalid cancel cause");
                active.get(request.params.invocation_id)?.abort(new Error(`Invocation canceled: ${request.params.cause}`));
                continue;
            }
            object(request, ["jsonrpc", "id", "method", "params"]);
            if (!ready) {
                if (request.method !== "initialize") throw new Error("First request must be initialize");
                await initialize(request);
            } else invoke(request);
        } catch (error) {
            if (typeof request?.id === "string") failure(request.id, error, -32602);
            else console.error(error);
            if (!ready || typeof request?.id !== "string") { process.exitCode = 1; break; }
        }
    }
    for (const controller of active.values()) controller.abort(new Error("Component stdin closed"));
}
