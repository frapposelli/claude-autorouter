//! Shared exact-installed-executable checks for npm and direct archives.
use crate::process::{CapturedProcess, capture_result};
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SUITES: &[&str] = &[
    "commands",
    "edge_cases",
    "launcher_logging",
    "onboarding_local",
    "launcher_contracts",
    "startup_options",
    "config_contracts",
];

pub fn host_target() -> Result<&'static str, String> {
    host_target_for(
        std::env::consts::OS,
        std::env::consts::ARCH,
        cfg!(target_endian = "little"),
    )
}
fn host_target_for(
    os: &str,
    architecture: &str,
    little_endian: bool,
) -> Result<&'static str, String> {
    match (os, architecture) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "x86") => Ok("i686-unknown-linux-gnu"),
        ("linux", "arm") => Ok("armv7-unknown-linux-gnueabihf"),
        ("linux", "powerpc64") if little_endian => Ok("powerpc64le-unknown-linux-gnu"),
        ("linux", "s390x") => Ok("s390x-unknown-linux-gnu"),
        ("linux", "loongarch64") => Ok("loongarch64-unknown-linux-gnu"),
        ("linux", "riscv64") => Ok("riscv64gc-unknown-linux-gnu"),
        _ => Err("Installed smoke has no qualified host architecture mapping".into()),
    }
}

pub fn host_can_run(target: &str) -> Result<bool, String> {
    let host = host_target()?;
    Ok(target == host
        || (std::env::consts::OS == "linux"
            && matches!(std::env::consts::ARCH, "aarch64" | "x86_64")
            && target == format!("{}-unknown-linux-musl", std::env::consts::ARCH)))
}

pub fn run(
    root: &Path,
    launcher: &Path,
    native_executable: &Path,
    archive: &Path,
    archive_sha: &Value,
) -> Result<Value, String> {
    run_with(
        root,
        launcher,
        native_executable,
        archive,
        archive_sha,
        |command| capture_result(command, b"", Duration::from_secs(180)),
    )
}

fn run_with(
    root: &Path,
    launcher: &Path,
    native_executable: &Path,
    archive: &Path,
    archive_sha: &Value,
    execute: impl FnOnce(&mut Command) -> Result<CapturedProcess, String>,
) -> Result<Value, String> {
    let mut lifecycle = Command::new("cargo");
    lifecycle.args(["test", "--offline", "--locked", "-p", "claude-autorouter"]);
    for suite in SUITES {
        lifecycle.args(["--test", suite]);
    }
    lifecycle
        .current_dir(root.join("rust"))
        .env("AUTOROUTER_TEST_EXECUTABLE", launcher)
        .env("AUTOROUTER_TEST_NATIVE_EXECUTABLE", native_executable);
    let result = execute(&mut lifecycle)
        .map_err(|error| format!("Installed archive CLI lifecycle check failed: {error}"))?;
    if !result.status.success() {
        // Retain only completed bounded mock-test captures, never timed-out or
        // overflowing partial output, using the same private-file policy.
        let persist = || -> Result<PathBuf, String> {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "Invalid fixture report clock")?
                .as_nanos();
            let path = archive
                .parent()
                .ok_or("Missing archive parent")?
                .join(format!("installed-lifecycle-failure-{nonce}.json"));
            let report = json!({"kind":"synthetic_installed_lifecycle_failure","archive_sha256":archive_sha,"exit":result.status.to_string(),"stdout":String::from_utf8_lossy(&result.stdout),"stderr":String::from_utf8_lossy(&result.stderr)});
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .map_err(|_| "Cannot create private lifecycle diagnostics")?;
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|_| "Cannot set private lifecycle diagnostic permissions")?;
            file.write_all(
                &serde_json::to_vec_pretty(&report)
                    .map_err(|_| "Cannot serialize synthetic lifecycle diagnostics")?,
            )
            .map_err(|_| "Cannot write synthetic lifecycle diagnostics")?;
            Ok(path)
        };
        let diagnostic = match persist() {
            Ok(path) => format!("synthetic diagnostics retained at {}", path.display()),
            Err(error) => format!("cannot retain synthetic diagnostics: {error}"),
        };
        return Err(format!(
            "Installed archive CLI lifecycle failed ({}); {diagnostic}",
            result.status
        ));
    }
    let output = String::from_utf8(result.stdout).map_err(|_| "Invalid lifecycle test output")?;
    let summaries = output
        .lines()
        .filter(|line| line.starts_with("test result: ok."))
        .collect::<Vec<_>>();
    if summaries.len() != SUITES.len()
        || summaries.iter().any(|line| {
            line.strip_prefix("test result: ok. ")
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse::<usize>().ok())
                .is_none_or(|count| count == 0)
        })
    {
        return Err(
            "Installed archive lifecycle did not run all seven nonempty test suites".into(),
        );
    }
    Ok(
        json!({"passed":true,"test_suites":SUITES.iter().map(|suite|format!("crates/claude-autorouter/tests/{suite}.rs")).collect::<Vec<_>>(),"summaries":summaries}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn linux_x86_process_maps_to_i686_without_inferring_other_os_support() {
        assert_eq!(
            host_target_for("linux", "x86", true).unwrap(),
            "i686-unknown-linux-gnu"
        );
        assert!(host_target_for("macos", "x86", true).is_err());
        assert!(host_target_for("windows", "x86", true).is_err());
        assert!(host_target_for("linux", "powerpc64", false).is_err());
    }

    #[test]
    fn exact_installed_paths_and_every_suite_are_required() {
        for counts in [vec![1; 7], vec![1; 6], vec![1; 8], vec![0; 7]] {
            let result = run_with(
                Path::new("/synthetic repository"),
                Path::new("/installed path/bin/claude-autorouter"),
                Path::new("/installed path/native/claude-autorouter"),
                Path::new("/synthetic archive.tgz"),
                &json!("a".repeat(64)),
                |command| {
                    let args = command
                        .get_args()
                        .map(|arg| arg.to_string_lossy().into_owned())
                        .collect::<Vec<_>>();
                    assert_eq!(
                        args[0..5],
                        ["test", "--offline", "--locked", "-p", "claude-autorouter"]
                    );
                    assert_eq!(
                        args[5..]
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|row| row[1].as_str())
                            .collect::<Vec<_>>(),
                        SUITES
                    );
                    let environment = command
                        .get_envs()
                        .collect::<std::collections::BTreeMap<_, _>>();
                    assert_eq!(
                        environment[std::ffi::OsStr::new("AUTOROUTER_TEST_EXECUTABLE")],
                        Some(std::ffi::OsStr::new(
                            "/installed path/bin/claude-autorouter"
                        ))
                    );
                    assert_eq!(
                        environment[std::ffi::OsStr::new("AUTOROUTER_TEST_NATIVE_EXECUTABLE")],
                        Some(std::ffi::OsStr::new(
                            "/installed path/native/claude-autorouter"
                        ))
                    );
                    Ok(CapturedProcess {
                        status: std::process::ExitStatus::from_raw(0),
                        stdout: counts
                            .iter()
                            .map(|count| format!("test result: ok. {count} passed; 0 failed;\n"))
                            .collect::<String>()
                            .into_bytes(),
                        stderr: Vec::new(),
                    })
                },
            );
            assert_eq!(result.is_ok(), counts == vec![1; 7]);
        }
    }
}
