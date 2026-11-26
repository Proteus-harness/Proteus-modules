import { Effect, Layer, Stream } from "effect";
import { LayerNode } from "@opencode/util/effect/layer-node";
import { Global } from "@opencode/util/global";
import { FSUtil } from "@opencode/util/fs-util";
import * as Location from "@opencode/core/location";
import * as Bus from "@opencode/core/bus";
import * as Discovery from "@opencode/core/instruction-discovery";
import * as BuiltIns from "@opencode/core/instructions/builtins";
import * as Instructions from "@opencode/core/instructions/index";
import * as Watcher from "@opencode/core/filesystem/watcher";
import { Plugin as InstructionPlugin } from "@opencode/core/config/plugin/instruction";
import { nativeLocation } from "./location.mjs";
import { nativeGlobal } from "./shell.mjs";

// Avoid the discarded admission hash only for small, ordinary encoded sources.
function canSkipInitialDiff(observed) {
    let size = 0;
    const addText = value => typeof value === "string" && (size += value.length) <= 256 * 1024;
    for (const { key, value } of observed) {
        if (value === Instructions.unavailable) return false;
        if (value === Instructions.removed) continue;
        if (key === "core/date" || key === "core/environment") {
            if (!addText(value)) return false;
            continue;
        }
        if (key !== "core/instructions" || !Array.isArray(value) || value.length > 64) return false;
        for (const file of value) {
            if (!file || !Object.hasOwn(file, "path") || !Object.hasOwn(file, "content")
                || Object.keys(file).length !== 2 || !addText(file.path) || !addText(file.content)) return false;
        }
    }
    return true;
}

export async function nativeInitialContext(cwd, signal) {
    const globals = nativeGlobal(cwd);
    const location = await nativeLocation(cwd, signal);
    const layer = LayerNode.compile(LayerNode.group([Discovery.node, BuiltIns.node, FSUtil.node, Location.node, Global.node, Watcher.node]), { replacements: [
        Location.node.replace(Layer.succeed(Location.Service, location)),
        Global.node.replace(Global.layerWith(globals)),
        Bus.node.replace(Layer.succeed(Bus.Service, { publish: () => Effect.void })),
        // A fresh context build rescans using the native plugin. No watcher or
        // filesystem state is retained across independently authorized slots.
        Watcher.node.replace(Layer.succeed(Watcher.Service, { subscribe: () => Effect.succeed(Stream.empty) })),
    ] });
    return Effect.runPromise(Effect.scoped(Effect.gen(function* () {
        yield* InstructionPlugin.effect({});
        const discovery = yield* Discovery.Service;
        const builtins = yield* BuiltIns.Service;
        const sources = Instructions.combine([yield* builtins.load(), yield* discovery.load()]);
        const observed = yield* Instructions.read(sources);
        // Keep the native error and hashing path for blocked or unusual inputs.
        yield* (canSkipInitialDiff(observed) ? Effect.void : Instructions.diff(observed));
        const values = Object.fromEntries(observed.filter(entry => entry.value !== Instructions.removed && entry.value !== Instructions.unavailable).map(entry => [entry.key, entry.value]));
        return { text: Instructions.renderInitial(sources, values), values, globals, location };
    }).pipe(Effect.provide(layer))), { signal });
}

export function createContextExport(binding) {
    if (binding.slot !== "context" || binding.module_id !== "opencode_context" || binding.contract_version !== "v3" || binding.composition !== "select_one" || Object.keys(binding.module_config).length) throw new Error("Expected context/opencode_context v3 select_one without config");
    return {
        manifest: { slot: "context", module_id: "opencode_context", contract_version: "v3", composition: "select_one", module_features: [], config_schema: null },
        async invoke(method, params, { signal }) {
            if (method !== "build") throw new Error(`Unsupported OpenCode context method ${method}`);
            const context = await nativeInitialContext(params.task.cwd, signal);
            return { result: { summary: null, token_estimate: null, chunks: [{
                source: "opencode:instructions", path: null, content: context.text,
                render_mode: "verbatim", score: null,
                metadata: { "opencode/instruction_values": context.values, "opencode/project_root": context.location.project.directory },
            }] } };
        },
    };
}
