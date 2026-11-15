import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { Effect } from '../../../workflow/opencode-runtime/node_modules/effect/dist/index.js';
import { LayerNode } from '../../../workflow/opencode-runtime/node_modules/@opencode/util/dist/effect/layer-node.js';
import * as ModelsDev from '../../../workflow/opencode-runtime/node_modules/@opencode/core/dist/models-dev.js';
import { createNativeRuntime } from '../runtime.mjs';
import { modelExport, modelFailure } from '../adapter.mjs';
const snapshots = await Effect.runPromise(Effect.scoped(Effect.gen(function* () {
    return yield* (yield* ModelsDev.Service).get();
}).pipe(Effect.provide(LayerNode.compile(ModelsDev.configured({ fetch: false }))))));
const binding = { slot: 'model', module_id: 'opencode', contract_version: 'v13', module_config: { provider: 'opencode-go' } };
const request = () => ({ model: { provider: 'opencode', model: 'deepseek-v4.1-flash' }, reasoning: { effort: 'high' },
    instructions: [{ text: 'Exact Core instructions' }], tools: [], tool_choice: 'None', response_format: 'Text',
    limits: { max_output_tokens: 256 }, sampling: { temperature: null, top_p: null },
    cache: { cache_instructions: false, cache_context: false, routing_key: null },
    messages: [{ id: 'user', role: 'User', parts: [{ payload: { Text: { text: 'hi' } } }] }],
});
async function fixture(t, respond) {
    const received = [];
    const server = createServer(async (req, res) => {
        let body = ''; for await (const chunk of req) body += chunk;
        received.push(JSON.parse(body)); respond(res);
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    t.after(() => new Promise(resolve => { server.closeAllConnections(); server.close(resolve); }));
    const source = snapshots.find(s => s.info.id === 'opencode-go');
    const selected = { ...source, info: { ...source.info, settings: { ...source.info.settings,
        baseURL: `http://127.0.0.1:${server.address().port}/v1` } } };
    const runtime = createNativeRuntime(binding.module_config, { loadSnapshots: async () => [selected] });
    const adapter = modelExport(binding, { runtime, credential: async () => ({ type: 'key', key: 'synthetic-key' }) });
    return { adapter, received };
}
function chunk(res, delta, finish_reason = null, usage) {
    res.write('data: ' + JSON.stringify({ id: 'synthetic', model: 'deepseek-v4.1-flash', object: 'chat.completion.chunk',
        choices: [{ index: 0, delta, finish_reason }], ...(usage ? { usage } : {}) }) + '\n\n');
}
function success(res) {
    res.writeHead(200, { 'Content-Type': 'text/event-stream' });
    chunk(res, { role: 'assistant', reasoning_content: 'think' });
    chunk(res, { content: 'answer' });
    chunk(res, {}, 'stop', { prompt_tokens: 10, completion_tokens: 7, completion_tokens_details: { reasoning_tokens: 2 }, prompt_tokens_details: { cached_tokens: 3 } });
    res.end('data: [DONE]\n\n');
}

test('actual OpenCode resolver and native LLM client preserve model, reasoning, usage and acked streaming', async t => {
    const { adapter, received } = await fixture(t, success), progress = [];
    const result = await adapter.invoke('stream', { request: request(), stream: true }, { signal: new AbortController().signal,
        hostCall: async (method, event) => { assert.equal(method, 'host.model.emit'); progress.push(event); } });
    assert.equal(result.terminal.kind, 'response');
    assert.equal(result.terminal.response.usage.output_tokens, 7);
    assert.equal(result.terminal.response.usage.reasoning_output_tokens, 2);
    assert.equal(result.terminal.response.usage.cached_input_tokens, 3);
    assert.ok(progress.some(row => row.event.ReasoningDelta));
    assert.ok(progress.some(row => row.event.TextDelta));
    assert.equal(result.event_count, progress.length);
    assert.deepEqual(progress.map(row => row.sequence), progress.map((_, index) => index + 1));
    assert.equal(received.length, 1);
    assert.equal(received[0].model, 'deepseek-v4.1-flash');
    assert.equal(received[0].reasoning_effort, 'high');
    assert.equal(received[0].max_tokens ?? received[0].max_completion_tokens, 256);
    assert.ok(received[0].messages.some(row => row.content === 'Exact Core instructions'));
});

test('stream false returns terminal data without progress and without extra requests', async t => {
    const { adapter, received } = await fixture(t, success);
    const result = await adapter.invoke('stream', { request: request(), stream: false }, {
        signal: new AbortController().signal, hostCall: async () => { throw Error('No progress expected'); } });
    assert.equal(result.terminal.kind, 'response'); assert.equal(result.event_count, 0); assert.equal(received.length, 1);
});

test('unset Core output limit preserves the native SDK request default without a synthetic 4096 cap', async t => {
    const { adapter, received } = await fixture(t, success);
    const input = request(); input.limits.max_output_tokens = null;
    const result = await adapter.invoke('stream', { request: input, stream: false }, {
        signal: new AbortController().signal, hostCall: async () => {} });
    assert.equal(result.terminal.kind, 'response');
    assert.equal(received[0].max_tokens, undefined);
    assert.equal(received[0].max_completion_tokens, undefined);
});

test('native HTTP classifications preserve terminal auth and context failures', async t => {
    for (const [status, message, expected] of [[401, 'invalid API key', 'other'], [400, 'maximum context length exceeded', 'context_window_exceeded'], [429, 'overloaded', 'retryable']]) {
        const { adapter } = await fixture(t, res => {
            res.writeHead(status, { 'Content-Type': 'application/json' }); res.end(JSON.stringify({ error: { message } }));
        });
        const result = await adapter.invoke('stream', { request: request(), stream: true }, {
            signal: new AbortController().signal, hostCall: async () => {} });
        assert.equal(result.terminal.kind, 'request_error');
        const kind = result.terminal.failure.kind;
        assert.equal(typeof kind === 'object' ? Object.keys(kind)[0] : kind, expected, JSON.stringify(result));
        assert.ok(!JSON.stringify(result).includes('synthetic-key'));
    }
});

test('EOF after visible text is a disconnected failure, not success', async t => {
    const { adapter } = await fixture(t, res => {
        res.writeHead(200, { 'Content-Type': 'text/event-stream' }); chunk(res, { content: 'partial' }); res.end();
    });
    const result = await adapter.invoke('stream', { request: request(), stream: true }, {
        signal: new AbortController().signal, hostCall: async () => {} });
    assert.equal(result.terminal.kind, 'stream_error');
    assert.equal(result.terminal.failure.kind, 'stream_disconnected');
});

test('failure diagnostics omit native credential-bearing messages and validate config', () => {
    assert.equal(modelFailure({ reason: { _tag: 'Authentication', message: 'secret' } }).kind, 'other');
    assert.ok(!JSON.stringify(modelFailure({ message: 'secret' })).includes('secret'));
    assert.throws(() => modelExport({ ...binding, module_config: { provider: 'opencode-go', unknown: true } }), /Invalid/);
});

test('only a native completed message survives a subsequent stream failure', async () => {
    const runtime = createNativeRuntime(binding.module_config, { loadSnapshots: async () => snapshots });
    const adapter = modelExport(binding, { runtime, credential: async () => ({ type: 'key', key: 'synthetic-key' }),
        stream: async (_request, _signal, accept) => {
            await accept({ type: 'text-start', id: 'one' });
            await accept({ type: 'text-delta', id: 'one', text: 'complete' });
            await accept({ type: 'text-end', id: 'one' });
            await accept({ type: 'finish', reason: { normalized: 'stop' } });
            throw { reason: { _tag: 'Transport' } };
        } });
    const result = await adapter.invoke('stream', { request: request(), stream: false }, {
        signal: new AbortController().signal, hostCall: async () => {} });
    assert.equal(result.terminal.kind, 'stream_error');
    assert.equal(result.terminal.failure.kind, 'stream_disconnected');
    assert.equal(result.terminal.failure.completed_messages.length, 1);
    assert.equal(result.terminal.failure.completed_messages[0].parts[0].payload.Text.text, 'complete');
});

test('cancellation reports interrupted without retrying or keeping unfinished content', async () => {
    const runtime = createNativeRuntime(binding.module_config, { loadSnapshots: async () => snapshots });
    const controller = new AbortController(); let calls = 0;
    const adapter = modelExport(binding, { runtime, credential: async () => ({ type: 'key', key: 'synthetic-key' }),
        stream: async (_request, signal, accept) => {
            calls++;
            await accept({ type: 'text-delta', id: 'one', text: 'unfinished' });
            controller.abort(); signal.throwIfAborted();
        } });
    const result = await adapter.invoke('stream', { request: request(), stream: true }, {
        signal: controller.signal, hostCall: async () => {} });
    assert.equal(result.terminal.kind, 'stream_error'); assert.equal(result.terminal.failure.kind, 'interrupted');
    assert.deepEqual(result.terminal.failure.completed_messages, []); assert.equal(calls, 1);
});
