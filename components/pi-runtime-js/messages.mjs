export function piContent(result) {
    const content = [];
    if (result.output || result.error)
        content.push({ type: "text", text: result.ok ? result.output : (result.error ?? result.output) });
    for (const item of result.content) {
        if (item.type === "text") content.push({ type: "text", text: item.text });
        else if (item.type === "image")
            content.push({ type: "image", data: item.data, mimeType: item.mime_type });
        else throw new Error("Unsupported tool content: " + item.type);
    }
    return content;
}

export function usage(value) {
    const cached = value?.cached_input_tokens ?? 0;
    const written = value?.cache_creation_input_tokens ?? 0;
    return {
        input: (value?.input_tokens ?? 0) - cached - written,
        output: value?.output_tokens ?? 0,
        cacheRead: cached,
        cacheWrite: written,
        totalTokens: (value?.input_tokens ?? 0) + (value?.output_tokens ?? 0),
        cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
    };
}

export function piMessages(messages, model) {
    const names = new Map();
    return messages.map((message) => {
        const content = [];
        const native = message.role === "Assistant" ? message.metadata?.pi_native : null;
        for (const [i, { payload }] of message.parts.entries()) {
            if (payload.Text) content.push({ type: "text", text: payload.Text.text });
            else if (payload.Context) {
                const c = payload.Context.chunk;
                const text =
                    c.render_mode === "verbatim"
                        ? c.content
                        : `Context from ${c.source}${c.path ? " (" + c.path + ")" : ""}:
${c.content}`;
                content.push({ type: "text", text });
            } else if (payload.ToolCall) {
                const { id, name, args } = payload.ToolCall.call;
                names.set(id, name);
                content.push({ type: "toolCall", id, name, arguments: args });
            } else if (payload.ToolResult) {
                const r = payload.ToolResult.result;
                return {
                    role: "toolResult",
                    toolName: names.get(r.call_id) ?? message.name,
                    toolCallId: r.call_id,
                    isError: !r.ok,
                    content: piContent(r),
                    details: r.metadata?.["pi/details"],
                    timestamp: 0,
                };
            } else if (payload.Reasoning || payload.ReasoningSummary) {
                content.push({
                    type: "thinking",
                    thinking: (payload.Reasoning ?? payload.ReasoningSummary).text,
                    ...((payload.Reasoning ?? payload.ReasoningSummary).signature
                        ? { thinkingSignature: (payload.Reasoning ?? payload.ReasoningSummary).signature }
                        : {}),
                });
            } else if (payload.Opaque) {
                // Redacted Pi thinking is provider replay data, not displayable
                // reasoning. Unknown opaque formats stay outside the Pi input.
                const { format, data } = payload.Opaque;
                if (format === "pi/thinking" && data?.type === "thinking"
                    && data.redacted === true && typeof data.thinking === "string")
                    content.push(structuredClone(data));
            } else if (payload.Image?.image) {
                content.push({
                    type: "image",
                    data: payload.Image.image.data ?? "",
                    mimeType: payload.Image.image.mime_type,
                });
            } else throw new Error("Unsupported Pi message part: " + Object.keys(payload));
            if (native && !payload.Opaque) {
                // Historical journals retain full native blocks. Preserve their
                // provider fields, with the canonical content owning the visible data.
                const fields = native.parts?.[i] ?? native.content?.[i];
                if (fields) content[content.length - 1] = { ...fields, ...content.at(-1) };
            }
        }
        if (message.role === "System" || message.role === "Developer")
            return { role: "system", content: content.map((c) => c.text).join("\n"), timestamp: 0 };
        if (message.role === "User") return { role: "user", content, timestamp: 0 };
        if (message.role === "Assistant")
            return {
                role: "assistant",
                provider: model.provider,
                api: model.api,
                model: model.id,
                timestamp: 0,
                usage: usage(message.metadata?.pi_usage),
                stopReason: content.some((p) => p.type === "toolCall") ? "toolUse" : "stop",
                ...(native?.header ??
                    (native?.content
                        ? Object.fromEntries(Object.entries(native).filter(([k]) => k !== "content"))
                        : {})),
                content,
            };
        throw new Error("Unsupported Pi history role: " + message.role);
    });
}

export function assistant(response, model) {
    const items = piMessages(response.messages, model);
    return {
        role: "assistant",
        content: items.flatMap((m) => m.content),
        usage: usage(response.usage),
        api: model.api,
        provider: model.provider,
        model: model.id,
        timestamp: 0,
        stopReason:
            { ToolCalls: "toolUse", Length: "length", Stop: "stop" }[response.finish_reason] ?? "error",
    };
}

export function finishedStream(message) {
    return {
        async *[Symbol.asyncIterator]() {
            yield { type: "done", reason: message.stopReason, message };
        },
        async result() {
            return message;
        },
    };
}
