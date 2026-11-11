import { workflow } from "./loop.mjs";
import { preparation } from "./compaction.mjs";
import { context } from "./context.mjs";

const slots = {
    workflow: { id: "pi_loop", version: "v20", method: "run", handle: workflow },
    hook: { id: "pi_compact", version: "v6", method: "hook.invoke", handle: preparation },
    context: { id: "pi_context", version: "v3", method: "build", handle: context },
};

export function createExport(binding) {
    const contract = slots[binding.slot];
    if (
        !contract ||
        binding.module_id !== contract.id ||
        binding.contract_version !== contract.version ||
        Object.keys(binding.module_config).some(key => key !== "catalog_provider" || binding.slot === "context") ||
        (binding.module_config.catalog_provider !== undefined &&
            !["openai-codex", "opencode-go"].includes(binding.module_config.catalog_provider))
    )
        throw new Error("Invalid Pi export binding");
    return {
        manifest: {
            slot: binding.slot,
            module_id: contract.id,
            contract_version: contract.version,
            composition: binding.slot === "hook" ? "ordered_many" : "select_one",
            module_features: [],
            config_schema: null,
        },
        invoke(method, params, invocation) {
            if (method !== contract.method) throw new Error("Unsupported Pi invocation " + method);
            return contract.handle(params, {
                ...invocation,
                catalogProvider: binding.module_config.catalog_provider ?? "openai-codex",
            });
        },
    };
}
