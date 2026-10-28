//! Shared wiring for independently linked implementations. Wire, authority and
//! callbacks remain owned by the pinned Core SDK; this crate selects no modules.
mod exports;
mod hosts;
mod process_memory;
mod registry;

pub use exports::ModuleExport;
pub use process_memory::initialize as initialize_process;
pub use proteus_contracts as contracts;
pub use registry::CollectedModules;

use anyhow::{Result, bail};
use proteus_contracts::{
    contracts::ProcessComponentExportInitialize, domain::ModuleConfigSchema,
    process_module::ModuleRegistry,
};

/// One package admits only its declared exports. A `*` module id permits
/// caller-defined identities within that slot (for configured model aliases).
pub fn main(
    name: &str,
    version: &str,
    exports: &str,
    register: impl Fn(&mut dyn ModuleRegistry, &ProcessComponentExportInitialize) -> Result<()>,
    schema: impl Fn(&ProcessComponentExportInitialize) -> Option<ModuleConfigSchema>,
) {
    initialize_process();
    let result = (|| {
        let args: Vec<_> = std::env::args_os().collect();
        if args.len() == 2 && args[1] == "--version" {
            println!("{name} {version}");
            return Ok(());
        }
        if args.len() != 1 {
            bail!("usage: {name} [--version]");
        }
        let exports: std::collections::BTreeMap<String, Vec<String>> =
            serde_json::from_str(exports)?;
        proteus_module_sdk::run(|binding| {
            if !exports
                .get(&binding.slot)
                .is_some_and(|ids| ids.iter().any(|id| id == "*" || id == &binding.module_id))
            {
                bail!(
                    "{name} has no {} module {:?}",
                    binding.slot,
                    binding.module_id
                );
            }
            let mut modules = CollectedModules::with_config(binding.module_config.clone());
            register(&mut modules, &binding)?;
            let config_schema = schema(&binding);
            Ok(Box::new(ModuleExport::new(binding, modules, config_schema)))
        })
    })();
    if let Err(error) = result {
        eprintln!("{name}: {error:#}");
        std::process::exit(1);
    }
}
