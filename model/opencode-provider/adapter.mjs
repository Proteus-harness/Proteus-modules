import { randomUUID } from 'node:crypto';
import { Effect, Stream } from '../../workflow/opencode-runtime/node_modules/effect/dist/index.js';
import { LLMClient } from '../../workflow/opencode-runtime/node_modules/@opencode/ai/dist/route/client.js';
import { RequestExecutor } from '../../workflow/opencode-runtime/node_modules/@opencode/ai/dist/route/executor.js';
import { createNativeRuntime } from './runtime.mjs';
import { createCredentialReader } from './credentials.mjs';
import { configSchema } from './config-schema.mjs';
import { toNativeRequest } from './messages.mjs';
import { NativeResponseCollector } from './response.mjs';
import { createGoQuotaReader } from '../../components/model-runtime-js/opencode-go-quota.mjs';

export function modelFailure(error, { signal, started = false } = {}) {
    const reason = error?.reason ?? error;
    let kind = 'other';
    if (signal?.aborted || error?.name === 'AbortError') kind = 'interrupted';
    else if (reason?.classification === 'context-overflow' || reason?.classification === 'payload-too-large') kind = 'context_window_exceeded';
    else if (reason?._tag === 'RateLimit' || reason?._tag === 'ProviderInternal')
        kind = { retryable: { retry_delay_ms: Number.isSafeInteger(reason.retryAfterMs) && reason.retryAfterMs >= 0 ? reason.retryAfterMs : null } };
    else if (reason?._tag === 'Transport' || reason?._tag === 'Timeout' || error?.code === 'OPENCODE_PREMATURE_EOF' ||
        (reason?._tag === 'InvalidProviderOutput' && reason.classification === 'incomplete-stream'))
        kind = started ? 'stream_disconnected' : { retryable: { retry_delay_ms: null } };
    // Never expose native error objects: diagnostics can contain credentials or request bodies.
    let message = error?.code === 'OPENCODE_MISSING_SESSION' ?
        'OpenCode Go requires client_metadata.session_id for stable session routing' :
        reason?._tag === 'Authentication' ? 'OpenCode provider authentication failed' :
        reason?._tag === 'InvalidRequest' ? 'OpenCode provider rejected the model request' :
        kind === 'context_window_exceeded' ? 'OpenCode model context limit exceeded' :
        kind === 'interrupted' ? 'OpenCode model request interrupted' :
        kind === 'stream_disconnected' ? 'OpenCode model stream disconnected before completion' :
        typeof kind === 'object' ? 'OpenCode provider request can be retried' : 'OpenCode model request failed';
    // Only schema-owned categories and numeric status are public. Native
    // messages, bodies, URLs and response headers may contain request secrets.
    const tags = ['Authentication', 'InvalidRequest', 'QuotaExceeded', 'RateLimit',
        'ProviderInternal', 'Transport', 'Timeout', 'InvalidProviderOutput',
        'UnsupportedOperation', 'ProviderConfiguration', 'NoRoute'];
    const details = [];
    if (tags.includes(reason?._tag)) details.push(reason._tag);
    const status = reason?.http?.status;
    if (Number.isInteger(status) && status >= 100 && status <= 599) details.push('HTTP ' + status);
    if (details.length) message += ' (' + details.join('; ') + ')';
    return { kind, message, completed_messages: [] };
}

export function modelExport(binding, dependencies = {}) {
    const config = binding.module_config;
    if (binding.slot !== 'model' || typeof binding.module_id !== 'string' || !binding.module_id.trim() ||
        binding.module_id !== binding.module_id.trim() || binding.module_id === '*' || /[\x00-\x1f\x7f]/.test(binding.module_id) || binding.contract_version !== 'v14' ||
        !config || typeof config.provider !== 'string' || !config.provider.trim() ||
        Object.keys(config).some(key => !['provider', 'api_key', 'database_path', 'models_file', 'models_url'].includes(key)) ||
        ['api_key', 'database_path', 'models_file', 'models_url'].some(key => config[key] !== undefined && typeof config[key] !== 'string') ||
        (config.api_key !== undefined && !config.api_key.trim()))
        throw new Error('Invalid OpenCode model configuration');
    const runtime = dependencies.runtime ?? createNativeRuntime(config);
    const quota = config.provider === 'opencode-go' ? (dependencies.quota ?? createGoQuotaReader()) : null;
    async function credential(signal) {
        return (dependencies.credential ?? createCredentialReader({ providerID: config.provider,
            databasePath: config.database_path, apiKey: config.api_key,
            envNames: config.api_key === undefined ? await runtime.environment(signal) : [] }))(signal);
    }
    const stream = dependencies.stream ?? (async (request, signal, accept) => {
        await Effect.runPromise(Stream.runForEach(LLMClient.stream(request), event => Effect.promise(() => accept(event))).pipe(
            Effect.provide(LLMClient.layer), Effect.provide(RequestExecutor.fetchLayer)), { signal });
    });
    return {
        manifest: { slot: 'model', module_id: binding.module_id, contract_version: 'v14', composition: 'select_one',
            module_features: ['model.catalog.v1', ...(quota ? ['model.quota.v1'] : [])], config_schema: configSchema },
        async invoke(method, params, { signal, hostCall }) {
            if (method === 'catalog') return runtime.catalog(signal);
            if (method === 'quota') {
                if (!quota) throw new Error('OpenCode quota is unsupported for this provider');
                return quota({ getAuth: async () => ({ auth: { apiKey: (await credential(signal)).key } }) }, signal);
            }
            if (method === 'describe') return runtime.describe(params.model.model, signal);
            if (method !== 'stream') throw new Error('Unsupported OpenCode model operation');
            let count = 0, started = false;
            const completedMessages = [];
            const collector = new NativeResponseCollector(randomUUID());
            try {
                const r = params.request;
                if (r.operation !== undefined && r.operation !== 'Complete')
                    throw new Error('OpenCode provider bridge does not support native model compaction');
                const resolved = await runtime.resolve(r.model.model, r.reasoning.effort, await credential(signal), signal);
                const request = await toNativeRequest(r, resolved.model, { signal, modelInfo: resolved.modelInfo });
                await stream(request, signal, async event => {
                    if (['text-start', 'text-delta', 'reasoning-start', 'reasoning-delta', 'tool-input-start', 'tool-call'].includes(event.type)) started = true;
                    const events = collector.push(event);
                    for (const event of events) if (event.MessageCompleted) {
                        started = true;
                        completedMessages.push(event.MessageCompleted.message);
                    }
                    if (params.stream) for (const event of events) {
                        // Process model callbacks start at zero. Count only
                        // acknowledged deliveries so terminal event_count is
                        // the number Core actually accepted.
                        await hostCall('host.model.emit', { sequence: count, event });
                        count++;
                    }
                });
                return { event_count: count, terminal: { kind: 'response', response: collector.finish() } };
            } catch (error) {
                const failure = modelFailure(error, { signal, started });
                failure.completed_messages = completedMessages;
                return { event_count: count, terminal: { kind: started ? 'stream_error' : 'request_error',
                    failure } };
            }
        },
    };
}
