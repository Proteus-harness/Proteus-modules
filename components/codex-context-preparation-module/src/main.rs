fn main() {
    proteus_module_runtime::main(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        include_str!("../exports.json"),
        |registry, _binding| Ok(codex_context_preparation::register_modules(registry)?),
        |_binding| Some(codex_context_preparation::config_schema()),
    );
}
