#!/usr/bin/env node
import { runComponent } from "../../components/module-runtime-js/component.mjs";
import { createPiToolsExport } from "./adapter.mjs";

await runComponent(async binding => {
    if (binding.slot === "model") {
        const {modelExport} = await import("../../model/pi-provider/adapter.mjs");
        return modelExport(binding);
    }
    if (binding.slot === "tool") return createPiToolsExport(binding);
    const { createJsonLineExport } = await import("../../workflow/pi-runtime/exports.mjs");
    return createJsonLineExport(binding);
});
