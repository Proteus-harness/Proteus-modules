import test from 'node:test';
import assert from 'node:assert/strict';
import { resolveModel } from '../../../workflow/pi-runtime/upstream.mjs';
import { OPENCODE_GO_MODELS } from '../node_modules/@earendil-works/pi-ai/dist/providers/opencode-go.models.js';
import { OPENAI_CODEX_MODELS } from '../node_modules/@earendil-works/pi-ai/dist/providers/openai-codex.models.js';

test('loop and compaction use the exact upstream descriptor for each provider', () => {
    for (const catalog of [OPENCODE_GO_MODELS, OPENAI_CODEX_MODELS]) {
        for (const id of Object.keys(catalog))
            assert.equal(resolveModel({model:id}, catalog[id].provider), catalog[id]);
    }
});

test('missing model does not substitute another provider or model', () => {
    assert.throws(() => resolveModel({model:'unavailable-test-model'}), /absent/);
    assert.throws(() => resolveModel({}), /absent/);
});

test('Codex default keeps the existing descriptor for overlapping model IDs', () => {
    for (const id of Object.keys(OPENAI_CODEX_MODELS))
        assert.equal(resolveModel({model:id}), OPENAI_CODEX_MODELS[id]);
    assert.equal(resolveModel({model:'deepseek-v4.1-flash'}, 'opencode-go'), OPENCODE_GO_MODELS['deepseek-v4.1-flash']);
});

test('Flash cannot infer a provider from its model ID or canonical export', () => {
    assert.throws(() => resolveModel({provider:'pi_codex', model:'deepseek-v4.1-flash'}), /absent/);
    assert.throws(() => resolveModel({model:'deepseek-v4.1-flash'}, 'unknown'), /absent/);
});

import { createExport } from '../../../workflow/pi-runtime/exports.mjs';
function binding(slot, module_id, contract_version, module_config = {}) {
    return {slot, module_id, contract_version, module_config};
}

test('compaction binding uses its explicit Go catalog without a model request', async () => {
    const hook = createExport(binding('hook', 'pi_compact', 'v6', {catalog_provider:'opencode-go'}));
    const answer = await hook.invoke('hook.invoke', {event:{
        event:'before_context', input:{request:{model:{provider:'pi_codex', model:'deepseek-v4.1-flash'}}},
        output:{messages:[]},
    }}, {signal:new AbortController().signal, hostCall:() => {throw Error('unexpected network');}});
    assert.deepEqual(answer, {result:{action:'continue'}});
    const unbound = createExport(binding('hook', 'pi_compact', 'v6'));
    await assert.rejects(unbound.invoke('hook.invoke', {event:{
        event:'before_context', input:{request:{model:{model:'deepseek-v4.1-flash'}}}, output:{messages:[]},
    }}, {}), /absent/);
});

test('workflow binding resolves Go before invoking host callbacks', async () => {
    const flow = createExport(binding('workflow','pi_loop','v20',{catalog_provider:'opencode-go'}));
    const answer = await flow.invoke('run', {task:{},history:[],runtime:{model_ref:{model:'deepseek-v4.1-flash'}}}, {
        hostCall:() => {throw Error('reached-host');}, signal:new AbortController().signal,
    });
    assert.equal(answer.status,'error');
    assert.equal(answer.failure.message,'reached-host');
    assert.throws(() => createExport(binding('workflow','pi_loop','v20',{catalog_provider:'bad'})), /Invalid/);
    assert.throws(() => createExport(binding('context','pi_context','v3',{catalog_provider:'opencode-go'})), /Invalid/);
});
