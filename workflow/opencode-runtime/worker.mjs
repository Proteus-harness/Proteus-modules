#!/usr/bin/env node
import { runComponent } from "../../components/module-runtime-js/component.mjs";

await runComponent(async binding => {
    if (binding.slot === "model") {
        const { modelExport } = await import("../../model/opencode-provider/adapter.mjs");
        return modelExport(binding);
    }
    if (binding.slot === "hook" && binding.module_id === "opencode_prompt") {
        const { createPromptExport } = await import("./prompt.mjs");
        return createPromptExport(binding);
    }
    if (binding.slot === "hook" && binding.module_id === "opencode_compact") {
        const { createCompactionExport } = await import("./compaction.mjs");
        return createCompactionExport(binding);
    }
    if (binding.slot === "tool") {
        const { createFilesExport } = await import("./files.mjs");
        return createFilesExport(binding);
    }
    if (binding.slot === "context") {
        const { createContextExport } = await import("./context.mjs");
        return createContextExport(binding);
    }
    if (binding.slot === "workflow" && binding.module_id === "opencode_loop") {
        const { createBrokerWorkflowExport } = await import("./workflow.mjs");
        return createBrokerWorkflowExport(binding);
    }
    throw new Error(`OpenCode ${binding.slot}/${binding.module_id} adapter is unavailable`);
});
