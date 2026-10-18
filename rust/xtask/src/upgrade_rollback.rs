//! Isolated, offline npm upgrade/rollback using immutable local archives.
//! No user settings, Keychain entries, registry writes, or provider calls.
use crate::package::{self, archive};
use crate::process::capture;
use crate::tool_process::{Scratch, write_private};
use autorouter_core::auth::Environment;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
include!("../../crates/autorouter-core/src/json_compare_macros.rs");
contract_json_equal!(same_json, a, b => a.as_f64() == b.as_f64());
struct VerifiedNative {
    bytes: Vec<u8>,
    verification: Value,
}
impl VerifiedNative {
    fn retain(&self, destination: &Path) -> Result<(), String> {
        write_private(destination, &self.bytes)
    }
}
fn verify_native(path: &Path) -> Result<VerifiedNative, String> {
    let bytes = crate::release::regular(path, archive::MAX_COMPRESSED as u64)?;
    let decoded = archive::decode(&bytes, archive::NATIVE)?;
    let files = &decoded.files;
    let manifest: Value = serde_json::from_slice(
        &files
            .get("package.json")
            .ok_or("Native package manifest is absent")?
            .bytes,
    )
    .map_err(|_| "Invalid native package manifest")?;
    let verification = if manifest["private"] == true {
        package::verify_decoded(&bytes, &decoded)?
    } else {
        let build = crate::release::native_files(files, &manifest)?;
        json!({"verified":true,"kind":"native_npm_candidate","release_approved":false,"final_authorization":false,"sha256":hash(&bytes),"source":build["source"],"qualification":build["qualification"]})
    };
    Ok(VerifiedNative {
        bytes,
        verification,
    })
}
fn public_path(path: &str, patterns: &[Value]) -> bool {
    if matches!(path, "package.json" | "README.md" | "LICENSE") {
        return true;
    }
    patterns.iter().filter_map(Value::as_str).any(|pattern| {
        if let Some((prefix, suffix)) = pattern.split_once('*') {
            path.strip_prefix(prefix)
                .and_then(|s| s.strip_suffix(suffix))
                .is_some_and(|s| !s.contains('/'))
        } else {
            path == pattern
        }
    })
}
fn git_bytes(root: &Path, commit: &str, path: &str) -> Result<Vec<u8>, String> {
    capture(
        Command::new("git")
            .args([
                "--no-pager",
                "show",
                "--no-ext-diff",
                "--no-textconv",
                &format!("{commit}:{path}"),
            ])
            .current_dir(root),
        b"",
        Duration::from_secs(20),
    )
}
fn npm(env: &Environment, cwd: &Path, args: &[&str], paths: &[&Path]) -> Result<Vec<u8>, String> {
    capture(
        Command::new("npm")
            .args(args)
            .args(paths)
            .env_clear()
            .envs(env)
            .current_dir(cwd),
        b"",
        Duration::from_secs(120),
    )
}
fn baseline_archive(
    root: &Path,
    scratch: &Path,
    output: &Path,
    env: &Environment,
) -> Result<(PathBuf, String), String> {
    let baseline: Value = serde_json::from_slice(
        &fs::read(root.join("rust/parity/baseline.json"))
            .map_err(|_| "Cannot read baseline manifest")?,
    )
    .map_err(|_| "Invalid baseline manifest")?;
    let commit = baseline["baseline_commit"]
        .as_str()
        .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("Invalid frozen baseline identity")?;
    let manifest_bytes = git_bytes(root, commit, "package.json")?;
    let manifest: Value =
        serde_json::from_slice(&manifest_bytes).map_err(|_| "Invalid frozen package manifest")?;
    if manifest["name"] != "claude-autorouter" || manifest["version"] != "0.5.2" {
        return Err("Unexpected frozen package identity".into());
    }
    let patterns = manifest["files"]
        .as_array()
        .ok_or("Frozen package has no file allowlist")?;
    let tree = capture(
        Command::new("git")
            .args(["ls-tree", "-r", "--name-only", commit])
            .current_dir(root),
        b"",
        Duration::from_secs(20),
    )?;
    let tree = std::str::from_utf8(&tree).map_err(|_| "Invalid frozen source paths")?;
    let source = scratch.join("frozen-package");
    fs::create_dir(&source).map_err(|_| "Cannot stage frozen package")?;
    let mut expected = BTreeMap::new();
    for path in tree.lines().filter(|path| public_path(path, patterns)) {
        if path.starts_with('/')
            || path
                .split('/')
                .any(|v| v.is_empty() || v == ".." || v == ".")
        {
            return Err("Invalid frozen package path".into());
        }
        let bytes = git_bytes(root, commit, path)?;
        let target = source.join(path);
        fs::create_dir_all(target.parent().unwrap())
            .map_err(|_| "Cannot stage frozen package directory")?;
        write_private(&target, &bytes)?;
        fs::set_permissions(
            &target,
            fs::Permissions::from_mode(if path.starts_with("bin/") {
                0o755
            } else {
                0o644
            }),
        )
        .map_err(|_| "Cannot set frozen package mode")?;
        expected.insert(path.to_owned(), bytes);
    }
    let output_dir = output.join("baseline");
    fs::create_dir(&output_dir).map_err(|_| "Cannot create baseline archive directory")?;
    npm(
        env,
        &source,
        &[
            "pack",
            "--offline",
            "--ignore-scripts",
            "--json",
            "--pack-destination",
        ],
        &[&output_dir],
    )?;
    let tarball = output_dir.join("claude-autorouter-0.5.2.tgz");
    let bytes = crate::release::regular(&tarball, archive::MAX_COMPRESSED as u64)?;
    let (files, _) = archive::decode(&bytes, archive::HISTORICAL)?.into_parts();
    crate::release::legacy_files(&files)?;
    if files.len() != expected.len()
        || files
            .iter()
            .any(|(path, entry)| expected.get(path) != Some(&entry.bytes))
    {
        return Err("Frozen npm archive differs from its Git source allowlist".into());
    }
    write_private(
        &tarball.with_extension("tgz.sha256"),
        format!("{}  claude-autorouter-0.5.2.tgz\n", hash(&bytes)).as_bytes(),
    )?;
    Ok((tarball, commit.to_owned()))
}
fn install(
    tarball: &Path,
    prefix: &Path,
    cwd: &Path,
    env: &Environment,
    policy: archive::ArchivePolicy,
) -> Result<(), String> {
    let bytes = crate::release::regular(tarball, archive::MAX_COMPRESSED as u64)?;
    let (files, _) = archive::decode(&bytes, policy)?.into_parts();
    npm(
        env,
        cwd,
        &[
            "install",
            "--global",
            "--offline",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            "--prefix",
        ],
        &[prefix, tarball],
    )?;
    crate::release_install::compare_installed(
        &prefix.join("lib/node_modules/claude-autorouter"),
        &files,
    )
    .map_err(|_| "Installed upgrade/rollback files do not match the exact archive")?;
    let launcher = prefix.join("bin/claude-autorouter");
    if !fs::symlink_metadata(&launcher)
        .map_err(|_| "Installed command is absent")?
        .file_type()
        .is_symlink()
    {
        return Err("npm did not create its command symlink".into());
    }
    Ok(())
}
fn command(prefix: &Path, cwd: &Path, env: &Environment, args: &[&str]) -> Result<Vec<u8>, String> {
    capture(
        Command::new(prefix.join("bin/claude-autorouter"))
            .args(args)
            .env_clear()
            .envs(env)
            .current_dir(cwd),
        b"",
        Duration::from_secs(20),
    )
}
fn report(prefix: &Path, cwd: &Path, env: &Environment, args: &[&str]) -> Result<Value, String> {
    serde_json::from_slice(&command(prefix, cwd, env, args)?)
        .map_err(|_| "Installed command did not return a JSON report".into())
}
fn inspect(prefix: &Path, cwd: &Path, env: &Environment) -> Result<Value, String> {
    let config = report(prefix, cwd, env, &["config", "show", "--json"])?;
    let history = report(
        prefix,
        cwd,
        env,
        &["sessions", "show", "autorouter-session-rehearsal", "--json"],
    )?;
    if config["valid"] != true
        || history["records"]
            .as_array()
            .is_none_or(|rows| rows.len() != 3)
        || history["summary"]["coverage"]["legacy_decisions"] != 1
        || history["summary"]["savings"]["priced_requests"] != 1
    {
        return Err(
            "Rehearsal did not exercise valid settings, both history schemas and recorded pricing"
                .into(),
        );
    }
    Ok(json!({"config":config,"history":history}))
}
fn fixture(scratch: &Path) -> Result<(PathBuf, PathBuf, Vec<u8>), String> {
    let history = scratch.join("history");
    fs::create_dir(&history).map_err(|_| "Cannot create synthetic history")?;
    let mut rows = Vec::new();
    for (schema, id) in [(1, "legacy"), (2, "current")] {
        rows.push(json!({"schema_version":schema,"event":"decision","timestamp":"2026-10-05T12:00:00.000Z","request_id":id,"session_id":"synthetic-session","requested_model":"claude-haiku-4-5-20251001","selected_model":"claude-haiku-4-5-20251001","source":"jev","reason":"classified","decision_latency_ms":12}));
    }
    rows.push(json!({"schema_version":2,"event":"outcome","timestamp":"2026-10-05T12:00:00.000Z","request_id":"current","session_id":"synthetic-session","status":"completed","http_status":200,"completion_confirmed":true,"confirmed_model":"claude-haiku-4-5-20251001","baseline_model":"claude-opus-5-5","pricing_version":autorouter_core::savings::PRICING_VERSION,"usage_complete":true,"usage":{"input_tokens":1000,"output_tokens":100},"total_latency_ms":600}));
    let bytes = rows
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let path = history.join("autorouter-session-rehearsal.jsonl");
    write_private(&path, bytes.as_bytes())?;
    let config = scratch.join("config.json");
    write_private(&config,serde_json::to_vec_pretty(&json!({"AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_AUTH_MODE":"api-key","AUTOROUTER_SECRET_STORE":"file","TYPESAFE_API_KEY":"synthetic-rehearsal-jev","ANTHROPIC_API_KEY":"synthetic-rehearsal-provider","AUTOROUTER_PORT":"8007","AUTOROUTER_SESSION_LOG_DIR":history,"AUTOROUTER_SESSION_LOG_MODE":"metadata","ENABLE_TOOL_SEARCH":"auto:5"})).map_err(|_|"Cannot encode synthetic settings")?.as_slice())?;
    Ok((config, path, bytes.into_bytes()))
}
// Two failures precede rollback; the fake child copies its live allocations.
fn failed_startup(
    prefix: &Path,
    cwd: &Path,
    env: &Environment,
    config: &Path,
    history: &Path,
    native: &VerifiedNative,
) -> Result<Value, String> {
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::os::unix::fs::MetadataExt;
    let io_error = |error: std::io::Error| format!("Recovery I/O: {:?}", error.kind());
    let invalid = "Invalid installed recovery observation";
    let private_dir = |path: &Path| -> Result<(), String> {
        fs::create_dir(path).map_err(io_error)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
        Ok(())
    };
    let snapshot = |path: &Path| -> Result<_, String> {
        let info = fs::symlink_metadata(path).map_err(io_error)?;
        if !info.is_file() || info.nlink() != 1 || info.len() > 1024 * 1024 {
            return Err("Invalid bounded recovery file".into());
        }
        Ok((
            fs::read(path).map_err(io_error)?,
            json!({"dev":info.dev(),"ino":info.ino(),"mode":info.mode(),"uid":info.uid(),
                "gid":info.gid(),"nlink":info.nlink(),"bytes":info.len(),"mtime":info.mtime(),
                "mtime_nsec":info.mtime_nsec(),"ctime":info.ctime(),"ctime_nsec":info.ctime_nsec()}),
        ))
    };
    let history_directory = history
        .parent()
        .ok_or("Missing rehearsal history directory")?;
    let names = |path: &Path| -> Result<BTreeSet<std::ffi::OsString>, String> {
        fs::read_dir(path)
            .map_err(io_error)?
            .map(|entry| {
                entry
                    .map(|entry| entry.file_name())
                    .map_err(|_| "Cannot read recovery history entry".into())
            })
            .collect()
    };
    let before = (
        snapshot(config)?,
        snapshot(history)?,
        names(history_directory)?,
    );
    let files = archive::decode(&native.bytes, archive::NATIVE)?.files;
    let package_root = prefix.join("lib/node_modules/claude-autorouter");
    let host = package::lifecycle::host_target()?;
    let musl = format!("{}-unknown-linux-musl", std::env::consts::ARCH);
    let target = if std::env::consts::OS == "linux"
        && matches!(std::env::consts::ARCH, "aarch64" | "x86_64")
        && files.contains_key(&format!("native/{musl}/claude-autorouter"))
    {
        musl.as_str()
    } else {
        host
    };
    let executable = package_root
        .join(format!("native/{target}/claude-autorouter"))
        .canonicalize()
        .map_err(io_error)?;
    let fake_bin = cwd.join("failed-startup-fake-bin");
    let observations = cwd.join("failed-startup-observations");
    for path in [&fake_bin, &observations] {
        private_dir(path)?;
    }
    let script = fake_bin.join("claude");
    write_private(&script, br#"#!/bin/sh
set -eu
umask 077
[ "$AUTOROUTER_EVALUATOR" = jev ] || exit 71
[ "$#" = 2 ] && [ "$1" = --settings ] || exit 72
[ -z "${TYPESAFE_API_KEY+x}${AUTOROUTER_TOKEN+x}${AUTOROUTER_CONFIG+x}" ] || exit 73
[ "${#ANTHROPIC_API_KEY}" = 64 ] || exit 74
[ -f "$2" ] && [ ! -L "$2" ] && [ -f "$AUTOROUTER_STATUS_FILE" ] && [ ! -L "$AUTOROUTER_STATUS_FILE" ] || exit 75
/bin/cp -p "$2" "$AUTOROUTER_REHEARSAL_WITNESS/settings.json"
/bin/cp -p "$AUTOROUTER_STATUS_FILE" "$AUTOROUTER_REHEARSAL_WITNESS/status.json"
printf '%s\n%s\n%s\n' "$ANTHROPIC_BASE_URL" "$AUTOROUTER_STATUS_FILE" "$2"
exit 23
"#)?;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
    let forbidden = TcpListener::bind("127.0.0.1:0").map_err(io_error)?;
    let sentinel = forbidden.local_addr().map_err(io_error)?;
    forbidden.set_nonblocking(true).map_err(io_error)?;
    // Establish permitted loopback before counting queued successful connections.
    let wait = Duration::from_millis(250);
    let probe = TcpStream::connect_timeout(&sentinel, wait)
        .map_err(|error| format!("Recovery positive connect: {:?}", error.kind()))?;
    let started = std::time::Instant::now();
    let mut accepted = forbidden.accept();
    while matches!(&accepted, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock) {
        std::thread::yield_now();
        if started.elapsed() >= wait {
            break;
        }
        accepted = forbidden.accept();
    }
    let (accepted, _) =
        accepted.map_err(|error| format!("Recovery positive accept: {:?}", error.kind()))?;
    drop((probe, accepted));
    let mut schedules = Vec::new();
    for (id, fake_child, code) in [
        ("fake_claude_nonzero", true, 23),
        ("missing_claude", false, 1),
    ] {
        let temporary = cwd.join(format!("failed-startup-{id}-temp"));
        private_dir(&temporary)?;
        let mut failure_env = env.clone();
        for (key, value) in [
            ("TMPDIR", temporary.clone().into_os_string()),
            ("AUTOROUTER_PORT", "0".into()),
            ("AUTOROUTER_DEBUG", "1".into()),
            ("AUTOROUTER_STATUSLINE", "1".into()),
            ("AUTOROUTER_CLIENT_PROFILE", "compatible".into()),
            (
                "AUTOROUTER_JEV_URL",
                format!("http://{sentinel}/v1/systemone").into(),
            ),
            (
                "AUTOROUTER_UPSTREAM_URL",
                format!("http://{sentinel}").into(),
            ),
        ] {
            failure_env.insert(key.into(), value);
        }
        if fake_child {
            failure_env.insert("PATH".into(), fake_bin.clone().into_os_string());
            failure_env.insert(
                "AUTOROUTER_REHEARSAL_WITNESS".into(),
                observations.clone().into_os_string(),
            );
        }
        // Each case starts from the original empty-PATH synthetic environment.
        let result = crate::process::capture_result(
            Command::new(prefix.join("bin/claude-autorouter"))
                .arg("claude")
                .env_clear()
                .envs(&failure_env)
                .current_dir(cwd),
            b"",
            Duration::from_secs(20),
        )?;
        let stderr = std::str::from_utf8(&result.stderr).map_err(|_| invalid)?;
        let first = stderr.lines().next().ok_or(invalid)?;
        let base = first
            .strip_prefix("AutoRouter listening on ")
            .and_then(|line| line.strip_suffix(" (api-key authentication)"))
            .ok_or(invalid)?;
        let address: SocketAddr = base
            .strip_prefix("http://")
            .ok_or(invalid)?
            .parse()
            .map_err(|_| invalid)?;
        let expected = format!(
            "AutoRouter listening on {base} (api-key authentication)\n\
AutoRouter uses Haiku-compatible requests with client thinking disabled; Jev selects the upstream model.\n{}",
            if fake_child {
                ""
            } else {
                "Could not launch Claude Code. Ensure `claude` is installed and on PATH.\n"
            }
        );
        if result.status.code() != Some(code)
            || stderr != expected
            || address.ip() != std::net::Ipv4Addr::LOCALHOST
            || address.port() == 0
        {
            return Err("Wrong installed failure result".into());
        }
        let stdout = std::str::from_utf8(&result.stdout).map_err(|_| invalid)?;
        let live_resources = if fake_child {
            let lines: Vec<_> = stdout.lines().collect();
            if lines.len() != 3
                || lines[0] != base
                || stdout != format!("{}\n{}\n{}\n", lines[0], lines[1], lines[2])
            {
                return Err("Missing live resource witness".into());
            }
            let status_path = Path::new(lines[1]);
            let settings_path = Path::new(lines[2]);
            let directory = status_path.parent().ok_or(invalid)?;
            if directory.parent() != Some(temporary.as_path())
                || settings_path.parent() != Some(directory)
                || status_path.file_name() != Some(OsStr::new("state.json"))
                || settings_path.file_name() != Some(OsStr::new("claude-settings.json"))
                || !directory
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.starts_with("autorouter-status-"))
            {
                return Err("Live resources escaped owned TMPDIR".into());
            }
            let copied = |name| -> Result<_, String> {
                let (bytes, metadata) = snapshot(&observations.join(name))?;
                let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid)?;
                Ok((
                    value,
                    hash(&bytes),
                    metadata["mode"].as_u64().ok_or(invalid)? & 0o7777,
                ))
            };
            let (status, status_hash, status_mode) = copied("status.json")?;
            let (settings, settings_hash, settings_mode) = copied("settings.json")?;
            let positive = |key: &str| status[key].as_u64().filter(|n| *n > 0).ok_or(invalid);
            let pid = positive("pid")?;
            let heartbeat = positive("heartbeat_at")?;
            if pid > u32::MAX as u64
                || !same_json(
                    &status,
                    &json!({"version":1,"pid":pid,"heartbeat_at":heartbeat,"sessions":{},"savings":{}}),
                )
                || !same_json(
                    &settings,
                    &json!({"statusLine":autorouter_runtime::status_settings::status_line_settings(&executable)}),
                )
                || status_mode != 0o600
                || settings_mode != 0o600
            {
                return Err("Wrong complete live resource snapshot".into());
            }
            for path in [status_path, settings_path, directory] {
                match fs::symlink_metadata(path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    _ => return Err("Live resources survived child failure".into()),
                }
            }
            json!({"status_path":status_path,"settings_path":settings_path,"status":status,"settings":settings,
                "status_sha256":status_hash,"settings_sha256":settings_hash,
                "status_copy_mode":status_mode,"settings_copy_mode":settings_mode,
                "original_resources_removed":true})
        } else {
            if !stdout.is_empty() {
                return Err("Missing-Claude unexpectedly returned child output".into());
            }
            Value::Null
        };
        match TcpStream::connect_timeout(&address, wait) {
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
            _ => return Err("Failed startup listener remained open".into()),
        }
        match forbidden.accept() {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            _ => return Err("Unexpected queued backend connection".into()),
        }
        if (
            snapshot(config)?,
            snapshot(history)?,
            names(history_directory)?,
        ) != before
        {
            return Err("Failed startup changed settings/history".into());
        }
        if !names(&temporary)?.is_empty() {
            return Err("Failed startup retained resources".into());
        }
        fs::remove_dir(&temporary).map_err(io_error)?;
        schedules.push(json!({"id":id,"exit_code":code,"stdout":stdout,"stderr":stderr,
            "gateway":base,"gateway_closed":true,"zero_accepted_or_queued_backend_connections":true,
            "live_resources":live_resources,
            "config_history_bytes_and_metadata_unchanged":true,"history_directory_entries_unchanged":true,
            "temporary_directory_empty":true}));
    }
    Ok(
        json!({"schema_version":1,"kind":"installed_failed_startup_recovery","schedules":schedules,
        "limits":["two_synthetic_same_prefix_schedules_only","no_connection_attempt_or_internal_close_order_claim",
            "no_partial_write_or_concurrent_owner_coverage","no_process_tree_or_crash_guarantee"]}),
    )
}
fn execute(root: &Path, native: &Path, output: &Path) -> Result<Value, String> {
    // Keep the admitted archive bytes, rather than reopening a path that may
    // change while the frozen baseline is prepared.
    let native = verify_native(native)?;
    if output.exists() {
        return Err("Rehearsal destination already exists; choose a fresh path".into());
    }
    fs::create_dir_all(output.parent().ok_or("Invalid rehearsal destination")?)
        .map_err(|_| "Cannot create rehearsal parent")?;
    fs::create_dir(output).map_err(|_| "Cannot create rehearsal destination")?;
    let scratch = Scratch::new("upgrade-rollback")?;
    let mut npm_env = crate::release_install::npm_environment(&scratch.0.join("npm"))
        .map_err(|_| "Cannot create isolated npm environment")?;
    npm_env.insert("HOME".into(), scratch.0.clone().into_os_string());
    let (baseline, commit) = baseline_archive(root, &scratch.0, output, &npm_env)?;
    let candidate = output.join("candidate.tgz");
    native.retain(&candidate)?;
    let prefix = scratch.0.join("install prefix ' $ with spaces");
    let (config, history, history_before) = fixture(&scratch.0)?;
    let mut env = Environment::new();
    for (key, value) in [
        ("HOME", scratch.0.clone().into_os_string()),
        ("XDG_CONFIG_HOME", scratch.0.clone().into_os_string()),
        ("AUTOROUTER_CONFIG", config.clone().into_os_string()),
        (
            "PATH",
            npm_env
                .get(OsStr::new("PATH"))
                .cloned()
                .ok_or("PATH is required for npm rehearsal")?,
        ),
    ] {
        env.insert(key.into(), value);
    }
    let replace = |archive, policy| install(archive, &prefix, &scratch.0, &npm_env, policy);
    replace(&baseline, archive::HISTORICAL)?;
    let read = |env| inspect(&prefix, &scratch.0, env);
    let set_port = |env, port| {
        command(
            &prefix,
            &scratch.0,
            env,
            &["config", "set", "AUTOROUTER_PORT", port],
        )
    };
    let original = read(&env)?;
    let config_bytes = || fs::read(&config).map_err(|_| "Cannot inspect synthetic configuration");
    let config_before = config_bytes()?;
    replace(&candidate, archive::NATIVE)?;
    let no_node = scratch.0.join("without-node");
    fs::create_dir(&no_node).map_err(|_| "Cannot isolate native PATH")?;
    let mut native_env = env.clone();
    native_env.insert("PATH".into(), no_node.into_os_string());
    if !same_json(&read(&native_env)?, &original) {
        return Err("Native upgrade changed configuration/history interpretation".into());
    }
    if config_bytes()? != config_before {
        return Err("Native read-only upgrade changed saved configuration".into());
    }
    let startup_failure =
        failed_startup(&prefix, &scratch.0, &native_env, &config, &history, &native)?;
    if !same_json(&read(&native_env)?, &original) {
        return Err("Native inspection did not recover after failed startup".into());
    }
    set_port(&native_env, "8008")?;
    let edited = read(&native_env)?;
    if edited["config"]["settings"]["AUTOROUTER_PORT"]["value"] != 8008 {
        return Err("Native edit was not persisted".into());
    }
    let saved = fs::read(&config).map_err(|_| "Cannot inspect updated configuration")?;
    let values: Value = serde_json::from_slice(&saved)
        .map_err(|_| "Invalid saved configuration after native edit")?;
    if values["ANTHROPIC_API_KEY"] != "synthetic-rehearsal-provider"
        || values["TYPESAFE_API_KEY"] != "synthetic-rehearsal-jev"
        || values["ENABLE_TOOL_SEARCH"] != "auto:5"
    {
        return Err("Native edit failed to preserve unrelated saved settings".into());
    }
    if fs::metadata(&config)
        .map_err(|_| "Cannot inspect configuration mode")?
        .permissions()
        .mode()
        & 0o777
        != 0o600
    {
        return Err("Updated configuration is not private".into());
    }
    replace(&baseline, archive::HISTORICAL)?;
    if !same_json(&read(&env)?, &edited) {
        return Err("Rollback cannot read native-edited settings and historical records".into());
    }
    set_port(&env, "8009")?;
    let rollback_edit = read(&env)?;
    replace(&candidate, archive::NATIVE)?;
    if !same_json(&read(&native_env)?, &rollback_edit) {
        return Err("Re-upgrade cannot read rollback-edited settings".into());
    }
    if fs::read(&history).map_err(|_| "Cannot inspect synthetic history")? != history_before {
        return Err("Upgrade/rollback changed historical bytes".into());
    }
    let baseline_hash = hash(&crate::release::regular(
        &baseline,
        archive::MAX_COMPRESSED as u64,
    )?);
    let private_directory = scratch.0.clone();
    drop(scratch);
    if private_directory.exists() {
        return Err("Rehearsal temporary directory cleanup did not complete".into());
    }
    let mut report = json!({"schema_version":1,"kind":"offline_upgrade_rollback_rehearsal","passed":true,"baseline_commit":commit,"baseline_archive_sha256":baseline_hash,"native_archive_sha256":hash(&native.bytes),"native_verification":native.verification,"platform":{"os":std::env::consts::OS,"arch":std::env::consts::ARCH},"stages":["baseline","native","rollback","native_after_rollback"],"verified":["exact_installed_archive_bytes","npm_command_symlink","offline_ignore_scripts","native_without_node_on_path","same_config_path_without_migration","existing_file_credentials_retained","private_atomic_edit","history_schema_1_and_2","recorded_pricing","history_bytes_unchanged","isolated_cleanup"],"limits":["local_host_only","synthetic_file_credentials_only","no_real_keychain_or_provider","no_publication_or_cutover_approval"]});
    report["failed_startup"] = startup_failure;
    Ok(report)
}
pub fn run(args: &[String], root: &Path) -> Result<bool, String> {
    if args == ["--help"] {
        println!(
            "Usage: cargo xtask upgrade-rollback --native ARCHIVE --output FRESH_DIRECTORY\nOffline same-prefix npm baseline/native/rollback rehearsal with synthetic settings/history. No user configuration, Keychain or provider access."
        );
        return Ok(true);
    }
    let mut native = None;
    let mut output = None;
    let mut seen = BTreeSet::new();
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        if !seen.insert(flag) {
            return Err("Duplicate rehearsal option".into());
        }
        let value = args.next().ok_or("Missing rehearsal option value")?;
        match flag.as_str() {
            "--native" => native = Some(root.join(value)),
            "--output" => output = Some(root.join(value)),
            _ => return Err("Unknown rehearsal option".into()),
        }
    }
    let native = native.ok_or("--native is required")?;
    let output = output.ok_or("--output is required")?;
    let report = execute(root, &native, &output)?;
    write_private(
        &output.join("report.json"),
        &(serde_json::to_string_pretty(&report).map_err(|_| "Cannot encode rehearsal report")?
            + "\n")
            .into_bytes(),
    )?;
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A header-inspection fixture only: it is never executed and carries no
    // source-build, license, platform or release qualification.
    fn native_fixture(marker: &str) -> Vec<u8> {
        let target = "aarch64-apple-darwin";
        let mut executable = vec![0u8; 32];
        executable[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        executable[4..8].copy_from_slice(&0x0100000cu32.to_le_bytes());
        executable[12..16].copy_from_slice(&2u32.to_le_bytes());
        let native = format!("native/{target}/claude-autorouter");
        let mut files = BTreeMap::new();
        for path in [
            "README.md",
            "LICENSE",
            "THIRD-PARTY-LICENSES.txt",
            "platforms.json",
        ] {
            files.insert(
                path.to_owned(),
                archive::Entry {
                    bytes: format!("Synthetic unqualified fixture: {marker}").into_bytes(),
                    mode: 0o644,
                },
            );
        }
        files.insert(
            "package.json".into(),
            archive::Entry {
                bytes: serde_json::to_vec(&json!({"name":"claude-autorouter","version":"0.5.2","private":true,"bin":{"claude-autorouter":"bin/autorouter"}})).unwrap(),
                mode: 0o644,
            },
        );
        files.insert(
            "bin/autorouter".into(),
            archive::Entry {
                bytes: include_bytes!("../../distribution/autorouter.sh").to_vec(),
                mode: 0o755,
            },
        );
        files.insert(
            native.clone(),
            archive::Entry {
                bytes: executable.clone(),
                mode: 0o755,
            },
        );
        let declarations = files.iter().map(|(path,entry)|json!({"path":path,"sha256":hash(&entry.bytes),"bytes":entry.bytes.len(),"mode":entry.mode})).collect::<Vec<_>>();
        let build = json!({"schema_version":1,"kind":"native_npm_feasibility","release_approved":false,"version":"0.5.2","source":{"commit":"a".repeat(40),"dirty":true,"cargo_lock_sha256":"b".repeat(64),"provenance":"unverified_prebuilt"},"files":declarations,"artifacts":[{"target":target,"path":native,"sha256":hash(&executable),"bytes":executable.len(),"inspection":package::binary::inspect(target,&executable).unwrap()}],"dependencies":[],"license_files_missing":[],"license_review":"pending","platform_qualification":"pending"});
        files.insert(
            "build-manifest.json".into(),
            archive::Entry {
                bytes: serde_json::to_vec(&build).unwrap(),
                mode: 0o644,
            },
        );
        archive::encode_root("package", &files, archive::NATIVE).unwrap()
    }

    #[test]
    fn retained_candidate_keeps_verified_bytes_after_source_replacement_or_removal() {
        let original = native_fixture("admitted");
        let replacement = native_fixture("replacement");
        assert_ne!(hash(&original), hash(&replacement));
        for replace in [false, true] {
            let scratch = Scratch::new("upgrade-owned-input").unwrap();
            let source = scratch.0.join("source.tgz");
            write_private(&source, &original).unwrap();
            let admitted = verify_native(&source).unwrap();
            assert_eq!(admitted.verification["verified"], true);
            assert_eq!(admitted.verification["release_approved"], false);
            assert_eq!(
                admitted.verification["source_to_binary_provenance"],
                "unverified"
            );
            assert_eq!(admitted.verification["platform_qualification"], "pending");
            assert_eq!(admitted.verification["sha256"], hash(&original));

            fs::remove_file(&source).unwrap();
            if replace {
                write_private(&source, &replacement).unwrap();
                assert_eq!(
                    verify_native(&source).unwrap().verification["sha256"],
                    hash(&replacement)
                );
            }
            let retained = scratch.0.join("candidate.tgz");
            admitted.retain(&retained).unwrap();
            assert_eq!(fs::read(&retained).unwrap(), original);
            assert_eq!(
                verify_native(&retained).unwrap().verification["sha256"],
                admitted.verification["sha256"]
            );
            assert_eq!(
                fs::metadata(&retained).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn invalid_or_oversized_native_input_fails_before_output_creation() {
        let scratch = Scratch::new("upgrade-invalid-input").unwrap();
        for kind in ["missing", "malformed", "oversized", "symlink"] {
            let source = scratch.0.join(format!("{kind}.tgz"));
            match kind {
                "malformed" => write_private(&source, b"not a native archive").unwrap(),
                "oversized" => {
                    write_private(&source, b"").unwrap();
                    fs::OpenOptions::new()
                        .write(true)
                        .open(&source)
                        .unwrap()
                        .set_len(archive::MAX_COMPRESSED as u64 + 1)
                        .unwrap();
                }
                "symlink" => {
                    let real = scratch.0.join("valid.tgz");
                    write_private(&real, &native_fixture("untrusted link")).unwrap();
                    std::os::unix::fs::symlink(real, &source).unwrap();
                }
                _ => {}
            }
            let output = scratch.0.join(kind).join("new-output");
            assert!(execute(&scratch.0, &source, &output).is_err());
            assert!(!output.parent().unwrap().exists());
        }
    }

    #[test]
    fn frozen_public_allowlist_does_not_pick_up_adjacent_private_files() {
        let patterns = vec![
            json!("src/*.mjs"),
            json!("bin/*.mjs"),
            json!("docs/reference.md"),
        ];
        for path in [
            "README.md",
            "LICENSE",
            "package.json",
            "src/router.mjs",
            "bin/autorouter.mjs",
            "docs/reference.md",
        ] {
            assert!(public_path(path, &patterns));
        }
        for path in [
            ".env",
            "src/private.json",
            "src/sub/private.mjs",
            "docs/session.jsonl",
            "artifacts/config.json",
            "AGENTS.md",
            "test/router.test.mjs",
        ] {
            assert!(!public_path(path, &patterns));
        }
    }
    #[test]
    fn options_fail_before_creating_an_output_or_invoking_npm() {
        for args in [
            vec!["--output", "unused"],
            vec!["--native", "missing"],
            vec!["--native", "missing", "--native", "other"],
            vec!["--unknown", "value"],
        ] {
            assert!(
                run(
                    &args.into_iter().map(str::to_owned).collect::<Vec<_>>(),
                    Path::new("/synthetic")
                )
                .is_err()
            );
        }
    }
}
