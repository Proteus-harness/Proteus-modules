use anyhow::{Result, bail};
use proteus_contracts::process_module::{ModuleRegistry, ProcessModuleResult};
use proteus_module_runtime::CollectedModules;
use serde_json::Value;

type RegisterFn = fn(&mut dyn ModuleRegistry) -> ProcessModuleResult<()>;

pub fn load(slot: &str, module_id: &str, module_config: Value) -> Result<CollectedModules> {
    let mut modules = CollectedModules::with_config(module_config);
    if slot == "hook" {
        if module_id == "codex_compact" {
            codex_context_preparation::register_modules(&mut modules)?;
        } else if module_id == "tool_selection.dynamic" {
            tool_selection_hook::register_modules(&mut modules)?;
        } else {
            hook_pack::register_hook(&mut modules, module_id)?;
        }
        return Ok(modules);
    }
    if slot == "model" {
        model_pack::register_model(&mut modules, module_id)?;
        return Ok(modules);
    }
    if (slot, module_id) == ("tool", "reference.tools") {
        for register in [
            file_tools::register_modules as RegisterFn,
            git_tools::register_modules,
            shell_tool::register_modules,
            plan_tool::register_modules,
            rust_lsp::register_modules,
            skill_pack::register_modules,
            policy_pack::register_modules,
        ] {
            register(&mut modules).map_err(anyhow::Error::new)?;
        }
        return Ok(modules);
    }
    let register: RegisterFn = match (slot, module_id) {
        ("tool", "file_tools") => file_tools::register_modules,
        ("tool", "git_tools") => git_tools::register_modules,
        ("tool", "shell_tools") => shell_tool::register_modules,
        ("tool", "plan_tool") => plan_tool::register_modules,
        ("tool", "rust_lsp") => rust_lsp::register_modules,
        ("tool", "skill_tool") => skill_pack::register_modules,
        ("tool", "policy_tools") => policy_pack::register_modules,
        ("tool" | "context_provider", "rg_search") => rg_search::register_modules,
        ("tool", "direct_patch") => direct_patch::register_modules,
        ("tool", "codex_patch") => codex_patch::register_modules,
        ("tool" | "context_provider", "jsonl_memory") => memory_pack::register_modules,
        ("tool" | "context_provider", "sqlite_memory") => sqlite_memory::register_modules,
        ("context", "simple" | "repo_aware" | "codex_context") => context_pack::register_modules,
        ("context_provider", "skills") => skill_pack::register_modules,
        ("policy", "allow_all" | "ask_write" | "codex_policy" | "opencode_policy") => {
            policy_pack::register_modules
        }
        (
            "workflow",
            "coding.single_loop"
            | "coding.codex_loop"
            | "coding.plan_execute_review"
            | "coding.project_check",
        ) => coding_workflow::register_modules,
        _ => bail!("reference module has no {slot} module {module_id:?}"),
    };

    register(&mut modules).map_err(anyhow::Error::new)?;
    Ok(modules)
}
