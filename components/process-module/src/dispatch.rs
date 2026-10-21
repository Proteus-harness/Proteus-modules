pub fn run() -> anyhow::Result<()> {
    proteus_module_sdk::run(|binding| Ok(Box::new(crate::exports::ModuleExport::load(binding)?)))
}
