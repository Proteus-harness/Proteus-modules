import { homedir } from "node:os";
import { join } from "node:path";
import { Effect, Layer } from "effect";
import { LayerNode } from "@opencode/util/effect/layer-node";
import { Global } from "@opencode/util/global";
import { CrossSpawnSpawner } from "@opencode/util/cross-spawn-spawner";
import { ChildProcessSpawner } from "effect/unstable/process/ChildProcessSpawner";
import * as Environment from "@opencode/core/environment/index";
import * as Shell from "@opencode/core/shell";
import * as ShellSelect from "@opencode/core/shell/select";
import * as Job from "@opencode/core/job";
import * as KV from "@opencode/core/kv";
import * as Config from "@opencode/core/config";
import * as Session from "@opencode/core/session";
import * as SessionEnvironment from "@opencode/core/session/environment";
import * as PluginHooks from "@opencode/core/plugin/hooks";

export function nativeGlobal(_cwd) {
    const root = process.env.PROTEUS_OPENCODE_RUNTIME_DIR ?? join(
        process.env.XDG_STATE_HOME ?? join(homedir(), ".local/state"), "proteus/opencode-runtime",
    );
    return Global.make(Object.fromEntries(["data", "cache", "config", "state", "tmp", "bin", "log", "repos"].map(name => [name, join(root, name)])));
}

export function shellNodes(cwd) {
    const values = new Map();
    const environment = Layer.effect(Environment.Service, Effect.gen(function* () {
        const spawner = yield* ChildProcessSpawner;
        return { files: Environment.makeFiles(Environment.makeLocalDriver(spawner)), spawner };
    }).pipe(Effect.provide(LayerNode.compile(CrossSpawnSpawner.node))));
    return {
        nodes: [Config.node, Session.node, Shell.node, ShellSelect.node, Job.node, SessionEnvironment.node, PluginHooks.node],
        replacements: [
            Environment.node.replace(environment),
            Global.node.replace(Global.layerWith(nativeGlobal(cwd))),
            Config.node.replace(Layer.succeed(Config.Service, { entries: () => Effect.succeed([]) })),
            KV.node.replace(Layer.succeed(KV.Service, {
                get: key => Effect.sync(() => values.get(key)),
                set: (key, value) => Effect.sync(() => { values.set(key, value); }),
                remove: key => Effect.sync(() => { values.delete(key); }),
                scan: () => Effect.succeed({ entries: [] }),
            })),
            Session.node.replace(Layer.succeed(Session.Service, {
                synthetic: () => Effect.fail(new Error("OpenCode background session notifications are unavailable in this Core tool slot")),
            })),
        ],
    };
}
