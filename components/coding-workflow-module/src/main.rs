fn main() {
    proteus_module_runtime::main(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        include_str!("../exports.json"),
        |registry, _binding| Ok(coding_workflow::register_modules(registry)?),
        |binding| Some(coding_workflow::config_schema(&binding.module_id)),
    );
}
