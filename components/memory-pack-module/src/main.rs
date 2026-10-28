fn main() {
    proteus_module_runtime::main(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        include_str!("../exports.json"),
        |registry, _binding| Ok(memory_pack::register_modules(registry)?),
        |_binding| Some(memory_pack::config_schema()),
    );
}
