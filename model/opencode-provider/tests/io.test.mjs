import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Schema } from '../../../workflow/opencode-runtime/node_modules/effect/dist/index.js';
import { LanguageModel, LLMEvent, LLMRequest } from '../../../workflow/opencode-runtime/node_modules/@opencode/ai/dist/index.js';
import { toNativeRequest } from '../messages.mjs';
import { NativeResponseCollector } from '../response.mjs';

const model = LanguageModel.make({ id: 'synthetic-model', provider: 'synthetic-provider', route: {} });
const part = payload => ({ part_id: randomUUID(), provenance: 'user', scope: 'conversation', payload });
const message = (role, payloads, fields = {}) => ({ id: randomUUID(), role, phase: null,
    parts: payloads.map(part), name: null, tool_call_id: null, metadata: null, ...fields });
const text = value => ({ Text: { text: value } });
const request = fields => ({
    model: { provider: 'logical-profile', model: 'synthetic-model' },
    instructions: [{ kind: 'System', text: 'core instructions\nverbatim', priority: 0 }],
    messages: [message('User', [text('question')])], tools: [], tool_choice: 'Auto', response_format: 'Text',
    sampling: { temperature: null, top_p: null }, reasoning: { effort: 'high', summary: false, budget_tokens: null },
    limits: { max_input_tokens: null, max_output_tokens: null },
    cache: { cache_instructions: false, cache_context: false, routing_key: null },
    client_metadata: {}, metadata: null, ...fields,
});
const finish = (reason = 'stop', fields = {}) => LLMEvent.finish({ reason: { normalized: reason }, ...fields });
const call = (id, args = {}) => ({ id, name: 'edit', args, surface: 'function', raw_arguments: null });
const result = (id, fields = {}) => ({ call_id: id, ok: true, output: 'done', content: [], error: null, metadata: null, ...fields });

test('request is an actual native schema with verbatim instructions and unmodified model', async () => {
    const input = request({
        instructions: [{ kind: 'Developer', text: 'first', priority: 20 }, { kind: 'System', text: 'second', priority: 0 }],
        messages: [message('Developer', [text('chronological instruction')]), message('User', [text('A'),
            { Context: { chunk: { source: 'fixture', path: null, content: 'untrusted context',
                render_mode: 'verbatim', score: null, metadata: null } } }, text('B')])],
    });
    const output = await toNativeRequest(input, model);
    assert.ok(output instanceof LLMRequest);
    assert.ok(Schema.is(LLMRequest)(output));
    assert.equal(output.model, model);
    assert.deepEqual(output.system.map(item => item.text), ['first', 'second']);
    assert.equal(output.messages[0].role, 'system');
    assert.equal(output.messages[1].role, 'user');
    assert.deepEqual(output.messages[1].content.map(item => item.text), ['A', 'untrusted context', 'B']);
    assert.equal(output.generation, undefined);
    assert.equal(output.providerOptions, undefined);
    assert.equal(output.cache, 'none');
    assert.equal(output.toolChoice.type, 'auto');
});


test('context rendering follows canonical mode without changing request data or part order', async () => {
    const chunks = [
        { source: 'search', path: null, content: 'found', render_mode: 'source_annotated', metadata: { render_mode: 'verbatim' } },
        { source: 'memory', path: 'src/δ.rs', content: 'line 1\r\nline 2\n', render_mode: 'source_annotated', metadata: null },
        { source: 'empty-path', path: '', content: 'value', render_mode: 'source_annotated', metadata: null },
        { source: 'empty-content', path: null, content: '', render_mode: 'source_annotated', metadata: null },
        { source: 'private-label', path: 'private-path', content: 'exact\r\n  text\n', render_mode: 'verbatim', metadata: { render_mode: 'source_annotated' } },
        { source: 'empty-verbatim', path: null, content: '', render_mode: 'verbatim', metadata: null },
    ].map(chunk => ({ ...chunk, score: null }));
    const expected = ['Context from search:\nfound', 'Context from memory (src/δ.rs):\nline 1\r\nline 2\n',
        'Context from empty-path ():\nvalue', 'Context from empty-content:\n', 'exact\r\n  text\n', ''];
    const input = request({ messages: [message('User', [text('before'),
        ...chunks.map(chunk => ({ Context: { chunk } })), text('after')])] });
    const original = structuredClone(input);
    for (const chunk of chunks) Object.freeze(chunk);
    const output = await toNativeRequest(input, model);
    assert.ok(Schema.is(LLMRequest)(output));
    assert.deepEqual(output.messages[0].content.map(item => item.text), ['before', ...expected, 'after']);
    assert.deepEqual(input, original);
});

test('sampling and explicit output limit map once without a fallback token cap', async () => {
    const output = await toNativeRequest(request({ tool_choice: 'None',
        limits: { max_input_tokens: 123, max_output_tokens: 65000 },
        sampling: { temperature: 0, top_p: 0.9 },
        cache: { cache_instructions: true, cache_context: true, routing_key: 'session-key' },
    }), model);
    assert.equal(output.toolChoice.type, 'none');
    assert.deepEqual({ ...output.generation }, { maxTokens: 65000, temperature: 0, topP: 0.9 });
    assert.deepEqual(output.cache, { tools: true, system: true, messages: { tail: 1 } });
    assert.equal(output.promptCacheKey, 'session-key');
    assert.equal((await toNativeRequest(request(), model)).generation, undefined);
});

test('native function schema and output schema preserve JSON without workflow fields', async () => {
    const inputSchema = { type: 'object', properties: { value: { type: 'string' } }, required: ['value'] };
    const outputSchema = { type: 'object', properties: { ok: { type: 'boolean' } } };
    const output = await toNativeRequest(request({ tools: [{ name: 'edit', description: 'edit file',
        input_schema: inputSchema, surface: { kind: 'workflow_function', strict: true, output_schema: outputSchema },
        safety: 'mutating', supports_parallel_tool_calls: true, timeout_ms: 1000, metadata: { secret: 'not provider data' },
    }] }), model);
    assert.equal(output.tools[0].type, 'tool');
    assert.deepEqual(output.tools[0].inputSchema, inputSchema);
    assert.deepEqual(output.tools[0].outputSchema, outputSchema);
    assert.equal(output.tools[0].metadata, undefined);
});

test('tool history preserves IDs, order, multi-content and failed result semantics', async () => {
    const output = await toNativeRequest(request({ messages: [
        message('Assistant', [text('before'), { ToolCall: { call: call('one', { x: 1 }) } }, text('after')]),
        message('Tool', [{ ToolResult: { result: result('one', { ok: false, error: 'failed', content: [
            { type: 'text', text: 'detail' }, { type: 'image', mime_type: 'image/png', data: 'eA==' },
            { type: 'json', value: { partial: true } },
        ] }) } }]),
    ] }), model);
    assert.deepEqual(output.messages[0].content.map(item => item.type), ['text', 'tool-call', 'text']);
    assert.equal(output.messages[0].content[1].id, 'one');
    const tool = output.messages[1].content[0];
    assert.equal(tool.name, 'edit');
    assert.equal(tool.id, 'one');
    assert.equal(tool.result.type, 'error');
    assert.equal(tool.result.value.error.message, 'failed');
    assert.deepEqual(tool.result.value.content, [{ type: 'text', text: 'done' }, { type: 'text', text: 'detail' },
        { type: 'file', mime: 'image/png', uri: 'data:image/png;base64,eA==' }, { type: 'text', text: '{"partial":true}' }]);
});

test('function history with original raw JSON replays decoded canonical arguments', async () => {
    const canonicalCall = { ...call('checkpoint-call', { count: 3, enabled: true }),
        raw_arguments: '{"count":"3","enabled":"true"}' };
    const history = JSON.parse(JSON.stringify(message('Assistant', [{ ToolCall: { call: canonicalCall } }])));
    const native = await toNativeRequest(request({ messages: [history] }), model);
    const replayedCall = native.messages[0].content[0];
    assert.equal(replayedCall.id, canonicalCall.id);
    assert.deepEqual(replayedCall.input, canonicalCall.args);
    assert.notDeepEqual(replayedCall.input, JSON.parse(canonicalCall.raw_arguments));
    assert.equal(history.parts[0].payload.ToolCall.call.raw_arguments, canonicalCall.raw_arguments);
    const collector = new NativeResponseCollector(randomUUID());
    collector.push(LLMEvent.toolCall({ id: replayedCall.id, name: replayedCall.name, input: replayedCall.input }));
    collector.push(finish('tool-calls'));
    assert.deepEqual(collector.finish().tool_calls[0], { ...canonicalCall, raw_arguments: null });
    // A previously repaired call must not be parsed again even if its original
    // raw provider payload was incomplete.
    history.parts[0].payload.ToolCall.call.raw_arguments = '{incomplete';
    assert.deepEqual((await toNativeRequest(request({ messages: [history] }), model)).messages[0].content[0].input,
        canonicalCall.args);
});

test('inline and local images are native assets; pre-aborted IO reads nothing', async () => {
    const dir = await mkdtemp(join(tmpdir(), 'opencode-model-io-'));
    try {
        const path = join(dir, 'synthetic.png');
        await writeFile(path, Buffer.from('synthetic image'));
        const output = await toNativeRequest(request({ messages: [message('User', [text('before'),
            { Image: { image: { mime_type: 'image/png', data: 'eA==' } } },
            { Image: { image: { mime_type: 'image/png', path } } }, text('after')])] }), model);
        assert.deepEqual(output.messages[0].content.map(item => item.type), ['text', 'media', 'media', 'text']);
        assert.equal(output.messages[0].content[1].media.inline().base64, 'eA==');
        assert.equal(output.messages[0].content[2].media.inline().base64, Buffer.from('synthetic image').toString('base64'));
        const controller = new AbortController(); controller.abort();
        await assert.rejects(toNativeRequest(request({ messages: [message('User', [
            { Image: { image: { mime_type: 'image/png', path: '/nonexistent-synthetic-image' } } },
        ])] }), model, { signal: controller.signal }), { name: 'AbortError' });
    } finally { await rm(dir, { recursive: true, force: true }); }
});

test('unsupported controls and opaque payloads fail explicitly without becoming prompt text', async () => {
    for (const fields of [
        { tool_choice: 'Required' }, { tool_choice: { Tool: 'edit' } }, { response_format: 'Json' },
        { response_format: { JsonSchema: { name: 'shape', schema: {}, strict: true } } },
        { tools: [{ surface: { kind: 'freeform' } }] },
        { tools: [{ surface: { kind: 'provider_hosted' } }] },
        { messages: [message('Assistant', [{ Opaque: { format: 'pi/thinking', data: { secret: 'redacted' } } }])] },
        { messages: [message('Assistant', [{ ToolCall: { call: { ...call('one'), surface: 'freeform', raw_arguments: 'edit file' } } }])] },
        { messages: [message('Tool', [{ ToolResult: { result: result('unknown') } }])] },
    ]) await assert.rejects(toNativeRequest(request(fields), model), { code: 'OPENCODE_UNSUPPORTED_REQUEST' });
});

test('native text and reasoning deltas share one message identity and complete block order', () => {
    const id = randomUUID(), collector = new NativeResponseCollector(id), emitted = [];
    for (const event of [
        LLMEvent.reasoningStart({ id: 'r' }), LLMEvent.reasoningDelta({ id: 'r', text: 'think' }),
        LLMEvent.reasoningEnd({ id: 'r', providerMetadata: { opaque: { signature: 'signed' } } }),
        LLMEvent.textStart({ id: 'a' }), LLMEvent.textDelta({ id: 'a', text: 'ans' }),
        LLMEvent.textEnd({ id: 'a', text: 'answer' }),
        LLMEvent.textStart({ id: 'b' }), LLMEvent.textEnd({ id: 'b', text: 'second block' }), finish(),
    ]) { assert.ok(Schema.is(LLMEvent)(event)); emitted.push(...collector.push(event)); }
    assert.deepEqual(emitted.filter(item => item.TextDelta).map(item => item.TextDelta.text), ['ans', 'wer', '\nsecond block']);
    assert.deepEqual(emitted.filter(item => item.ReasoningDelta).map(item => item.ReasoningDelta.text), ['think']);
    assert.ok(emitted.filter(item => item.TextDelta || item.ReasoningDelta).every(item => (item.TextDelta ?? item.ReasoningDelta).message_id === id));
    const response = collector.finish();
    assert.equal(response.messages[0].id, id);
    assert.deepEqual(response.messages[0].parts.map(item => Object.keys(item.payload)[0]), ['Reasoning', 'Text', 'Text']);
    assert.deepEqual(response.messages[0].metadata.opencode_native.parts[0], { type: 'reasoning', providerMetadata: { opaque: { signature: 'signed' } } });
    assert.equal(response.finish_reason, 'Stop');
    assert.equal(response.end_turn, true);
    assert.equal(response.usage, null);
    assert.equal(emitted.at(-2).MessageCompleted.message, response.messages[0]);
});

test('authoritative native end corrections are retained in the completed item', () => {
    const collector = new NativeResponseCollector(randomUUID());
    collector.push(LLMEvent.textDelta({ id: 'text', text: 'draft' }));
    assert.deepEqual(collector.push(LLMEvent.textEnd({ id: 'text', text: 'corrected' })), []);
    collector.push(finish());
    assert.equal(collector.finish().messages[0].parts[0].payload.Text.text, 'corrected');
});

test('inclusive usage and native billing metadata are never double-counted', () => {
    const collector = new NativeResponseCollector(randomUUID());
    collector.push(LLMEvent.stepFinish({ index: 0, reason: { normalized: 'stop' }, usage: {
        inputTokens: 100, outputTokens: 40, cacheReadInputTokens: 70, cacheWriteInputTokens: 10,
        nonCachedInputTokens: 20, reasoningTokens: 25, providerMetadata: { synthetic: { billing: 'raw' } },
    } }));
    const events = collector.push(finish('stop', { providerMetadata: { synthetic: { response_id: 'id' } } }));
    assert.deepEqual(collector.finish().usage, { input_tokens: 100, output_tokens: 40,
        cached_input_tokens: 70, cache_creation_input_tokens: 10, reasoning_output_tokens: 25 });
    assert.deepEqual(events[0], { Usage: { usage: collector.finish().usage } });
    assert.deepEqual(collector.finish().provider_metadata.usage, { synthetic: { billing: 'raw' } });
    const partial = new NativeResponseCollector(randomUUID());
    partial.push(finish('stop', { usage: { inputTokens: 10 } }));
    assert.equal(partial.finish().usage, null);
});

test('parallel tool inputs retain original encounter order and exact final IDs/arguments', () => {
    const collector = new NativeResponseCollector(randomUUID());
    collector.push(LLMEvent.textDelta({ id: 'text', text: 'working' }));
    collector.push(LLMEvent.toolInputStart({ id: 'first', name: 'edit' }));
    collector.push(LLMEvent.toolInputStart({ id: 'second', name: 'edit' }));
    collector.push(LLMEvent.toolInputDelta({ id: 'first', name: 'edit', text: '{"a":', input: {} }));
    collector.push(LLMEvent.toolInputDelta({ id: 'first', name: 'edit', text: '1}' }));
    collector.push(LLMEvent.toolInputEnd({ id: 'second', name: 'edit' }));
    collector.push(LLMEvent.toolCall({ id: 'second', name: 'edit', input: { b: 2 } }));
    collector.push(LLMEvent.toolInputEnd({ id: 'first', name: 'edit' }));
    const finished = collector.push(LLMEvent.toolCall({ id: 'first', name: 'edit', input: { a: 1 }, providerMetadata: { opaque: { source: 'first' } } }));
    assert.deepEqual(finished, [{ ToolCallFinished: { call: call('first', { a: 1 }) } }]);
    collector.push(finish('tool-calls'));
    const output = collector.finish();
    assert.deepEqual(output.tool_calls, [call('first', { a: 1 }), call('second', { b: 2 })]);
    assert.deepEqual(output.messages[0].parts.map(item => Object.keys(item.payload)[0]), ['Text', 'ToolCall', 'ToolCall']);
    assert.equal(output.end_turn, false);
});

test('signed and redacted native metadata survives JSON persistence and next request unchanged', async () => {
    const collector = new NativeResponseCollector(randomUUID());
    const providerMetadata = { opaque: { signature: 'signature+/=', redacted: { data: 'hidden-blob' } } };
    collector.push(LLMEvent.reasoningStart({ id: 'r', providerMetadata }));
    collector.push(LLMEvent.reasoningEnd({ id: 'r' }));
    collector.push(LLMEvent.toolCall({ id: 'call', name: 'edit', input: { text: 'unique argument' },
        namespace: 'workspace', providerMetadata: { opaque: { item_id: 'native-call' } } }));
    collector.push(finish('tool-calls'));
    const persisted = JSON.parse(JSON.stringify(collector.finish()));
    const native = await toNativeRequest(request({ messages: persisted.messages }), model);
    assert.equal(native.messages[0].content[0].text, '');
    assert.deepEqual(native.messages[0].content[0].providerMetadata, providerMetadata);
    assert.equal(native.messages[0].content[1].namespace, 'workspace');
    assert.deepEqual(native.messages[0].content[1].providerMetadata, { opaque: { item_id: 'native-call' } });
    const metadata = JSON.stringify(persisted.messages[0].metadata);
    assert.ok(!metadata.includes('unique argument'));
    assert.equal(collector.events, undefined);
    const signed = await toNativeRequest(request({ messages: [message('Assistant', [
        { Reasoning: { text: 'readable', signature: 'generic-encrypted-signature' } },
    ])] }), model);
    assert.equal(signed.messages[0].content[0].encrypted, 'generic-encrypted-signature');
});

test('native encrypted reasoning field is replayed without interpreting provider blobs', async () => {
    const providerMetadata = { synthetic: { encrypted: 'provider-specific-opaque-value' } };
    const history = message('Assistant', [{ Reasoning: { text: '', signature: null } }], {
        metadata: { opencode_native: { parts: [{ type: 'reasoning', encrypted: 'native-encrypted', providerMetadata }] } },
    });
    const persisted = JSON.parse(JSON.stringify(history));
    const native = await toNativeRequest(request({ messages: [persisted] }), model);
    assert.equal(native.messages[0].content[0].encrypted, 'native-encrypted');
    assert.equal(native.messages[0].content[0].text, '');
    assert.deepEqual(native.messages[0].content[0].providerMetadata, providerMetadata);
    delete persisted.metadata.opencode_native.parts[0].encrypted;
    const opaqueOnly = await toNativeRequest(request({ messages: [persisted] }), model);
    assert.equal(opaqueOnly.messages[0].content[0].encrypted, undefined);
    persisted.parts[0].payload.Reasoning.signature = 'canonical-signature';
    persisted.metadata.opencode_native.parts[0].encrypted = 'old-native-encrypted';
    const canonicalWins = await toNativeRequest(request({ messages: [persisted] }), model);
    assert.equal(canonicalWins.messages[0].content[0].encrypted, 'canonical-signature');
});

test('EOF, malformed tools, provider errors and unsupported native output never succeed', () => {
    const eof = new NativeResponseCollector(randomUUID());
    eof.push(LLMEvent.textDelta({ id: 'x', text: 'partial' }));
    assert.throws(() => eof.finish(), { code: 'OPENCODE_PREMATURE_EOF' });
    for (const event of [
        LLMEvent.toolInputError({ id: 'x', name: 'edit', raw: '{broken' }),
        LLMEvent.toolResult({ id: 'x', name: 'edit', result: { type: 'text', value: 'provider result' } }),
        LLMEvent.toolError({ id: 'x', name: 'edit', message: 'provider tool error' }),
        LLMEvent.toolCall({ id: 'x', name: 'edit', input: {}, providerExecuted: true }),
        { type: 'tool-call', id: 'x', name: 'edit', input: undefined },
        { type: 'tool-call', id: 'x', name: 'edit', input: { invalid: NaN } },
        { type: 'compaction', provider: 'synthetic', encrypted: 'opaque' },
    ]) {
        const collector = new NativeResponseCollector(randomUUID());
        assert.throws(() => collector.push(event), { code: 'OPENCODE_MALFORMED_OUTPUT' });
        assert.throws(() => collector.finish(), { code: 'OPENCODE_MALFORMED_OUTPUT' });
    }
    const provider = new NativeResponseCollector(randomUUID());
    assert.throws(() => provider.push(LLMEvent.providerError({ message: 'raw provider failure', classification: 'context-overflow' })),
        { code: 'OPENCODE_PROVIDER_ERROR', classification: 'context-overflow' });
    const incomplete = new NativeResponseCollector(randomUUID());
    incomplete.push(LLMEvent.toolInputStart({ id: 'x', name: 'edit' }));
    assert.throws(() => incomplete.push(finish('tool-calls')), { code: 'OPENCODE_MALFORMED_OUTPUT' });
});

test('terminal state rejects duplicate IDs, contradictory finish and invalid usage', () => {
    const duplicate = new NativeResponseCollector(randomUUID());
    duplicate.push(LLMEvent.toolCall({ id: 'x', name: 'edit', input: {} }));
    assert.throws(() => duplicate.push(LLMEvent.toolCall({ id: 'x', name: 'edit', input: {} })), { code: 'OPENCODE_MALFORMED_OUTPUT' });
    for (const reason of ['stop', 'tool-calls']) {
        const collector = new NativeResponseCollector(randomUUID());
        if (reason === 'stop') collector.push(LLMEvent.toolCall({ id: 'x', name: 'edit', input: {} }));
        assert.throws(() => collector.push(finish(reason)), { code: 'OPENCODE_MALFORMED_OUTPUT' });
    }
    const usage = new NativeResponseCollector(randomUUID());
    assert.throws(() => usage.push(finish('stop', { usage: { inputTokens: -1, outputTokens: 2 } })), { code: 'OPENCODE_MALFORMED_OUTPUT' });
    const final = new NativeResponseCollector(randomUUID()); final.push(finish());
    assert.throws(() => final.push(finish()), { code: 'OPENCODE_MALFORMED_OUTPUT' });
});
