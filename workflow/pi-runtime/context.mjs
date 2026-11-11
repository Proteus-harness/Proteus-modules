import { homedir } from "node:os";
import { join } from "node:path";
import { loadProjectContextFiles } from "./upstream.mjs";

export function context(params) {
    const files = loadProjectContextFiles({
        cwd: params.task.cwd,
        agentDir: process.env.PI_CODING_AGENT_DIR ?? join(homedir(), ".pi", "agent"),
    });
    return {
        result: {
            summary: null,
            token_estimate: null,
            chunks: files.map((file) => ({
                source: "pi:project_context",
                path: file.path,
                content: file.content,
                render_mode: "verbatim",
                score: null,
                metadata: null,
            })),
        },
    };
}
