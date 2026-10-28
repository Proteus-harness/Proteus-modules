fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|arg| arg == "auth")
        && args.get(2).is_some_and(|arg| arg == "openai_codex")
    {
        proteus_module_runtime::initialize_process();
        if let Err(error) = model_pack::adapters::codex_auth::cli::run(
            std::iter::once(args[0].clone()).chain(args.into_iter().skip(3)),
        ) {
            eprintln!("{}: {error:#}", env!("CARGO_PKG_NAME"));
            std::process::exit(1);
        }
        return;
    }
    proteus_module_runtime::main(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        include_str!("../exports.json"),
        |registry, binding| Ok(model_pack::register_model(registry, &binding.module_id)?),
        |binding| {
            Some(model_pack::adapters::config_schema::config_schema(
                &binding.module_config,
            ))
        },
    );
}
