import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { existsSync } from 'node:fs';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fixture, component, appServer, nativeCatalog, flash, second, core, node, worker, moduleBin } from './model-fixture.mjs';

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

test('one native worker keeps Go and Zen connections independent under concrete export IDs', async () => {
    const f = await fixture(), source = nativeCatalog();
    source.opencode = { ...source['opencode-go'], id: 'opencode', name: 'Synthetic Zen',
        models: { 'zen-only': { ...source['opencode-go'].models[flash], id: 'zen-only', name: 'Zen only' } } };
    await writeFile(f.catalog, JSON.stringify(source));
    const authBefore = await readFile(f.database);
    const host = component(f, f.config, { 'work-go': f.config,
        'personal-zen': { ...f.config, provider: 'opencode' } });
    try {
        const initialized = await host.initialize();
        assert.equal(initialized.error, undefined);
        const exports = initialized.result.exports;
        assert.deepEqual(exports.map(exported => exported.module_id), ['work-go', 'personal-zen']);
        assert.deepEqual(exports[0].module_features, ['model.catalog.v1', 'model.quota.v1']);
        assert.deepEqual(exports[1].module_features, ['model.catalog.v1']);
        const [go, zen] = await Promise.all([
            host.invoke('catalog', null, 'work-go').result,
            host.invoke('catalog', null, 'personal-zen').result,
        ]);
        assert.deepEqual(go.result.models.map(model => model.id).sort(), [flash, second].sort());
        assert.deepEqual(zen.result.models.map(model => model.id), ['zen-only']);
        assert.match((await host.invoke('describe', { model: { model: 'zen-only' } }, 'work-go').result).error.message, /unavailable/);
        assert.match((await host.invoke('describe', { model: { model: flash } }, 'personal-zen').result).error.message, /unavailable/);
        assert.deepEqual(await readFile(f.database), authBefore);
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

test('ready Core acknowledges native overflow compaction before resampling and preserves one tool effect through cold resume',
    { timeout: 45000, skip: readyCore }, async () => {
    const f = await fixture(), requests = [];
    const summary = '## Objective\nFinish the current task.\n\n## Work State\n### Completed\nEarlier inspection.\n\n## Next Move\nContinue from the retained tool result.';
    const callID = 'append-once';
    function stream(response, content, call) {
        response.writeHead(200, { 'Content-Type': 'text/event-stream' });
        const chunk = (delta, reason = null, usage) => response.write('data: ' + JSON.stringify({
            id: 'synthetic', model: flash, object: 'chat.completion.chunk',
            choices: [{ index: 0, delta, finish_reason: reason }], ...(usage ? { usage } : {}),
        }) + '\n\n');
        chunk({ role: 'assistant' });
        if (call) chunk({ tool_calls: [{ index: 0, id: callID, type: 'function', function: {
            name: 'shell', arguments: JSON.stringify({ command: 'printf x >> counter', description: 'Append once' }),
        } }] });
        else chunk({ content });
        chunk({}, call ? 'tool_calls' : 'stop', { prompt_tokens: 100, completion_tokens: 10,
            prompt_tokens_details: { cached_tokens: 0 }, completion_tokens_details: { reasoning_tokens: 0 } });
        response.end('data: [DONE]\n\n');
    }
    const remote = createServer(async (request, response) => {
        let body = ''; for await (const chunk of request) body += chunk;
        requests.push(JSON.parse(body));
        if (requests.length === 3) {
            response.writeHead(400, { 'Content-Type': 'application/json' });
            response.end(JSON.stringify({ error: { message: 'maximum context length exceeded', type: 'invalid_request_error', code: 'context_length_exceeded' } }));
        } else stream(response, requests.length === 1 ? 'Earlier inspection complete.' :
            requests.length === 4 ? summary : requests.length === 5 ? 'finished after overflow' : 'cold resumed', requests.length === 2);
    });
    await new Promise(resolve => remote.listen(0, '127.0.0.1', resolve));
    await writeFile(f.catalog, JSON.stringify(nativeCatalog(`http://127.0.0.1:${remote.address().port}/v1`)));
    const runtime = { command: node, args: [worker], env_allowlist: Object.keys(f.env), exports: {
        workflow: { opencode_loop: {} }, context: { opencode_context: {} },
        hook: { opencode_prompt: {}, opencode_compact: {} }, tool: { opencode_tools: {} },
    } };
    const profile = { modules: { workflow: 'opencode_loop', context: 'opencode_context', policy: 'opencode_policy',
        hooks: ['opencode_prompt', 'opencode_compact'] },
        components: { runtime, policy: { command: moduleBin('proteus-policy-pack-module'), exports: { policy: { opencode_policy: {} } } } },
        module_config: { hook: { opencode_compact: { keep: 0 } }, policy: { opencode_policy: { rules: [{ permission: '*', action: 'allow' }] } } },
        tools: { enabled: ['shell'] }, permissions: { mode: 'normal' } };
    let server, session;
    try {
        server = await appServer(f, 'overflow', profile);
        session = (await server.request('/new-session', {})).output.session_dir;
        const warmup = await server.request('/send', { session_dir: session,
            text: 'Original task: ' + 'Earlier project context. '.repeat(3000) });
        assert.equal(warmup.error, null, JSON.stringify(warmup));
        const completed = await server.request('/send', { session_dir: session, text: 'Append once, then finish.' });
        assert.equal(completed.error, null, JSON.stringify(completed));
        assert.equal(await readFile(join(f.workspace, 'counter'), 'utf8'), 'x');
        assert.equal(requests.length, 5);
        assert.equal(requests[3].tools?.length ?? 0, 0, 'summary cannot execute tools');
        assert.match(JSON.stringify(requests[4].messages), /conversation-checkpoint/);
        assert.match(JSON.stringify(requests[4].messages), /printf x >> counter/);
        assert.match(JSON.stringify(requests[4].messages), /Tool result/);
        const journal = (await readFile(join(session, 'journal.jsonl'), 'utf8')).trim().split('\n').map(JSON.parse);
        const settled = journal.filter(row => row.kind === 'turn_settled');
        assert.equal(settled.at(-1).payload.value.status, 'success');
        const results = journal.filter(row => row.kind === 'tool_result_recorded'
            && row.payload.value.result.call_id === callID);
        assert.equal(results.length, 1);
        const checkpointIndex = journal.findIndex(row => row.kind === 'history_mutated'
            && row.payload.value.mutation === 'checkpoint'
            && row.payload.value.compaction?.reason === 'context_window_exceeded');
        assert.ok(checkpointIndex >= 0, 'overflow replacement must be durable in the canonical journal');
        const nextSampling = journal.findIndex((row, index) => index > checkpointIndex
            && row.kind === 'model_request_recorded');
        assert.ok(nextSampling > checkpointIndex, 'the authoritative replacement precedes the next request');
        assert.deepEqual(await server.close(), { code: 0, signal: null }); server = undefined;
        server = await appServer(f, 'overflow-restart', profile);
        assert.equal((await server.request('/resume', { session_dir: session })).error, null);
        const resumed = await server.request('/send', { session_dir: session, text: 'Report the retained state without tools.' });
        assert.equal(resumed.error, null, JSON.stringify(resumed));
        assert.equal(await readFile(join(f.workspace, 'counter'), 'utf8'), 'x');
        assert.equal(requests.length, 6);
        assert.match(JSON.stringify(requests[5].messages), /printf x >> counter/);
    } finally {
        try { if (server) assert.deepEqual(await server.close(), { code: 0, signal: null }); }
        finally { remote.closeAllConnections(); await new Promise(resolve => remote.close(resolve)); }
    }
});
