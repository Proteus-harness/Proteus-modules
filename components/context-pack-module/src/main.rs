fn main() {
    proteus_module_runtime::main(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        include_str!("../exports.json"),
        |registry, _binding| Ok(context_pack::register_modules(registry)?),
        |binding| Some(context_pack::config_schema(&binding.module_id)),
    );
}
