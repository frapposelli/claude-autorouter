//! Synthetic storage tooling. Validation never retains performance samples.
#[path = "benchmark_storage_child.rs"]
mod child;
#[path = "benchmark_storage_legacy.rs"]
mod legacy;
use crate::tool_process::{InputAction, RunOptions, Scratch, Signals, run_child};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

pub(super) const PROTOCOL: &str = include_str!("../../parity/storage-benchmark-v1.json");
const EXPECTATIONS: &str = include_str!("../../parity/storage-contracts.capture.json");
const EXPECTATIONS_SHA: &str = "e900b763342ee16aadcb8cabb7eaacbfde1171f4e754443b1c02c7806acf82b9";
const CHILD_LIMIT: usize = 8 * 1024 * 1024;
const RUN_LIMIT: usize = 64 * 1024 * 1024;
const SCENARIOS: &[&str] = &[
    "status_normal",
    "status_held",
    "status_create_failure",
    "status_initial_failure",
    "status_readiness_timeout",
    "status_write_recovery",
    "status_rename_recovery",
    "status_concurrent_close",
    "log_normal",
    "log_metadata",
    "log_queue_bound",
    "log_session_bound",
    "log_continuous",
    "log_append_failure",
    "log_close",
    "log_init_warning_failure",
];
const HELP: &str = "cargo xtask benchmark-storage --validate --output NEW_DIRECTORY [--implementation native|paired] [--native XTASK_EXECUTABLE] [--node NODE_EXECUTABLE --reference FROZEN_DIRECTORY]\ncargo xtask benchmark-storage --measure --output NEW_DIRECTORY [--implementation native|paired] [--profile paired|legacy-status-workload]\ncargo xtask benchmark-storage --compare-status-legacy REPORT.json --label LABEL\n\nExactly one mode is required. Validation uses synthetic local storage and retains no numerical performance data. Measurement is explicit, descriptive and never a CI acceptance gate. Native-only execution needs no Node. Paired execution verifies the frozen reference. The old --module JavaScript import option is replaced by a native xtask executable and a verified --reference directory; arbitrary modules are not loaded. Outputs are exclusive new evidence directories; historical reports are read-only. No sockets, inference, downloads, credentials or user configuration.";
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn read(path: &Path, max: usize) -> Result<Vec<u8>, String> {
    read_regular(path, max, true)
}
fn read_public_input(path: &Path, max: usize) -> Result<Vec<u8>, String> {
    // Cargo may hardlink an executable into target/debug. Source and executable
    // fingerprints need immutable byte comparisons, not private-file ownership.
    read_regular(path, max, false)
}
fn read_regular(path: &Path, max: usize, require_unique_link: bool) -> Result<Vec<u8>, String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags((nix::fcntl::OFlag::O_NOFOLLOW | nix::fcntl::OFlag::O_NONBLOCK).bits())
        .open(path)
        .map_err(|_| "Cannot open regular storage evidence")?;
    let metadata = file
        .metadata()
        .map_err(|_| "Cannot inspect storage evidence")?;
    if !metadata.is_file()
        || (require_unique_link && metadata.nlink() != 1)
        || metadata.len() > max as u64
    {
        return Err("Storage evidence type or byte bound rejected".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read storage evidence")?;
    if bytes.len() > max {
        return Err("Storage evidence byte bound exceeded".into());
    }
    Ok(bytes)
}
struct Limited {
    bytes: Vec<u8>,
    maximum: usize,
}
impl Write for Limited {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        if input.len() > self.maximum.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("Storage report exceeds bound"));
        }
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encode(value: &Value, maximum: usize) -> Result<Vec<u8>, String> {
    let mut output = Limited {
        bytes: Vec::new(),
        maximum,
    };
    serde_json::to_writer(&mut output, value)
        .map_err(|_| "Storage report exceeds encoding bound")?;
    Ok(output.bytes)
}
fn save(path: &Path, value: &Value, total: &mut usize) -> Result<(), String> {
    let bytes = encode(value, CHILD_LIMIT)?;
    if bytes.len().saturating_add(1) > RUN_LIMIT.saturating_sub(*total) {
        return Err("Storage run evidence exceeds bound".into());
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags((nix::fcntl::OFlag::O_NOFOLLOW | nix::fcntl::OFlag::O_NONBLOCK).bits())
        .open(path)
        .map_err(|_| "Refusing to overwrite storage evidence")?;
    file.write_all(&bytes)
        .and_then(|_| file.write_all(b"\n"))
        .map_err(|_| "Cannot save storage evidence")?;
    *total += bytes.len() + 1;
    Ok(())
}
fn descriptor(path: &Path, maximum: usize) -> Result<Value, String> {
    let bytes = read_public_input(path, maximum)?;
    Ok(json!({"path":path,"bytes":bytes.len(),"sha256":sha(&bytes)}))
}
fn semantic_sha(value: &Value) -> Result<String, String> {
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(object) => {
                let keys = object.keys().collect::<std::collections::BTreeSet<_>>();
                Value::Object(
                    keys.into_iter()
                        .map(|key| (key.clone(), sorted(&object[key])))
                        .collect(),
                )
            }
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            _ => value.clone(),
        }
    }
    let bytes = encode(&sorted(value), CHILD_LIMIT)?;
    let document =
        autorouter_core::js_json::JsDocument::parse(&bytes).map_err(|_| "Invalid semantic JSON")?;
    Ok(sha(document.stringify().as_bytes()))
}
fn expected(id: &str) -> Result<String, String> {
    if sha(EXPECTATIONS.as_bytes()) != EXPECTATIONS_SHA {
        return Err("Storage expectation pin changed".into());
    }
    let value: Value =
        serde_json::from_str(EXPECTATIONS).map_err(|_| "Invalid storage expectations")?;
    if value["protocol_sha256"] != sha(PROTOCOL.as_bytes()) {
        return Err("Storage expectation protocol mismatch".into());
    }
    value["cases"]
        .as_array()
        .ok_or("Missing storage expectations")?
        .iter()
        .find(|row| row["id"] == id)
        .and_then(|row| row["semantic_sha256"].as_str())
        .map(str::to_owned)
        .ok_or("Missing storage semantic expectation".into())
}
fn normalize_snapshot(value: &mut Value, fixed_clock: bool) -> Result<(), String> {
    let timestamp = |v: &Value| {
        v.as_f64().is_some_and(|n| {
            n.is_finite()
                && n >= 0.
                && n.fract() == 0.
                && n <= 9007199254740991.
                && (!fixed_clock || n == 1760000000000.)
        })
    };
    if value["pid"].as_u64() != Some(u64::from(std::process::id()))
        || !timestamp(&value["heartbeat_at"])
        || value["sessions"].as_object().is_none_or(|sessions| {
            sessions
                .values()
                .any(|session| !session.is_object() || !timestamp(&session["updated_at"]))
        })
    {
        return Err("Invalid original status identity or clock field".into());
    }
    value["pid"] = json!(0);
    value["heartbeat_at"] = json!(0);
    for session in value["sessions"].as_object_mut().unwrap().values_mut() {
        session["updated_at"] = json!(0);
    }
    Ok(())
}
#[derive(Debug)]
struct Options {
    mode: String,
    output: Option<PathBuf>,
    implementation: String,
    native: Option<PathBuf>,
    node: Option<PathBuf>,
    reference: Option<PathBuf>,
    profile: String,
    legacy: Option<PathBuf>,
    label: Option<String>,
}
fn parse(args: &[String]) -> Result<Options, String> {
    let mut result = Options {
        mode: String::new(),
        output: None,
        implementation: "native".into(),
        native: None,
        node: None,
        reference: None,
        profile: "paired".into(),
        legacy: None,
        label: None,
    };
    let mut seen = std::collections::HashSet::new();
    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        if !seen.insert(key) {
            return Err("Duplicate storage option".into());
        }
        index += 1;
        if key == "--validate" || key == "--measure" {
            if !result.mode.is_empty() {
                return Err("Choose exactly one storage mode".into());
            }
            result.mode = key[2..].into();
            continue;
        }
        if key == "--module" {
            return Err("--module is replaced by --native XTASK_EXECUTABLE and verified --reference FROZEN_DIRECTORY".into());
        }
        let value = args
            .get(index)
            .filter(|s| !s.is_empty() && !s.starts_with("--"))
            .ok_or("Storage option requires a value")?;
        index += 1;
        match key {
            "--output" => result.output = Some(value.into()),
            "--implementation" => result.implementation = value.clone(),
            "--native" => result.native = Some(value.into()),
            "--node" => result.node = Some(value.into()),
            "--reference" => result.reference = Some(value.into()),
            "--profile" => result.profile = value.clone(),
            "--label" => result.label = Some(value.clone()),
            "--compare-status-legacy" => {
                if !result.mode.is_empty() {
                    return Err("Choose exactly one storage mode".into());
                }
                result.mode = "legacy".into();
                result.legacy = Some(value.into());
            }
            _ => return Err("Unknown storage option".into()),
        }
    }
    if !["validate", "measure", "legacy"].contains(&result.mode.as_str())
        || !["native", "paired"].contains(&result.implementation.as_str())
        || !["paired", "legacy-status-workload"].contains(&result.profile.as_str())
    {
        return Err("Invalid or missing storage mode/profile/implementation".into());
    }
    if result.mode == "legacy" {
        if result.label.as_ref().is_none_or(|v| v.len() > 100)
            || seen
                .iter()
                .any(|k| !["--compare-status-legacy", "--label"].contains(k))
        {
            return Err(
                "Legacy comparison requires only --compare-status-legacy REPORT --label LABEL"
                    .into(),
            );
        }
    } else if result.output.is_none()
        || result.label.is_some()
        || (result.mode == "validate" && seen.contains("--profile"))
        || (result.implementation == "native"
            && (result.node.is_some() || result.reference.is_some()))
    {
        return Err("Invalid storage option combination".into());
    }
    Ok(result)
}
fn program(path: Option<&Path>, fallback: &str) -> Result<PathBuf, String> {
    let path = path
        .map(Path::to_owned)
        .or_else(|| {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|p| p.join(fallback))
                .find(|p| p.is_file())
        })
        .ok_or("Storage executable unavailable")?;
    path.canonicalize()
        .map_err(|_| "Cannot resolve storage executable".into())
}
fn valid_child(
    report: &Value,
    scenario: &str,
    implementation: &str,
    mode: &str,
    profile: &str,
) -> Result<(), String> {
    if report["schema_version"] != 1
        || report["kind"] != "storage_child"
        || report["scenario"] != scenario
        || report["implementation"] != implementation
        || report["mode"] != mode
        || report["protocol_sha256"] != sha(PROTOCOL.as_bytes())
        || report["passed"] != true
        || report["cleanup"]["active_io"] != 0
        || report["cleanup"]["joined"] != true
        || report["cleanup"]["scratch_empty"] != true
        || report["numerical_values_retained"] != (mode == "measure")
        || !report["semantics"].is_object()
    {
        return Err("Storage child identity/result/cleanup mismatch".into());
    }
    let semantic = &report["semantics"];
    if mode == "validate" {
        if semantic_sha(semantic)? != expected(scenario)? {
            return Err("Storage semantic expectation mismatch".into());
        }
        if semantic.get("raw").is_some() {
            return Err("Validation cannot retain performance samples".into());
        }
        if scenario.starts_with("log_") {
            let files = semantic["files"]
                .as_object()
                .ok_or("Missing storage files")?;
            let rows: usize = files
                .values()
                .map(|v| v.as_array().map_or(usize::MAX, Vec::len))
                .try_fold(0usize, |a, b| a.checked_add(b))
                .ok_or("Invalid storage row count")?;
            let accepted = semantic["accepted"]
                .as_u64()
                .ok_or("Missing admission count")? as usize;
            if semantic["close_calls"] != 1
                || semantic["late_rejected"] != true
                || rows
                    != if scenario == "log_append_failure" {
                        0
                    } else {
                        accepted
                    }
                || accepted > 3000
            {
                return Err("Storage admission/drain mismatch".into());
            }
            let expected = match scenario {
                "log_normal" | "log_metadata" => Some(8),
                "log_session_bound" => Some(128),
                "log_continuous" => Some(800),
                "log_close" => Some(3),
                "log_append_failure" => Some(1),
                "log_init_warning_failure" => Some(0),
                _ => None,
            };
            if expected.is_some_and(|n| accepted != n)
                || (scenario == "log_session_bound" && files.len() != 128)
                || (scenario == "log_queue_bound" && !(100..=1000).contains(&accepted))
            {
                return Err("Storage fixture count mismatch".into());
            }
            for rows in files.values() {
                for row in rows.as_array().unwrap() {
                    if row["schema_version"] != 2
                        || row["timestamp"] != "<timestamp>"
                        || row["request_id"].as_str().is_none()
                    {
                        return Err("Storage row schema mismatch".into());
                    }
                }
            }
            if ["log_continuous", "log_close", "log_queue_bound"].contains(&scenario) {
                for (i, row) in semantic["files"]["session-a"]
                    .as_array()
                    .ok_or("Missing ordered session")?
                    .iter()
                    .enumerate()
                {
                    if row["request_id"] != format!("request-{i}") {
                        return Err("Storage row order mismatch".into());
                    }
                }
            }
            let warnings = usize::from(
                [
                    "log_queue_bound",
                    "log_session_bound",
                    "log_append_failure",
                    "log_init_warning_failure",
                ]
                .contains(&scenario),
            );
            if semantic["warnings"] != warnings {
                return Err("Storage warning mismatch".into());
            }
        } else if [
            "status_create_failure",
            "status_initial_failure",
            "status_readiness_timeout",
        ]
        .contains(&scenario)
        {
            if semantic["ready"] != false || semantic["late_update_ignored"] != true {
                return Err("Storage readiness mismatch".into());
            }
        } else {
            if semantic["ready"] != true
                || semantic["private"] != true
                || semantic["removed"] != true
            {
                return Err("Storage lifecycle mismatch".into());
            }
            let snapshot = &semantic["snapshot"];
            if snapshot["version"] != 1 || snapshot["pid"] != 0 || snapshot["heartbeat_at"] != 0 {
                return Err("Storage snapshot mismatch".into());
            }
            let sessions = snapshot["sessions"]
                .as_object()
                .ok_or("Missing status sessions")?;
            let bursts: usize = if ["status_held", "status_concurrent_close"].contains(&scenario) {
                2000
            } else if scenario == "status_normal" {
                60
            } else {
                3
            };
            if sessions.len() != 20.min(bursts) {
                return Err("Status session count mismatch".into());
            }
            for i in bursts.saturating_sub(20)..bursts {
                let row = sessions
                    .get(&format!("s-{}", i % 20))
                    .ok_or("Missing status session identity")?;
                if row["request_id"] != format!("r-{i}") || row["phase"] != "ready" {
                    return Err("Stale status snapshot".into());
                }
            }
        }
    } else {
        validate_measurement(semantic, scenario, profile)?;
    }
    Ok(())
}
async fn invoke(
    command: &mut Command,
    scenario: &str,
    implementation: &str,
    mode: &str,
    profile: &str,
    cancel: &CancellationToken,
) -> Result<(Value, Value), String> {
    let mut output = Vec::new();
    let response = CancellationToken::new();
    let execution = run_child(
        command,
        RunOptions {
            timeout: Duration::from_secs(if mode == "measure" { 65 } else { 20 }),
            grace: Duration::from_secs(5),
            max_stdout: Some(CHILD_LIMIT + 1),
            interactive: false,
            response: &response,
            cancel,
            initial: InputAction::default(),
        },
        |bytes| {
            output.extend_from_slice(bytes);
            InputAction {
                bytes: Vec::new(),
                close: output.last() == Some(&b'\n'),
            }
        },
    )
    .await;
    let parsed: Result<Value, _> = serde_json::from_slice(&output);
    let report = parsed.unwrap_or_else(
        |_| json!({"invalid_report":true,"stdout_bytes":output.len(),"stdout_sha256":sha(&output)}),
    );
    let mut execution = execution;
    if mode == "validate" {
        execution.as_object_mut().unwrap().remove("duration_ms");
    }
    if execution["exit_code"] != 0
        || execution["cancelled"] == true
        || execution["timed_out"] == true
        || execution["output_limit_exceeded"] == true
        || execution["stderr_bytes"] != 0
    {
        return Ok((
            report,
            json!({"process":execution,"accepted":false,"reason":"child_process_failed"}),
        ));
    }
    let verdict = valid_child(&report, scenario, implementation, mode, profile);
    Ok((
        report,
        json!({"process":execution,"accepted":verdict.is_ok(),"reason":verdict.err()}),
    ))
}
fn condition_schema(host: Value) -> Value {
    json!({"host":host,"native_build":{"package_version":env!("CARGO_PKG_VERSION"),"driver_profile":if cfg!(debug_assertions){"debug"}else{"release"},"target_os":std::env::consts::OS,"target_arch":std::env::consts::ARCH,"compiler_provenance":"Installed rustc is environment information, not proof of candidate compilation; executable/source hashes remain separate."},"clock":{"native":"std::time::Instant monotonic elapsed time","reference":"node:perf_hooks performance.now monotonic elapsed time","event_clock":"Validation uses fixed event time; measurement uses each public API normal wall clock. Normalized snapshot timestamps are not timing samples"},"workers":{"driver_async_threads":1,"native_async_threads":1,"reference_javascript_threads":1,"background_workers":"Runtime filesystem/background workers remain enabled; no process-tree resource measurements."},"power_and_load":"Observed settings/load retained where available; workload is uncontrolled and representative-hardware acceptance remains false"})
}
fn measurement_conditions(root: &Path) -> Value {
    let output = |program: &str, args: &[&str]| {
        crate::process::capture(
            std::process::Command::new(program)
                .args(args)
                .current_dir(root.join("rust")),
            b"",
            Duration::from_secs(5),
        )
        .ok()
        .filter(|b| b.len() <= 16384)
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| s.trim().to_owned())
    };
    let (cpu, memory, power, load) = if cfg!(target_os = "macos") {
        (
            output("/usr/sbin/sysctl", &["-n", "machdep.cpu.brand_string"]),
            output("/usr/sbin/sysctl", &["-n", "hw.memsize"]),
            output("/usr/bin/pmset", &["-g", "custom"]),
            output("/usr/sbin/sysctl", &["-n", "vm.loadavg"]),
        )
    } else {
        (
            std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("model name"))
                    .map(str::to_owned)
            }),
            std::fs::read_to_string("/proc/meminfo").ok().and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("MemTotal:"))
                    .map(str::to_owned)
            }),
            None,
            std::fs::read_to_string("/proc/loadavg").ok(),
        )
    };
    condition_schema(
        json!({"os_release":output("/usr/bin/uname",&["-r"]),"cpu":cpu,"memory":memory,"logical_cpus":std::thread::available_parallelism().map(usize::from).ok(),"power":power,"background_load":load,"installed_rustc":output("rustc",&["--version","--verbose"]),"source_commit":output("git",&["rev-parse","HEAD"]),"environment_allowlist_only":true}),
    )
}
fn validate_measurement(value: &Value, scenario: &str, profile: &str) -> Result<(), String> {
    let (warmup, samples) = match profile {
        "paired" => (200, 2000),
        "legacy-status-workload" => (0, 60),
        _ => return Err("Invalid measurement profile".into()),
    };
    let delay = match scenario {
        "measure_normal" => 0,
        "measure_slow" => 20,
        _ => return Err("Invalid measurement scenario".into()),
    };
    if value["profile"] != profile
        || value["warmup_bursts"] != warmup
        || value["measured_bursts"] != samples
        || value["injected_write_delay_ms"] != delay
        || value["acceptance_qualified"] != false
        || !["allocations", "open_file_descriptors", "cpu", "peak_rss"]
            .iter()
            .all(|key| value.get(key) == Some(&Value::Null))
        || value["snapshot_writes"]
            .as_u64()
            .is_none_or(|n| n == 0 || n > 10000)
    {
        return Err("Measurement workload or qualification mismatch".into());
    }
    let raw = value["raw"]
        .as_object()
        .ok_or("Missing measurement samples")?;
    let metrics = [
        "update_ms",
        "flush_admission_ms",
        "flush_completion_ms",
        "timer_lateness_ms",
        "ready_ms",
        "close_ms",
    ];
    if raw.len() != metrics.len() {
        return Err("Measurement metric set mismatch".into());
    }
    for key in metrics {
        let rows = raw
            .get(key)
            .and_then(Value::as_array)
            .ok_or("Missing measurement metric")?;
        let valid_count = match key {
            "ready_ms" | "close_ms" => rows.len() == 1,
            "timer_lateness_ms" => (1..=10000).contains(&rows.len()),
            _ => rows.len() == samples as usize,
        };
        if !valid_count
            || rows
                .iter()
                .any(|v| v.as_f64().is_none_or(|n| !n.is_finite() || n < 0.))
        {
            return Err("Measurement sample count or value mismatch".into());
        }
    }
    let snapshot = &value["snapshot"];
    if snapshot["version"] != 1
        || snapshot["pid"] != 0
        || snapshot["heartbeat_at"] != 0
        || snapshot["sessions"]
            .as_object()
            .is_none_or(|v| v.len() != 20)
    {
        return Err("Measurement final snapshot mismatch".into());
    }
    for i in samples - 20..samples {
        let session = &snapshot["sessions"][format!("s-{}", i % 20)];
        if session["request_id"] != format!("r-{i}")
            || session["phase"] != "ready"
            || session["updated_at"] != 0
        {
            return Err("Measurement final session mismatch".into());
        }
    }
    Ok(())
}
fn statistics(raw: &Value) -> Result<Value, String> {
    let mut stats = json!({});
    if raw.as_object().is_none_or(|v| v.is_empty()) {
        return Err("Missing measurement metrics".into());
    }
    for (key, values) in raw.as_object().ok_or("Missing measurement samples")? {
        let mut values = values
            .as_array()
            .filter(|v| !v.is_empty() && v.len() <= 10000)
            .ok_or("Invalid measurement sample count")?
            .iter()
            .map(|v| {
                v.as_f64()
                    .filter(|n| n.is_finite() && *n >= 0.)
                    .ok_or("Invalid measurement sample")
            })
            .collect::<Result<Vec<_>, _>>()?;
        values.sort_by(f64::total_cmp);
        let q = |p: f64| values[((values.len() - 1) as f64 * p).floor() as usize];
        stats[key] = json!({"samples":values.len(),"p50":q(0.5),"p95":q(0.95),"p99":if values.len()>=2000{Some(q(0.99))}else{None},"max":values.last(),"method":"floor((n-1)*p); p99 omitted below 2000 observations"});
    }
    Ok(stats)
}
async fn execute(options: Options, root: &Path) -> Result<bool, String> {
    let signals = Signals::new()?;
    let cancel = &signals.token;
    let native = options
        .native
        .clone()
        .unwrap_or(std::env::current_exe().map_err(|_| "Cannot locate native storage executable")?)
        .canonicalize()
        .map_err(|_| "Cannot resolve native storage executable")?;
    let paired = options.implementation == "paired";
    let reference = options
        .reference
        .clone()
        .unwrap_or_else(|| root.join("artifacts/rust-rewrite/reference"));
    let node = if paired {
        crate::reference::verify(root, &reference)?;
        Some(program(options.node.as_deref(), "node")?)
    } else {
        None
    };
    let output = options.output.as_ref().unwrap();
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent
        .canonicalize()
        .map_err(|_| "Storage output parent must exist")?;
    let output = parent.join(output.file_name().ok_or("Invalid storage output")?);
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&output)
        .map_err(|_| "Storage output must be a new directory")?;
    let conditions = if options.mode == "measure" {
        measurement_conditions(root)
    } else {
        Value::Null
    };
    let mut total = 0;
    let protocol: Value =
        serde_json::from_str(PROTOCOL).map_err(|_| "Invalid embedded storage protocol")?;
    let mut inputs = vec![
        descriptor(&native, 256 * 1024 * 1024)?,
        descriptor(
            &root.join("rust/parity/storage-benchmark-v1.json"),
            1024 * 1024,
        )?,
    ];
    for path in [
        "rust/xtask/src/benchmark_storage.rs",
        "rust/xtask/src/main.rs",
        "rust/xtask/src/benchmark_storage_child.rs",
        "rust/xtask/src/benchmark_storage_legacy.rs",
        "rust/crates/autorouter-runtime/src/status_store.rs",
        "rust/crates/autorouter-runtime/src/session_log.rs",
        "rust/crates/autorouter-core/src/status_state.rs",
        "rust/crates/autorouter-core/src/telemetry_event.rs",
        "rust/parity/storage-contracts.capture.json",
    ] {
        inputs.push(descriptor(&root.join(path), 2 * 1024 * 1024)?);
    }
    if let Some(node) = &node {
        // Only paired runs load the oracle and its verifier. Native validation
        // must also work in an extracted source tree without Node helpers.
        for path in [
            "rust/parity/storage-reference.mjs",
            "scripts/rust-reference.mjs",
            "rust/parity/baseline.json",
        ] {
            inputs.push(descriptor(&root.join(path), 2 * 1024 * 1024)?);
        }
        inputs.push(descriptor(node, 256 * 1024 * 1024)?);
    }
    let immutable = output.join("input-snapshot");
    fs::create_dir(&immutable).map_err(|_| "Cannot create storage input snapshot")?;
    for input in &inputs {
        let path = Path::new(
            input["path"]
                .as_str()
                .ok_or("Invalid storage source path")?,
        );
        if path == native || node.as_deref() == Some(path) {
            continue;
        }
        if let Ok(relative) = path.strip_prefix(root) {
            if relative.starts_with("rust/target") || relative.starts_with("target") {
                continue;
            }
            let bytes = read_public_input(path, 2 * 1024 * 1024)?;
            if bytes.len() > RUN_LIMIT.saturating_sub(total) {
                return Err("Storage input snapshot bound".into());
            }
            let destination = immutable.join(relative);
            fs::create_dir_all(destination.parent().unwrap())
                .map_err(|_| "Cannot create storage snapshot parent")?;
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(destination)
                .and_then(|mut f| f.write_all(&bytes))
                .map_err(|_| "Cannot retain storage input")?;
            total += bytes.len();
        }
    }
    save(
        &output.join("preflight.json"),
        &json!({"schema_version":1,"kind":"storage_preflight","mode":options.mode,"implementation":options.implementation,"protocol":protocol,"protocol_sha256":sha(PROTOCOL.as_bytes()),"inputs":inputs,"measurement_conditions":conditions,"reference_verified":paired,"baseline_commit":protocol["baseline_commit"],"provider_calls":0,"numerical_acceptance":false}),
        &mut total,
    )?;
    let rounds = if options.mode == "validate" {
        1
    } else if options.profile == "paired" {
        5
    } else {
        3
    };
    let scenarios = if options.mode == "validate" {
        SCENARIOS
    } else {
        &["measure_normal", "measure_slow"]
    };
    let deadline = tokio::time::Instant::now()
        + Duration::from_secs(if options.mode == "validate" {
            600
        } else {
            1800
        });
    let mut rows = Vec::new();
    let mut comparisons = Vec::new();
    let mut passed = true;
    let deadline_cancel = cancel.clone();
    let deadline_owner = tokio::spawn(async move {
        tokio::time::sleep_until(deadline).await;
        deadline_cancel.cancel();
    });
    for round in 0..rounds {
        for scenario in scenarios {
            if cancel.is_cancelled() || tokio::time::Instant::now() >= deadline {
                passed = false;
                break;
            }
            let order = if !paired {
                vec!["native"]
            } else if round % 2 == 0 {
                vec!["node", "native"]
            } else {
                vec!["native", "node"]
            };
            let mut observations = BTreeMapForPair::default();
            for implementation in order {
                let scratch = Scratch::new("storage-child")?;
                let mut command = if implementation == "native" {
                    let mut c = Command::new(&native);
                    c.arg("__storage-child");
                    c
                } else {
                    let mut c = Command::new(node.as_ref().unwrap());
                    c.arg(immutable.join("rust/parity/storage-reference.mjs"))
                        .arg(&reference);
                    c
                };
                command
                    .args([
                        *scenario,
                        scratch.0.to_str().ok_or("Non-UTF8 storage scratch")?,
                        &options.mode,
                        &options.profile,
                    ])
                    .env_clear()
                    .env("HOME", &scratch.0)
                    .env("TMPDIR", &scratch.0)
                    .current_dir(&scratch.0);
                let (mut report, execution) = invoke(
                    &mut command,
                    scenario,
                    implementation,
                    &options.mode,
                    &options.profile,
                    cancel,
                )
                .await?;
                if options.mode == "measure" && execution["accepted"] == true {
                    report["statistics"] = statistics(&report["semantics"]["raw"])?;
                }
                let name = format!("{round}-{scenario}-{implementation}.json");
                save(
                    &output.join(&name),
                    &json!({"report":report,"execution":execution}),
                    &mut total,
                )?;
                let accepted = execution["accepted"] == true;
                passed &= accepted;
                if accepted {
                    observations.insert(implementation.to_owned(), report["semantics"].clone());
                }
                let scratch_path = scratch.0.clone();
                drop(scratch);
                let removed = !scratch_path.exists();
                passed &= removed;
                rows.push(json!({"round":round,"scenario":scenario,"implementation":implementation,"evidence":name,"accepted":accepted,"scratch_removed":removed}));
                if cancel.is_cancelled() {
                    break;
                }
            }
            if paired && options.mode == "validate" {
                let equal = observations.len() == 2
                    && observations.get("node").and_then(|v| semantic_sha(v).ok())
                        == observations
                            .get("native")
                            .and_then(|v| semantic_sha(v).ok());
                passed &= equal;
                comparisons.push(json!({"round":round,"scenario":scenario,"equal":equal,"comparison":"exact ECMAScript canonical semantic hash; no tolerance"}));
            }
        }
    }
    deadline_owner.abort();
    let _ = deadline_owner.await;
    let identity_checks=inputs.iter().map(|input|{let path=Path::new(input["path"].as_str().unwrap());let check=read_public_input(path,256*1024*1024).map(|bytes|json!({"path":path,"unchanged":sha(&bytes)==input["sha256"].as_str().unwrap()&&bytes.len()as u64==input["bytes"].as_u64().unwrap()})).unwrap_or_else(|_|json!({"path":path,"unchanged":false}));passed&=check["unchanged"]==true;check}).collect::<Vec<_>>();
    if paired && crate::reference::verify(root, &reference).is_err() {
        passed = false;
    }
    save(
        &output.join("postflight.json"),
        &json!({"inputs":identity_checks,"reference_reverified":paired,"passed":passed}),
        &mut total,
    )?;
    let expected = rounds * scenarios.len() * if paired { 2 } else { 1 };
    passed &= rows.len() == expected && !cancel.is_cancelled();
    let summary = json!({"schema_version":1,"kind":"native_storage_benchmark","mode":options.mode,"profile":options.profile,"completed":rows.len()==expected,"passed":passed,"expected_executions":expected,"executions":rows,"comparisons":comparisons,"protocol_sha256":sha(PROTOCOL.as_bytes()),"numerical_values_retained":options.mode=="measure","acceptance_qualified":false,"representative_hardware_acceptance":false,"overall_plan_performance_gate":"incomplete","provider_calls":0,"measurement_conditions_after":if options.mode=="measure"{measurement_conditions(root)}else{Value::Null},"unqualified":protocol["unqualified"],"limits":"Synthetic isolated public storage APIs. No HTTP or inference; wrapper ownership is not an OS descriptor audit. Numerical mode is descriptive and cannot use historical synchronous-baseline gates."});
    save(&output.join("report.json"), &summary, &mut total)?;
    println!(
        "{}",
        json!({"report":output.join("report.json"),"passed":passed,"mode":options.mode,"acceptance_qualified":false})
    );
    Ok(passed)
}
type BTreeMapForPair = std::collections::BTreeMap<String, Value>;
pub fn child(args: &[String]) -> Result<(), String> {
    child::run(args)
}
pub fn run(args: &[String], root: &Path) -> Result<bool, String> {
    if args == ["--help"] {
        println!("{HELP}");
        return Ok(true);
    }
    let options = parse(args)?;
    if options.mode == "legacy" {
        let report = legacy::compare(
            &read(options.legacy.as_ref().unwrap(), 16 * 1024 * 1024)?,
            options.label.as_deref().unwrap(),
        )?;
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|_| "Cannot encode legacy comparison")?
        );
        return Ok(report["passed"] == true);
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Cannot start storage driver")?
        .block_on(execute(options, root))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }
    fn valid_metadata() -> Value {
        let manifest: Value = serde_json::from_str(EXPECTATIONS).unwrap();
        json!({"schema_version":1,"kind":"storage_child","implementation":"native","scenario":"log_metadata","mode":"validate","protocol_sha256":sha(PROTOCOL.as_bytes()),"passed":true,"semantics":manifest["negative_control_fixture"],"error":null,"cleanup":{"active_io":0,"joined":true,"scratch_empty":true},"numerical_values_retained":false})
    }
    #[test]
    fn modes_are_explicit_and_native_never_accepts_reference_loading() {
        assert!(parse(&args(&["--validate", "--output", "new"])).is_ok());
        for bad in [
            vec![],
            vec!["--output", "new"],
            vec!["--validate", "--measure", "--output", "new"],
            vec!["--validate", "--validate", "--output", "new"],
            vec!["--validate", "--output", "new", "--node", "/node"],
            vec!["--validate", "--output", "new", "--profile", "paired"],
            vec!["--module", "private.mjs"],
            vec!["--validate", "--output", "new", "--unknown", "value"],
        ] {
            assert!(parse(&args(&bad)).is_err(), "{bad:?}");
        }
    }
    #[test]
    fn full_frozen_semantics_reject_forged_model_usage_pricing_and_warning() {
        let valid = valid_metadata();
        valid_child(&valid, "log_metadata", "native", "validate", "paired").unwrap();
        let key = valid["semantics"]["files"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone();
        for mutation in 0..6 {
            let mut wrong = valid.clone();
            match mutation {
                0 => wrong["semantics"]["files"][&key][0]["selected_model"] = json!("forged-model"),
                1 => wrong["semantics"]["files"][&key][1]["usage"]["input_tokens"] = json!(101),
                2 => wrong["semantics"]["files"][&key][1]["pricing_version"] = json!("future"),
                3 => wrong["semantics"]["warning_messages"] = json!(["different warning"]),
                4 => wrong["semantics"]["files"][&key]
                    .as_array_mut()
                    .unwrap()
                    .reverse(),
                _ => {
                    wrong["semantics"]["files"]
                        .as_object_mut()
                        .unwrap()
                        .remove(&key);
                }
            }
            assert!(valid_child(&wrong, "log_metadata", "native", "validate", "paired").is_err());
        }
    }
    #[test]
    fn identity_missing_cleanup_and_numerical_forgery_are_rejected() {
        for key in [
            "kind",
            "scenario",
            "mode",
            "implementation",
            "protocol_sha256",
            "passed",
            "cleanup",
            "numerical_values_retained",
            "semantics",
        ] {
            let mut wrong = valid_metadata();
            wrong.as_object_mut().unwrap().remove(key);
            assert!(
                valid_child(&wrong, "log_metadata", "native", "validate", "paired").is_err(),
                "{key}"
            );
        }
        let mut wrong = valid_metadata();
        wrong["cleanup"]["active_io"] = json!(1);
        assert!(valid_child(&wrong, "log_metadata", "native", "validate", "paired").is_err());
    }
    #[test]
    fn fifo_symlink_and_actual_byte_bounds_are_rejected_before_read() {
        let scratch = Scratch::new("storage-input-test").unwrap();
        let fifo = scratch.0.join("fifo");
        nix::unistd::mkfifo(
            &fifo,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        assert!(read(&fifo, 1024).is_err());
        assert!(read_public_input(&fifo, 1024).is_err());
        let file = scratch.file("input", b"12345").unwrap();
        assert!(read(&file, 4).is_err());
        assert!(read_public_input(&file, 4).is_err());
        assert_eq!(read(&file, 5).unwrap(), b"12345");
        assert_eq!(read_public_input(&file, 5).unwrap(), b"12345");
        let link = scratch.0.join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(read(&link, 5).is_err());
        assert!(read_public_input(&link, 5).is_err());
        let hard = scratch.0.join("hard");
        fs::hard_link(&file, &hard).unwrap();
        assert!(read(&hard, 5).is_err());
        assert!(read(&file, 5).is_err());
        assert_eq!(read_public_input(&hard, 5).unwrap(), b"12345");
        assert_eq!(descriptor(&file, 5).unwrap()["sha256"], sha(b"12345"));
        assert_eq!(descriptor(&hard, 5).unwrap()["sha256"], sha(b"12345"));
        assert!(descriptor(&hard, 4).is_err());
        assert!(read_public_input(&scratch.0, 1024).is_err());
    }
    #[test]
    fn encoding_and_aggregate_newline_are_bounded_before_write() {
        assert!(encode(&json!("abcd"), 5).is_err());
        assert_eq!(encode(&json!("abcd"), 6).unwrap(), b"\"abcd\"");
        let scratch = Scratch::new("storage-budget-test").unwrap();
        let file = scratch.0.join("report");
        let mut total = RUN_LIMIT - 1;
        assert!(save(&file, &json!(0), &mut total).is_err());
        assert!(!file.exists());
        let mut total = 0;
        save(&file, &json!(0), &mut total).unwrap();
        assert_eq!(total, 2);
        assert!(save(&file, &json!(1), &mut total).is_err());
    }
    #[test]
    fn canonical_hash_preserves_values_without_object_order_or_integer_float_artifacts() {
        assert_eq!(
            semantic_sha(&json!({"z":0.0,"a":[1,2]})).unwrap(),
            semantic_sha(&json!({"a":[1,2],"z":0})).unwrap()
        );
        assert_ne!(
            semantic_sha(&json!([1, 2])).unwrap(),
            semantic_sha(&json!([2, 1])).unwrap()
        );
        assert_ne!(
            semantic_sha(&json!(0.00001)).unwrap(),
            semantic_sha(&json!(0.00002)).unwrap()
        );
    }
    #[test]
    fn numerical_schema_and_percentiles_are_tested_without_measurement() {
        let conditions = condition_schema(
            json!({"cpu":"synthetic","memory":16000000000u64,"power":"uncontrolled","background_load":"synthetic"}),
        );
        assert_eq!(conditions["workers"]["native_async_threads"], 1);
        assert!(
            conditions["clock"]["native"]
                .as_str()
                .unwrap()
                .contains("monotonic")
        );
        assert!(
            conditions["native_build"]["compiler_provenance"]
                .as_str()
                .unwrap()
                .contains("not proof")
        );
        let stats = statistics(&json!({"update_ms":[9,1,5]})).unwrap();
        assert_eq!(stats["update_ms"]["p50"], 5.0);
        assert_eq!(stats["update_ms"]["p95"], 5.0);
        assert_eq!(stats["update_ms"]["p99"], Value::Null);
        assert!(statistics(&json!({"bad":[]})).is_err());
        assert!(statistics(&json!({"bad":[-1]})).is_err());
    }
    #[test]
    fn measurement_reports_require_full_protocol_without_running_measurements() {
        for profile in ["paired", "legacy-status-workload"] {
            let (warmup, count) = if profile == "paired" {
                (200, 2000)
            } else {
                (0, 60)
            };
            let mut sessions = json!({});
            for i in count - 20..count {
                sessions[format!("s-{}", i % 20)] =
                    json!({"request_id":format!("r-{i}"),"phase":"ready","updated_at":0});
            }
            let value = json!({"profile":profile,"warmup_bursts":warmup,"measured_bursts":count,"injected_write_delay_ms":20,"acceptance_qualified":false,"allocations":null,"open_file_descriptors":null,"cpu":null,"peak_rss":null,"snapshot_writes":2,"snapshot":{"version":1,"pid":0,"heartbeat_at":0,"sessions":sessions},"raw":{"update_ms":vec![0;count],"flush_admission_ms":vec![0;count],"flush_completion_ms":vec![0;count],"timer_lateness_ms":[0],"ready_ms":[0],"close_ms":[0]}});
            validate_measurement(&value, "measure_slow", profile).unwrap();
            for key in value.as_object().unwrap().keys() {
                let mut wrong = value.clone();
                wrong.as_object_mut().unwrap().remove(key);
                assert!(
                    validate_measurement(&wrong, "measure_slow", profile).is_err(),
                    "{key}"
                );
            }
            for key in value["raw"].as_object().unwrap().keys() {
                for bad in [json!([]), json!([-1]), json!("wrong")] {
                    let mut wrong = value.clone();
                    wrong["raw"][key] = bad;
                    assert!(
                        validate_measurement(&wrong, "measure_slow", profile).is_err(),
                        "{key}"
                    );
                }
            }
            let mut wrong = value.clone();
            wrong["raw"]["unknown"] = json!([0]);
            assert!(validate_measurement(&wrong, "measure_slow", profile).is_err());
            assert!(validate_measurement(&value, "measure_normal", profile).is_err());
            let mut wrong = value.clone();
            wrong["acceptance_qualified"] = json!(true);
            assert!(validate_measurement(&wrong, "measure_slow", profile).is_err());
        }
        assert!(statistics(&json!({})).is_err());
    }
    #[test]
    fn snapshot_projection_rejects_missing_wrong_or_invalid_original_fields() {
        let original = json!({"pid":std::process::id(),"heartbeat_at":1760000000000u64,"sessions":{"s":{"updated_at":1760000000000u64}}});
        let mut valid = original.clone();
        normalize_snapshot(&mut valid, true).unwrap();
        for field in ["pid", "heartbeat_at", "updated_at"] {
            for value in [
                None,
                Some(Value::Null),
                Some(json!("private")),
                Some(json!(-1)),
            ] {
                let mut wrong = original.clone();
                let target = if field == "updated_at" {
                    wrong["sessions"]["s"].as_object_mut().unwrap()
                } else {
                    wrong.as_object_mut().unwrap()
                };
                if let Some(value) = value {
                    target.insert(field.into(), value);
                } else {
                    target.remove(field);
                }
                let unchanged = wrong.clone();
                assert!(normalize_snapshot(&mut wrong, true).is_err());
                assert_eq!(wrong, unchanged);
            }
        }
    }
    #[tokio::test]
    async fn malformed_nonzero_missing_and_cancelled_children_never_pass() {
        for (program, args) in [
            ("/bin/sh", vec!["-c", "printf '{\\n'"]),
            ("/bin/sh", vec!["-c", "exit 37"]),
            ("/definitely/not/a/storage-program", vec![]),
        ] {
            let cancel = CancellationToken::new();
            let (_, result) = invoke(
                Command::new(program).args(args),
                "log_metadata",
                "native",
                "validate",
                "paired",
                &cancel,
            )
            .await
            .unwrap();
            assert_eq!(result["accepted"], false);
        }
        let cancel = CancellationToken::new();
        let child_cancel = cancel.clone();
        let deadline = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            child_cancel.cancel();
        });
        let (_, result) = invoke(
            Command::new("/bin/sh").args(["-c", "exec /bin/sleep 60"]),
            "log_metadata",
            "native",
            "validate",
            "paired",
            &cancel,
        )
        .await
        .unwrap();
        deadline.await.unwrap();
        assert_eq!(result["accepted"], false);
        assert_eq!(result["process"]["cancelled"], true);
        assert_eq!(result["process"]["duration_ms"], Value::Null);
    }
}
