import * as pi from "@earendil-works/pi-coding-agent";
import { readFileSync } from "node:fs";
const packageEntry = new URL(import.meta.resolve("@earendil-works/pi-coding-agent"));
const packageInfo = JSON.parse(readFileSync(new URL("../package.json", packageEntry), "utf8"));
if (packageInfo.version !== "1.5.2") throw new Error(`Pi runtime requires 1.5.2, found ${packageInfo.version}`);
export const importPi = relative => import(new URL(relative, packageEntry));
const { createCodingToolDefinitions } = await importPi("./core/tools/index.js");

export const loadPi = () => pi;

export function createPiDefinitions(cwd) {
    return createCodingToolDefinitions(cwd);
}

export function specFor(definition) {
    return {
        name: definition.name,
        description: definition.description,
        input_schema: definition.parameters,
        surface: { kind: "function", strict: false, output_schema: null },
        safety: definition.name === "read" ? "ReadOnly" : definition.name === "bash" ? "RunsCommands" : "WritesFiles",
        supports_parallel_tool_calls: definition.executionMode !== "sequential",
        timeout_ms: Number.MAX_SAFE_INTEGER,
        metadata: {
            "pi/package": "@earendil-works/pi-coding-agent@1.5.2",
            "pi/output_schema": definition.outputSchema ?? null,
            "pi/execution_mode": definition.executionMode ?? null,
            "pi/prompt_snippet": definition.promptSnippet ?? null,
            "pi/prompt_guidelines": definition.promptGuidelines ?? [],
        },
    };
}

export function toToolResult(callId, value, failure) {
    if (failure) {
        return { call_id: callId, ok: false, output: "", content: [], error: String(failure.message ?? failure), metadata: null };
    }
    const content = value.content.map(block => {
        if (block.type === "text") return { type: "text", text: block.text };
        if (block.type === "image") return { type: "image", mime_type: block.mimeType, data: block.data };
        throw new Error(`Unsupported Pi tool content type ${block.type}`);
    });
    const output = value.content.filter(block => block.type === "text").map(block => block.text).join("\n");
    return {
        call_id: callId, ok: value.isError !== true, output,
        content: content.filter(block => block.type !== "text"), error: value.isError ? output : null,
        metadata: { "pi/details": value.details ?? null, "pi/structured_content": value.structuredContent ?? null },
    };
}

export async function executePiTool(input, signal) {
    const definition = createPiDefinitions(input.cwd).find(tool => tool.name === input.call.name);
    if (!definition) throw new Error(`Unknown Pi tool ${input.call.name}`);
    const args = input.call.raw_arguments == null ? input.call.args : JSON.parse(input.call.raw_arguments);
    const prepared = definition.prepareArguments ? definition.prepareArguments(args) : args;
    try {
        const result = await definition.execute(input.call.id, prepared, signal);
        return toToolResult(input.call.id, result);
    } catch (failure) {
        return toToolResult(input.call.id, null, failure);
    }
}

export function createPiToolsExport(binding) {
    if (binding.module_id !== "pi_tools" || binding.contract_version !== "v6" || binding.composition !== "ordered_many") {
        throw new Error("Pi tools require tool/pi_tools v6 ordered_many");
    }
    if (Object.keys(binding.module_config).length) throw new Error("Pi tools take no module config");
    return {
        manifest: { slot: "tool", module_id: "pi_tools", contract_version: "v6", composition: "ordered_many", module_features: [], config_schema: null },
        async invoke(method, params, { signal }) {
            if (method === "list") {
                if (params !== null) throw new Error("Pi tool list params must be null");
                return { result: createPiDefinitions(process.cwd()).map(tool => ({ spec: specFor(tool), model_visible: true, user_command: null })) };
            }
            if (method === "invoke") return { result: await executePiTool(params, signal) };
            throw new Error(`Unsupported Pi tool method ${method}`);
        },
    };
}
