//! Native bootstrap auditor: its dependency graph excludes inspected vendor code.
mod audit_support;
// This entry point uses only the synchronous capture subset of the shared
// process module; ordinary xtask separately checks its resource/capture API.
mod native_provenance;
#[allow(dead_code)]
mod process;
mod production_features;
#[path = "package_binary.rs"]
mod provenance_binary;
mod supplier_units;
mod vendor_audit;
use std::path::PathBuf;

fn main() {
    if cfg!(feature = "runtime-tools") {
        eprintln!(
            "The bootstrap auditor must be built with --no-default-features; use ordinary xtask for a post-build audit."
        );
        std::process::exit(1);
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root must exist");
    let args = match std::env::args_os()
        .skip(1)
        .map(|arg| arg.into_string())
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(args) => args,
        Err(_) => {
            eprintln!("Audit arguments must contain valid UTF-8.");
            std::process::exit(1);
        }
    };
    let result = match args.first().map(String::as_str) {
        None | Some("--help" | "help") => {
            println!(
                "Usage: cargo run -p xtask --no-default-features --bin autorouter-audit -- vendor [--only NAME]\n       cargo run -p xtask --no-default-features --bin autorouter-audit -- production-features --self-test|--build-report PATH\n       cargo run -p xtask --no-default-features --bin autorouter-audit -- bootstrap --build-report PATH\n\nExplicit supplier guards: autorouter-audit supplier-units --out artifacts/rust-rewrite/supplier-unit-ci [--target-dir REPOSITORY_RELATIVE_EXISTING_CACHE]\n\nGNU CI provenance: autorouter-audit binary-provenance --help\n\nIndependent offline source and ordinary release-feature auditors. No runtime-tools feature, provider calls or user configuration access."
            );
            Ok(true)
        }
        Some("vendor") => vendor_audit::run(&args[1..], &root),
        Some("supplier-units") => supplier_units::run(&args[1..], &root),
        Some("bootstrap") => production_features::verify_bootstrap(&args[1..], &root),
        Some("production-features") => production_features::run(&args[1..], &root),
        Some("binary-provenance") => native_provenance::run(&args[1..], &root),
        _ => Err("Unknown audit command; use --help".into()),
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
