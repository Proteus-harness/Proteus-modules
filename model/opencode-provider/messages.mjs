// Pure model/v14 -> @opencode/ai bridge. Agent prompts, model selection,
// authentication and provider variants belong to their own slots.
import { readFile } from 'node:fs/promises';
import { contextText } from '../../components/model-runtime-js/context.mjs';
import { LLM, Media } from '../../workflow/opencode-runtime/node_modules/@opencode/ai/dist/index.js';

function unsupported(message) {
    const error = new Error(message);
    error.code = 'OPENCODE_UNSUPPORTED_REQUEST';
    throw error;
}

function nativeExtras(message, index, type) {
    const extra = message.metadata?.opencode_native?.parts?.[index];
    if (!extra) return {};
    if (extra.type !== type) unsupported('Native history part type does not match canonical content');
    // Only restore schema fields, never spread an arbitrary provider blob into
    // a canonical block. Text and tool arguments have one authoritative copy.
    const result = {};
    for (const key of ['providerMetadata', 'metadata', 'cache', 'encrypted', 'namespace', 'providerExecuted'])
        if (extra[key] !== undefined) result[key] = extra[key];
    return result;
}

async function imagePart(image, signal) {
    signal?.throwIfAborted();
    let source;
    if (image.path) {
        const data = await readFile(image.path, { signal });
        signal?.throwIfAborted();
        source = { type: 'bytes', data, mediaType: image.mime_type };
    } else if (typeof image.data === 'string') {
        source = { type: 'base64', data: image.data, mediaType: image.mime_type };
    } else unsupported('Image requires inline data or a local path');
    return { type: 'media', media: new Media.Asset({ source }) };
}

function toolResult(result) {
    const content = [{ type: 'text', text: result.output }];
    for (const block of result.content ?? []) {
        if (block.type === 'text') content.push({ type: 'text', text: block.text });
        else if (block.type === 'json') content.push({ type: 'text', text: JSON.stringify(block.value) });
        else if (block.type === 'image') content.push({
            type: 'file', mime: block.mime_type, uri: `data:${block.mime_type};base64,${block.data}`,
        });
        else unsupported('Unsupported model tool-result content: ' + block.type);
    }
    if (!result.ok) return { type: 'error', value: {
        error: { message: result.error ?? 'Tool failed' }, content,
    } };
    return { type: 'content', value: content };
}

function toolDefinition(tool) {
    const kind = tool.surface?.kind ?? 'function';
    if (kind !== 'function' && kind !== 'workflow_function')
        unsupported('Unsupported native tool surface: ' + kind);
    return {
        name: tool.name, description: tool.description, inputSchema: tool.input_schema,
        ...(tool.surface?.output_schema != null ? { outputSchema: tool.surface.output_schema } : {}),
    };
}

export async function toNativeRequest(request, model, { signal, modelInfo } = {}) {
    signal?.throwIfAborted();
    if (request.response_format !== 'Text') unsupported('OpenCode model slot currently requires Text response format');
    if (request.tool_choice !== 'None' && request.tool_choice !== 'Auto')
        unsupported('Unsupported OpenCode tool choice');
    // Native SessionModelRequest binds Go routing to the session, including
    // follow-up requests. Keep Core's stable identity rather than inventing a
    // new affinity on each model call or forwarding arbitrary client metadata.
    const sessionID = request.client_metadata?.session_id;
    const hasSession = typeof sessionID === 'string' && sessionID.trim().length > 0;
    if (modelInfo?.providerID === 'opencode-go' && !hasSession) {
        const error = new Error('OpenCode Go requires client_metadata.session_id for stable session routing');
        error.code = 'OPENCODE_MISSING_SESSION';
        throw error;
    }
    const headers = hasSession ? {
        'x-opencode-session-id': sessionID, 'x-opencode-session': sessionID,
        'x-session-affinity': sessionID, 'X-Session-Id': sessionID,
        'x-opencode-client': 'proteus',
    } : undefined;
    const names = new Map();
    for (const message of request.messages)
        for (const part of message.parts)
            if (part.payload.ToolCall) names.set(part.payload.ToolCall.call.id, part.payload.ToolCall.call.name);

    const messages = [];
    for (const message of request.messages) {
        signal?.throwIfAborted();
        const role = { System: 'system', Developer: 'system', User: 'user', Assistant: 'assistant', Tool: 'tool' }[message.role];
        if (!role) unsupported('Unsupported canonical message role');
        const content = [];
        for (let index = 0; index < message.parts.length; index++) {
            const payload = message.parts[index].payload;
            let native;
            if (payload.Text) native = { type: 'text', text: payload.Text.text };
            else if (payload.Context) native = { type: 'text', text: contextText(payload.Context.chunk) };
            else if (payload.FileRef && payload.FileRef.content != null)
                native = { type: 'text', text: payload.FileRef.content };
            else if (payload.Image) native = await imagePart(payload.Image.image, signal);
            else if (payload.Reasoning) native = {
                type: 'reasoning', text: payload.Reasoning.text,
                ...(payload.Reasoning.signature != null ? { encrypted: payload.Reasoning.signature } : {}),
            };
            else if (payload.ReasoningSummary) native = { type: 'reasoning', text: payload.ReasoningSummary.text };
            else if (payload.ToolCall) {
                const call = payload.ToolCall.call;
                if (call.surface !== 'function')
                    unsupported('Native OpenCode history requires decoded function calls');
                // Core may retain original provider JSON alongside decoded or
                // coerced args. The canonical args remain authoritative.
                native = { type: 'tool-call', id: call.id, name: call.name, input: call.args };
            } else if (payload.ToolResult) {
                const result = payload.ToolResult.result;
                const name = names.get(result.call_id) ?? message.name;
                if (!name) unsupported('Tool result has no matching tool name');
                native = { type: 'tool-result', id: result.call_id, name, result: toolResult(result) };
            } else unsupported('Unsupported canonical model content: ' + Object.keys(payload).join(','));
            content.push({ ...nativeExtras(message, index, native.type), ...native });
        }
        const header = message.metadata?.opencode_native?.header;
        messages.push({ id: message.id, role, content,
            ...(header?.providerMetadata ? { providerMetadata: header.providerMetadata } : {}),
            ...(header?.metadata ? { metadata: header.metadata } : {}),
            ...(header?.native ? { native: header.native } : {}),
        });
    }
    const generation = {};
    if (request.limits.max_output_tokens != null) generation.maxTokens = request.limits.max_output_tokens;
    if (request.sampling.temperature != null) generation.temperature = request.sampling.temperature;
    if (request.sampling.top_p != null) generation.topP = request.sampling.top_p;
    signal?.throwIfAborted();
    return LLM.request({
        model,
        ...(headers ? { http: { headers } } : {}),
        system: request.instructions.map(instruction => ({ type: 'text', text: instruction.text })),
        messages, tools: request.tools.map(toolDefinition),
        toolChoice: request.tool_choice === 'None' ? 'none' : 'auto',
        ...(Object.keys(generation).length ? { generation } : {}),
        cache: request.cache.cache_instructions || request.cache.cache_context ? {
            tools: request.cache.cache_instructions, system: request.cache.cache_instructions,
            ...(request.cache.cache_context ? { messages: { tail: 1 } } : {}),
        } : 'none',
        ...(request.cache.routing_key != null ? { promptCacheKey: request.cache.routing_key } : {}),
    });
}
