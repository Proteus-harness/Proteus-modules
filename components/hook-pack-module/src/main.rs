fn main() {
    proteus_module_runtime::main(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        include_str!("../exports.json"),
        |registry, binding| Ok(hook_pack::register_hook(registry, &binding.module_id)?),
        |binding| Some(hook_pack::config_schema(&binding.module_id)),
    );
}
