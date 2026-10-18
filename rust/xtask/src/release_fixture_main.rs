//! Harmless contributor install fixture; never the AutoRouter product CLI.
fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() != 1 {
        std::process::exit(64);
    }
    match arguments[0].to_str() {
        Some("--version") => println!("0.4.0"),
        Some("--help") => println!("claude-autorouter setup: synthetic native install fixture"),
        _ => std::process::exit(64),
    }
}
