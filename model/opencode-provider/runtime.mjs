import { Effect, Layer } from '../../workflow/opencode-runtime/node_modules/effect/dist/index.js';
import { LayerNode } from '../../workflow/opencode-runtime/node_modules/@opencode/util/dist/effect/layer-node.js';
import { FSUtil } from '../../workflow/opencode-runtime/node_modules/@opencode/util/dist/fs-util.js';
import * as ModelsDev from '../../workflow/opencode-runtime/node_modules/@opencode/core/dist/models-dev.js';
import * as KV from '../../workflow/opencode-runtime/node_modules/@opencode/core/dist/kv.js';
import * as Bus from '../../workflow/opencode-runtime/node_modules/@opencode/core/dist/bus.js';
import { mergeOverlay } from '../../workflow/opencode-runtime/node_modules/@opencode/core/dist/provider.js';
import { hasPackageCatalogSeed, readPackageCatalogSeed } from './catalog-seed.mjs';

let nativeResolveModel, loadingNativeResolveModel;
function loadNativeResolveModel() {
    return loadingNativeResolveModel ??= import('../../workflow/opencode-runtime/node_modules/@opencode/core/dist/model-resolver.js')
        .then(({ resolveModel }) => {
            if (typeof resolveModel !== 'function') throw new Error('Invalid OpenCode model resolver');
            return nativeResolveModel = resolveModel;
        })
        .finally(() => { loadingNativeResolveModel = undefined; });
}

const CACHE_MS = 300_000;
// Exact @opencode/core 2.0.22 key for the default public source.
const NATIVE_CATALOG_KEY = 'models-dev:catalog';
// ModelsDev's native KV and event bus both acquire the complete session database.
// A metadata lookup owns only transient catalog state and has no external subscribers.
export function catalogStateReplacements(onNativeCatalogWrite) {
    const kv = Layer.sync(KV.Service, () => {
        const values = new Map();
        return KV.Service.of({
            get: key => Effect.sync(() => values.get(key)),
            set: (key, value) => Effect.sync(() => {
                values.set(key, value);
                if (onNativeCatalogWrite && key === NATIVE_CATALOG_KEY) onNativeCatalogWrite();
            }),
            remove: key => Effect.sync(() => { values.delete(key); }),
            scan: options => Effect.sync(() => {
                const limit = Number.isNaN(options.limit) ? 100 : Math.min(Math.max(Math.floor(options.limit ?? 100), 1), 1000);
                const rows = [...values].filter(([key]) => key.startsWith(options.prefix) &&
                    (options.after === undefined || Buffer.compare(Buffer.from(key), Buffer.from(options.after)) > 0))
                    .sort(([a], [b]) => Buffer.compare(Buffer.from(a), Buffer.from(b)));
                const entries = rows.slice(0, limit).map(([key, value]) => ({ key, value }));
                return rows.length <= limit ? { entries } : { entries, next: entries.at(-1).key };
            }),
        });
    });
    const bus = Layer.succeed(Bus.Service, Bus.Service.of({
        publish: definition => definition === ModelsDev.Event.Refreshed ? Effect.void :
            Effect.die(new Error('Unexpected event in OpenCode catalog scope')),
    }));
    return [KV.node.replace(kv), Bus.node.replace(bus)];
}
// A single provider snapshot is retained, within a logical metadata budget.
// Oversized catalogs still work but are not retained; this is not a heap/RSS estimate.
function cacheable(value) {
    let bytes = 0, nodes = 0;
    const seen = new WeakSet();
    function visit(item, depth) {
        if (++nodes > 65_536 || depth > 64) return false;
        bytes += typeof item === 'string' ? Buffer.byteLength(item) : 8;
        if (bytes > 4 * 1024 * 1024) return false;
        if (item === null || typeof item !== 'object' || seen.has(item)) return true;
        seen.add(item);
        for (const key in item) if (Object.hasOwn(item, key)) {
            bytes += Buffer.byteLength(key);
            if (!visit(item[key], depth + 1)) return false;
        }
        return true;
    }
    return visit(value, 0);
}
// This export implements canonical chat; the full native metadata remains available
// to its owner, while image/audio generators and zero-token decision rows cannot run here.
const supportsChat = model => model.capabilities.input.includes('text') &&
    model.capabilities.output.includes('text') && model.limit.context > 0 && model.limit.output > 0;
export function descriptor(model) {
    return { adapter_id: 'opencode', hosted_tools: [], capabilities: {
        supports_tools: model.capabilities.tools, supports_parallel_tool_calls: model.capabilities.tools,
        supports_freeform_tools: false, supports_streaming: true, supports_json_schema: false,
        supports_system_role: true, supports_developer_role: true, supports_cache_hints: true,
        supports_reasoning_config: model.variants.length > 0 || !!model.compatibility?.reasoningField,
        supports_compaction: false,
        supports_image_input: model.capabilities.input.includes('image'), supports_file_input: false,
        provider_hosted_tools: [], max_input_tokens: model.limit.input ?? model.limit.context, max_output_tokens: model.limit.output,
    } };
}
export function catalogEntry(model) {
    const efforts = model.variants.map(variant => variant.id);
    return { id: model.id, display_name: model.name, description: null,
        hidden: !model.enabled || model.status === 'deprecated', reasoning_efforts: efforts,
        default_reasoning_effort: efforts.includes(model.settings?.reasoningEffort) ? model.settings.reasoningEffort : null };
}
// Private path token: only the in-memory scoped FSUtil below can open it.
const seedFile = 'proteus:opencode:packaged-model-catalog';
async function loadPackageSnapshots(provider, signal) {
    const catalog = await readPackageCatalogSeed(provider, signal);
    const seedFS = FSUtil.node.replace(Layer.succeed(FSUtil.Service, FSUtil.Service.of({
        readJson(path) {
            return path === seedFile ? Effect.succeed(catalog) : Effect.die(new Error('Unexpected catalog seed path'));
        },
    })));
    return Effect.runPromise(Effect.scoped(Effect.gen(function* () {
        return yield* (yield* ModelsDev.Service).get();
    }).pipe(Effect.provide(LayerNode.compile(
        ModelsDev.configured({ file: seedFile, fetch: false, snapshot: false }),
        { replacements: [...catalogStateReplacements(), seedFS] },
    )))), { signal });
}

function snapshotReader(providerID, load, now, cacheMs, requiredCache = false, retainedSeed) {
    let cache, pending;
    async function snapshot(signal) {
        signal?.throwIfAborted();
        if (cache && now() - cache.at < cacheMs) return cache.value;
        if (!pending || pending.controller.signal.aborted) {
            const controller = new AbortController(), flight = { controller, readers: 0, done: false };
            pending = flight;
            // A later or in-flight seed normalization cannot change this live flight.
            const resident = retainedSeed?.();
            flight.promise = Promise.resolve().then(() =>
                resident ? load(controller.signal, resident) : load(controller.signal),
            ).then(snapshots => {
                const value = snapshots.find(item => item.info.id === providerID);
                if (!value) throw new Error('OpenCode provider is absent from the model catalog: ' + providerID);
                const ids = new Set();
                for (const model of value.models) {
                    if (!model.id?.trim() || ids.has(model.id)) throw new Error('Invalid OpenCode model catalog identity');
                    ids.add(model.id);
                }
                if (!controller.signal.aborted && pending === flight) {
                    const fits = cacheable(value);
                    if (requiredCache && !fits) throw new Error('OpenCode packaged model catalog exceeds the retention budget');
                    cache = fits ? { at: now(), value } : undefined;
                }
                return value;
            }).finally(() => { flight.done = true; if (pending === flight) pending = undefined; });
        }
        const flight = pending; flight.readers++;
        let abort;
        try {
            const canceled = new Promise((_, reject) => {
                abort = () => reject(signal.reason ?? new Error('Catalog lookup canceled'));
                if (signal) { signal.addEventListener('abort', abort, { once: true }); if (signal.aborted) abort(); }
            });
            return await Promise.race([flight.promise, canceled]);
        } finally {
            if (signal && abort) signal.removeEventListener('abort', abort);
            if (--flight.readers === 0 && !flight.done) flight.controller.abort(new Error('Catalog readers canceled'));
        }
    }
    snapshot.retained = () => cache?.value;
    return snapshot;
}

export function createNativeRuntime(config, { loadSnapshots, now = Date.now, cacheMs = CACHE_MS } = {}) {
    const providerID = config.provider;
    // A covered exact model always resolves from this package, regardless of
    // previous RPC order, which Worker is alive, or a later remote catalog miss.
    const seed = !loadSnapshots && hasPackageCatalogSeed(config) ?
        snapshotReader(providerID, signal => loadPackageSnapshots(providerID, signal), now, Infinity, true) : null;
    const retainedSeed = config.models_file === undefined && config.models_url === undefined ? seed?.retained : undefined;
    const load = loadSnapshots ?? ((signal, resident) => {
        let nativeCatalogWritten = false;
        const onNativeCatalogWrite = resident ? () => { nativeCatalogWritten = true; } : undefined;
        return Effect.runPromise(Effect.scoped(Effect.gen(function* () {
            const service = yield* ModelsDev.Service;
            // Own the refresh lifetime: no background fiber remains after this lookup.
            if (!config.models_file) yield* service.refresh(false);
            const snapshots = yield* service.get();
            if (!resident || nativeCatalogWritten || snapshots.length) return snapshots;
            signal.throwIfAborted();
            // No live write: reuse the same already-normalized bundled provider
            // instead of populating the SDK's global all-provider bundled cache.
            return [resident];
        }).pipe(Effect.provide(LayerNode.compile(ModelsDev.configured({
            fetch: false, ...(config.models_file ? { file: config.models_file } : {}),
            ...(config.models_url ? { url: config.models_url, snapshot: false } : {}),
            ...(resident ? { snapshot: false } : {}),
        }), { replacements: catalogStateReplacements(onNativeCatalogWrite) })))), { signal });
    });
    const snapshot = snapshotReader(providerID, load, now, cacheMs, false, retainedSeed);
    const eligible = (model, id) => model.id === id && model.providerID === providerID && supportsChat(model);
    async function modelSnapshot(id, signal) {
        if (seed && id !== undefined) {
            const local = await seed(signal);
            if (local.models.some(model => eligible(model, id))) return local;
        }
        return snapshot(signal);
    }
    async function info(id, signal) {
        let snap = seed && id !== undefined ? await seed(signal) : undefined;
        let model = snap?.models.find(model => eligible(model, id));
        if (!model) {
            snap = await snapshot(signal);
            model = snap.models.find(model => model.id === id);
            if (!model || model.providerID !== providerID) throw new Error('OpenCode model unavailable: ' + id);
            if (!supportsChat(model)) throw new Error('OpenCode model does not support canonical chat: ' + id);
        }
        return { ...model, package: model.package ?? snap.info.package,
            settings: mergeOverlay(snap.info.settings, model.settings) };
    }
    return {
        async catalog(signal) {
            const models = (await snapshot(signal)).models
                .filter(model => model.providerID === providerID && supportsChat(model)).map(catalogEntry);
            if (seed && models.length) {
                const local = new Map((await seed(signal)).models.map(model => [model.id, model]));
                for (const entry of models) {
                    const effective = local.get(entry.id);
                    // Keep live discovery, ordering, labels and visibility. Core
                    // validates effort here, so these two fields match describe/resolve.
                    if (effective && eligible(effective, entry.id)) {
                        const { reasoning_efforts, default_reasoning_effort } = catalogEntry(effective);
                        entry.reasoning_efforts = reasoning_efforts;
                        entry.default_reasoning_effort = default_reasoning_effort;
                    }
                }
            }
            return { models };
        },
        async describe(id, signal) { return descriptor(await info(id, signal)); },
        async environment(signal, id) { return (await modelSnapshot(id, signal)).environment; },
        info,
        async resolve(id, effort, credential, signal) {
            const modelInfo = await info(id, signal);
            if (effort != null && !modelInfo.variants.some(variant => variant.id === effort))
                throw new Error('Unsupported OpenCode reasoning effort: ' + effort);
            signal?.throwIfAborted();
            const resolveModel = nativeResolveModel ?? await loadNativeResolveModel();
            signal?.throwIfAborted();
            const model = await Effect.runPromise(resolveModel(modelInfo, effort ?? undefined, credential), { signal });
            return { model, modelInfo };
        },
    };
}
