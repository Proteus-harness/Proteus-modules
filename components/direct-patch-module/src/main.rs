fn main() {
    proteus_module_runtime::main(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        include_str!("../exports.json"),
        |registry, _binding| Ok(direct_patch::register_modules(registry)?),
        |_binding| Some(Default::default()),
    );
}
