// Share the actual command graph with contributor integration tests.
#[cfg(not(test))]
fn main() {
    xtask::run_cli();
}

// Keep every existing --bin xtask unit-test selector and current_exe fixture.
#[cfg(test)]
include!("cli_root.rs");

#[cfg(test)]
fn main() {
    run_cli();
}
