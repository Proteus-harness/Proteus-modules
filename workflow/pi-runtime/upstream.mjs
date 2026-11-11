// These are the entry points in the locked Pi distribution installed by pi-tools.
export { runAgentLoop } from "../../tool/pi-tools/node_modules/@earendil-works/pi-agent-core/dist/agent-loop.js";
export { buildSystemPrompt } from "../../tool/pi-tools/node_modules/@earendil-works/pi-coding-agent/dist/core/system-prompt.js";
export { loadProjectContextFiles } from "../../tool/pi-tools/node_modules/@earendil-works/pi-coding-agent/dist/core/resource-loader.js";
export {
    convertToLlm,
    createCompactionSummaryMessage,
} from "../../tool/pi-tools/node_modules/@earendil-works/pi-coding-agent/dist/core/messages.js";
export {
    prepareCompaction,
    compact,
    shouldCompact,
    estimateContextTokens,
    DEFAULT_COMPACTION_SETTINGS,
} from "../../tool/pi-tools/node_modules/@earendil-works/pi-coding-agent/dist/core/compaction/compaction.js";
import { OPENAI_CODEX_MODELS } from "../../tool/pi-tools/node_modules/@earendil-works/pi-ai/dist/providers/openai-codex.models.js";

import { OPENCODE_GO_MODELS } from "../../tool/pi-tools/node_modules/@earendil-works/pi-ai/dist/providers/opencode-go.models.js";

// Loop limits and compaction must use the explicitly bound provider catalog.
// Export ID and model name never select or substitute a runtime provider.
export function resolveModel(ref, catalogProvider = "openai-codex") {
    const catalogs = { "openai-codex": OPENAI_CODEX_MODELS, "opencode-go": OPENCODE_GO_MODELS };
    const model = catalogs[catalogProvider]?.[ref?.model];
    if (!model) throw new Error("Requested model absent from selected Pi provider catalog: " + catalogProvider + "/" + ref?.model);
    return model;
}

export { validateToolArguments } from "../../tool/pi-tools/node_modules/@earendil-works/pi-ai/dist/index.js";
export { createCodingToolDefinitions } from "../../tool/pi-tools/node_modules/@earendil-works/pi-coding-agent/dist/core/tools/index.js";
