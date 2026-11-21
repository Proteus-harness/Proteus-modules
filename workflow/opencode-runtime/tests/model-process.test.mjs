import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { existsSync } from 'node:fs';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fixture, component, appServer, nativeCatalog, flash, second, core } from './model-fixture.mjs';

// Module-only checkouts still exercise the real v3 worker. A ready Core artifact
// additionally enables the host/UI DTO boundary; tests never build that artifact.
const readyCore = existsSync(core) ? false : 'Ready sibling portable Core binary is unavailable';

test('native OpenCode catalog and capabilities cross the real v3 worker, then worker drops cleanly', async () => {
    const f = await fixture(), host = component(f);
    try {
        const initialized = await host.initialize();
        assert.equal(initialized.error, undefined);
        assert.deepEqual(initialized.result.exports[0].module_features, ['model.catalog.v1', 'model.quota.v1']);
        const catalog = await host.invoke('catalog').result;
        assert.equal(catalog.error, undefined);
        assert.deepEqual(catalog.result.models.map(m => m.id).sort(), [flash, second].sort());
        assert.deepEqual(catalog.result.models.find(m => m.id === flash).reasoning_efforts, ['low', 'high']);
        const described = await host.invoke('describe', { model: { model: flash } }).result;
        assert.equal(described.result.adapter_id, 'opencode');
        assert.equal(described.result.capabilities.max_input_tokens, 128000);
        assert.equal(described.result.capabilities.max_output_tokens, 8192);
        assert.equal(described.result.capabilities.supports_tools, true);
        const unknown = await host.invoke('describe', { model: { model: 'unknown-model' } }).result;
        assert.match(unknown.error.message, /unavailable/);
        assert.equal((await host.invoke('catalog').result).result.models.length, 2);
    } finally { assert.deepEqual(await host.close(), { code: 0, signal: null }); }
});

test('model metadata, options, validation and persisted selection cross ready Core and native worker', { timeout: 45000, skip: readyCore }, async () => {
    const f = await fixture();
    let server = await appServer(f, 'first');
    let session;
    try {
        const bootstrap = await server.request('/bootstrap');
        session = bootstrap.session_dir;
        if (!session) session = (await server.request('/new-session', {})).output.session_dir;
        assert.ok(session);
        const query = '?session_dir=' + encodeURIComponent(session);
        const local = await server.request('/config/local' + query);
        assert.equal(local.model.name, flash);
        const discovered = await server.request('/config' + query);
        assert.equal(discovered.model_catalog_error, null);
        assert.equal(discovered.model.name, flash);
        assert.deepEqual(discovered.model_options.map(m => m.name).sort(), [flash, second].sort());
        assert.deepEqual(discovered.reasoning.effort_options, ['low', 'high']);
        assert.equal(discovered.reasoning.effort, 'high');
        const changed = await server.request('/model', { session_dir: session, model: second });
        assert.equal(changed.error, null);
        assert.equal(changed.output.config.model.name, second);
        assert.deepEqual(changed.output.config.reasoning.effort_options, ['low']);
        const rejected = await server.request('/effort', { session_dir: session, effort: 'high' });
        assert.ok(rejected.error);
        const unknown = await server.request('/model', { session_dir: session, model: 'unknown-model' });
        assert.ok(unknown.error);
        const accepted = await server.request('/effort', { session_dir: session, effort: 'low' });
        assert.equal(accepted.error, null);
        const active = await server.request('/config' + query);
        assert.equal(active.model.name, second);
        assert.equal(active.reasoning.effort, 'low');
    } finally { assert.deepEqual(await server.close(), { code: 0, signal: null }); }
    server = await appServer(f, 'restart');
    try {
        const resumed = await server.request('/resume', { session_dir: session });
        assert.equal(resumed.error, null);
        const restored = await server.request('/config?session_dir=' + encodeURIComponent(session));
        assert.equal(restored.model.name, second);
        assert.equal(restored.reasoning.effort, 'low');
        assert.equal(restored.model_options.length, 2);
    } finally { assert.deepEqual(await server.close(), { code: 0, signal: null }); }
});

test('Core quota uses the native database identity and preserves Go windows and USD budget', { timeout: 30000, skip: readyCore }, async () => {
    const f = await fixture(), requests = [];
    const remote = createServer((request, response) => {
        requests.push({ url: request.url, authorization: request.headers.authorization });
        const resetsAt = '2030-01-01T00:00:00Z';
        const meter = percent => ({ status: 'ok', percent, resetsAt });
        const body = request.url === '/zen/go/v1/usage'
            ? { usage: { rolling: meter(10), weekly: meter(25), monthly: meter(40) } }
            : [{ ruleID: 'synthetic-budget', scope: 'each', per: 'set', coverage: 'all',
                limitDollars: 20, spentDollars: 5, models: [], vendors: [], picked: true, shared: false, resetsAt }];
        response.writeHead(200, { 'Content-Type': 'application/json' }); response.end(JSON.stringify(body));
    });
    await new Promise(resolve => remote.listen(0, '127.0.0.1', resolve));
    f.preload = join(f.dir, 'quota-loopback.mjs');
    const origin = `http://127.0.0.1:${remote.address().port}`;
    // Only transport is redirected. The actual model export, SQLite credential reader,
    // shared quota implementation, process adapter and Core DTO parsing all execute.
    await writeFile(f.preload, `const original = globalThis.fetch;
globalThis.fetch = (input, options) => {
    const url = new URL(input);
    if (url.origin !== 'https://opencode.ai' || !['/zen/go/v1/usage', '/console/api/policies/current-budgets'].includes(url.pathname))
        throw new Error('Unexpected synthetic fixture request');
    return original(${JSON.stringify(origin)} + url.pathname, options);
};\n`);
    let server;
    try {
        server = await appServer(f, 'quota');
        let session = (await server.request('/bootstrap')).session_dir;
        if (!session) session = (await server.request('/new-session', {})).output.session_dir;
        const path = '/model/quota?session_dir=' + encodeURIComponent(session);
        const snapshot = await server.request(path);
        assert.equal(snapshot.plan, 'OpenCode Go');
        assert.deepEqual(snapshot.buckets[0].windows.map(w => w.used_percent), [10, 25, 40]);
        assert.deepEqual(snapshot.buckets[1].windows[0].amount, { used: 5, limit: 20, unit: 'USD' });
        assert.deepEqual(snapshot.notices ?? [], []);
        assert.deepEqual(requests.map(r => r.url).sort(), ['/console/api/policies/current-budgets', '/zen/go/v1/usage']);
        assert.ok(requests.every(r => r.authorization === 'Bearer synthetic-local-only'));
        await server.request(path);
        assert.equal(requests.length, 2, 'second Core quota read shares the export cache');
    } finally {
        try { if (server) assert.deepEqual(await server.close(), { code: 0, signal: null }); }
        finally { remote.closeAllConnections(); await new Promise(resolve => remote.close(resolve)); }
    }
});

test('worker cancellation aborts native catalog HTTP and permits a fresh lookup', { timeout: 30000 }, async () => {
    const f = await fixture();
    let requests = 0, entered;
    const started = new Promise(resolve => { entered = resolve; });
    const remote = createServer((request, response) => {
        if (++requests === 1) { entered(); return; }
        response.writeHead(200, { 'Content-Type': 'application/json' }); response.end(JSON.stringify(nativeCatalog()));
    });
    await new Promise(resolve => remote.listen(0, '127.0.0.1', resolve));
    const host = component(f, { provider: 'opencode-go', database_path: f.database,
        models_url: `http://127.0.0.1:${remote.address().port}/models.json` });
    try {
        assert.equal((await host.initialize()).error, undefined);
        const pending = host.invoke('catalog');
        await Promise.race([started, pending.result.then(frame => { throw new Error('Catalog completed before cancellation: ' + JSON.stringify(frame)); })]);
        host.cancel(pending.id);
        const canceled = await pending.result;
        assert.equal(canceled.error.code, -32800);
        const retry = await host.invoke('catalog').result;
        assert.equal(retry.error, undefined);
        assert.equal(retry.result.models.length, 2);
        assert.equal(requests, 2);
    } finally {
        await host.close(); remote.closeAllConnections(); await new Promise(resolve => remote.close(resolve));
    }
});
