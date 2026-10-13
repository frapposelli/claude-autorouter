//! Portable opt-in paired local-model trials; prepare never makes inference.
#[cfg(test)]
#[path = "compare_local_integration.rs"]
mod integration;
#[path = "compare_local_schema.rs"]
mod schema;
use crate::audit_support::{digest, regular};
use crate::process::owned::Owner;
use autorouter_core::config::{validate_ollama_endpoint, validate_ollama_model};
use autorouter_runtime::bounded_json::read_response_document;
use autorouter_runtime::evaluator::ollama_questions;
use autorouter_runtime::http_client::{HttpTransport, NativeHttpClient};
use bytes::Bytes;
use http_body_util::Full;
use hyper::Request;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const PROTOCOL: &str = include_str!("../../parity/local-model-orchestration-v1.json");
const V1: &str = include_str!("../../parity/local-model-comparison-v1.json");
const V2: &str = include_str!("../../parity/local-model-comparison-v2.json");
const FIXTURE: &str = "test/fixtures/ollama-routing.json";
const FIXTURE_SHA: &str = "1ee6ab593e47d87a67e922c0afb0c9db5787d8a27d1a0ff3e24baa0543291fc5";
const MAX_REPORT: u64 = 2 * 1024 * 1024;
const MAX_BINARY: u64 = 256 * 1024 * 1024;
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    schema_version: u32,
    kind: String,
    prepared_at: String,
    reference: String,
    endpoint: String,
    model: Value,
    warm_timeout_ms: u64,
    fixed_protocol: String,
    fixture_sha256: String,
    rubric_sha256: String,
    node_version: String,
    static_identity: Value,
    copied_binaries: Value,
    operator_declared_conditions: Value,
    observed_initial_conditions: Value,
    historical_model_trial: bool,
    source_to_binary_provenance_verified: bool,
    shipping_artifact_qualified: bool,
    representative_hardware_qualified: bool,
    provider_calls_performed: u64,
}

fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    if bytes.len() as u64 > MAX_REPORT {
        return Err("Local comparison evidence exceeds its byte bound".into());
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| "Refusing to overwrite local comparison evidence")?;
    file.write_all(bytes)
        .map_err(|_| "Cannot write private local comparison evidence".into())
}
fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|_| "Cannot encode local comparison evidence")?;
    bytes.push(b'\n');
    write_new(path, &bytes, 0o600)
}
fn private_directory(path: &Path) -> Result<PathBuf, String> {
    fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .map_err(|_| "Refusing existing or inaccessible local comparison output")?;
    path.canonicalize()
        .map_err(|_| "Cannot resolve private local comparison output".into())
}
fn check_private_directory(path: &Path) -> Result<(), String> {
    let info = fs::symlink_metadata(path)
        .map_err(|_| "Cannot inspect private local comparison directory")?;
    if !info.is_dir() || info.permissions().mode() & 0o777 != 0o700 {
        return Err("Local comparison directory must be a private regular directory".into());
    }
    Ok(())
}
fn fixture(root: &Path, reference: &Path) -> Result<Vec<u8>, String> {
    let actual = regular(&root.join(FIXTURE), MAX_REPORT)?;
    let frozen = regular(&reference.join(FIXTURE), MAX_REPORT)?;
    if digest(&actual) != FIXTURE_SHA || actual != frozen {
        return Err("Frozen local comparison fixture identity differs".into());
    }
    schema::fixture(&actual)?;
    Ok(actual)
}
fn copy_executable(source: &Path, destination: &Path) -> Result<Value, String> {
    let bytes = regular(source, MAX_BINARY)?;
    let before =
        fs::symlink_metadata(source).map_err(|_| "Cannot inspect evaluator executable identity")?;
    let mut target = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(destination)
        .map_err(|_| "Refusing existing evaluator executable copy")?;
    target
        .write_all(&bytes)
        .map_err(|_| "Cannot copy evaluator executable")?;
    let after =
        fs::symlink_metadata(source).map_err(|_| "Cannot recheck evaluator executable identity")?;
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || bytes != regular(source, MAX_BINARY)?
    {
        return Err("Evaluator executable changed during preparation".into());
    }
    Ok(json!({"sha256":digest(&bytes),"bytes":bytes.len()}))
}
fn child_env(command: &mut Command, temporary: &Path) {
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("TMPDIR", temporary);
}
async fn metadata_command(command: Command, cancel: &CancellationToken) -> Result<Vec<u8>, String> {
    let mut owner = Owner::metadata(command, cancel);
    let output = if cancel.is_cancelled() {
        owner.close().await?
    } else {
        owner.join().await?
    };
    if output.status["complete"] != true || output.status["exit_code"] != 0 {
        return Err("Local comparison metadata command failed or exceeded bounds".into());
    }
    if !output.stderr.is_empty() {
        return Err("Local comparison metadata command emitted diagnostics".into());
    }
    Ok(output.stdout)
}
async fn node_metadata(
    node: &Path,
    reference: &Path,
    temporary: &Path,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let module = url::Url::from_file_path(reference.join("src/ollama-evaluator.mjs"))
        .map_err(|_| "Invalid frozen evaluator module path")?;
    let source = format!(
        "import {{OLLAMA_QUESTIONS}} from {}; process.stdout.write(JSON.stringify({{version:process.version,rubric:OLLAMA_QUESTIONS}}));",
        serde_json::to_string(module.as_str()).unwrap()
    );
    let mut command = Command::new(node);
    command
        .args(["--input-type=module", "-e", &source])
        .current_dir(reference);
    child_env(&mut command, temporary);
    let bytes = metadata_command(command, cancel).await?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid frozen Node metadata")?;
    if value["version"] != "v22.14.0" || value["rubric"] != ollama_questions() {
        return Err("Pinned Node version or evaluator rubric differs".into());
    }
    Ok(
        json!({"version":value["version"],"rubric_sha256":digest(ollama_questions().to_string().as_bytes())}),
    )
}
async fn local_api<T: HttpTransport>(
    transport: &T,
    endpoint: &str,
    path: &str,
    body: Option<Value>,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let token = cancel.child_token();
    let _guard = token.clone().drop_guard();
    let action = async {
        let request = if let Some(body) = body {
            Request::post(format!("{endpoint}{path}"))
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(body.to_string())))
        } else {
            Request::get(format!("{endpoint}{path}")).body(Full::new(Bytes::new()))
        }
        .map_err(|_| "Invalid local metadata request")?;
        let response = transport
            .request(request)
            .await
            .map_err(|_| "Cannot reach local Ollama metadata")?;
        if !response.status().is_success() {
            return Err("Local Ollama metadata returned an unsuccessful status".into());
        }
        let (parts, body) = response.into_parts();
        let document = read_response_document(body, &parts.headers, 1024 * 1024, &token)
            .await
            .map_err(|_| "Invalid, oversized or interrupted local metadata")?;
        // Preserve JavaScript JSON numeric values in canonical serialized form;
        // the generic lossy observer projects every integer as serde f64.
        serde_json::from_slice(document.stringify().as_bytes())
            .map_err(|_| "Local metadata cannot be represented as scalar JSON".into())
    };
    tokio::select! {biased;_=cancel.cancelled()=>Err("Local comparison cancelled".into()),_=tokio::time::sleep(Duration::from_secs(5))=>Err("Local comparison metadata timed out".into()),result=action=>result}
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}
fn remote(value: &Value) -> bool {
    ["remote_model", "remote_host"]
        .iter()
        .any(|key| value.get(key).is_some_and(truthy))
}
fn model_identity(
    version: &Value,
    tags: &Value,
    details: &Value,
    tag: &str,
) -> Result<Value, String> {
    let version = version["version"]
        .as_str()
        .filter(|s| s.len() <= 100)
        .ok_or("Invalid Ollama version metadata")?;
    let captures = regex::Regex::new(r"^([0-9]+)\.([0-9]+)\.([0-9]+)(?:[-+][A-Za-z0-9.+-]+)?$")
        .unwrap()
        .captures(version)
        .ok_or("Invalid Ollama version metadata")?;
    let major = captures[1]
        .parse::<u64>()
        .map_err(|_| "Invalid Ollama version metadata")?;
    let minor = captures[2]
        .parse::<u64>()
        .map_err(|_| "Invalid Ollama version metadata")?;
    if major == 0 && minor < 35 {
        return Err("Local comparison requires Ollama>=0.35".into());
    }
    let models = tags["models"]
        .as_array()
        .ok_or("Invalid installed model metadata")?;
    let selected: Vec<_> = models
        .iter()
        .filter(|m| m["name"] == tag || m["model"] == tag)
        .collect();
    if selected.len() != 1 {
        return Err(
            "Select one exact already-installed local model tag; no download is performed".into(),
        );
    }
    let selected = selected[0];
    let digest_value = selected["digest"]
        .as_str()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("Missing exact installed model digest")?;
    let size = selected["size"]
        .as_u64()
        .filter(|n| *n > 0 && *n <= 9_007_199_254_740_991)
        .ok_or("Missing model download size")?;
    if remote(selected) || remote(details) || details.get("error").is_some_and(truthy) {
        return Err("Only an identified local decision-capable model is admitted".into());
    }
    let mut capability_sources = vec![];
    for (source, metadata) in [("/api/show", details), ("/api/tags", selected)] {
        if let Some(capabilities) = metadata.get("capabilities") {
            let capabilities = capabilities
                .as_array()
                .ok_or("Invalid installed model capabilities")?;
            if capabilities.iter().any(|v| {
                !v.as_str()
                    .is_some_and(|s| s.len() <= 100 && !s.chars().any(char::is_control))
            }) {
                return Err("Invalid installed model capabilities".into());
            }
            if capabilities.contains(&json!("decision")) {
                capability_sources.push(source);
            }
        }
    }
    if capability_sources.is_empty() {
        return Err("Installed model does not advertise decision capability".into());
    }
    let mut observed = json!({});
    for field in [
        "parameter_size",
        "quantization_level",
        "format",
        "family",
        "runner",
    ] {
        if let Some(value) = details["details"]
            .get(field)
            .or_else(|| selected["details"].get(field))
        {
            let text = value
                .as_str()
                .filter(|s| s.len() <= 200 && !s.chars().any(char::is_control))
                .ok_or("Invalid installed model details")?;
            observed[field] = json!(text);
        }
    }
    if !observed["parameter_size"]
        .as_str()
        .is_some_and(|s| !s.trim().is_empty())
    {
        return Err("Ollama did not identify local model parameter size".into());
    }
    Ok(
        json!({"tag":tag,"digest":digest_value,"download_bytes":size,"ollama_version":version,"protocol":"/v1/systemone","decision_capability_observed":true,"decision_capability_sources":capability_sources,"details":observed}),
    )
}
async fn inspect<T: HttpTransport>(
    transport: &T,
    endpoint: &str,
    tag: &str,
    cancel: &CancellationToken,
) -> Result<(Value, Value), String> {
    let version = local_api(transport, endpoint, "/api/version", None, cancel).await?;
    let tags = local_api(transport, endpoint, "/api/tags", None, cancel).await?;
    let details = local_api(
        transport,
        endpoint,
        "/api/show",
        Some(json!({"model":tag})),
        cancel,
    )
    .await?;
    let identity = model_identity(&version, &tags, &details, tag)?;
    let residency = local_api(transport, endpoint, "/api/ps", None, cancel).await?;
    residency["models"]
        .as_array()
        .ok_or("Invalid resident model metadata")?;
    Ok((identity, residency))
}
async fn conditions(cancel: &CancellationToken, temporary: &Path) -> Value {
    let mut out = json!({"observed_at":autorouter_runtime::server_events::timestamp(),"architecture":std::env::consts::ARCH,"platform":std::env::consts::OS,"controlled_hardware_qualification":false});
    let commands: Vec<(&str, &str, Vec<&str>)> = if cfg!(target_os = "macos") {
        vec![
            (
                "total_memory_bytes",
                "/usr/sbin/sysctl",
                vec!["-n", "hw.memsize"],
            ),
            ("load_average", "/usr/sbin/sysctl", vec!["-n", "vm.loadavg"]),
            ("power", "/usr/bin/pmset", vec!["-g", "batt"]),
            ("power_settings", "/usr/bin/pmset", vec!["-g", "custom"]),
            ("memory_pages", "/usr/bin/vm_stat", vec![]),
            ("memory_pressure", "/usr/bin/memory_pressure", vec!["-Q"]),
            ("swap", "/usr/sbin/sysctl", vec!["vm.swapusage"]),
            ("os_release", "/usr/bin/uname", vec!["-r"]),
        ]
    } else {
        vec![("os_release", "/usr/bin/uname", vec!["-r"])]
    };
    for (label, program, args) in commands {
        let mut command = Command::new(program);
        command.args(args);
        child_env(&mut command, temporary);
        out[label] = match metadata_command(command, cancel).await {
            Ok(bytes) => json!({"available":true,"text":String::from_utf8_lossy(&bytes).trim()}),
            Err(_) => json!({"available":false}),
        };
    }
    if cfg!(target_os = "linux") {
        for (label, path) in [
            ("memory_pages", "/proc/meminfo"),
            ("load_average", "/proc/loadavg"),
        ] {
            out[label] = match regular(Path::new(path), 65536) {
                Ok(bytes) => {
                    json!({"available":true,"text":String::from_utf8_lossy(&bytes).trim()})
                }
                Err(_) => json!({"available":false}),
            };
        }
        out["power"] = json!({"available":false,"method":"No portable power collector declared for this platform"});
    }
    out
}
fn static_identity(
    root: &Path,
    reference: &Path,
    node: &Path,
    native: &Path,
) -> Result<Value, String> {
    for (path, embedded) in [
        ("rust/parity/local-model-orchestration-v1.json", PROTOCOL),
        ("rust/parity/local-model-comparison-v1.json", V1),
        ("rust/parity/local-model-comparison-v2.json", V2),
    ] {
        if regular(&root.join(path), MAX_REPORT)? != embedded.as_bytes() {
            return Err(
                "Frozen model protocol source differs from the executable declaration".into(),
            );
        }
    }
    if crate::reference::verify(root, reference)? != 111 {
        return Err("Local comparison requires the complete111-file frozen reference".into());
    }
    let fixture = fixture(root, reference)?;
    Ok(
        json!({"baseline_manifest_sha256":digest(&regular(&root.join("rust/parity/baseline.json"),MAX_REPORT)?),"reference_files_verified":111,"fixture_sha256":digest(&fixture),"native_source_manifest":crate::bundle::source_identity(root)?,"node_sha256":digest(&regular(node,MAX_BINARY)?),"native_helper_sha256":digest(&regular(native,MAX_BINARY)?),"protocol_sha256":digest(PROTOCOL.as_bytes()),"finite_protocol_sha256":digest(V1.as_bytes()),"zero_protocol_sha256":digest(V2.as_bytes()),"rubric_sha256":digest(ollama_questions().to_string().as_bytes())}),
    )
}

#[derive(Debug)]
enum Action {
    Help,
    Prepare {
        output: PathBuf,
        reference: PathBuf,
        node: PathBuf,
        endpoint: String,
        model: String,
        warm: u64,
    },
    Run {
        declaration: PathBuf,
        output: PathBuf,
    },
}
fn arguments(args: &[String], root: &Path, cwd: &Path) -> Result<Action, String> {
    if args == ["--help"] || args.is_empty() {
        return Ok(Action::Help);
    }
    let action = args[0].as_str();
    if !["prepare", "run"].contains(&action) {
        return Err("Use compare-local-evaluators prepare or run; see --help".into());
    }
    let mut fields = BTreeMap::new();
    let mut i = 1;
    while i < args.len() {
        let flag = args[i].as_str();
        let allowed = if action == "prepare" {
            &[
                "--output",
                "--reference",
                "--node",
                "--endpoint",
                "--model",
                "--warm-timeout-ms",
                "--conditions",
            ][..]
        } else {
            &["--declaration", "--output", "--conditions"][..]
        };
        if !allowed.contains(&flag)
            || args
                .get(i + 1)
                .is_none_or(|s| s.is_empty() || s.starts_with("--"))
            || fields.insert(flag, args[i + 1].as_str()).is_some()
        {
            return Err("Unknown, duplicate or incomplete local comparison option".into());
        }
        i += 2;
    }
    if fields.get("--conditions") != Some(&"normal-use-exclusive-ollama-builds-paused") {
        return Err("Declare --conditions normal-use-exclusive-ollama-builds-paused; observations remain separate".into());
    }
    let required = |key: &str| {
        fields
            .get(key)
            .copied()
            .ok_or("Missing required local comparison option")
    };
    let path = |text: &str| autorouter_core::config::resolve_path(cwd, Path::new(text));
    let output = path(required("--output")?);
    if action == "run" {
        return Ok(Action::Run {
            declaration: path(required("--declaration")?),
            output,
        });
    }
    let model = validate_ollama_model(required("--model")?)?;
    let endpoint = validate_ollama_endpoint(
        fields
            .get("--endpoint")
            .copied()
            .unwrap_or("http://127.0.0.1:11434"),
    )?;
    let warm=match required("--warm-timeout-ms")?{"1500"=>1500,"0"=>0,_=>return Err("Warm deadline must be explicitly1500 or0; fixed thresholds/samples cannot be overridden".into())};
    Ok(Action::Prepare {
        output,
        reference: path(
            fields.get("--reference").copied().unwrap_or(
                root.join("artifacts/rust-rewrite/reference")
                    .to_str()
                    .ok_or("Invalid reference path")?,
            ),
        ),
        node: path(required("--node")?),
        endpoint,
        model,
        warm,
    })
}
async fn prepare(root: &Path, action: Action, cancel: &CancellationToken) -> Result<bool, String> {
    let Action::Prepare {
        output,
        reference,
        node,
        endpoint,
        model,
        warm,
    } = action
    else {
        return Err("Invalid prepare action".into());
    };
    let reference = reference
        .canonicalize()
        .map_err(|_| "Missing frozen Node reference")?;
    let node = node
        .canonicalize()
        .map_err(|_| "Missing pinned Node executable")?;
    let native = std::env::current_exe()
        .map_err(|_| "Cannot resolve native comparison tool")?
        .canonicalize()
        .map_err(|_| "Cannot resolve native comparison tool")?;
    let before = static_identity(root, &reference, &node, &native)?;
    let scratch = crate::tool_process::Scratch::new("local-comparison-prepare")?;
    let node_info = node_metadata(&node, &reference, &scratch.0, cancel).await?;
    let transport =
        NativeHttpClient::new().map_err(|_| "Cannot construct local metadata transport")?;
    let (identity, resident) = inspect(&transport, &endpoint, &model, cancel).await?;
    if !resident["models"].as_array().unwrap().is_empty() {
        return Err(
            "Ollama has resident models; leave them unchanged and prepare when idle".into(),
        );
    }
    let observed = conditions(cancel, &scratch.0).await;
    if cancel.is_cancelled() {
        return Err("Local comparison preparation cancelled".into());
    }
    if static_identity(root, &reference, &node, &native)? != before {
        return Err("Local comparison sources changed during preparation".into());
    }
    let directory = private_directory(&output)?;
    let native_copy = copy_executable(&native, &directory.join("native-xtask"))?;
    let node_copy = copy_executable(&node, &directory.join("node"))?;
    if static_identity(
        root,
        &reference,
        &directory.join("node"),
        &directory.join("native-xtask"),
    )? != before
    {
        return Err("Sources or executable copies changed before declaration freeze".into());
    }
    let (model_after, resident_after) = inspect(&transport, &endpoint, &model, cancel).await?;
    if model_after != identity || !resident_after["models"].as_array().unwrap().is_empty() {
        return Err("Model/runtime or residency changed before declaration freeze".into());
    }
    let declaration = json!({"schema_version":1,"kind":"prepared_portable_local_model_comparison","prepared_at":autorouter_runtime::server_events::timestamp(),"reference":reference.to_string_lossy(),"endpoint":endpoint,"model":identity,"warm_timeout_ms":warm,"fixed_protocol":if warm==0{"v2"}else{"v1"},"fixture_sha256":FIXTURE_SHA,"rubric_sha256":node_info["rubric_sha256"],"node_version":node_info["version"],"static_identity":before,"copied_binaries":{"native":native_copy,"node":node_copy},"operator_declared_conditions":{"normal_use":true,"usual_apps_may_remain_open":true,"exclusive_ollama_instance_no_competing_requests":true,"agent_builds_tests_paused_during_run":true},"observed_initial_conditions":observed,"historical_model_trial":false,"source_to_binary_provenance_verified":false,"shipping_artifact_qualified":false,"representative_hardware_qualified":false,"provider_calls_performed":0});
    write_json(&directory.join("declaration.json"), &declaration)?;
    let declaration_bytes = regular(&directory.join("declaration.json"), MAX_REPORT)?;
    write_new(
        &directory.join("declaration.sha256"),
        format!("{}\n", digest(&declaration_bytes)).as_bytes(),
        0o600,
    )?;
    write_json(
        &directory.join("protocol.json"),
        &serde_json::from_str(PROTOCOL).unwrap(),
    )?;
    println!("Prepared local evaluator comparison; no inference or downloads performed.");
    Ok(true)
}

fn load_declaration(directory: &Path) -> Result<Value, String> {
    check_private_directory(directory)?;
    let bytes = regular(&directory.join("declaration.json"), MAX_REPORT)?;
    let checksum = regular(&directory.join("declaration.sha256"), 128)?;
    if String::from_utf8(checksum)
        .map_err(|_| "Invalid declaration checksum")?
        .trim()
        != digest(&bytes)
    {
        return Err("Local comparison declaration checksum differs".into());
    }
    let typed: Declaration = serde_json::from_slice(&bytes)
        .map_err(|_| "Invalid prepared local comparison declaration")?;
    let declaration =
        serde_json::to_value(typed).map_err(|_| "Invalid prepared local comparison declaration")?;
    if declaration["schema_version"] != 1
        || declaration["kind"] != "prepared_portable_local_model_comparison"
        || !matches!(declaration["warm_timeout_ms"].as_u64(), Some(0 | 1500))
        || declaration["fixture_sha256"] != FIXTURE_SHA
        || declaration["node_version"] != "v22.14.0"
        || declaration["operator_declared_conditions"]["exclusive_ollama_instance_no_competing_requests"]
            != true
        || declaration["operator_declared_conditions"]["agent_builds_tests_paused_during_run"]
            != true
        || declaration["operator_declared_conditions"]["normal_use"] != true
        || declaration["fixed_protocol"]
            != if declaration["warm_timeout_ms"] == 0 {
                json!("v2")
            } else {
                json!("v1")
            }
        || declaration["rubric_sha256"] != declaration["static_identity"]["rubric_sha256"]
        || declaration["copied_binaries"]["native"]["sha256"]
            != declaration["static_identity"]["native_helper_sha256"]
        || declaration["copied_binaries"]["node"]["sha256"]
            != declaration["static_identity"]["node_sha256"]
        || declaration["historical_model_trial"] != false
        || declaration["source_to_binary_provenance_verified"] != false
        || declaration["shipping_artifact_qualified"] != false
        || declaration["representative_hardware_qualified"] != false
        || declaration["provider_calls_performed"] != 0
    {
        return Err("Invalid or altered fixed local comparison declaration".into());
    }
    validate_ollama_endpoint(
        declaration["endpoint"]
            .as_str()
            .ok_or("Invalid declared local endpoint")?,
    )?;
    validate_ollama_model(
        declaration["model"]["tag"]
            .as_str()
            .ok_or("Invalid declared local model")?,
    )?;
    let copied_protocol: Value =
        serde_json::from_slice(&regular(&directory.join("protocol.json"), MAX_REPORT)?)
            .map_err(|_| "Invalid copied local comparison protocol")?;
    if copied_protocol != serde_json::from_str::<Value>(PROTOCOL).unwrap() {
        return Err("Copied local comparison protocol changed".into());
    }
    Ok(declaration)
}
fn recheck(root: &Path, directory: &Path, declaration: &Value) -> Result<Vec<Value>, String> {
    check_private_directory(directory)?;
    let current_executable = std::env::current_exe()
        .map_err(|_| "Cannot inspect running comparison helper")?
        .canonicalize()
        .map_err(|_| "Cannot inspect running comparison helper")?;
    if digest(&regular(&current_executable, MAX_BINARY)?)
        != declaration["static_identity"]["native_helper_sha256"]
    {
        return Err(
            "Running comparison helper differs from the prepared immutable executable".into(),
        );
    }
    let reference = Path::new(
        declaration["reference"]
            .as_str()
            .ok_or("Missing declared reference")?,
    );
    let current = static_identity(
        root,
        reference,
        &directory.join("node"),
        &directory.join("native-xtask"),
    )?;
    if current != declaration["static_identity"] {
        return Err("Source, fixture, reference, Node, helper or protocol identity drifted; prepare a new declaration".into());
    }
    schema::fixture(&fixture(root, reference)?)
}
async fn cleanup<T: HttpTransport>(
    transport: &T,
    endpoint: &str,
    model: &Value,
) -> Result<Value, String> {
    let cancel = CancellationToken::new();
    let tag = model["tag"].as_str().ok_or("Invalid admitted model")?;
    let (identity, resident) = inspect(transport, endpoint, tag, &cancel).await?;
    if identity != *model {
        return Err("Model identity drift prevents candidate cleanup".into());
    }
    let models = resident["models"].as_array().unwrap();
    let candidate: Vec<_> = models
        .iter()
        .filter(|m| m["name"] == tag || m["model"] == tag)
        .collect();
    if candidate.iter().any(|m| m["digest"] != model["digest"]) {
        return Err("Resident candidate digest differs; cleanup left model state unchanged".into());
    }
    let unrelated = models.len() - candidate.len();
    if !candidate.is_empty() {
        local_api(
            transport,
            endpoint,
            "/api/generate",
            Some(json!({"model":tag,"stream":false,"keep_alive":0})),
            &cancel,
        )
        .await?;
    }
    let after = local_api(transport, endpoint, "/api/ps", None, &cancel).await?;
    let after_models = after["models"]
        .as_array()
        .ok_or("Invalid cleanup residency response")?;
    if after_models
        .iter()
        .any(|m| m["name"] == tag || m["model"] == tag)
    {
        return Err("Admitted candidate remains resident after bounded cleanup".into());
    }
    Ok(
        json!({"verified_candidate_unloaded":true,"candidate_unload_requested":!candidate.is_empty(),"unrelated_model_count_before":unrelated,"resident_count_after":after_models.len(),"unrelated_models_modified":false}),
    )
}

async fn execute(
    root: &Path,
    directory: &Path,
    output: &Path,
    cancel: &CancellationToken,
) -> Result<bool, String> {
    let declaration = load_declaration(directory)?;
    let selected = recheck(root, directory, &declaration)?;
    let endpoint = declaration["endpoint"].as_str().unwrap();
    let tag = declaration["model"]["tag"].as_str().unwrap();
    let transport =
        NativeHttpClient::new().map_err(|_| "Cannot construct local comparison transport")?;
    let (identity, resident) = inspect(&transport, endpoint, tag, cancel).await?;
    if identity != declaration["model"] || !resident["models"].as_array().unwrap().is_empty() {
        return Err(
            "Declared model/runtime drifted or Ollama is busy; leave existing state unchanged"
                .into(),
        );
    }
    let output = private_directory(output)?;
    write_json(&output.join("declaration.json"), &declaration)?;
    let mut comparisons = vec![];
    let mut executions = vec![];
    let mut failures = vec![];
    let action:Result<(),String>=async{
        for pair in 1..=5{
            let order=if pair%2==1{["node","native"]}else{["native","node"]};
            let mut reports=BTreeMap::new();
            for engine in order{
                if cancel.is_cancelled(){return Err("Local comparison cancelled".into());}
                recheck(root,directory,&declaration)?;
                let(actual,resident)=inspect(&transport,endpoint,tag,cancel).await?;
                if actual!=declaration["model"]||!resident["models"].as_array().unwrap().is_empty(){return Err("Model/runtime drift or conflicting resident model before engine admission".into());}
                let observed=conditions(cancel,&output).await;
                write_json(&output.join(format!("pair-{pair}-{engine}-before.json")),&json!({"observed":observed,"operator_declared":declaration["operator_declared_conditions"],"resident_model_count":0,"model_identity":actual}))?;
                let report_path=output.join(format!("pair-{pair}-{engine}.json"));write_new(&report_path,b"",0o600)?;
                let report_inode=fs::symlink_metadata(&report_path).map_err(|_|"Cannot inspect owned report identity")?;
                let mut command=if engine=="node"{let mut command=Command::new(directory.join("node"));command.arg(Path::new(declaration["reference"].as_str().unwrap()).join("scripts/evaluate-ollama.mjs"));command}else{let mut command=Command::new(directory.join("native-xtask"));command.arg("evaluate-ollama");command};
                command.args(["--models",tag,"--endpoint",endpoint,"--split","heldout","--rounds","3","--stress-rounds","8","--cold-timeout-ms","60000","--min-agreement","1","--max-under-route-rate","0","--timeout-ms",&declaration["warm_timeout_ms"].to_string(),"--output"]).arg(&report_path).current_dir(root);child_env(&mut command,&output);
                let mut owner=Owner::start(command,cancel);
                let child=owner.join().await;
                let execution_status=match &child{Ok(child)=>child.status.clone(),Err(error)=>error.evidence()};
                executions.push(json!({"pair":pair,"engine":engine,"status":execution_status,"report":report_path.file_name().unwrap().to_string_lossy()}));
                // Attempt create-only evidence before cleanup, but never let an
                // evidence I/O failure skip the admitted candidate's cleanup.
                let saved:Result<(),String>=(||{
                    write_json(&output.join(format!("pair-{pair}-{engine}-execution.json")),&execution_status)?;
                    if let Ok(child)=&child{
                        write_new(&output.join(format!("pair-{pair}-{engine}.stdout.log")),&child.stdout,0o600)?;
                        write_new(&output.join(format!("pair-{pair}-{engine}.stderr.log")),&child.stderr,0o600)?;
                    }Ok(())
                })();
                // Joined child/group termination and pipe closure precede fresh
                // candidate residency cleanup. This does not assert daemon or
                // arbitrary descendant reaping beyond the observed API boundary.
                let admitted=match &child{Ok(child)=>child.status["spawned"]==true,Err(error)=>error.spawned};
                let cleaned=if !admitted{
                    Ok(json!({"candidate_unload_requested":false,"not_admitted":true,"unrelated_models_modified":false}))
                }else{cleanup(&transport,endpoint,&declaration["model"]).await};
                let observed=conditions(&CancellationToken::new(),&output).await;
                write_json(&output.join(format!("pair-{pair}-{engine}-after.json")),&json!({"observed":observed,"cleanup":match &cleaned{Ok(value)=>value.clone(),Err(_)=>json!({"verified_candidate_unloaded":false})}}))?;
                saved?;
                let child=child?;
                cleaned?;
                if child.status["complete"]!=true||!matches!(child.status["exit_code"].as_i64(),Some(0|1)){return Err("Owned evaluator child failed, cancelled, exceeded bounds or ended unexpectedly".into());}
                let after_inode=fs::symlink_metadata(&report_path).map_err(|_|"Missing owned evaluator report")?;
                if after_inode.dev()!=report_inode.dev()||after_inode.ino()!=report_inode.ino(){return Err("Owned evaluator report changed file identity".into());}
                let report:Value=serde_json::from_slice(&regular(&report_path,MAX_REPORT)?).map_err(|_|"Incomplete or invalid original evaluator report")?;
                let validated=schema::report(&report,&declaration,&selected)?;
                if (child.status["exit_code"]==0)!=validated.quality_passed{return Err("Original child exit status disagrees with recomputed strict quality acceptance".into());}
                recheck(root,directory,&declaration)?;
                let(actual,resident)=inspect(&transport,endpoint,tag,cancel).await?;
                if actual!=declaration["model"]||!resident["models"].as_array().unwrap().is_empty(){return Err("Model/runtime drift, conflict or unverified unload after engine".into());}
                reports.insert(engine,validated);
            }
            let comparison=schema::compare(reports.get("node").unwrap(),reports.get("native").unwrap());
            write_json(&output.join(format!("pair-{pair}-comparison.json")),&comparison)?;comparisons.push(comparison);
        }Ok(())
    }.await;
    if let Err(error) = &action {
        failures.push(json!({"error":error,"cancelled":cancel.is_cancelled()}));
    }
    let complete = action.is_ok() && comparisons.len() == 5;
    let language_port_equal =
        complete && comparisons.iter().all(|r| r["language_port_equal"] == true);
    let quality_passed = complete
        && comparisons
            .iter()
            .all(|r| r["node_quality_passed"] == true && r["native_quality_passed"] == true);
    write_json(
        &output.join("comparison.json"),
        &json!({"schema_version":1,"kind":"portable_paired_local_model_comparison","complete":complete,"language_port_equal":language_port_equal,"strict_model_quality_passed":quality_passed,"passed":language_port_equal&&quality_passed,"prepared_model":declaration["model"],"warm_timeout_ms":declaration["warm_timeout_ms"],"pairs":comparisons,"executions":executions,"failures":failures,"historical_protocols_replaced":false,"representative_hardware_accepted":false,"shipping_artifact_qualified":false,"gateway_performance_accepted":false,"downstream_task_quality_accepted":false}),
    )?;
    println!(
        "Paired local evaluator evidence saved; quality and language-port outcomes are separate."
    );
    Ok(language_port_equal && quality_passed)
}
pub fn run(args: &[String], root: &Path) -> Result<bool, String> {
    let cwd =
        std::env::current_dir().map_err(|_| "Cannot resolve local comparison working directory")?;
    let action = arguments(args, root, &cwd)?;
    if matches!(action, Action::Help) {
        println!(
            "cargo xtask compare-local-evaluators prepare --model INSTALLED_TAG --node NODE22.14_EXECUTABLE --warm-timeout-ms 1500|0 --conditions normal-use-exclusive-ollama-builds-paused --output NEW_DECLARATION_DIRECTORY [--reference FROZEN_REFERENCE] [--endpoint LOOPBACK_URL]\ncargo xtask compare-local-evaluators run --declaration DECLARATION_DIRECTORY --conditions normal-use-exclusive-ollama-builds-paused --output NEW_RUN_DIRECTORY\nPrepare makes bounded metadata calls only; downloads/loads/unloads nothing. Run explicitly performs five alternating Node/native local inference pairs using fixed held-out/stress policy, positive cold/process deadlines and optional zero warm deadline. Existing models are left unchanged unless they are the exact admitted candidate; no Claude/Jev calls, API keys, Git or private wrappers are required. Outputs stay private and create-only; retain every failed/interrupted run. Operator conditions are declarations, not controlled-hardware proof."
        );
        return Ok(true);
    }
    crate::evaluation::runtime()?.block_on(async {
        let signals = crate::tool_process::Signals::new()?;
        match action {
            Action::Prepare { .. } => prepare(root, action, &signals.token).await,
            Action::Run {
                declaration,
                output,
            } => execute(root, &declaration, &output, &signals.token).await,
            Action::Help => unreachable!(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use autorouter_runtime::http_client::HttpError;
    use http_body_util::BodyExt;
    use hyper::Response;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };
    struct MetadataMock {
        unrelated: bool,
        wrong_digest: bool,
        resident: AtomicBool,
        calls: Mutex<Vec<(String, Value)>>,
    }
    impl HttpTransport for MetadataMock {
        type ResponseBody = Full<Bytes>;
        async fn request(
            &self,
            request: Request<Full<Bytes>>,
        ) -> Result<Response<Self::ResponseBody>, HttpError> {
            assert!(request.headers().get("authorization").is_none());
            assert!(request.headers().get("x-api-key").is_none());
            let (parts, body) = request.into_parts();
            let bytes = body.collect().await.unwrap().to_bytes();
            let body = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice::<Value>(&bytes).unwrap()
            };
            let path = parts.uri.path();
            self.calls.lock().unwrap().push((path.into(), body.clone()));
            let value = match path {
                "/api/version" => json!({"version":"0.40.0"}),
                "/api/tags" => {
                    json!({"models":[{"name":"synthetic:1","digest":"a".repeat(64),"size":100,"details":{"parameter_size":"synthetic"}}]})
                }
                "/api/show" => {
                    assert_eq!(body, json!({"model":"synthetic:1"}));
                    json!({"capabilities":["decision"],"details":{"parameter_size":"synthetic"}})
                }
                "/api/ps" => {
                    let mut models = vec![];
                    if self.resident.load(Ordering::SeqCst) {
                        models.push(json!({"name":"synthetic:1","digest":if self.wrong_digest{"b".repeat(64)}else{"a".repeat(64)}}));
                    }
                    if self.unrelated {
                        models.push(
                            json!({"name":"unrelated-private-model:1","digest":"c".repeat(64)}),
                        );
                    }
                    json!({"models":models})
                }
                "/api/generate" => {
                    assert_eq!(
                        body,
                        json!({"model":"synthetic:1","stream":false,"keep_alive":0})
                    );
                    self.resident.store(false, Ordering::SeqCst);
                    json!({"done":true})
                }
                _ => panic!("Mock observed forbidden inference/download endpoint"),
            };
            Ok(Response::new(Full::new(Bytes::from(value.to_string()))))
        }
    }
    fn metadata_mock(unrelated: bool, wrong_digest: bool, resident: bool) -> MetadataMock {
        MetadataMock {
            unrelated,
            wrong_digest,
            resident: AtomicBool::new(resident),
            calls: Mutex::new(vec![]),
        }
    }
    #[tokio::test]
    async fn prepare_inspection_makes_only_bounded_metadata_requests_and_leaves_residency_unchanged()
     {
        let mock = metadata_mock(true, false, true);
        let (identity, resident) = inspect(
            &mock,
            "http://127.0.0.1:11434",
            "synthetic:1",
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(identity["tag"], "synthetic:1");
        assert_eq!(resident["models"].as_array().unwrap().len(), 2);
        assert!(mock.resident.load(Ordering::SeqCst));
        assert_eq!(
            mock.calls
                .lock()
                .unwrap()
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            ["/api/version", "/api/tags", "/api/show", "/api/ps"]
        );
    }
    #[tokio::test]
    async fn cleanup_preserves_unrelated_models_and_refuses_changed_candidate_digest() {
        for wrong in [false, true] {
            let mock = metadata_mock(true, wrong, true);
            let (identity, _) = inspect(
                &mock,
                "http://127.0.0.1:11434",
                "synthetic:1",
                &CancellationToken::new(),
            )
            .await
            .unwrap();
            let result = cleanup(&mock, "http://127.0.0.1:11434", &identity).await;
            let calls = mock.calls.lock().unwrap();
            if wrong {
                assert!(result.is_err());
                assert!(mock.resident.load(Ordering::SeqCst));
                assert!(calls.iter().all(|(path, _)| path != "/api/generate"));
            } else {
                let result = result.unwrap();
                assert_eq!(result["unrelated_model_count_before"], 1);
                assert_eq!(result["resident_count_after"], 1);
                assert_eq!(result["unrelated_models_modified"], false);
                assert!(!mock.resident.load(Ordering::SeqCst));
                assert_eq!(
                    calls
                        .iter()
                        .filter(|(path, _)| path == "/api/generate")
                        .count(),
                    1
                );
            }
        }
    }
    #[test]
    fn args_require_declared_conditions_fixed_deadline_and_explicit_installed_model() {
        let valid = [
            "prepare",
            "--model",
            "tev1:4b",
            "--node",
            "/synthetic/node",
            "--warm-timeout-ms",
            "0",
            "--conditions",
            "normal-use-exclusive-ollama-builds-paused",
            "--output",
            "new",
        ]
        .map(str::to_owned);
        assert!(matches!(
            arguments(&valid, Path::new("/synthetic"), Path::new("/synthetic")),
            Ok(Action::Prepare { warm: 0, .. })
        ));
        for forbidden in ["15000", "-1", "false"] {
            let mut invalid = valid.clone();
            invalid[6] = forbidden.into();
            assert!(arguments(&invalid, Path::new("/synthetic"), Path::new("/synthetic")).is_err());
        }
        let mut duplicate = valid.to_vec();
        duplicate.extend(["--model".into(), "nimble:9b".into()]);
        assert!(arguments(&duplicate, Path::new("/synthetic"), Path::new("/synthetic")).is_err());
    }
    #[test]
    fn metadata_admission_requires_decision_capability_exact_digest_and_local_identity() {
        let version = json!({"version":"0.40.0"});
        let tags = json!({"models":[{"name":"tev1:4b","digest":"a".repeat(64),"size":100,"details":{"parameter_size":"4B"}}]});
        let details = json!({"capabilities":["decision"],"details":{"parameter_size":"4B"}});
        let admitted = model_identity(&version, &tags, &details, "tev1:4b").unwrap();
        assert_eq!(admitted["digest"], "a".repeat(64));
        let mut tags_only = tags.clone();
        tags_only["models"][0]["capabilities"] = json!(["decision"]);
        let tags_admitted = model_identity(
            &version,
            &tags_only,
            &json!({"details":{"parameter_size":"4B"}}),
            "tev1:4b",
        )
        .unwrap();
        assert_eq!(
            tags_admitted["decision_capability_sources"],
            json!(["/api/tags"])
        );
        for details in [
            json!({"capabilities":["completion"],"details":{"parameter_size":"4B"}}),
            json!({"remote_host":"synthetic-private-remote","capabilities":["decision"],"details":{"parameter_size":"4B"}}),
        ] {
            let error = model_identity(&version, &tags, &details, "tev1:4b").unwrap_err();
            assert!(!error.contains("synthetic-private"));
        }
        assert!(model_identity(&json!({"version":"0.34.9"}), &tags, &details, "tev1:4b").is_err());
    }
    #[test]
    fn evidence_creation_and_reading_reject_symlinks_and_overwrite_without_payload_diagnostics() {
        let scratch = crate::tool_process::Scratch::new("local-evidence-test").unwrap();
        let path = scratch.0.join("evidence.json");
        write_json(&path, &json!({"synthetic":"private-marker"})).unwrap();
        assert!(write_json(&path, &json!({})).is_err());
        std::os::unix::fs::symlink(&path, scratch.0.join("link")).unwrap();
        let error = regular(&scratch.0.join("link"), MAX_REPORT).unwrap_err();
        assert!(!error.contains("private-marker"));
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    #[test]
    fn declaration_duplicate_unknown_keys_checksum_and_protocol_drift_are_rejected_before_calls() {
        let scratch = crate::tool_process::Scratch::new("local-declaration-schema-test").unwrap();
        let value = json!({"schema_version":1,"kind":"prepared_portable_local_model_comparison","prepared_at":"synthetic","reference":"/synthetic/reference","endpoint":"http://127.0.0.1:11434","model":{"tag":"synthetic:1"},"warm_timeout_ms":0,"fixed_protocol":"v2","fixture_sha256":FIXTURE_SHA,"rubric_sha256":"synthetic-rubric","node_version":"v22.14.0","static_identity":{"native_helper_sha256":"synthetic-native","node_sha256":"synthetic-node","rubric_sha256":"synthetic-rubric"},"copied_binaries":{"native":{"sha256":"synthetic-native"},"node":{"sha256":"synthetic-node"}},"operator_declared_conditions":{"normal_use":true,"exclusive_ollama_instance_no_competing_requests":true,"agent_builds_tests_paused_during_run":true},"observed_initial_conditions":{},"historical_model_trial":false,"source_to_binary_provenance_verified":false,"shipping_artifact_qualified":false,"representative_hardware_qualified":false,"provider_calls_performed":0});
        let original = serde_json::to_vec(&value).unwrap();
        fs::write(scratch.0.join("declaration.json"), &original).unwrap();
        fs::write(scratch.0.join("declaration.sha256"), digest(&original)).unwrap();
        fs::write(scratch.0.join("protocol.json"), PROTOCOL).unwrap();
        assert!(load_declaration(&scratch.0).is_ok());
        for mutation in 0..4 {
            let bytes = match mutation {
                0 => format!(
                    "{{\"schema_version\":1,{}",
                    String::from_utf8(original.clone())
                        .unwrap()
                        .strip_prefix('{')
                        .unwrap()
                )
                .into_bytes(),
                1 => {
                    let mut changed = value.clone();
                    changed["unknown_private_extension"] = json!("synthetic-private-marker");
                    serde_json::to_vec(&changed).unwrap()
                }
                _ => original.clone(),
            };
            fs::write(scratch.0.join("declaration.json"), &bytes).unwrap();
            fs::write(
                scratch.0.join("declaration.sha256"),
                if mutation == 2 {
                    "wrong-checksum".into()
                } else {
                    digest(&bytes)
                },
            )
            .unwrap();
            fs::write(
                scratch.0.join("protocol.json"),
                if mutation == 3 {
                    b"{}".as_slice()
                } else {
                    PROTOCOL.as_bytes()
                },
            )
            .unwrap();
            let error = load_declaration(&scratch.0).unwrap_err();
            assert!(!error.contains("synthetic-private-marker"));
        }
    }
}
