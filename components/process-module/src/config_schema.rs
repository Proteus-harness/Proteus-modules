use proteus_contracts::domain::ModuleConfigSchema;
use serde_json::Value;

pub(crate) fn describe(slot: &str, id: &str, config: &Value) -> Option<ModuleConfigSchema> {
    Some(match slot {
        "model" => model_pack::adapters::config_schema::config_schema(config),
        "context" => context_pack::config_schema(id),
        "workflow" => coding_workflow::config_schema(id),
        "hook" if id == "tool_selection.dynamic" => tool_selection_hook::config_schema(),
        "policy" => policy_pack::config_schema(id),
        "hook" if id == "codex_compact" => codex_context_preparation::config_schema(),
        "hook" => hook_pack::config_schema(id),
        "tool" | "context_provider" if id == "jsonl_memory" => memory_pack::config_schema(),
        "tool" | "context_provider" if id == "sqlite_memory" => sqlite_memory::config_schema(),
        "tool" | "context_provider" if id == "rg_search" => ModuleConfigSchema::default(),
        "tool" if id == "codex_patch" => codex_patch::config_schema(),
        "tool" if matches!(id, "shell_tools" | "reference.tools") => shell_tool::config_schema(),
        // These packs read no module config.
        "tool"
            if matches!(
                id,
                "file_tools"
                    | "git_tools"
                    | "plan_tool"
                    | "policy_tools"
                    | "rust_lsp"
                    | "skill_tool"
                    | "direct_patch"
            ) =>
        {
            ModuleConfigSchema::default()
        }
        "context_provider" if id == "skills" => ModuleConfigSchema::default(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn standalone_shell_and_aggregate_reference_advertise_one_canonical_schema() {
        let expected = shell_tool::config_schema();
        expected.validate().unwrap();
        for id in ["shell_tools", "reference.tools"] {
            assert_eq!(
                describe("tool", id, &serde_json::json!({})),
                Some(expected.clone())
            );
        }
        assert_eq!(
            describe("tool", "file_tools", &serde_json::json!({})),
            Some(ModuleConfigSchema::default())
        );
    }
}
