mod config_schema;
mod dispatch;
mod registry;

fn main() {
    proteus_module_runtime::initialize_process();
    if let Err(error) = run() {
        eprintln!("proteus-reference-module: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let args = std::env::args_os().collect::<Vec<_>>();
    if args.len() == 2 && args[1] == "--version" {
        println!("proteus-reference-module {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.len() == 1 {
        return dispatch::run();
    }
    if args.get(1).is_some_and(|arg| arg == "auth")
        && args.get(2).is_some_and(|arg| arg == "openai_codex")
    {
        return model_pack::adapters::codex_auth::cli::run(
            std::iter::once(args[0].clone()).chain(args.into_iter().skip(3)),
        );
    }
    anyhow::bail!("usage: proteus-reference-module [auth openai_codex <login|status|logout>]");
}
