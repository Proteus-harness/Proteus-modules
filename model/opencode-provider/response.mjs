import { randomUUID } from 'node:crypto';

const textEventTypes = ['text-start', 'text-delta', 'text-end'];
const reasoningEventTypes = ['reasoning-start', 'reasoning-delta', 'reasoning-end'];
const toolEventTypes = ['tool-input-start', 'tool-input-delta', 'tool-input-end', 'tool-call'];

const finishReasons = {
    stop: 'Stop', length: 'Length', 'tool-calls': 'ToolCalls',
    'content-filter': 'ContentFilter', error: 'Error', unknown: 'Unknown',
};

function malformed(message, code = 'OPENCODE_MALFORMED_OUTPUT') {
    const error = new Error(message);
    error.code = code;
    return error;
}

function tokenCount(value) {
    if (!Number.isInteger(value) || value < 0 || value > 0xffff_ffff)
        throw malformed('Invalid native model token count');
    return value;
}

function canonicalUsage(usage) {
    if (!usage) return null;
    for (const key of ['inputTokens', 'outputTokens', 'cacheReadInputTokens', 'cacheWriteInputTokens', 'reasoningTokens'])
        if (usage[key] !== undefined) tokenCount(usage[key]);
    if (usage.inputTokens === undefined || usage.outputTokens === undefined) return null;
    const optional = key => usage[key] === undefined ? null : tokenCount(usage[key]);
    return {
        input_tokens: tokenCount(usage.inputTokens), output_tokens: tokenCount(usage.outputTokens),
        cached_input_tokens: optional('cacheReadInputTokens'),
        cache_creation_input_tokens: optional('cacheWriteInputTokens'),
        reasoning_output_tokens: optional('reasoningTokens'),
    };
}

function isJson(value, seen = new Set()) {
    if (value === null || typeof value === 'string' || typeof value === 'boolean') return true;
    if (typeof value === 'number') return Number.isFinite(value);
    if (typeof value !== 'object' || seen.has(value)) return false;
    seen.add(value);
    const valid = (Array.isArray(value) || Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null)
        && Object.values(value).every(item => isJson(item, seen));
    seen.delete(value);
    return valid;
}

// Unlike LLMResponse.reduce, keep one mutable content copy and no event log.
// Provider metadata is retained separately from visible text and tool inputs.
export class NativeResponseCollector {
    #seenText = false;
    #seenReasoning = false;
    #lastBlock;

    constructor(messageId) {
        this.messageId = messageId;
        this.parts = [];
        this.blocks = new Map();
        this.usage = undefined;
        this.reason = undefined;
        this.providerMetadata = undefined;
        this.response = undefined;
        this.failure = undefined;
    }

    block(event, type) {
        if (typeof event.id !== 'string' || !event.id) throw malformed('Native content block lacks an id');
        let block = this.#lastBlock;
        if (!block || block.type !== type || block.id !== event.id) {
            const key = `${type}:${event.id}`;
            block = this.blocks.get(key);
            if (!block) {
                block = { type, id: event.id, text: '', partId: randomUUID() };
                this.blocks.set(key, block);
                this.parts.push(block);
            }
            this.#lastBlock = block;
        }
        if (event.providerMetadata !== undefined) block.providerMetadata = event.providerMetadata;
        return block;
    }

    text(event, type) {
        const block = this.block(event, type);
        if (block.ended) throw malformed('Native content continued after its end');
        if (event.type.endsWith('-start')) {
            if (block.started) throw malformed('Duplicate native content start');
            block.started = true;
        }
        const suffix = event.type.endsWith('-delta') ? event.text : event.type.endsWith('-end') ? event.text : undefined;
        if (suffix !== undefined && typeof suffix !== 'string') throw malformed('Native text must be a string');
        let delta = '';
        if (suffix !== undefined) {
            if (event.type.endsWith('-end')) {
                // Native end events may carry an authoritative completed value.
                // A correction is published by MessageCompleted at finish.
                if (suffix.length > block.text.length && suffix.startsWith(block.text)) delta = suffix.slice(block.text.length);
                block.text = suffix;
            } else { delta = suffix; block.text += suffix; }
        }
        if (event.type.endsWith('-end')) block.ended = true;
        if (!delta) return [];
        // Canonical text/reasoning projection separates distinct visible blocks.
        // Do not expose signed/redacted provider metadata as readable reasoning.
        if (!block.visible) {
            const seen = type === 'text' ? this.#seenText : this.#seenReasoning;
            if (seen) delta = '\n' + delta;
            if (type === 'text') this.#seenText = true;
            else this.#seenReasoning = true;
            block.visible = true;
        }
        return type === 'text'
            ? [{ TextDelta: { message_id: this.messageId, phase: null, text: delta } }]
            : [{ ReasoningDelta: { message_id: this.messageId, text: delta } }];
    }

    tool(event) {
        if (event.providerExecuted) throw malformed('Provider-executed tools are unsupported by this model slot');
        const block = this.block(event, 'tool-call');
        if (typeof event.name !== 'string' || !event.name || (block.name !== undefined && block.name !== event.name))
            throw malformed('Native tool name changed or is missing');
        if (block.call) throw malformed('Native tool call continued after completion');
        block.name = event.name;
        if (event.namespace !== undefined) block.namespace = event.namespace;
        if (event.providerExecuted !== undefined) block.providerExecuted = event.providerExecuted;
        if (event.type === 'tool-input-start') {
            if (block.started) throw malformed('Duplicate native tool input start');
            block.started = true;
            return [{ ToolCallDelta: { call_id: event.id, name: event.name, args_delta: '' } }];
        }
        if (event.type === 'tool-input-delta') {
            if (block.inputEnded || typeof event.text !== 'string') throw malformed('Invalid native tool input delta');
            return [{ ToolCallDelta: { call_id: event.id, name: event.name, args_delta: event.text } }];
        }
        if (event.type === 'tool-input-end') { block.inputEnded = true; return []; }
        if (!isJson(event.input)) throw malformed('Native tool input is not JSON');
        block.call = { id: event.id, name: event.name, args: event.input, surface: 'function', raw_arguments: null };
        delete block.text;
        return [{ ToolCallFinished: { call: block.call } }];
    }

    push(event) {
        if (this.failure) throw this.failure;
        if (this.response) throw malformed('Native events continued after finish');
        try {
            if (textEventTypes.includes(event.type)) return this.text(event, 'text');
            if (reasoningEventTypes.includes(event.type)) return this.text(event, 'reasoning');
            if (event.type === 'provider-error' || event.type === 'error') {
                const error = malformed('Native model provider reported an error', 'OPENCODE_PROVIDER_ERROR');
                error.classification = event.classification;
                throw error;
            }
            if (event.type === 'tool-input-error') throw malformed('Native model tool input could not be decoded');
            if (toolEventTypes.includes(event.type)) return this.tool(event);
            if (event.type === 'step-start') return [];
            if (event.type !== 'step-finish' && event.type !== 'finish')
                throw malformed('Unsupported native model output: ' + event.type);
            if (event.usage !== undefined) this.usage = event.usage;
            if (event.providerMetadata !== undefined) this.providerMetadata = event.providerMetadata;
            if (event.type === 'step-finish') return [];
            if (!finishReasons[event.reason?.normalized]) throw malformed('Native model has no valid finish reason');
            this.reason = event.reason;
            this.response = this.buildResponse();
            // The response owns the final parts; no more events can use the builder.
            this.parts.length = 0;
            this.blocks.clear();
            this.#lastBlock = undefined;
            const events = [];
            if (this.response.usage) events.push({ Usage: { usage: this.response.usage } });
            events.push({ MessageCompleted: { message: this.response.messages[0] } });
            events.push({ Done: { finish_reason: this.response.finish_reason } });
            return events;
        } catch (error) { this.failure = error; throw error; }
    }

    buildResponse() {
        const calls = [], parts = [], nativeParts = [];
        for (const block of this.parts) {
            let payload;
            if (block.type === 'text') payload = { Text: { text: block.text } };
            else if (block.type === 'reasoning') payload = { Reasoning: { text: block.text, signature: null } };
            else {
                if (!block.call) throw malformed('Native model finished with incomplete tool input');
                calls.push(block.call);
                payload = { ToolCall: { call: block.call } };
            }
            const extra = { type: block.type };
            for (const key of ['providerMetadata', 'namespace', 'providerExecuted'])
                if (block[key] !== undefined) extra[key] = block[key];
            nativeParts.push(extra);
            parts.push({ part_id: block.partId, provenance: 'model', scope: 'conversation', payload });
        }
        const finish_reason = finishReasons[this.reason.normalized];
        if ((finish_reason === 'Stop' && calls.length) || (finish_reason === 'ToolCalls' && !calls.length))
            throw malformed('Native finish reason does not match tool calls');
        const message = { id: this.messageId, role: 'Assistant', phase: null, parts, name: null, tool_call_id: null,
            metadata: { opencode_native: { parts: nativeParts } } };
        return { messages: [message], tool_calls: calls, finish_reason, usage: canonicalUsage(this.usage),
            end_turn: finish_reason === 'Stop' ? true : finish_reason === 'ToolCalls' ? false : null,
            provider_metadata: { finish_reason: this.reason,
                ...(this.providerMetadata !== undefined ? { providerMetadata: this.providerMetadata } : {}),
                ...(this.usage?.providerMetadata !== undefined ? { usage: this.usage.providerMetadata } : {}),
            } };
    }

    finish() {
        if (this.failure) throw this.failure;
        if (!this.response) throw malformed('Native model stream ended before finish', 'OPENCODE_PREMATURE_EOF');
        return this.response;
    }
}
