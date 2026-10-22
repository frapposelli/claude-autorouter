//! Explicit matched live trials; prepare invokes metadata commands only.
#[path = "compare_live_schema.rs"]
mod schema;
#[cfg(test)]
#[path = "compare_live_tests.rs"]
mod tests;
use crate::audit_support::{digest, regular};
use crate::compare_local_evaluators::{
    check_private_directory, conditions, copy_executable, metadata_command, private_directory,
    write_json,
};
use crate::process::owned::Owner;
use autorouter_core::auth::Environment;
use autorouter_core::config::{Evaluator, read_config, require_keys};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use tokio_util::sync::CancellationToken;

const PROTOCOL: &str = include_str!("../../parity/live-comparison-protocol-v2.json");
const ORCHESTRATION: &str = include_str!("../../parity/live-orchestration-v1.json");
const MAX: u64 = 2 * 1024 * 1024;
const MAX_BINARY: u64 = 256 * 1024 * 1024;
const CONDITION: &str = "normal-use-builds-paused-auth-unchanged";
const CASES: &[&str] = &[
    "simple",
    "medium",
    "difficult",
    "coding",
    "continuation",
    "thinking_continuation",
];
const PROFILES: &[&str] = &["compatible", "auto"];

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    schema_version: u32,
    kind: String,
    prepared_at: String,
    source_commit: String,
    identities: Value,
    programs: Value,
    configuration: Value,
    metadata: Value,
    operator_declared: Value,
    observed: Value,
    credentials_persisted: bool,
    authentication_verified: bool,
    inference_performed: bool,
    source_to_binary_provenance_verified: bool,
}

#[derive(Debug)]
enum Action {
    Help,
    Prepare {
        output: PathBuf,
        node: PathBuf,
        claude: PathBuf,
        cargo: PathBuf,
        commit: String,
    },
    Run {
        declaration: PathBuf,
        output: PathBuf,
    },
}
fn parse_args(args: &[String], cwd: &Path) -> Result<Action, String> {
    if args == ["--help"] {
        return Ok(Action::Help);
    }
    let Some(action) = args.first() else {
        return Err("Use compare-live --help".into());
    };
    let mut values = BTreeMap::new();
    for pair in args[1..].chunks(2) {
        if pair.len() != 2
            || pair[1].is_empty()
            || pair[1].starts_with("--")
            || values.insert(pair[0].as_str(), pair[1].as_str()).is_some()
        {
            return Err("Unknown, repeated or incomplete live-comparison option".into());
        }
    }
    if values.remove("--conditions") != Some(CONDITION) {
        return Err("Explicit live-comparison conditions are required".into());
    }
    let output = cwd.join(
        values
            .remove("--output")
            .ok_or("A new private output is required")?,
    );
    match action.as_str() {
        "prepare" => {
            let node = cwd.join(
                values
                    .remove("--node")
                    .ok_or("A pinned Node executable is required")?,
            );
            let claude = cwd.join(
                values
                    .remove("--claude")
                    .ok_or("The existing Claude executable is required")?,
            );
            let cargo = cwd.join(
                values
                    .remove("--cargo")
                    .ok_or("The existing Cargo executable is required")?,
            );
            let commit = values
                .remove("--source-commit")
                .ok_or("Declare the source commit before live trials")?;
            if commit.len() != 40
                || !commit.bytes().all(|b| b.is_ascii_hexdigit())
                || !values.is_empty()
            {
                return Err("Invalid source identity or unsupported live option".into());
            }
            Ok(Action::Prepare {
                output,
                node,
                claude,
                cargo,
                commit: commit.to_ascii_lowercase(),
            })
        }
        "run" => {
            let declaration = cwd.join(
                values
                    .remove("--declaration")
                    .ok_or("A prepared live declaration is required")?,
            );
            if !values.is_empty() {
                return Err("Unsupported live-comparison run option".into());
            }
            Ok(Action::Run {
                declaration,
                output,
            })
        }
        _ => Err("Use compare-live --help".into()),
    }
}

fn profile_env(profile: &str, base: &Environment) -> Environment {
    let mut env = base.clone();
    for name in ["NODE_OPTIONS", "NODE_PATH", "BASH_ENV", "ENV"] {
        env.remove(&OsString::from(name));
    }
    env.insert("AUTOROUTER_CLIENT_PROFILE".into(), profile.into());
    env.insert("AUTOROUTER_AUTH_MODE".into(), "subscription".into());
    env
}
fn value_env(env: &Environment) -> Value {
    json!(
        env.iter()
            .map(|(k, v)| (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned()
            ))
            .collect::<BTreeMap<_, _>>()
    )
}
fn configuration(root: &Path, base: &Environment) -> Result<Value, String> {
    let mut result = json!({});
    for profile in PROFILES {
        let env = profile_env(profile, base);
        let config = read_config(&value_env(&env), false, root)?;
        require_keys(&config)?;
        if config.evaluator != Evaluator::Jev {
            return Err(
                "This paired live protocol requires the explicitly configured hosted Jev evaluator"
                    .into(),
            );
        }
        let model = if *profile == "compatible" {
            &config.models.haiku
        } else {
            &config.models.sonnet
        };
        let projection = json!({"evaluator":"jev","authMode":"subscription","clientProfile":profile,
            "models":config.models,"jevModel":config.jev_model,"jevTimeoutMs":config.jev_timeout_ms,
            "jevEndpointSha256":digest(config.jev_endpoint.as_bytes()),"upstreamSha256":digest(config.upstream.as_bytes()),
            "tokenCountTimeoutMs":config.token_count_timeout_ms,"minConfidence":config.min_confidence,
            "stateChars":config.state_chars,"maxBodyBytes":config.max_body_bytes,"cacheEntries":config.cache_entries,
            "cacheTtlMs":config.cache_ttl_ms,"turnTtlMs":config.turn_ttl_ms,"turnEntries":config.turn_entries,
            "upstreamTimeoutMs":config.upstream_timeout_ms});
        if secrets(base).iter().any(|s| {
            config.jev_endpoint.contains(s.as_str()) || config.upstream.contains(s.as_str())
        }) {
            return Err("Credential-like endpoint cannot be fingerprinted".into());
        }
        // Header values can contain credentials. Only a presence flag is durable;
        // actual values remain in the inherited environment, never a fingerprint.
        let parent = value_env(base);
        let thinking = parent
            .get("MAX_THINKING_TOKENS")
            .cloned()
            .unwrap_or(Value::Null);
        if !thinking.is_null()
            && !thinking.as_str().is_some_and(|s| {
                !s.is_empty() && s.len() <= 10 && s.bytes().all(|b| b.is_ascii_digit())
            })
        {
            return Err("Unrecognized thinking condition cannot enter public live identity".into());
        }
        result[profile] = json!({"config":projection,"client_model":model,"permission_mode":if *profile=="auto"{"auto"}else{"dontAsk"},"thinking_env":thinking,"custom_headers_present":parent.get("ANTHROPIC_CUSTOM_HEADERS").is_some(),"evaluator_credential_present":config.jev_key.as_ref().is_some_and(|s|!s.is_empty())});
    }
    Ok(result)
}

fn secrets(base: &Environment) -> Vec<String> {
    let mut values = Vec::new();
    for (key, value) in base {
        let key = key.to_string_lossy().to_ascii_uppercase();
        let value = value.to_string_lossy();
        let exact = [
            "TYPESAFE_API_KEY",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "AUTOROUTER_TOKEN",
        ]
        .contains(&key.as_str());
        let sensitive = key.ends_with("_KEY")
            || key.ends_with("_TOKEN")
            || key.contains("AUTH_HEADER")
            || key.ends_with("_HEADERS")
            || ["SECRET", "PASSWORD", "COOKIE", "CREDENTIAL"]
                .iter()
                .any(|word| key.contains(word));
        if !value.is_empty() && (exact || (sensitive && value.len() >= 8)) {
            values.push(value.into_owned());
            if key.contains("HEADERS") {
                for line in values.last().unwrap().clone().lines() {
                    if let Some((_, value)) = line.split_once(':') {
                        let value = value.trim();
                        if value.len() >= 8 {
                            values.push(value.into());
                        }
                    }
                }
            }
        }
    }
    values
}
fn private_material(value: &Value, secrets: &[String]) -> bool {
    match value {
        Value::String(s) => secrets.iter().any(|secret| s.contains(secret.as_str())),
        Value::Array(a) => a.iter().any(|v| private_material(v, secrets)),
        Value::Object(o) => o.iter().any(|(k, v)| {
            secrets.iter().any(|s| k.contains(s.as_str())) || private_material(v, secrets)
        }),
        _ => false,
    }
}
fn identities(root: &Path, programs: &Value) -> Result<Value, String> {
    let mut files = json!({});
    for (path, embedded) in [
        (
            "rust/parity/live-comparison-protocol-v2.json",
            Some(PROTOCOL),
        ),
        (
            "rust/parity/live-orchestration-v1.json",
            Some(ORCHESTRATION),
        ),
        ("rust/xtask/fixtures/live-cases-v2.json", None),
        ("rust/xtask/fixtures/merge-intervals.rs", None),
        ("rust/xtask/fixtures/merge-intervals.test.rs", None),
        ("rust/xtask/fixtures/repair-cargo.toml", None),
        ("rust/parity/live-gateway.mjs", None),
        ("rust/crates/autorouter-core/src/evaluation_report.rs", None),
    ] {
        let bytes = regular(&root.join(path), MAX)?;
        if embedded.is_some_and(|s| s.as_bytes() != bytes) {
            return Err("Frozen live protocol differs from helper source".into());
        }
        files[path] = json!({"sha256":digest(&bytes),"bytes":bytes.len()});
    }
    let reference = root.join("artifacts/rust-rewrite/reference");
    if crate::reference::verify(root, &reference)? != 111 {
        return Err("Complete frozen Node reference required".into());
    }
    let mut binaries = json!({});
    for name in ["node", "native", "claude", "cargo"] {
        let path = programs[name]["path"]
            .as_str()
            .ok_or("Missing prepared executable identity")?;
        binaries[name] = json!(digest(&regular(Path::new(path), MAX_BINARY)?));
    }
    Ok(
        json!({"files":files,"binaries":binaries,"native_sources":crate::bundle::source_identity(root)?,"baseline_manifest_sha256":digest(&regular(&root.join("rust/parity/baseline.json"),MAX)?),"reference_files":111}),
    )
}

fn metadata_env(command: &mut Command, env: &Environment) {
    command.env_clear();
    for name in [
        "PATH",
        "HOME",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "RUSTUP_TOOLCHAIN",
    ] {
        if let Some(value) = env.get(&OsString::from(name)) {
            command.env(name, value);
        }
    }
}
async fn metadata(
    program: &Path,
    option: &str,
    name: &str,
    cancel: &CancellationToken,
    base: &Environment,
) -> Result<String, String> {
    let mut command = Command::new(program);
    std::os::unix::process::CommandExt::arg0(&mut command, name);
    command.arg(option);
    metadata_env(&mut command, base);
    let bytes = metadata_command(command, cancel).await?;
    let text = String::from_utf8(bytes).map_err(|_| "Invalid client metadata")?;
    if text.len() > 65536 || secrets(base).iter().any(|s| text.contains(s.as_str())) {
        return Err("Private or oversized client metadata rejected".into());
    }
    if option == "--version"
        && (text.len() > 200
            || !text
                .trim()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || " .()/+-_".contains(c)))
    {
        return Err("Unrecognized client version metadata".into());
    }
    if name == "node" && text.trim() != "v22.14.0" {
        return Err("Pinned Node22.14.0 required".into());
    }
    Ok(text.trim().into())
}

async fn prepare(
    action: Action,
    root: &Path,
    cancel: &CancellationToken,
    base: &Environment,
) -> Result<bool, String> {
    let Action::Prepare {
        output,
        node,
        claude,
        cargo,
        commit,
    } = action
    else {
        return Err("Invalid live preparation".into());
    };
    let native = std::env::current_exe()
        .map_err(|_| "Cannot resolve native helper")?
        .canonicalize()
        .map_err(|_| "Cannot resolve native helper")?;
    let mut programs = json!({});
    for (name, path) in [
        ("node", node),
        ("claude", claude),
        ("cargo", cargo),
        ("native", native),
    ] {
        let path = path
            .canonicalize()
            .map_err(|_| "Missing existing live executable")?;
        programs[name] = json!({"path":path});
    }
    let before = identities(root, &programs)?;
    let config = configuration(root, base)?;
    let node_version = metadata(
        Path::new(programs["node"]["path"].as_str().unwrap()),
        "--version",
        "node",
        cancel,
        base,
    )
    .await?;
    let claude_version = metadata(
        Path::new(programs["claude"]["path"].as_str().unwrap()),
        "--version",
        "claude",
        cancel,
        base,
    )
    .await?;
    if claude_version.len() > 100 {
        return Err("Claude version exceeds original live report metadata width".into());
    }
    let help = metadata(
        Path::new(programs["claude"]["path"].as_str().unwrap()),
        "--help",
        "claude",
        cancel,
        base,
    )
    .await?;
    for flag in [
        "--safe-mode",
        "--restricted",
        "--permission-mode",
        "--no-session-persistence",
        "--strict-mcp-config",
    ] {
        if !help.contains(flag) {
            return Err("Existing Claude lacks required isolated validation flags".into());
        }
    }
    let cargo_version = metadata(
        Path::new(programs["cargo"]["path"].as_str().unwrap()),
        "--version",
        "cargo",
        cancel,
        base,
    )
    .await?;
    let scratch = crate::tool_process::Scratch::new("paired-live-prepare")?;
    let observed = conditions(cancel, &scratch.0).await;
    if cancel.is_cancelled()
        || identities(root, &programs)? != before
        || configuration(root, base)? != config
    {
        return Err("Live identity changed or preparation cancelled".into());
    }
    let directory = private_directory(&output)?;
    for name in ["node", "native"] {
        let copy = directory.join(name);
        let copied = copy_executable(Path::new(programs[name]["path"].as_str().unwrap()), &copy)?;
        programs[name] = json!({"path":copy,"copy":copied});
    }
    if identities(root, &programs)? != before {
        return Err("Executable copies or source changed before declaration".into());
    }
    let declaration = json!({"schema_version":1,"kind":"prepared_paired_live_language_port_comparison","prepared_at":autorouter_runtime::server_events::timestamp(),"source_commit":commit,"identities":before,"programs":programs,"configuration":config,"metadata":{"node_version":node_version,"claude_version":claude_version,"claude_help_sha256":digest(help.as_bytes()),"cargo_version":cargo_version,"max_turns_supported":help.contains("--max-turns")},"operator_declared":{"normal_use":true,"builds_tests_paused":true,"same_authenticated_account_and_credential_settings":true,"source_commit_independently_recorded":true},"observed":observed,"credentials_persisted":false,"authentication_verified":false,"inference_performed":false,"source_to_binary_provenance_verified":false});
    if private_material(&declaration, &secrets(base)) {
        return Err("Private configuration cannot enter a live declaration".into());
    }
    write_json(&directory.join("declaration.json"), &declaration)?;
    write_json(
        &directory.join("declaration-checksum.json"),
        &json!({"sha256":digest(&regular(&directory.join("declaration.json"),MAX)?)}),
    )?;
    println!("Prepared matched live trial; no authentication or inference was tested.");
    Ok(true)
}

fn declaration(path: &Path) -> Result<Value, String> {
    check_private_directory(path)?;
    let bytes = regular(&path.join("declaration.json"), MAX)?;
    let typed: Declaration = serde_json::from_slice(&bytes)
        .map_err(|_| "Invalid or ambiguous paired live declaration")?;
    let value = serde_json::to_value(typed).map_err(|_| "Invalid paired live declaration")?;
    let checksum: Value =
        serde_json::from_slice(&regular(&path.join("declaration-checksum.json"), MAX)?)
            .map_err(|_| "Missing paired live checksum")?;
    if checksum["sha256"] != digest(&bytes)
        || value["schema_version"] != 1
        || value["kind"] != "prepared_paired_live_language_port_comparison"
        || value["inference_performed"] != false
        || value["credentials_persisted"] != false
        || value["authentication_verified"] != false
        || value["source_to_binary_provenance_verified"] != false
        || value["metadata"]["node_version"] != "v22.14.0"
        || !value["metadata"]["max_turns_supported"].is_boolean()
        || !value["source_commit"]
            .as_str()
            .is_some_and(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err("Paired live declaration checksum or scope differs".into());
    }
    Ok(value)
}
fn recheck(root: &Path, declaration: &Value, base: &Environment) -> Result<(), String> {
    let actual = std::env::current_exe().map_err(|_| "Cannot resolve running live helper")?;
    if digest(&regular(&actual, MAX_BINARY)?) != declaration["identities"]["binaries"]["native"]
        || identities(root, &declaration["programs"])? != declaration["identities"]
        || configuration(root, base)? != declaration["configuration"]
    {
        return Err(
            "Source, reference, client, helper or live configuration drifted; prepare anew".into(),
        );
    }
    Ok(())
}
fn child(
    root: &Path,
    declaration: &Value,
    tools: &Path,
    profile: &str,
    engine: &str,
    base: &Environment,
) -> Command {
    let mut command = Command::new(declaration["programs"]["native"]["path"].as_str().unwrap());
    command.args([
        "live-validation",
        "--engine",
        engine,
        "--permission-mode",
        if profile == "auto" { "auto" } else { "dontAsk" },
        "--client-model",
        declaration["configuration"][profile]["client_model"]
            .as_str()
            .unwrap(),
        "--timeout-ms",
        "120000",
    ]);
    let mut env = profile_env(profile, base);
    let mut path = tools.as_os_str().to_os_string();
    path.push(":");
    if let Some(existing) = env.get(&OsString::from("PATH")) {
        path.push(existing);
    }
    env.insert("PATH".into(), path);
    env.insert(
        "ANTHROPIC_MODEL".into(),
        declaration["configuration"][profile]["client_model"]
            .as_str()
            .unwrap()
            .into(),
    );
    command.current_dir(root).env_clear().envs(env);
    command
}
async fn execute(
    root: &Path,
    path: &Path,
    output: &Path,
    cancel: &CancellationToken,
    base: &Environment,
    transform: impl Fn(Command) -> Command,
) -> Result<bool, String> {
    let declaration = declaration(path)?;
    if private_material(&declaration, &secrets(base)) {
        return Err("Private live declaration rejected without persistence".into());
    }
    recheck(root, &declaration, base)?;
    let output = private_directory(output)?;
    write_json(&output.join("declaration.json"), &declaration)?;
    let tools = output.join("programs");
    private_directory(&tools)?;
    for name in ["node", "claude", "cargo"] {
        std::os::unix::fs::symlink(
            declaration["programs"][name]["path"].as_str().unwrap(),
            tools.join(name),
        )
        .map_err(|_| "Cannot create owned live executable dispatch")?;
    }
    let private = secrets(base);
    let mut pairs = Vec::new();
    let mut failure = None;
    'trial: for profile in PROFILES {
        for pair in 1..=5 {
            let order = if pair % 2 == 1 {
                ["node-reference", "native"]
            } else {
                ["native", "node-reference"]
            };
            let mut reports = json!({});
            for engine in order {
                if cancel.is_cancelled() {
                    failure = Some("cancelled");
                    break 'trial;
                }
                if recheck(root, &declaration, base).is_err() {
                    failure = Some("identity_drift");
                    break 'trial;
                }
                let before = conditions(cancel, &output).await;
                write_json(
                    &output.join(format!("{profile}-{pair}-{engine}-before.json")),
                    &before,
                )?;
                let mut owner = Owner::start(
                    transform(child(root, &declaration, &tools, profile, engine, base)),
                    cancel,
                );
                let result = tokio::select! {biased;_=cancel.cancelled()=>owner.close().await,result=owner.join()=>result};
                let status = match &result {
                    Ok(v) => v.status.clone(),
                    Err(e) => e.evidence(),
                };
                write_json(
                    &output.join(format!("{profile}-{pair}-{engine}-execution.json")),
                    &status,
                )?;
                let after = conditions(&CancellationToken::new(), &output).await;
                write_json(
                    &output.join(format!("{profile}-{pair}-{engine}-after.json")),
                    &after,
                )?;
                let report = match result {
                    Ok(result) => schema::extract(
                        &result.stdout,
                        &status,
                        &declaration,
                        profile,
                        engine,
                        &private,
                    ),
                    Err(_) => Err("owned_worker_failure".into()),
                };
                match report {
                    Ok(report) => {
                        write_json(
                            &output.join(format!("{profile}-{pair}-{engine}-report.json")),
                            &report,
                        )?;
                        reports[engine] = report;
                    }
                    Err(error) => {
                        let safe = if private_material(&json!(error), &private) {
                            "private_output_rejected".to_owned()
                        } else {
                            error
                        };
                        write_json(
                            &output.join(format!("{profile}-{pair}-{engine}-rejected.json")),
                            &json!({"complete":false,"rejection_reason":safe,"nested_cleanup_verified":false}),
                        )?;
                        failure = Some("invalid_or_incomplete_engine_report");
                        if cancel.is_cancelled() {
                            failure = Some("cancelled");
                        }
                        break 'trial;
                    }
                }

                if recheck(root, &declaration, base).is_err() {
                    failure = Some("identity_drift");
                    break 'trial;
                }
                if cancel.is_cancelled() {
                    failure = Some("cancelled");
                    break 'trial;
                }
            }
            let comparison = schema::compare(&reports, profile);
            write_json(
                &output.join(format!("{profile}-{pair}-comparison.json")),
                &comparison,
            )?;
            pairs
                .push(json!({"profile":profile,"pair":pair,"order":order,"comparison":comparison}));
        }
    }
    let complete = failure.is_none()
        && pairs.len() == 10
        && pairs.iter().all(|p| p["comparison"]["complete"] == true);
    let relative = complete
        && pairs
            .iter()
            .all(|p| p["comparison"]["relative_gates_passed"] == true);
    let absolute = complete
        && pairs
            .iter()
            .all(|p| p["comparison"]["absolute_original_quality_passed"] == true);
    write_json(
        &output.join("comparison.json"),
        &json!({"schema_version":1,"kind":"paired_live_language_port_comparison","complete":complete,"failure":failure,"pairs":pairs,"relative_gates_passed":relative,"absolute_original_quality_passed":absolute,"credentials_persisted":false,"provider_inference_opted_in":true,"actual_provider_calls_not_a_fixed_budget":true,"controlled_hardware_or_shipping_qualified":false,"nested_abnormal_group_cleanup_verified":false,"source_commit_verification":"operator declared; independently capture before approval","source_to_binary_provenance_verified":false}),
    )?;
    println!(
        "Matched live comparison retained privately; inherited quality and incomplete cleanup remain separate."
    );
    Ok(relative)
}
pub fn run(args: &[String], root: &Path) -> Result<bool, String> {
    let action = parse_args(
        args,
        &std::env::current_dir().map_err(|_| "Cannot resolve paired live directory")?,
    )?;
    if matches!(action, Action::Help) {
        println!(
            "compare-live prepare --node NODE22.14 --claude EXISTING_CLAUDE --cargo EXISTING_CARGO --source-commit SHA40 --conditions normal-use-builds-paused-auth-unchanged --output NEW_DIRECTORY\ncompare-live run --declaration DIRECTORY --conditions normal-use-builds-paused-auth-unchanged --output NEW_DIRECTORY\nPrepare calls only bounded local help/version metadata and persists no credentials; authentication remains untested. Run explicitly performs20 isolated live-validation invocations (120 synthetic cases, additional continuation/tool/provider calls) using existing Claude login and hosted Jev settings. No downloads, retries or permission bypass. Full default profile and all original/relative gates are retained; raw stderr and invalid stdout never persist. Outputs are private/create-only; abnormal nested process cleanup remains unverified."
        );
        return Ok(true);
    }
    crate::evaluation::runtime()?.block_on(async {
        let signals = crate::tool_process::Signals::new()?;
        let base = crate::tool_process::environment();
        match action {
            Action::Prepare { .. } => prepare(action, root, &signals.token, &base).await,
            Action::Run {
                declaration,
                output,
            } => {
                execute(
                    root,
                    &declaration,
                    &output,
                    &signals.token,
                    &base,
                    |command| command,
                )
                .await
            }
            Action::Help => unreachable!(),
        }
    })
}

#[cfg(test)]
pub(crate) fn original_route_shape(value: &Value) -> bool {
    schema::original_route_shape(value)
}
