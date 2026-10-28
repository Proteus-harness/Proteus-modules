use std::collections::HashMap;

use proteus_contracts::process_module::{
    ContextBuilderModuleObject, ContextProviderModuleObject, ModuleRegistry, PolicyModuleObject,
    ProcessModuleError, ProcessModuleResult, ToolModuleObject, WorkflowModuleObject,
};
use serde_json::{Value, json};

pub struct CollectedModules {
    pub models: HashMap<String, proteus_contracts::process_module::ModelModuleObject>,
    module_config: Value,
    pub tools: Vec<ToolModuleObject>,
    pub policies: HashMap<String, PolicyModuleObject>,
    pub context_providers: HashMap<String, ContextProviderModuleObject>,
    pub contexts: HashMap<String, ContextBuilderModuleObject>,
    pub hooks: HashMap<String, proteus_contracts::process_module::HookModuleObject>,
    pub workflows: HashMap<String, WorkflowModuleObject>,
}

impl CollectedModules {
    pub fn with_config(module_config: Value) -> Self {
        Self {
            models: HashMap::new(),
            module_config,
            tools: Vec::new(),
            policies: HashMap::new(),
            context_providers: HashMap::new(),
            contexts: HashMap::new(),
            workflows: HashMap::new(),
            hooks: HashMap::new(),
        }
    }
}

impl Default for CollectedModules {
    fn default() -> Self {
        Self::with_config(json!({}))
    }
}

fn insert<T>(map: &mut HashMap<String, T>, id: String, value: T) -> ProcessModuleResult<()> {
    if id.trim().is_empty() {
        return Err(ProcessModuleError::new("module id must not be empty"));
    }
    if map.insert(id.clone(), value).is_some() {
        return Err(ProcessModuleError::new(format!(
            "duplicate module id: {id}"
        )));
    }
    Ok(())
}

impl ModuleRegistry for CollectedModules {
    fn register_model(
        &mut self,
        module_id: String,
        model: proteus_contracts::process_module::ModelModuleObject,
    ) -> ProcessModuleResult<()> {
        insert(&mut self.models, module_id, model)
    }
    fn module_config(&self) -> &Value {
        &self.module_config
    }

    fn register_tool(&mut self, tool: ToolModuleObject) -> ProcessModuleResult<()> {
        self.tools.push(tool);
        Ok(())
    }

    fn register_policy(
        &mut self,
        module_id: String,
        policy: PolicyModuleObject,
    ) -> ProcessModuleResult<()> {
        insert(&mut self.policies, module_id, policy)
    }

    fn register_context_provider(
        &mut self,
        provider_id: String,
        provider: ContextProviderModuleObject,
    ) -> ProcessModuleResult<()> {
        insert(&mut self.context_providers, provider_id, provider)
    }

    fn register_context(
        &mut self,
        module_id: String,
        builder: ContextBuilderModuleObject,
    ) -> ProcessModuleResult<()> {
        insert(&mut self.contexts, module_id, builder)
    }

    fn register_hook(
        &mut self,
        module_id: String,
        hook: proteus_contracts::process_module::HookModuleObject,
    ) -> ProcessModuleResult<()> {
        insert(&mut self.hooks, module_id, hook)
    }

    fn register_workflow(
        &mut self,
        module_id: String,
        workflow: WorkflowModuleObject,
    ) -> ProcessModuleResult<()> {
        insert(&mut self.workflows, module_id, workflow)
    }
}
