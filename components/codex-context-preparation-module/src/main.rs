fn main() {
    proteus_module_runtime::main(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        include_str!("../exports.json"),
        |registry, binding| {
            Ok(if binding.module_id == "codex_base_instructions" {
                codex_context_preparation::register_base_instructions(registry)?
            } else {
                codex_context_preparation::register_modules(registry)?
            })
        },
        |binding| {
            Some(if binding.module_id == "codex_base_instructions" {
                Default::default()
            } else {
                codex_context_preparation::config_schema()
            })
        },
    );
}
