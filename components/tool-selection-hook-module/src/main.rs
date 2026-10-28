fn main() {
    proteus_module_runtime::main(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        include_str!("../exports.json"),
        |registry, _binding| Ok(tool_selection_hook::register_modules(registry)?),
        |_binding| Some(tool_selection_hook::config_schema()),
    );
}
