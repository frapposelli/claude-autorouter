//! Isolated public-install verification. Bytes are checked before execution.
use crate::package::archive::{self, Entry};
use crate::release::{Artifact, PACKAGE, REGISTRY, ReleaseError, mismatch, regular, unavailable};
use crate::tool_process::{self, InputAction, RunOptions, Scratch};
use autorouter_core::auth::Environment;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub struct Invocation {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: Environment,
}
pub trait CommandRunner: Send + Sync {
    fn run(
        &self,
        command: Invocation,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> impl Future<Output = Result<String, ReleaseError>> + Send;
}
pub struct NativeCommand;
impl CommandRunner for NativeCommand {
    async fn run(
        &self,
        command: Invocation,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<String, ReleaseError> {
        let mut stdout = Vec::new();
        let response = CancellationToken::new();
        let report = tool_process::run_child(
            Command::new(command.program)
                .args(command.args)
                .current_dir(command.cwd)
                .env_clear()
                .envs(command.env),
            RunOptions {
                timeout,
                grace: Duration::from_secs(1),
                max_stdout: Some(4 * 1024 * 1024),
                interactive: false,
                response: &response,
                cancel,
                initial: InputAction {
                    close: true,
                    ..Default::default()
                },
            },
            |bytes| {
                stdout.extend_from_slice(bytes);
                InputAction::default()
            },
        )
        .await;
        if cancel.is_cancelled() {
            return Err(crate::release::error("cancelled", "cancelled"));
        }
        if report["timed_out"] == true {
            return Err(unavailable("isolated_install_timeout"));
        }
        if report["exit_code"] != 0
            || report["output_limit_exceeded"] == true
            || report["stderr_bytes"]
                .as_u64()
                .is_some_and(|n| n > 4 * 1024 * 1024)
        {
            return Err(crate::release::error("command_failed", "command_failed"));
        }
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    }
}
pub fn npm_environment(directory: &Path) -> Result<Environment, ReleaseError> {
    std::fs::create_dir_all(directory).map_err(|_| unavailable("isolated_install_unavailable"))?;
    for name in ["user.npmrc", "global.npmrc"] {
        tool_process::write_private(&directory.join(name), b"")
            .map_err(|_| unavailable("isolated_install_unavailable"))?;
    }
    let inherited = [
        "PATH",
        "HOME",
        "USERPROFILE",
        "SystemRoot",
        "WINDIR",
        "TMPDIR",
        "TMP",
        "TEMP",
        "LANG",
        "LC_ALL",
    ];
    let mut env: Environment = std::env::vars_os()
        .filter(|(key, _)| inherited.iter().any(|v| key == OsStr::new(v)))
        .collect();
    for (key, value) in [
        ("npm_config_cache", directory.join("cache").into_os_string()),
        (
            "npm_config_userconfig",
            directory.join("user.npmrc").into_os_string(),
        ),
        (
            "npm_config_globalconfig",
            directory.join("global.npmrc").into_os_string(),
        ),
        ("npm_config_update_notifier", "false".into()),
        ("npm_config_audit", "false".into()),
        ("npm_config_fund", "false".into()),
        ("npm_config_ignore_scripts", "true".into()),
    ] {
        env.insert(key.into(), value);
    }
    Ok(env)
}
pub fn compare_installed(root: &Path, files: &BTreeMap<String, Entry>) -> Result<(), ReleaseError> {
    let mut directories = BTreeSet::from([PathBuf::new()]);
    for path in files.keys() {
        let mut parent = Path::new(path).parent();
        while let Some(path) = parent {
            directories.insert(path.to_path_buf());
            parent = path.parent();
        }
    }
    let mut pending = vec![PathBuf::new()];
    let mut found = BTreeSet::new();
    while let Some(relative) = pending.pop() {
        let current = root.join(&relative);
        let info = std::fs::symlink_metadata(&current)
            .map_err(|_| mismatch("installed_archive_structure"))?;
        if !info.is_dir() {
            return Err(mismatch("installed_archive_structure"));
        }
        for entry in
            std::fs::read_dir(&current).map_err(|_| mismatch("installed_archive_structure"))?
        {
            let entry = entry.map_err(|_| mismatch("installed_archive_structure"))?;
            let path = relative.join(entry.file_name());
            let kind = entry
                .file_type()
                .map_err(|_| mismatch("installed_archive_structure"))?;
            if kind.is_dir() && directories.contains(&path) {
                pending.push(path);
            } else if kind.is_file() {
                let expected = path
                    .to_str()
                    .and_then(|p| files.get(p))
                    .ok_or_else(|| mismatch("installed_archive_structure"))?;
                let actual = regular(&root.join(&path), expected.bytes.len() as u64)
                    .map_err(|_| mismatch("installed_archive_bytes"))?;
                if actual != expected.bytes {
                    return Err(mismatch("installed_archive_bytes"));
                }
                found.insert(path);
            } else {
                return Err(mismatch("installed_archive_structure"));
            }
        }
    }
    if found.len() != files.len() {
        return Err(mismatch("installed_archive_structure"));
    }
    Ok(())
}
pub trait Installer: Send + Sync {
    fn install(
        &self,
        artifact: &Artifact,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> impl Future<Output = Result<Value, ReleaseError>> + Send;
}
pub struct NativeInstaller;
impl Installer for NativeInstaller {
    async fn install(
        &self,
        artifact: &Artifact,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<Value, ReleaseError> {
        let bytes = regular(&artifact.archive, archive::MAX_ARCHIVE as u64)
            .map_err(|_| mismatch("canonical_archive_unavailable"))?;
        if crate::evaluation::digest(&bytes) != artifact.sha256 || bytes.len() != artifact.bytes {
            return Err(mismatch("canonical_archive_changed"));
        }
        let (files, _) =
            archive::decode(&bytes).map_err(|_| mismatch("canonical_archive_invalid"))?;
        install_with(artifact, &files, &NativeCommand, timeout, cancel).await
    }
}
pub async fn install_with<R: CommandRunner>(
    artifact: &Artifact,
    files: &BTreeMap<String, Entry>,
    runner: &R,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Value, ReleaseError> {
    let dir =
        Scratch::new("registry-verify").map_err(|_| unavailable("isolated_install_unavailable"))?;
    let started = Instant::now();
    let remaining = |maximum: Duration| {
        timeout
            .checked_sub(started.elapsed())
            .filter(|d| !d.is_zero())
            .map(|d| d.min(maximum))
            .ok_or_else(|| unavailable("isolated_install_timeout"))
    };
    let env = npm_environment(&dir.0.join("npm"))?;
    let prefix = dir.0.join("install prefix");
    let cwd = dir.0.join("unrelated cwd");
    std::fs::create_dir(&cwd).map_err(|_| unavailable("isolated_install_unavailable"))?;
    let mut args: Vec<OsString> = ["install", "--global", "--prefix"].map(Into::into).to_vec();
    args.push(prefix.clone().into_os_string());
    args.extend(
        [
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            "--prefer-online",
            "--registry",
        ]
        .map(Into::into),
    );
    args.push(format!("{REGISTRY}/").into());
    args.push(format!("{PACKAGE}@{}", artifact.version).into());
    runner
        .run(
            Invocation {
                program: "npm".into(),
                args,
                cwd: cwd.clone(),
                env: env.clone(),
            },
            remaining(Duration::from_secs(120))?,
            cancel,
        )
        .await
        .map_err(|e| {
            if e.code == "cancelled" {
                e
            } else {
                unavailable("isolated_install_unavailable")
            }
        })?;
    let cli = prefix.join("bin").join(PACKAGE);
    let installed = prefix.join("lib/node_modules").join(PACKAGE);
    compare_installed(&installed, files)?;
    let target = installed.join(if artifact.native {
        "bin/autorouter"
    } else {
        "bin/autorouter.mjs"
    });
    if cli
        .canonicalize()
        .map_err(|_| mismatch("installed_cli_target"))?
        != target
            .canonicalize()
            .map_err(|_| mismatch("installed_cli_target"))?
    {
        return Err(mismatch("installed_cli_target"));
    }
    let mut outputs = Vec::new();
    for flag in ["--version", "--help"] {
        let result = runner
            .run(
                Invocation {
                    program: cli.clone(),
                    args: vec![flag.into()],
                    cwd: cwd.clone(),
                    env: env.clone(),
                },
                remaining(Duration::from_secs(10))?,
                cancel,
            )
            .await
            .map_err(|e| {
                if e.code == "registry_unavailable" || e.code == "cancelled" {
                    e
                } else if started.elapsed() >= timeout {
                    unavailable("isolated_install_timeout")
                } else {
                    mismatch("installed_cli_failed")
                }
            })?;
        outputs.push(result);
    }
    if outputs[0].trim() != artifact.version
        || !outputs[1].contains(PACKAGE)
        || !outputs[1].contains("setup")
    {
        return Err(mismatch("installed_cli_identity"));
    }
    Ok(json!({"version":artifact.version,"help":true,"isolated":true,"registry":REGISTRY}))
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    struct Mock {
        calls: Mutex<Vec<Invocation>>,
        files: BTreeMap<String, Entry>,
        changed: bool,
    }
    impl CommandRunner for Mock {
        async fn run(
            &self,
            command: Invocation,
            _: Duration,
            _: &CancellationToken,
        ) -> Result<String, ReleaseError> {
            assert!(
                command
                    .env
                    .keys()
                    .all(|k| !k.to_string_lossy().contains("API_KEY")
                        && !k.to_string_lossy().starts_with("AUTOROUTER"))
            );
            if command.program == Path::new("npm") {
                assert!(command.args.contains(&"--ignore-scripts".into()));
                let prefix = PathBuf::from(&command.args[3]);
                let root = prefix.join("lib/node_modules/claude-autorouter");
                for (path, entry) in &self.files {
                    std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
                    std::fs::write(
                        root.join(path),
                        if self.changed {
                            b"changed" as &[u8]
                        } else {
                            &entry.bytes
                        },
                    )
                    .unwrap();
                }
                std::fs::create_dir_all(prefix.join("bin")).unwrap();
                std::os::unix::fs::symlink(
                    root.join("bin/autorouter"),
                    prefix.join("bin/claude-autorouter"),
                )
                .unwrap();
            }
            let result = if command.args.first().is_some_and(|a| a == "--version") {
                "0.4.0\n"
            } else {
                "claude-autorouter setup help\n"
            };
            self.calls.lock().unwrap().push(command);
            Ok(result.into())
        }
    }
    fn artifact() -> Artifact {
        Artifact {
            version: "0.4.0".into(),
            tag: "v0.4.0".into(),
            dist_tag: "latest".into(),
            archive: "synthetic".into(),
            filename: "claude-autorouter-0.4.0.tgz".into(),
            sha256: String::new(),
            integrity: String::new(),
            bytes: 0,
            native: true,
        }
    }
    #[tokio::test]
    async fn installed_bytes_are_checked_before_executable_and_temp_dirs_removed() {
        for changed in [false, true] {
            let files = BTreeMap::from([(
                "bin/autorouter".into(),
                Entry {
                    bytes: b"synthetic reviewed dispatcher".to_vec(),
                    mode: 0o755,
                },
            )]);
            let mock = Mock {
                calls: Mutex::new(Vec::new()),
                files: files.clone(),
                changed,
            };
            let result = install_with(
                &artifact(),
                &files,
                &mock,
                Duration::from_secs(5),
                &CancellationToken::new(),
            )
            .await;
            if changed {
                assert_eq!(
                    result.unwrap_err().detail["reason"],
                    "installed_archive_bytes"
                );
            } else {
                assert_eq!(result.unwrap()["help"], true);
            }
            let calls = mock.calls.lock().unwrap();
            assert_eq!(calls.len(), if changed { 1 } else { 3 });
            assert!(!calls[0].cwd.exists());
            assert!(calls[0].args.contains(&"claude-autorouter@0.4.0".into()));
        }
    }
}
