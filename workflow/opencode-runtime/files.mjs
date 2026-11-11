import { Effect, Layer, Schema } from "effect";
import { LayerNode } from "@opencode/util/effect/layer-node";
import { FSUtil } from "@opencode/util/fs-util";
import * as Environment from "@opencode/core/environment/index";
import * as Location from "@opencode/core/location";
import * as FileAccess from "@opencode/core/file-access";
import * as FileMutation from "@opencode/core/file-mutation";
import * as Permission from "@opencode/core/permission";
import * as Formatter from "@opencode/core/formatter";
import * as Reader from "@opencode/core/tool/read-filesystem";
import * as Instructions from "@opencode/core/session/instructions";
import * as Bus from "@opencode/core/bus";
import { SessionEvent } from "@opencode/core/session/event";
import * as Store from "@opencode/core/session/store";
import * as ToolRuntime from "@opencode/core/tool/runtime";
import { Plugin as InputRepair } from "@opencode/core/plugin/tool-input-repair";
import { instructionEvents } from "./instructions.mjs";
import { Plugin as Read } from "@opencode/core/tool/plugin/read";
import { Plugin as Write } from "@opencode/core/tool/plugin/write";
import { Plugin as Edit } from "@opencode/core/tool/plugin/edit";
import { Plugin as Patch } from "@opencode/core/tool/plugin/patch";
import { Plugin as Glob } from "@opencode/core/tool/plugin/glob";
import { Plugin as Grep } from "@opencode/core/tool/plugin/grep";
import * as Ripgrep from "@opencode/core/ripgrep";
import { Plugin as Shell } from "@opencode/core/tool/plugin/shell";
import { nativeLocation } from "./location.mjs";
import { shellNodes } from "./shell.mjs";

const plugins = [Read, Write, Edit];

export async function withNativeFiles(cwd, use, signal, history = [], includeShell = false) {
    const location = await nativeLocation(cwd, signal);
    const shell = includeShell ? shellNodes(cwd) : { nodes: [], replacements: [] };
    const files = Environment.makeFiles(Environment.makeLocalDriver(undefined));
    const synthetic = [];
    const permissions = {
        assert: input => input.action === "external_directory"
            ? Effect.fail(new Error("OpenCode external_directory approval is unavailable through this Core tool adapter"))
            : Effect.void,
    };
    const layer = LayerNode.compile(LayerNode.group([
        Environment.node, Location.node, Permission.node, FileAccess.node,
        FileMutation.node, Formatter.node, FSUtil.node, Reader.node, Instructions.node, ...shell.nodes, ...(includeShell ? [Ripgrep.node] : []),
    ]), { replacements: [
        Environment.node.replace(Layer.succeed(Environment.Service, { files, spawner: undefined })),
        Location.node.replace(Layer.succeed(Location.Service, location)),
        Permission.node.replace(Layer.succeed(Permission.Service, permissions)),
        Bus.node.replace(Layer.succeed(Bus.Service, { publish: (type, payload) => Effect.sync(() => { if (type === SessionEvent.Synthetic) synthetic.push({ ...payload, type: "synthetic" }); }) })),
        Store.node.replace(Layer.succeed(Store.Service, { context: () => Effect.succeed(instructionEvents(history)) })),
        ...shell.replacements,
    ] });
    return Effect.runPromise(Effect.scoped(Effect.gen(function* () {
        const tools = new Map();
        const hooks = [];
        const before = [];
        const ctx = {
            tool: {
                transform: edit => Effect.sync(() => edit({ add: tool => tools.set(tool.name, { ...tool, id: tool.name }) })),
                list: () => Effect.succeed([...tools.values()]),
                hook: (name, callback) => Effect.sync(() => { if (name === "execute.before") before.push(callback); }),
            },
            session: { hook: (name, callback) => Effect.sync(() => { if (name === "context") hooks.push(callback); }) },
        };
        for (const plugin of [...plugins, ...(includeShell ? [Shell, Glob, Grep, Patch] : [])]) yield* plugin.effect(ctx);
        yield* InputRepair.effect(ctx);
        const event = { model: { id: "" }, tools: Object.fromEntries(tools) };
        for (const callback of hooks) yield* callback(event);
        const result = yield* use(tools, before);
        return { result, synthetic };
    }).pipe(Effect.provide(layer))), { signal });
}

export async function nativeFileDefinitions(cwd, includeShell = false) {
    const { result } = await withNativeFiles(cwd, tools => Effect.succeed([...tools.values()].map(tool => ({
        name: tool.name, description: tool.description,
        input_schema: ToolRuntime.definition(tool).inputSchema,
        surface: { kind: "function", strict: false, output_schema: null },
        safety: ["read", "grep", "glob"].includes(tool.name) ? "ReadOnly" : tool.name === "shell" ? "RunsCommands" : "WritesFiles",
        supports_parallel_tool_calls: false,
        timeout_ms: Number.MAX_SAFE_INTEGER,
        metadata: { "opencode/package": "@opencode/core@2.0.22", "opencode/scope": "workspace-file-plugins" },
    }))), undefined, [], includeShell);
    return result;
}

export async function executeNativeFile(input, signal, snapshot) {
    try {
        const { result: native, synthetic } = await withNativeFiles(input.cwd, (tools, before) => Effect.gen(function* () {
            const tool = tools.get(input.call.name);
            if (!tool) return yield* Effect.fail(new Error(`Unknown OpenCode file tool ${input.call.name}`));
            const raw = input.call.raw_arguments == null ? input.call.args : JSON.parse(input.call.raw_arguments);
            const event = { tool: input.call.name, input: raw };
            for (const callback of before) yield* callback(event);
            const args = event.input;
            if (input.call.name === "shell" && args.background === true) return yield* Effect.fail(new Error(
                "OpenCode background shell jobs require durable session notifications unavailable through this Core tool adapter",
            ));
            const agent = input.attribution?.agent;
            return yield* ToolRuntime.execute(tool, args, {
                id: input.call.id, agent: "build", sessionID: String(agent?.session_id ?? input.attribution?.execution_id ?? "detached"),
                messageID: snapshot?.message_id ?? input.call.id, progress: () => Effect.void,
            });
        }), signal, snapshot?.conversation.messages, ["shell", "grep", "glob", "patch"].includes(input.call.name));
        const blocks = typeof native.content === "string" ? [{ type: "text", text: native.content }] : native.content ?? [];
        const content = [];
        for (const block of blocks) {
            if (block.type === "text") continue;
            const match = /^data:([^;]+);base64,(.*)$/s.exec(block.uri);
            if (!match) throw new Error("OpenCode returned an unsupported external file attachment");
            content.push({ type: match[1].startsWith("image/") ? "image" : "binary", mime_type: match[1], data: match[2] });
        }
        return { call_id: input.call.id, ok: true, output: blocks.filter(block => block.type === "text").map(block => block.text).join("\n"),
            content, error: null, metadata: { "opencode/output": native.output ?? null, "opencode/metadata": native.metadata ?? null, "opencode/synthetic": synthetic } };
    } catch (error) {
        return { call_id: input.call.id, ok: false, output: "", content: [], error: String(error.message ?? error), metadata: null };
    }
}

export function createFilesExport(binding) {
    if (binding.slot !== "tool" || binding.module_id !== "opencode_tools" || binding.contract_version !== "v6" || binding.composition !== "ordered_many") throw new Error("Expected tool/opencode_tools v6 ordered_many");
    if (Object.keys(binding.module_config).length) throw new Error("OpenCode files take no module config");
    return {
        manifest: { slot: "tool", module_id: "opencode_tools", contract_version: "v6", composition: "ordered_many", module_features: [], config_schema: null },
        async invoke(method, params, { signal, hostCall }) {
            if (method === "list") {
                if (params !== null) throw new Error("Expected null tool-list params");
                return { result: (await nativeFileDefinitions(process.cwd(), true)).map(spec => ({ spec, model_visible: true, user_command: null })) };
            }
            if (method === "invoke") {
                // Read-only, invocation-bound history; never select another session.
                const snapshot = params.attribution?.agent && params.call.name === "read"
                    ? await hostCall("host.conversation.read", {}) : undefined;
                return { result: await executeNativeFile(params, signal, snapshot) };
            }
            throw new Error(`Unsupported OpenCode file method ${method}`);
        },
    };
}
