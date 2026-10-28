fn main() {
    proteus_module_runtime::main(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        include_str!("../exports.json"),
        |registry, _binding| Ok(sqlite_memory::register_modules(registry)?),
        |_binding| Some(sqlite_memory::config_schema()),
    );
}
