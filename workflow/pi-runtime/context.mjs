import { homedir } from "node:os";
import { join } from "node:path";
import { DefaultResourceLoader, SettingsManager } from "./upstream.mjs";

export async function context(params, { signal } = {}) {
    signal?.throwIfAborted();
    const loader = new DefaultResourceLoader({
        cwd: params.task.cwd,
        agentDir: process.env.PI_CODING_AGENT_DIR ?? join(homedir(), ".pi", "agent"),
        // This profile intentionally excludes native packages/extensions/settings.
        // The real loader still owns AGENTS and SYSTEM/APPEND_SYSTEM discovery,
        // precedence, BOM stripping and prompt-file resolution.
        settingsManager: SettingsManager.inMemory(),
        noExtensions: true,
        noSkills: true,
        noPromptTemplates: true,
        noThemes: true,
    });
    await loader.reload();
    signal?.throwIfAborted();
    const chunks = loader.getAgentsFiles().agentsFiles.map((file) => ({
        source: "pi:project_context",
        path: file.path,
        content: file.content,
        render_mode: "verbatim",
        score: null,
        metadata: null,
    }));
    const system = loader.getSystemPrompt();
    if (system !== undefined) chunks.push({
        source: "pi:system_prompt", path: loader.getSystemPromptSource()?.path ?? null,
        content: system, render_mode: "verbatim", score: null, metadata: { "pi/prompt": "system" },
    });
    for (const [index, content] of loader.getAppendSystemPrompt().entries()) chunks.push({
        source: "pi:append_system_prompt", path: loader.getAppendSystemPromptSources()[index]?.path ?? null,
        content, render_mode: "verbatim", score: null, metadata: { "pi/prompt": "append" },
    });
    return {
        result: {
            summary: null,
            token_estimate: null,
            chunks,
        },
    };
}
