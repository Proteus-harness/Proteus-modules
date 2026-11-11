#!/usr/bin/env node
import { runComponent } from "../../components/module-runtime-js/component.mjs";
import { createPromptExport } from "./prompt.mjs";
import { createFilesExport } from "./files.mjs";
import { createContextExport } from "./context.mjs";
import { createCompactionExport } from "./compaction.mjs";

await runComponent(async binding => {
    if (binding.slot === "hook" && binding.module_id === "opencode_prompt") return createPromptExport(binding);
    if (binding.slot === "hook" && binding.module_id === "opencode_compact") return createCompactionExport(binding);
    if (binding.slot === "tool") return createFilesExport(binding);
    if (binding.slot === "context") return createContextExport(binding);
    if (binding.slot === "workflow" && binding.module_id === "opencode_loop") {
        const { createWorkflowExport } = await import("./workflow.mjs");
        return createWorkflowExport(binding);
    }
    throw new Error(`OpenCode ${binding.slot}/${binding.module_id} adapter is unavailable`);
});
