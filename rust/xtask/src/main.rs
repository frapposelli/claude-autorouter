mod benchmark;
mod bundle;
mod context_probe;
mod env_file;
mod evaluation;
mod live;
mod live_claude;
mod observer_fixture;
mod ollama_evaluation;
mod ollama_routing;
mod package;
mod parity;
mod process;
mod reference;
mod release;
mod release_install;
mod release_pack;
mod release_verify;
mod tool_process;
#[cfg(test)]
mod tool_tests;
mod upgrade_rollback;

use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root must exist");
    let supplied = match std::env::args_os()
        .skip(1)
        .map(|value| value.into_string())
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(supplied) => supplied,
        Err(_) => {
            eprintln!("Tool arguments must contain valid UTF-8.");
            std::process::exit(1);
        }
    };
    let args = match env_file::load_args(
        &supplied,
        &std::env::current_dir().expect("working directory"),
    ) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let result = match args.first().map(String::as_str) {
        None | Some("--help" | "help") => {
            println!(
                "Usage: cargo xtask [--env-file PATH] COMMAND [OPTIONS]\n\nChecks: parity, fixture, freeze-reference\nNative artifacts: package, package-inspect, package-verify, package-smoke, upgrade-rollback\nRelease: release-pack, release-check, release-verify\nSynthetic transport: benchmark, benchmark-bundle\nExplicit evaluator calls: evaluate, evaluate-ollama, test-ollama-routing\nExplicit Claude startup/live calls: context-probe, live-validation\n\nEach opt-in tool provides --help. Environment files load only when explicitly supplied before COMMAND. Jev/Anthropic calls, model benchmarks and Claude/MCP startup are never ordinary contribution checks."
            );
            Ok(true)
        }
        Some("release-pack") => release_pack::run(&args[1..], &root),
        Some("release-check") => release::run(&args[1..], &root),
        Some("release-verify") => release_verify::run(&args[1..], &root),
        Some("upgrade-rollback") => upgrade_rollback::run(&args[1..], &root),
        Some("benchmark-bundle") => bundle::run(&args[1..], &root),
        Some("benchmark") => benchmark::run(&args[1..], &root),
        Some("context-probe") => context_probe::run(&args[1..], &root),
        Some("live-validation") => live::run(&args[1..], &root),
        Some("evaluate") => evaluation::run(&args[1..], &root),
        Some("evaluate-ollama") => ollama_evaluation::run(&args[1..], &root),
        Some("test-ollama-routing") => ollama_routing::run(&args[1..], &root),
        Some("package" | "package-inspect" | "package-verify" | "package-smoke") => {
            package::run(&args, &root)
        }
        _ => parity::run(&args, &root).map(|()| true),
    };
    match result {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
