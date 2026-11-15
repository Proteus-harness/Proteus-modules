import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { Effect } from '../../../workflow/opencode-runtime/node_modules/effect/dist/index.js';
import { LayerNode } from '../../../workflow/opencode-runtime/node_modules/@opencode/util/dist/effect/layer-node.js';
import * as ModelsDev from '../../../workflow/opencode-runtime/node_modules/@opencode/core/dist/models-dev.js';
import * as KV from '../../../workflow/opencode-runtime/node_modules/@opencode/core/dist/kv.js';
import { catalogStateReplacements, createNativeRuntime } from '../runtime.mjs';
import { nativeCatalog, flash, second } from '../../../workflow/opencode-runtime/tests/model-fixture.mjs';

async function bundled() {
    return Effect.runPromise(Effect.scoped(Effect.gen(function* () {
        return yield* (yield* ModelsDev.Service).get();
    }).pipe(Effect.provide(LayerNode.compile(ModelsDev.configured({ fetch: false }),
        { replacements: catalogStateReplacements() })))));
}
const snapshots = await bundled();
const signal = () => new AbortController().signal;

test('catalog KV replacement preserves scans and owns its map per acquired scope', async () => {
    const layer = LayerNode.compile(KV.node, { replacements: catalogStateReplacements() });
    await Effect.runPromise(Effect.scoped(Effect.gen(function* () {
        const kv = yield* KV.Service;
        yield* kv.set('catalog:b', { version: 2 }); yield* kv.set('other:a', 3); yield* kv.set('catalog:a', 1);
        assert.deepEqual(yield* kv.get('catalog:b'), { version: 2 });
        assert.deepEqual(yield* kv.scan({ prefix: 'catalog:', limit: 1 }),
            { entries: [{ key: 'catalog:a', value: 1 }], next: 'catalog:a' });
        assert.deepEqual(yield* kv.scan({ prefix: 'catalog:', after: 'catalog:a', limit: NaN }),
            { entries: [{ key: 'catalog:b', value: { version: 2 } }] });
        yield* kv.remove('catalog:a'); assert.equal(yield* kv.get('catalog:a'), undefined);
    }).pipe(Effect.provide(layer))));
    assert.equal(await Effect.runPromise(Effect.scoped(Effect.gen(function* () {
        return yield* (yield* KV.Service).get('catalog:b');
    }).pipe(Effect.provide(layer)))), undefined);
});

test('real pinned OpenCode metadata exposes Go without config aliases or credentials', async () => {
    const runtime = createNativeRuntime({ provider: 'opencode-go' }, { loadSnapshots: async () => snapshots });
    const catalog = await runtime.catalog(signal());
    assert.ok(catalog.models.length > 32);
    const flash = catalog.models.find(item => item.id === 'deepseek-v4.1-flash');
    assert.equal(flash.display_name, 'DeepSeek V4.1 Flash');
    assert.deepEqual(flash.reasoning_efforts, ['low', 'high', 'max']);
    const description = await runtime.describe(flash.id, signal());
    assert.equal(description.adapter_id, 'opencode');
    assert.equal(description.capabilities.max_input_tokens, 1_000_000);
    assert.equal(description.capabilities.max_output_tokens, 384_000);
    assert.equal(description.capabilities.supports_freeform_tools, false);
    const info = await runtime.info(flash.id, signal());
    assert.equal(info.settings.baseURL, 'https://opencode.ai/zen/go/v1');
    assert.equal(info.package, '@opencode/ai/providers/openai-compatible');
    const resolved = await runtime.resolve(flash.id, 'high', { type: 'key', key: 'synthetic-key' }, signal());
    assert.ok(resolved.model);
    await assert.rejects(runtime.resolve(flash.id, 'xhigh', { type: 'key', key: 'synthetic-key' }, signal()), /Unsupported/);
    await assert.rejects(runtime.describe('missing', signal()), /unavailable/);
});

test('concurrent readers coalesce and refresh replaces only an expired snapshot', async () => {
    let calls = 0, clock = 0;
    const runtime = createNativeRuntime({ provider: 'opencode-go' }, {
        now: () => clock, cacheMs: 10,
        loadSnapshots: async () => { calls++; await new Promise(resolve => setTimeout(resolve, 10)); return snapshots; },
    });
    await Promise.all([runtime.catalog(signal()), runtime.catalog(signal()), runtime.describe('deepseek-v4.1-flash', signal())]);
    assert.equal(calls, 1);
    clock = 11; await runtime.catalog(signal()); assert.equal(calls, 2);
});

test('canceling one reader preserves another; canceling all aborts lookup and permits retry', async () => {
    let calls = 0, aborts = 0;
    const runtime = createNativeRuntime({ provider: 'opencode-go' }, { loadSnapshots: async abortSignal => {
        calls++;
        return new Promise((resolve, reject) => {
            const timer = setTimeout(() => resolve(snapshots), 30);
            abortSignal.addEventListener('abort', () => { aborts++; clearTimeout(timer); reject(abortSignal.reason); }, { once: true });
        });
    } });
    const first = new AbortController(), second = new AbortController();
    const canceled = runtime.catalog(first.signal), retained = runtime.catalog(second.signal);
    first.abort(); await assert.rejects(canceled); assert.ok((await retained).models.length); assert.equal(aborts, 0);
    assert.equal(calls, 1);
    const failing = createNativeRuntime({ provider: 'opencode-go' }, { loadSnapshots: async abortSignal => {
        return new Promise((resolve, reject) => abortSignal.addEventListener('abort', () => reject(abortSignal.reason), { once: true }));
    } });
    const controller = new AbortController(), request = failing.catalog(controller.signal);
    await new Promise(resolve => setImmediate(resolve)); controller.abort(); await assert.rejects(request);
});

test('catalog errors never become unsupported feature or fake fallback entries', async () => {
    const runtime = createNativeRuntime({ provider: 'opencode-go' }, { loadSnapshots: async () => { throw new Error('synthetic failure'); } });
    await assert.rejects(runtime.catalog(signal()), /synthetic failure/);
    const missing = createNativeRuntime({ provider: 'missing' }, { loadSnapshots: async () => snapshots });
    await assert.rejects(missing.catalog(signal()), /absent/);
});

test('oversized metadata is served without retaining it in the owned cache', async () => {
    const source = snapshots.find(item => item.info.id === 'opencode-go');
    const large = { ...source, info: { ...source.info, description: 'x'.repeat(4 * 1024 * 1024) } };
    let calls = 0;
    const runtime = createNativeRuntime({ provider: 'opencode-go' }, { loadSnapshots: async () => { calls++; return [large]; } });
    assert.ok((await runtime.catalog(signal())).models.length);
    assert.ok((await runtime.catalog(signal())).models.length);
    assert.equal(calls, 2);
});

test('chat catalog filters native non-chat and zero-token metadata without model ID heuristics', async () => {
    const source = snapshots.find(item => item.info.id === 'opencode-go');
    const base = source.models.find(item => item.id === 'deepseek-v4.1-flash');
    const models = [base,
        { ...base, id: 'image', capabilities: { ...base.capabilities, output: ['image'] } },
        { ...base, id: 'audio', capabilities: { ...base.capabilities, input: ['audio'] } },
        { ...base, id: 'decision', limit: { context: 0, output: 0 } },
        { ...base, id: 'other-provider', providerID: 'elsewhere' },
        { ...base, id: 'deprecated-chat', status: 'deprecated', limit: { context: 1000, input: 900, output: 100 } },
    ];
    const runtime = createNativeRuntime({ provider: 'opencode-go' }, { loadSnapshots: async () => [{ ...source, models }] });
    const catalog = await runtime.catalog(signal());
    assert.deepEqual(catalog.models.map(model => model.id), [base.id, 'deprecated-chat']);
    assert.equal(catalog.models[1].hidden, true);
    assert.equal((await runtime.describe('deprecated-chat', signal())).capabilities.max_input_tokens, 900);
    for (const id of ['image', 'audio', 'decision']) await assert.rejects(runtime.describe(id, signal()), /canonical chat/);
    await assert.rejects(runtime.describe('other-provider', signal()), /unavailable/);
});

test('actual configured ModelsDev refreshes HTTP metadata after the owned TTL and coalesces readers', async t => {
    let calls = 0, version = 1, clock = 0;
    const server = createServer((req, res) => {
        calls++; assert.equal(req.url, '/api.json');
        const catalog = nativeCatalog();
        if (version === 1) delete catalog['opencode-go'].models[second];
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify(catalog));
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    t.after(() => new Promise(resolve => { server.closeAllConnections(); server.close(resolve); }));
    const runtime = createNativeRuntime({ provider: 'opencode-go', models_url: `http://127.0.0.1:${server.address().port}` },
        { now: () => clock, cacheMs: 10 });
    const first = await Promise.all([runtime.catalog(signal()), runtime.catalog(signal())]);
    assert.deepEqual(first[0].models.map(model => model.id), [flash]);
    assert.deepEqual(first[0].models[0].reasoning_efforts, ['low', 'high']);
    assert.equal(calls, 1);
    version = 2;
    assert.equal((await runtime.catalog(signal())).models.length, 1); assert.equal(calls, 1);
    clock = 11;
    assert.deepEqual((await runtime.catalog(signal())).models.map(model => model.id), [flash, second]);
    assert.equal(calls, 2);
});

test('fresh native HTTP catalog process acquires no session database or migration bootstrap', async t => {
    let calls = 0;
    const server = createServer((_req, res) => { calls++; res.end(JSON.stringify(nativeCatalog())); });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    t.after(() => new Promise(resolve => { server.closeAllConnections(); server.close(resolve); }));
    const moduleURL = new URL('../runtime.mjs', import.meta.url).href;
    const config = { provider: 'opencode-go', models_url: `http://127.0.0.1:${server.address().port}` };
    const source = `import {createNativeRuntime} from ${JSON.stringify(moduleURL)};
        const runtime = createNativeRuntime(${JSON.stringify(config)});
        console.log(JSON.stringify(await runtime.catalog(new AbortController().signal)));`;
    const { stdout, stderr } = await promisify(execFile)(process.execPath, ['--input-type=module', '-e', source], { timeout: 10000 });
    assert.equal(calls, 1);
    assert.equal(JSON.parse(stdout).models.length, 2);
    assert.doesNotMatch(stdout + stderr, /database schema bootstrap|migrations:/i);
});
