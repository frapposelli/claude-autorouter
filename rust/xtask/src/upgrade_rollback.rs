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
fn same_json(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_json(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, value)| b.get(key).is_some_and(|other| same_json(value, other)))
        }
        _ => left == right,
    }
}
fn verify_native(path: &Path) -> Result<Value, String> {
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
    if manifest["private"] == true {
        return package::verify_decoded(&bytes, &decoded);
    }
    let build = crate::release::native_files(files, &manifest)?;
    Ok(
        json!({"verified":true,"kind":"native_npm_candidate","release_approved":false,"final_authorization":false,"sha256":hash(&bytes),"source":build["source"],"qualification":build["qualification"]}),
    )
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
fn execute(root: &Path, native: &Path, output: &Path) -> Result<Value, String> {
    let native_verification = verify_native(native)?;
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
    let native_bytes = crate::release::regular(native, archive::MAX_COMPRESSED as u64)?;
    let candidate = output.join("candidate.tgz");
    write_private(&candidate, &native_bytes)?;
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
    install(
        &baseline,
        &prefix,
        &scratch.0,
        &npm_env,
        archive::HISTORICAL,
    )?;
    let original = inspect(&prefix, &scratch.0, &env)?;
    let config_before = fs::read(&config).map_err(|_| "Cannot inspect synthetic configuration")?;
    install(&candidate, &prefix, &scratch.0, &npm_env, archive::NATIVE)?;
    let no_node = scratch.0.join("without-node");
    fs::create_dir(&no_node).map_err(|_| "Cannot isolate native PATH")?;
    let mut native_env = env.clone();
    native_env.insert("PATH".into(), no_node.into_os_string());
    if !same_json(&inspect(&prefix, &scratch.0, &native_env)?, &original) {
        return Err("Native upgrade changed configuration/history interpretation".into());
    }
    if fs::read(&config).map_err(|_| "Cannot inspect synthetic configuration")? != config_before {
        return Err("Native read-only upgrade changed saved configuration".into());
    }
    command(
        &prefix,
        &scratch.0,
        &native_env,
        &["config", "set", "AUTOROUTER_PORT", "8008"],
    )?;
    let edited = inspect(&prefix, &scratch.0, &native_env)?;
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
    install(
        &baseline,
        &prefix,
        &scratch.0,
        &npm_env,
        archive::HISTORICAL,
    )?;
    if !same_json(&inspect(&prefix, &scratch.0, &env)?, &edited) {
        return Err("Rollback cannot read native-edited settings and historical records".into());
    }
    command(
        &prefix,
        &scratch.0,
        &env,
        &["config", "set", "AUTOROUTER_PORT", "8009"],
    )?;
    let rollback_edit = inspect(&prefix, &scratch.0, &env)?;
    install(&candidate, &prefix, &scratch.0, &npm_env, archive::NATIVE)?;
    if !same_json(&inspect(&prefix, &scratch.0, &native_env)?, &rollback_edit) {
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
    Ok(
        json!({"schema_version":1,"kind":"offline_upgrade_rollback_rehearsal","passed":true,"baseline_commit":commit,"baseline_archive_sha256":baseline_hash,"native_archive_sha256":hash(&native_bytes),"native_verification":native_verification,"platform":{"os":std::env::consts::OS,"arch":std::env::consts::ARCH},"stages":["baseline","native","rollback","native_after_rollback"],"verified":["exact_installed_archive_bytes","npm_command_symlink","offline_ignore_scripts","native_without_node_on_path","same_config_path_without_migration","existing_file_credentials_retained","private_atomic_edit","history_schema_1_and_2","recorded_pricing","history_bytes_unchanged","isolated_cleanup"],"limits":["local_host_only","synthetic_file_credentials_only","no_real_keychain_or_provider","no_publication_or_cutover_approval"]}),
    )
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
