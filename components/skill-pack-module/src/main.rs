fn main() {
    proteus_module_runtime::main(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        include_str!("../exports.json"),
        |registry, _binding| Ok(skill_pack::register_modules(registry)?),
        |_binding| Some(Default::default()),
    );
}
