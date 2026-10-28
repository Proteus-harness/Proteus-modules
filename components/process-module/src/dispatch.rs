pub fn run() -> anyhow::Result<()> {
    proteus_module_sdk::run(|binding| {
        let modules = crate::registry::load(
            &binding.slot,
            &binding.module_id,
            binding.module_config.clone(),
        )?;
        let schema = crate::config_schema::describe(
            &binding.slot,
            &binding.module_id,
            &binding.module_config,
        );
        Ok(Box::new(proteus_module_runtime::ModuleExport::new(
            binding, modules, schema,
        )))
    })
}
