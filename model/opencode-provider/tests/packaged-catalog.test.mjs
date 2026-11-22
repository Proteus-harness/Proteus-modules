import assert from 'node:assert/strict';
import { after, test } from 'node:test';

const originalFetch = globalThis.fetch;
let fetches = 0;
globalThis.fetch = () => { fetches++; throw new Error('unexpected network in packaged catalog test'); };
after(() => { globalThis.fetch = originalFetch; assert.equal(fetches, 0); });
const { createNativeRuntime } = await import('../runtime.mjs');

// Real npm SDK, local package only: no catalog or unknown-model request belongs
// in this test because those intentionally use stock refresh.
for (const [provider, model, expected, efforts] of [
    ['opencode-go', 'deepseek-v4.1-flash', [1_000_000, 384_000], ['low', 'high', 'max']],
    ['opencode', 'gpt-5.4', [922_000, 128_000], ['none', 'low', 'medium', 'high', 'xhigh']],
]) {
    test(`packaged ${provider} exact model survives a fresh Worker runtime whose first request is resolve`, async () => {
        const signal = new AbortController().signal, config = { provider };
        const runtime = createNativeRuntime(config);
        const a = await runtime.describe(model, signal);
        assert.deepEqual([a.capabilities.max_input_tokens, a.capabilities.max_output_tokens], expected);
        assert.equal(a.adapter_id, 'opencode');
        const restarted = createNativeRuntime(config);
        const resolved = await restarted.resolve(model, efforts[0], { type: 'key', key: 'synthetic-fixture-only' }, signal);
        assert.ok(resolved.model);
        assert.deepEqual(resolved.modelInfo.variants.map(variant => variant.id), efforts);
        assert.deepEqual(await restarted.describe(model, signal), a);
        assert.deepEqual(await restarted.environment(signal, model), await runtime.environment(signal, model));
    });
}
