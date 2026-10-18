//! Exact direct-archive installation and mock launcher lifecycle, without npm.
use super::direct;
use crate::package::{archive, lifecycle};
use crate::process::capture;
use crate::release;
use crate::tool_process::Scratch;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path};
use std::process::Command;
use std::time::Duration;

pub(crate) fn input(path: &Path) -> Result<(Vec<u8>, archive::DecodedArchive, Value), String> {
    let bytes = release::regular(path, archive::MAX_COMPRESSED as u64)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("Invalid direct archive name")?;
    let sidecar = release::regular(&path.with_file_name(format!("{name}.sha256")), 1024)?;
    if sidecar != format!("{}  {name}\n", crate::evaluation::digest(&bytes)).as_bytes() {
        return Err("Direct archive checksum sidecar differs".into());
    }
    let decoded = archive::decode_root(&bytes, release::PACKAGE, archive::NATIVE)?;
    let manifest: Value = serde_json::from_slice(
        &decoded
            .files
            .get("build-manifest.json")
            .ok_or("Direct archive lacks manifest")?
            .bytes,
    )
    .map_err(|_| "Invalid direct archive manifest")?;
    let verified = if manifest["kind"] == "native_direct_feasibility" {
        crate::package::direct::verify_decoded(name, &bytes, &decoded)?
    } else {
        direct::verify_decoded(name, &bytes, &decoded)?
    };
    Ok((bytes, decoded, verified))
}

pub fn run(path: &Path, root: &Path) -> Result<Value, String> {
    let (_bytes, decoded, verified) = input(path)?;
    smoke_with(
        &verified,
        &decoded.files,
        |launcher, native| lifecycle::run(root, launcher, native, path, &verified["sha256"]),
        |command| capture(command, b"", Duration::from_secs(20)),
    )
}

pub(crate) fn install(root: &Path, files: &BTreeMap<String, archive::Entry>) -> Result<(), String> {
    fs::create_dir(root).map_err(|_| "Cannot create isolated direct installation")?;
    for (name, entry) in files {
        if name.is_empty()
            || !Path::new(name)
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        {
            return Err("Invalid direct installation path".into());
        }
        let path = root.join(name);
        fs::create_dir_all(path.parent().ok_or("Invalid direct installation path")?)
            .map_err(|_| "Cannot create direct installation directories")?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(entry.mode)
            .open(&path)
            .map_err(|_| "Cannot create direct installation file")?;
        file.set_permissions(fs::Permissions::from_mode(entry.mode))
            .map_err(|_| "Cannot set direct installation file permissions")?;
        file.write_all(&entry.bytes)
            .map_err(|_| "Cannot write direct installation file")?;
    }
    check(root, files)
}

pub(crate) fn check(root: &Path, files: &BTreeMap<String, archive::Entry>) -> Result<(), String> {
    // Compare the filesystem against the validated archive before executing any
    // installed bytes. The bounded reader independently rejects links/extras.
    crate::release_install::compare_installed(root, files)
        .map_err(|_| "Installed direct archive bytes differ from the verified payload")?;
    for (name, entry) in files {
        if fs::symlink_metadata(root.join(name))
            .map_err(|_| "Cannot inspect direct installation permissions")?
            .permissions()
            .mode()
            & 0o7777
            != entry.mode
        {
            return Err("Installed direct archive permissions differ".into());
        }
    }
    Ok(())
}

fn smoke_with(
    verified: &Value,
    files: &BTreeMap<String, archive::Entry>,
    lifecycle: impl FnOnce(&Path, &Path) -> Result<Value, String>,
    mut execute: impl FnMut(&mut Command) -> Result<Vec<u8>, String>,
) -> Result<Value, String> {
    let target = verified["target"]
        .as_str()
        .ok_or("Missing verified direct target")?;
    if !lifecycle::host_can_run(target)? {
        return Err("Direct archive target is incompatible with this smoke-test host".into());
    }
    let scratch = Scratch::new("direct-installed-smoke")?;
    let installed = scratch.0.join("install path ' $ with spaces");
    install(&installed, files)?;
    let executable = installed.join("bin/claude-autorouter");
    let cwd = scratch.0.join("unrelated cwd");
    let home = scratch.0.join("synthetic home");
    let no_node = scratch.0.join("without-node");
    for path in [&cwd, &home, &no_node] {
        fs::create_dir(path).map_err(|_| "Cannot create isolated direct smoke environment")?;
    }
    let mut output = Vec::new();
    for flag in ["--version", "--help"] {
        let mut command = Command::new(&executable);
        command
            .arg(flag)
            .current_dir(&cwd)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", &no_node)
            .env("AUTOROUTER_CONFIG", home.join("absent-config.json"));
        output.push(execute(&mut command)?);
    }
    if String::from_utf8_lossy(&output[0]).trim() != verified["version"].as_str().unwrap_or("")
        || !String::from_utf8_lossy(&output[1]).contains("Usage:")
    {
        return Err("Installed direct CLI help/version failed".into());
    }
    let lifecycle = lifecycle(&executable, &executable)?;
    if lifecycle["passed"] != true {
        return Err("Installed direct launcher lifecycle did not pass".into());
    }
    Ok(json!({
        "schema_version":1,"kind":"native_direct_installed_smoke","passed":true,
        "archive_kind":verified["kind"],"release_approved":false,
        "filename":verified["filename"],"sha256":verified["sha256"],"source":verified["source"],
        "target":target,"version":verified["version"],"distribution":"direct",
        "checks":{"byte_integrity":true,"file_permissions":true,"help_version":true,
            "no_node_runtime":true,"launch_cleanup":true},
        "paths_with_spaces_quotes_and_dollar":true,"unrelated_cwd":true,
        "complete_launcher_lifecycle":lifecycle,
        "limits":["current_host_and_exact_archive_only","mock_claude_and_evaluator_only",
            "oldest_os_libc_cpu_and_corporate_tls_require_separate_evidence",
            "no_real_keychain_or_provider","no_final_release_authorization"]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn payload() -> BTreeMap<String, archive::Entry> {
        BTreeMap::from([
            (
                "bin/claude-autorouter".into(),
                archive::Entry {
                    bytes: b"synthetic executable, never actually executed".to_vec(),
                    mode: 0o755,
                },
            ),
            (
                "README.md".into(),
                archive::Entry {
                    bytes: b"synthetic public documentation".to_vec(),
                    mode: 0o644,
                },
            ),
        ])
    }

    #[test]
    fn direct_installation_uses_verified_bytes_modes_empty_path_and_cleans_every_exit() {
        for failure in ["none", "help", "lifecycle"] {
            let verified = json!({"target":lifecycle::host_target().unwrap(),"version":"0.5.2","sha256":"a".repeat(64),"filename":"synthetic.tar.gz"});
            let paths = RefCell::new(Vec::new());
            let result = smoke_with(
                &verified,
                &payload(),
                |launcher, native| {
                    assert_eq!(launcher, native);
                    assert!(launcher.is_file());
                    if failure == "lifecycle" {
                        Err("synthetic lifecycle failed".into())
                    } else {
                        Ok(json!({"passed":true,"test_suites": ["synthetic mock suite"]}))
                    }
                },
                |command| {
                    let executable = Path::new(command.get_program());
                    let installed = executable.parent().unwrap().parent().unwrap();
                    paths.borrow_mut().push(installed.to_path_buf());
                    assert_eq!(
                        fs::read(executable).unwrap(),
                        payload()["bin/claude-autorouter"].bytes
                    );
                    assert_eq!(
                        fs::metadata(executable).unwrap().permissions().mode() & 0o777,
                        0o755
                    );
                    assert_eq!(
                        fs::metadata(installed.join("README.md"))
                            .unwrap()
                            .permissions()
                            .mode()
                            & 0o777,
                        0o644
                    );
                    let env = command.get_envs().collect::<BTreeMap<_, _>>();
                    let path = Path::new(env[std::ffi::OsStr::new("PATH")].unwrap());
                    assert_eq!(fs::read_dir(path).unwrap().count(), 0);
                    assert!(
                        command
                            .get_current_dir()
                            .unwrap()
                            .ends_with("unrelated cwd")
                    );
                    assert_eq!(env.len(), 3);
                    if command.get_args().next() == Some(std::ffi::OsStr::new("--version")) {
                        Ok(b"0.5.2\n".to_vec())
                    } else if failure == "help" {
                        Err("synthetic command failed".into())
                    } else {
                        Ok(b"Usage: claude-autorouter setup\n".to_vec())
                    }
                },
            );
            assert_eq!(result.is_ok(), failure == "none");
            assert_eq!(paths.borrow().len(), 2);
            assert!(paths.borrow().iter().all(|path| !path.exists()));
        }
    }

    #[test]
    fn incompatible_host_and_unsafe_install_paths_never_execute() {
        let other = if std::env::consts::OS == "macos" {
            "x86_64-unknown-linux-gnu"
        } else {
            "aarch64-apple-darwin"
        };
        assert!(
            smoke_with(
                &json!({"target":other}),
                &payload(),
                |_, _| panic!("must not run lifecycle"),
                |_| panic!("must not execute")
            )
            .is_err()
        );
        for name in ["../escape", "/absolute", "", "bin/../escape"] {
            let scratch = Scratch::new("direct-smoke-unsafe").unwrap();
            let files = BTreeMap::from([(
                name.into(),
                archive::Entry {
                    bytes: Vec::new(),
                    mode: 0o644,
                },
            )]);
            assert!(install(&scratch.0.join("installed"), &files).is_err());
        }
    }
}
