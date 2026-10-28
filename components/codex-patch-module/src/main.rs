fn main() {
    proteus_module_runtime::main(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        include_str!("../exports.json"),
        |registry, _binding| Ok(codex_patch::register_modules(registry)?),
        |_binding| Some(codex_patch::config_schema()),
    );
}
