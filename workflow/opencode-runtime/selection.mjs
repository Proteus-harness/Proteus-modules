import { Effect } from "effect";
import * as Environment from "@opencode/core/environment/index";
import * as FileAccess from "@opencode/core/file-access";
import * as FileMutation from "@opencode/core/file-mutation";
import * as Formatter from "@opencode/core/formatter";
import * as Location from "@opencode/core/location";
import * as Permission from "@opencode/core/permission";
import { Plugin as Patch } from "@opencode/core/tool/plugin/patch";

// Register the unchanged native patch hook without acquiring effect services.
// Captured file executors are never retained or invoked in this pure selector.
export async function selectNativeTools(model, specs, signal) {
    const hooks = [];
    const event = { model: { id: model.model }, tools: Object.fromEntries(specs.map(spec => [spec.name, spec])) };
    const ctx = {
        tool: { transform: () => Effect.void },
        session: { hook: (name, callback) => Effect.sync(() => { if (name === "context") hooks.push(callback); }) },
    };
    await Effect.runPromise(Effect.scoped(Effect.gen(function* () {
        yield* Patch.effect(ctx);
        for (const hook of hooks) yield* hook(event);
    }).pipe(...[Environment.Service, FileAccess.Service, FileMutation.Service, Formatter.Service, Location.Service, Permission.Service].map(service => Effect.provideService(service, {})))), { signal });
    return Object.values(event.tools);
}
