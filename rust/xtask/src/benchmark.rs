//! Explicit synthetic measurements of complete executables, never a CI timer gate.
#[path = "benchmark_measure.rs"]
mod measure;
#[path = "benchmark_service.rs"]
mod service;
use crate::process::read_bounded;
use crate::tool_process::Scratch;
use autorouter_runtime::http_client::{HttpTransport, NativeHttpClient};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::Request;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "synthetic-benchmark-local-credential";
const MODEL: &str = "claude-opus-5-5";
const SELECTED: &str = "claude-sonnet-5";
const PROTOCOL: &str = include_str!("../../parity/local-benchmark-v1.json");
const GATES: &str = include_str!("../../parity/performance-gates.json");
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn write(path: &Path, value: &Value) -> Result<(), String> {
    let bytes =
        serde_json::to_vec_pretty(value).map_err(|_| "Cannot serialize benchmark evidence")?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|_| "Refusing to overwrite benchmark evidence")?;
    file.write_all(&bytes)
        .map_err(|_| "Cannot write benchmark evidence".into())
}
#[derive(Clone)]
struct Executable {
    name: &'static str,
    program: PathBuf,
    arguments: Vec<String>,
}
impl Executable {
    fn command(&self, scratch: &Path, argument: &str) -> Command {
        let mut command = Command::new(&self.program);
        if self.name == "node" && argument == "statusline" {
            command.arg(Path::new(&self.arguments[0]).with_file_name("statusline.mjs"));
        } else {
            command.args(&self.arguments).arg(argument);
        }
        command
            .env_clear()
            .current_dir(scratch)
            .env("AUTOROUTER_CONFIG", scratch.join("absent-config.json"))
            .env("AUTOROUTER_EVALUATOR", "jev")
            .env("AUTOROUTER_AUTH_MODE", "api-key")
            .env("ANTHROPIC_API_KEY", "synthetic-benchmark-provider-key")
            .env("TYPESAFE_API_KEY", "synthetic-benchmark-evaluator-key")
            .env("AUTOROUTER_TOKEN", TOKEN)
            .env("AUTOROUTER_STATUSLINE", "0")
            .env("NO_COLOR", "")
            .env("COLUMNS", "80");
        for key in ["PATH", "LANG", "LC_ALL", "TMPDIR"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
    }
}
fn node_path() -> Result<PathBuf, String> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join("node"))
        .find(|p| p.is_file())
        .and_then(|p| p.canonicalize().ok())
        .ok_or("Cannot locate the frozen Node reference runtime".into())
}
#[derive(Clone)]
struct Workload {
    id: &'static str,
    tools: bool,
    cached: bool,
    delay_ms: u64,
    concurrency: usize,
}
fn workloads() -> Vec<Workload> {
    let mut result = Vec::new();
    for (id, tools, cached, delay, concurrency) in [
        ("small_miss", false, false, 0, &[1, 8, 32, 128][..]),
        ("small_delayed", false, false, 5, &[1][..]),
        ("large_catalog_miss", true, false, 0, &[1, 8, 32, 128][..]),
        ("cache_hit", false, true, 0, &[1, 8, 32, 128][..]),
    ] {
        for &concurrency in concurrency {
            result.push(Workload {
                id,
                tools,
                cached,
                delay_ms: delay,
                concurrency,
            });
        }
    }
    result
}
struct Fixture {
    bytes: Bytes,
    index_offset: usize,
    cached: bool,
}
impl Fixture {
    fn new(workload: &Workload, round: usize) -> Self {
        let mut value = json!({"model":MODEL,"max_tokens":128,"messages":[{"role":"user","content":format!("Synthetic benchmark {} round{round}: 000000000000",workload.id)}]});
        if workload.tools {
            value["tools"]=json!((0..300).map(|index|json!({"name":format!("synthetic_{index}"),"description":"Synthetic tool description. ".repeat(30),"input_schema":{"type":"object","properties":{"value":{"type":"string"}}}})).collect::<Vec<_>>());
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        let index_offset = bytes
            .windows(12)
            .position(|v| v == b"000000000000")
            .unwrap();
        Self {
            bytes: Bytes::from(bytes),
            index_offset,
            cached: workload.cached,
        }
    }
    fn body(&self, index: usize) -> Bytes {
        if self.cached {
            return self.bytes.clone();
        }
        let mut bytes = self.bytes.to_vec();
        bytes[self.index_offset..self.index_offset + 12]
            .copy_from_slice(format!("{index:012}").as_bytes());
        Bytes::from(bytes)
    }
}
struct Gateway {
    child: Child,
    address: std::net::SocketAddr,
    ready_ms: f64,
    _scratch: Scratch,
    stderr: PathBuf,
}
impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Gateway {
    async fn start(
        executable: &Executable,
        mock: &service::Mock,
        client: &NativeHttpClient,
    ) -> Result<Self, String> {
        let scratch = Scratch::new("benchmark-gateway")?;
        scratch.file("absent-config.json", b"{}")?;
        let reservation = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(|_| "Cannot reserve gateway benchmark port")?;
        let address = reservation
            .local_addr()
            .map_err(|_| "Cannot inspect reserved port")?;
        drop(reservation);
        let stderr = scratch.file("stderr", b"")?;
        let output = OpenOptions::new()
            .write(true)
            .open(&stderr)
            .map_err(|_| "Cannot open gateway diagnostics")?;
        let started = Instant::now();
        let child = executable
            .command(&scratch.0, "serve")
            .env("AUTOROUTER_PORT", address.port().to_string())
            .env(
                "AUTOROUTER_UPSTREAM_URL",
                format!("http://{}", mock.address),
            )
            .env(
                "AUTOROUTER_JEV_URL",
                format!("http://{}/v1/systemone", mock.address),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(output))
            .spawn()
            .map_err(|_| "Cannot launch benchmark gateway")?;
        let mut gateway = Self {
            child,
            address,
            ready_ms: 0.0,
            _scratch: scratch,
            stderr,
        };
        loop {
            if gateway
                .child
                .try_wait()
                .map_err(|_| "Cannot inspect benchmark gateway")?
                .is_some()
            {
                return Err(format!(
                    "{} gateway exited before readiness",
                    executable.name
                ));
            }
            let request = Request::get(format!("http://{address}/health"))
                .header("x-api-key", TOKEN)
                .body(Full::new(Bytes::new()))
                .unwrap();
            let ready = async {
                let response = client.request(request).await.ok()?;
                if response.status() != 200 {
                    return None;
                }
                let body = Limited::new(response.into_body(), 1024)
                    .collect()
                    .await
                    .ok()?
                    .to_bytes();
                (body.as_ref() == br#"{"status":"ok"}"#).then_some(())
            };
            if tokio::time::timeout(Duration::from_millis(250), ready)
                .await
                .ok()
                .flatten()
                .is_some()
            {
                break;
            }
            if started.elapsed() > Duration::from_secs(10) {
                return Err(format!("{} gateway readiness timed out", executable.name));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        gateway.ready_ms = started.elapsed().as_secs_f64() * 1000.0;
        Ok(gateway)
    }
    async fn stop(&mut self) -> Result<f64, String> {
        let started = Instant::now();
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.child.id() as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
        loop {
            if let Some(status) = self
                .child
                .try_wait()
                .map_err(|_| "Cannot wait for benchmark gateway cleanup")?
            {
                if !status.success() {
                    return Err("Benchmark gateway did not exit cleanly".into());
                }
                if std::net::TcpStream::connect_timeout(&self.address, Duration::from_millis(50))
                    .is_ok()
                {
                    return Err("Benchmark gateway left its listener open".into());
                }
                return Ok(started.elapsed().as_secs_f64() * 1000.0);
            }
            if started.elapsed() > Duration::from_secs(10) {
                return Err("Benchmark gateway cleanup timed out".into());
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
}
async fn request(
    client: &NativeHttpClient,
    address: std::net::SocketAddr,
    body: Bytes,
    index: usize,
    concurrency: usize,
) -> Result<(f64, f64), String> {
    let request = Request::post(format!("http://{address}/v1/messages"))
        .header("x-api-key", TOKEN)
        .header("content-type", "application/json")
        .header("x-claude-code-session-id", "synthetic-benchmark-session")
        .header(
            "x-claude-code-agent-id",
            format!("agent-{}", index % concurrency),
        )
        .header(
            "x-claude-code-prompt-id",
            format!("synthetic-prompt-{index}"),
        )
        .body(Full::new(body))
        .unwrap();
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(15), async {
        let response = client
            .request(request)
            .await
            .map_err(|_| "Benchmark HTTP request failed")?;
        let first = started.elapsed().as_secs_f64() * 1000.0;
        if response.status() != 200 {
            return Err("Benchmark response status mismatch".into());
        }
        let bytes = Limited::new(response.into_body(), 1024 * 1024)
            .collect()
            .await
            .map_err(|_| "Benchmark response exceeded limit or was truncated")?
            .to_bytes();
        if bytes.as_ref() != service::RESPONSE {
            return Err("Benchmark response bytes mismatch".into());
        }
        Ok((started.elapsed().as_secs_f64() * 1000.0, first))
    })
    .await
    .map_err(|_| "Benchmark HTTP request timed out")?
}
async fn batch(
    client: &NativeHttpClient,
    address: std::net::SocketAddr,
    fixture: Arc<Fixture>,
    concurrency: usize,
    start: usize,
    count: usize,
) -> Result<(Vec<f64>, Vec<f64>, f64), String> {
    let index = Arc::new(AtomicUsize::new(0));
    let mut tasks = tokio::task::JoinSet::new();
    let started = Instant::now();
    for _ in 0..concurrency {
        let client = client.clone();
        let index = index.clone();
        let fixture = fixture.clone();
        tasks.spawn(async move {
            let mut results = Vec::new();
            loop {
                let n = index.fetch_add(1, Ordering::SeqCst);
                if n >= count {
                    break;
                }
                results.push(
                    request(
                        &client,
                        address,
                        fixture.body(start + n),
                        start + n,
                        concurrency,
                    )
                    .await?,
                );
            }
            Ok::<_, String>(results)
        });
    }
    let mut times = Vec::with_capacity(count);
    let mut firsts = Vec::with_capacity(count);
    while let Some(result) = tasks.join_next().await {
        for (time, first) in result.map_err(|_| "Benchmark client task failed")?? {
            times.push(time);
            firsts.push(first);
        }
    }
    Ok((times, firsts, started.elapsed().as_secs_f64()))
}
async fn http(
    executable: Option<&Executable>,
    workload: &Workload,
    round: usize,
    validate: bool,
) -> Result<Value, String> {
    let fixture = Arc::new(Fixture::new(workload, round));
    let mock = service::Mock::start(
        workload.delay_ms,
        if executable.is_some() && !workload.tools {
            SELECTED
        } else {
            MODEL
        },
    )
    .await?;
    let client = NativeHttpClient::new().map_err(|_| "Cannot initialize benchmark HTTP client")?;
    let mut gateway = match executable {
        Some(executable) => Some(Gateway::start(executable, &mock, &client).await?),
        None => None,
    };
    let address = gateway.as_ref().map(|v| v.address).unwrap_or(mock.address);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let idle = match &gateway {
        Some(gateway) => measure::process(gateway.child.id()).await,
        None => Value::Null,
    };
    let warmup = if validate { 8 } else { 200 };
    let measured = if validate { 32 } else { 2000 };
    batch(
        &client,
        address,
        fixture.clone(),
        workload.concurrency,
        0,
        warmup,
    )
    .await?;
    let before = mock.counts.snapshot();
    let cpu_before = match &gateway {
        Some(gateway) => measure::process(gateway.child.id()).await,
        None => Value::Null,
    };
    let sampling = CancellationToken::new();
    let stop_sampling = sampling.clone();
    let pid = gateway.as_ref().map(|v| v.child.id());
    let sampler = tokio::spawn(async move {
        let mut peak = None;
        loop {
            if let Some(pid) = pid
                && let Some(rss) = measure::process(pid).await["rss_bytes"].as_u64()
            {
                peak = Some(peak.unwrap_or(0).max(rss));
            }
            tokio::select! {_=stop_sampling.cancelled()=>break,_=tokio::time::sleep(Duration::from_millis(100))=>{}}
        }
        peak
    });
    let measured_result = batch(
        &client,
        address,
        fixture.clone(),
        workload.concurrency,
        warmup,
        measured,
    )
    .await;
    sampling.cancel();
    let peak = sampler
        .await
        .map_err(|_| "Benchmark resource sampler failed")?;
    let (times, firsts, seconds) = measured_result?;
    let cpu_after = match &gateway {
        Some(gateway) => measure::process(gateway.child.id()).await,
        None => Value::Null,
    };
    let after = mock.counts.snapshot();
    let delta: Vec<_> = after
        .iter()
        .zip(before.iter())
        .map(|(a, b)| a - b)
        .collect();
    let expected_evaluations = if executable.is_some() && !workload.cached {
        measured as u64
    } else {
        0
    };
    // The frozen router speculatively counts against Haiku for a large catalog,
    // even when the evaluator subsequently selects a native-million Sonnet.
    let expected_counts = if executable.is_some() && workload.tools {
        measured as u64
    } else {
        0
    };
    if delta != [expected_evaluations, measured as u64, expected_counts, 0] {
        return Err(format!(
            "Benchmark deterministic call-count mismatch for {} c{} {}: expected evaluator {expected_evaluations}, upstream {measured}, count{expected_counts}, invalid0; observed {delta:?}",
            workload.id,
            workload.concurrency,
            executable.map(|v| v.name).unwrap_or("direct")
        ));
    }
    let cpu_delta_ms = cpu_before["cpu_seconds"]
        .as_f64()
        .zip(cpu_after["cpu_seconds"].as_f64())
        .map(|(a, b)| (b - a) * 1000.0);
    let resolution_ms = if cfg!(target_os = "macos") {
        10.0
    } else {
        1000.0
    };
    let cpu_ms = cpu_delta_ms
        .filter(|v| *v >= resolution_ms * 10.0)
        .map(|v| v / measured as f64);
    let ready = gateway.as_ref().map(|v| v.ready_ms);
    let stderr_bytes = gateway
        .as_ref()
        .and_then(|g| std::fs::metadata(&g.stderr).ok())
        .map(|v| v.len());
    if stderr_bytes.is_some_and(|v| v > 64 * 1024 * 1024) {
        return Err("Benchmark diagnostic output exceeded limit".into());
    }
    let cleanup = match &mut gateway {
        Some(g) => Some(g.stop().await?),
        None => None,
    };
    Ok(
        json!({"implementation":executable.map(|v|v.name).unwrap_or("direct_upstream"),"workload":workload.id,"concurrency":workload.concurrency,"round":round,"fixture_bytes":fixture.bytes.len(),"fixture_sha256":digest(&fixture.bytes),"response_sha256":digest(service::RESPONSE),"deterministic_counts_passed":true,"requests":measured,"warmup_requests":warmup,"evaluator_calls":delta[0],"upstream_calls":delta[1],"count_token_calls":delta[2],"duration_seconds":seconds,"requests_per_second":measured as f64/seconds,"route_ms":measure::stats(&times),"first_response_headers_ms":measure::stats(&firsts),"raw_route_ms":times,"raw_first_response_headers_ms":firsts,"ready_ms":ready,"cleanup_ms":cleanup,"idle_rss_bytes":idle["rss_bytes"],"sampled_peak_rss_bytes":peak,"cpu_time_per_request_ms":cpu_ms,"cpu_time_delta_ms":cpu_delta_ms,"cpu_time_resolution_ms":resolution_ms,"diagnostic_bytes":stderr_bytes,"allocations":null,"open_file_descriptors":null,"true_peak_rss_bytes":null}),
    )
}
async fn startup(
    executable: &Executable,
    argument: &str,
    round: usize,
    validate: bool,
) -> Result<Value, String> {
    let scratch = Scratch::new("benchmark-startup")?;
    let stdout = scratch.file("stdout", b"")?;
    let stderr = scratch.file("stderr", b"")?;
    let warmup = if validate { 1 } else { 10 };
    let count = if validate { 2 } else { 100 };
    let mut times = Vec::with_capacity(count);
    let mut identity = None;
    for index in 0..warmup + count {
        let mut command = tokio::process::Command::from(executable.command(&scratch.0, argument));
        command
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                File::create(&stdout).map_err(|_| "Cannot stage benchmark stdout")?,
            ))
            .stderr(Stdio::from(
                File::create(&stderr).map_err(|_| "Cannot stage benchmark stderr")?,
            ))
            .kill_on_drop(true);
        let started = Instant::now();
        let mut child = command
            .spawn()
            .map_err(|_| "Cannot launch startup benchmark")?;
        let status = match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(value) => value.map_err(|_| "Cannot wait for startup benchmark")?,
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err("Startup benchmark timed out".into());
            }
        };
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        if !status.success() || !read_bounded(&stderr, 1024 * 1024)?.is_empty() {
            return Err("Startup benchmark exited unsuccessfully or produced diagnostics".into());
        }
        let sha = digest(&read_bounded(&stdout, 1024 * 1024)?);
        if identity.as_ref().is_some_and(|old| old != &sha) {
            return Err("Startup output changed between identical invocations".into());
        }
        identity = Some(sha);
        if index >= warmup {
            times.push(elapsed)
        }
    }
    Ok(
        json!({"implementation":executable.name,"workload":match argument{"--help"=>"startup_help","--version"=>"startup_version",_=>"startup_status"},"round":round,"startup_ms":measure::stats(&times),"raw_startup_ms":times,"stdout_sha256":identity,"cold_disk_cache":false}),
    )
}
fn scalar(row: &Value, path: &[&str]) -> Option<f64> {
    let mut value = row;
    for key in path {
        value = &value[*key];
    }
    value.as_f64()
}
fn comparisons(rows: &[Value]) -> Vec<Value> {
    let mut results = Vec::new();
    let mut keys = std::collections::BTreeSet::new();
    for row in rows.iter().filter(|v| v["implementation"] == "node") {
        keys.insert((
            row["workload"].as_str().unwrap().to_owned(),
            row["concurrency"].as_u64().unwrap_or(0),
        ));
    }
    for (workload, concurrency) in keys {
        let pairs = (1..=5)
            .filter_map(|round| {
                let find = |name| {
                    rows.iter().find(|v| {
                        v["workload"] == workload
                            && v["concurrency"].as_u64().unwrap_or(0) == concurrency
                            && v["round"] == round
                            && v["implementation"] == name
                    })
                };
                find("node").zip(find("rust"))
            })
            .collect::<Vec<_>>();
        let paths: Vec<(&str, Vec<&str>, f64, bool)> = if workload.starts_with("startup_") {
            vec![(
                "startup_median_ratio",
                vec!["startup_ms", "p50"],
                0.5,
                false,
            )]
        } else {
            vec![
                ("idle_rss_ratio", vec!["idle_rss_bytes"], 0.6, false),
                (
                    "cpu_per_request_ratio",
                    vec!["cpu_time_per_request_ms"],
                    0.7,
                    false,
                ),
                ("throughput_ratio", vec!["requests_per_second"], 0.9, true),
            ]
        };
        for (metric, path, threshold, minimum) in paths {
            let ratios = pairs
                .iter()
                .filter_map(|(a, b)| {
                    scalar(a, &path)
                        .zip(scalar(b, &path))
                        .filter(|(a, _)| *a > 0.0)
                        .map(|(a, b)| b / a)
                })
                .collect::<Vec<_>>();
            let interval = measure::interval(&ratios);
            let status = match (interval["low"].as_f64(), interval["high"].as_f64()) {
                (Some(lo), Some(hi))
                    if if minimum {
                        lo >= threshold
                    } else {
                        hi <= threshold
                    } =>
                {
                    "observed_target_met"
                }
                (Some(lo), Some(hi))
                    if if minimum {
                        hi < threshold
                    } else {
                        lo > threshold
                    } =>
                {
                    "observed_target_not_met"
                }
                (Some(_), Some(_)) => "inconclusive",
                _ => "unmeasured",
            };
            results.push(json!({"workload":workload,"concurrency":concurrency,"metric":metric,"paired_rounds":ratios.len(),"interval":interval,"threshold":threshold,"minimum":minimum,"status":status,"acceptance_qualified":false}));
        }
        if !workload.starts_with("startup_") {
            for percentile in ["p95", "p99"] {
                let margins = pairs
                    .iter()
                    .filter_map(|(a, b)| {
                        scalar(a, &["route_ms", percentile])
                            .zip(scalar(b, &["route_ms", percentile]))
                            .map(|(a, b)| b - a - (a * 0.1).max(0.5))
                    })
                    .collect::<Vec<_>>();
                let interval = measure::interval(&margins);
                let status = match (interval["low"].as_f64(), interval["high"].as_f64()) {
                    (_, Some(hi)) if hi <= 0.0 => "no_regression_detected",
                    (Some(lo), _) if lo > 0.0 => "regression_detected",
                    (Some(_), Some(_)) => "inconclusive",
                    _ => "unmeasured",
                };
                results.push(json!({"workload":workload,"concurrency":concurrency,"metric":format!("{percentile}_regression_margin_ms"),"interval":interval,"absolute_noise_floor_ms":0.5,"relative_allowance":0.1,"status":status,"acceptance_qualified":false}));
            }
            if workload == "large_catalog_miss" && concurrency == 1 {
                let ratios = pairs
                    .iter()
                    .filter_map(|(a, b)| {
                        scalar(a, &["route_ms", "p95"])
                            .zip(scalar(b, &["route_ms", "p95"]))
                            .filter(|(a, _)| *a > 0.0)
                            .map(|(a, b)| b / a)
                    })
                    .collect::<Vec<_>>();
                results.push(json!({"workload":workload,"concurrency":concurrency,"metric":"large_catalog_http_p95_ratio","interval":measure::interval(&ratios),"threshold":0.5,"status":"reported_http_metric_processing_only_gate_unmeasured","acceptance_qualified":false}));
            }
        }
    }
    results
}
fn source_files(directory: &Path, root: &Path, rows: &mut Vec<Value>) -> Result<(), String> {
    let mut entries = std::fs::read_dir(directory)
        .map_err(|_| "Cannot inspect benchmark sources")?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Cannot inspect source entry")?;
    entries.sort_by_key(|v| v.file_name());
    for entry in entries {
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|_| "Cannot inspect source type")?;
        if kind.is_symlink() {
            return Err("Benchmark source tree contains a symbolic link".into());
        }
        if kind.is_dir() {
            source_files(&path, root, rows)?;
        } else if kind.is_file() {
            let bytes = read_bounded(&path, 16 * 1024 * 1024)?;
            rows.push(json!({"path":path.strip_prefix(root).unwrap().to_string_lossy(),"sha256":digest(&bytes),"bytes":bytes.len()}));
        }
    }
    Ok(())
}
async fn execute(
    root: &Path,
    destination: &Path,
    binary: &Path,
    validate: bool,
) -> Result<bool, String> {
    let reference = root.join("artifacts/rust-rewrite/reference");
    crate::reference::freeze(root, &reference)?;
    let node = Executable {
        name: "node",
        program: node_path()?,
        arguments: vec![
            reference
                .join("bin/autorouter.mjs")
                .to_string_lossy()
                .into_owned(),
        ],
    };
    let native = Executable {
        name: "rust",
        program: binary
            .canonicalize()
            .map_err(|_| "Cannot resolve native benchmark executable")?,
        arguments: Vec::new(),
    };
    let mut source = Vec::new();
    for path in [
        "rust/crates",
        "rust/xtask",
        "rust/distribution",
        "rust/vendor",
    ] {
        if !root.join(path).exists() {
            continue;
        }
        source_files(&root.join(path), root, &mut source)?;
    }
    for path in [
        "rust/Cargo.toml",
        "rust/Cargo.lock",
        "rust/rust-toolchain.toml",
    ] {
        let bytes = read_bounded(&root.join(path), 16 * 1024 * 1024)?;
        source.push(json!({"path":path,"sha256":digest(&bytes),"bytes":bytes.len()}));
    }
    let before = measure::environment(root);
    let preflight = json!({"schema_version":1,"kind":"synthetic_native_benchmark_preflight","mode":if validate{"deterministic_validation"}else{"exploratory_measurement"},"baseline_commit":"ea930c247626ce2af5ccdad721b5121417bf4ad8","protocol":serde_json::from_str::<Value>(PROTOCOL).unwrap(),"protocol_sha256":digest(PROTOCOL.as_bytes()),"gates":serde_json::from_str::<Value>(GATES).unwrap(),"gates_sha256":digest(GATES.as_bytes()),"candidate_sha256":digest(&read_bounded(&native.program,32*1024*1024)?),"node_binary_sha256":digest(&read_bounded(&node.program,256*1024*1024)?),"source_manifest":source,"environment":before,"provider_calls":0,"reference_integrity_verified":true});
    write(&destination.join("preflight.json"), &preflight)?;
    let mut rows = Vec::new();
    let mut sequence = 0;
    let mut save = |mut row: Value| -> Result<(), String> {
        if validate {
            row.as_object_mut().unwrap().retain(|key, _| {
                !matches!(
                    key.as_str(),
                    "startup_ms"
                        | "raw_startup_ms"
                        | "route_ms"
                        | "first_response_headers_ms"
                        | "raw_route_ms"
                        | "raw_first_response_headers_ms"
                        | "duration_seconds"
                        | "requests_per_second"
                        | "ready_ms"
                        | "cleanup_ms"
                        | "idle_rss_bytes"
                        | "sampled_peak_rss_bytes"
                        | "cpu_time_per_request_ms"
                        | "cpu_time_delta_ms"
                )
            });
        }
        sequence += 1;
        write(&destination.join(format!("row-{sequence:04}.json")), &row)?;
        rows.push(row);
        Ok(())
    };
    let rounds = if validate { 1 } else { 5 };
    for round in 1..=rounds {
        write(
            &destination.join(format!("round-{round}-before.json")),
            &json!({"background_load":measure::load()}),
        )?;
        let order = if round % 2 == 1 {
            [&node, &native]
        } else {
            [&native, &node]
        };
        for argument in ["--help", "--version", "statusline"] {
            let mut expected = None;
            for executable in order {
                let row = startup(executable, argument, round, validate).await?;
                if expected
                    .as_ref()
                    .is_some_and(|v| v != &row["stdout_sha256"])
                {
                    return Err(format!("Node/Rust startup output differs for {argument}"));
                }
                expected = Some(row["stdout_sha256"].clone());
                save(row)?;
            }
        }
        for workload in workloads() {
            save(http(None, &workload, round, validate).await?)?;
            for executable in order {
                save(http(Some(executable), &workload, round, validate).await?)?;
            }
        }
        write(
            &destination.join(format!("round-{round}-after.json")),
            &json!({"background_load":measure::load()}),
        )?;
    }
    drop(save);
    let report = json!({"schema_version":1,"kind":if validate{"synthetic_runtime_neutral_validation"}else{"synthetic_runtime_neutral_benchmark"},"completed":true,"deterministic_parity":true,"representative_hardware_acceptance":false,"overall_plan_performance_gate":"incomplete","rounds":rounds,"protocol_sha256":digest(PROTOCOL.as_bytes()),"comparisons":if validate{Vec::new()}else{comparisons(&rows)},"rows":rows,"environment_after":measure::environment(root),"unmeasured":["cold_disk_cache","npm_dispatcher_timing","gateway_processing_only_latency","true_peak_rss","allocations","open_file_descriptors","near_limit_body","identical_shared_evaluation","continuity","streaming","cancellation","upstream_failures","token_count_workloads","storage_slow_failed","status_polling","history_at_read_limits","resource_soak","real_evaluator_quality","other_hardware_and_platforms"],"limits":"Uncontrolled developer workstation; complete HTTP latency includes driver/network/mocks. Sampled peak RSS cannot certify the peak-memory target. No live-provider or model-quality evidence."});
    write(&destination.join("report.json"), &report)?;
    println!(
        "{}",
        json!({"report":destination.join("report.json").to_string_lossy(),"completed":true,"mode":if validate{"deterministic_validation"}else{"exploratory_measurement"},"rows":report["rows"].as_array().unwrap().len(),"overall_plan_performance_gate":"incomplete"})
    );
    Ok(true)
}
pub fn run(args: &[String], root: &Path) -> Result<bool, String> {
    let mut destination = None;
    let mut binary = root.join("rust/target/release/claude-autorouter");
    let mut validate = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--help" => {
                println!(
                    "cargo xtask benchmark --output NEW_DIRECTORY [--rust EXECUTABLE] [--validate]\nSynthetic loopback services only; --validate checks counts/bytes without retaining timing results. Full measurements use the frozen protocol and remain local opt-in evidence."
                );
                return Ok(true);
            }
            "--output" => {
                index += 1;
                destination = args.get(index).map(|v| root.join(v));
            }
            "--rust" => {
                index += 1;
                binary = root.join(args.get(index).ok_or("--rust requires a path")?);
            }
            "--validate" => validate = true,
            _ => return Err("Unknown benchmark option; use --help".into()),
        }
        index += 1;
    }
    let destination = destination.ok_or("--output requires a new directory")?;
    if !validate && cfg!(debug_assertions) {
        return Err("Numerical benchmarks require an optimized driver: cargo run --release --package xtask -- benchmark ...".into());
    }
    if destination.exists() {
        return Err("Benchmark evidence destination exists; choose a new directory".into());
    }
    std::fs::create_dir_all(&destination)
        .map_err(|_| "Cannot create benchmark evidence directory")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| "Cannot create benchmark driver runtime")?;
    match runtime.block_on(execute(root, &destination, &binary, validate)) {
        Ok(result) => Ok(result),
        Err(error) => {
            let _ = write(
                &destination.join("failure.json"),
                &json!({"completed":false,"error":error,"acceptance":false}),
            );
            Err(error)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn body_identity_changes_misses_and_retains_cache_input() {
        let workload = Workload {
            id: "fixture",
            tools: true,
            cached: false,
            delay_ms: 0,
            concurrency: 1,
        };
        let fixture = Fixture::new(&workload, 1);
        assert_ne!(fixture.body(1), fixture.body(2));
        assert_eq!(fixture.body(1).len(), fixture.body(2).len());
        let parsed: Value = serde_json::from_slice(&fixture.body(5)).unwrap();
        assert_eq!(parsed["tools"].as_array().unwrap().len(), 300);
        let cached = Fixture::new(
            &Workload {
                cached: true,
                ..workload
            },
            1,
        );
        assert_eq!(cached.body(1), cached.body(2));
    }
    #[test]
    fn regression_comparison_does_not_pass_missing_rounds() {
        let rows = vec![
            json!({"implementation":"node","workload":"startup_help","round":1,"startup_ms":{"p50":10}}),
            json!({"implementation":"rust","workload":"startup_help","round":1,"startup_ms":{"p50":1}}),
        ];
        assert_eq!(comparisons(&rows)[0]["status"], "unmeasured");
    }
    #[test]
    fn repeated_real_regression_is_not_hidden_by_noise_floor() {
        let mut rows = Vec::new();
        for round in 1..=5 {
            for (name, latency, throughput) in [("node", 1.0, 1000.0), ("rust", 3.0, 700.0)] {
                rows.push(json!({"implementation":name,"workload":"small_miss","concurrency":1,"round":round,"route_ms":{"p95":latency,"p99":latency},"requests_per_second":throughput}));
            }
        }
        let results = comparisons(&rows);
        assert_eq!(
            results
                .iter()
                .find(|v| v["metric"] == "p99_regression_margin_ms")
                .unwrap()["status"],
            "regression_detected"
        );
        assert_eq!(
            results
                .iter()
                .find(|v| v["metric"] == "throughput_ratio")
                .unwrap()["status"],
            "observed_target_not_met"
        );
    }
}
