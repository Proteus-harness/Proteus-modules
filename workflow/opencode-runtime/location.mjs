import { createHash } from "node:crypto";
import { Effect } from "effect";
import { LayerNode } from "@opencode/util/effect/layer-node";
import { FSUtil } from "@opencode/util/fs-util";
import { root } from "@opencode/core/project";

export async function nativeLocation(cwd, signal) {
    return Effect.runPromise(Effect.gen(function* () {
        const fs = yield* FSUtil.Service;
        const directory = yield* fs.resolve(cwd);
        const projectRoot = (yield* root(fs, directory)) ?? directory;
        const markers = yield* fs.up({ targets: [".git", ".hg"], start: directory, stop: projectRoot, mode: "first" });
        const marker = markers[0];
        return {
            directory, project: { id: createHash("sha256").update(projectRoot).digest("hex").slice(0, 16), directory: projectRoot, canonical: projectRoot },
            ...(marker ? { vcs: { type: marker.endsWith(".git") ? "git" : "hg", store: marker } } : {}),
        };
    }).pipe(Effect.provide(LayerNode.compile(FSUtil.node))), { signal });
}
