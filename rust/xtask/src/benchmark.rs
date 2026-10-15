//! Explicit synthetic measurements of complete executables, never a CI timer gate.
#[path = "benchmark_descriptors.rs"]
mod descriptors;
#[path = "benchmark_legacy.rs"]
mod legacy;
#[path = "benchmark_measure.rs"]
mod measure;
#[path = "benchmark_processing.rs"]
mod processing;
#[path = "benchmark_service.rs"]
mod service;
#[path = "benchmark_stream.rs"]
mod stream;
#[path = "benchmark_whole.rs"]
mod whole;
use crate::process::read_bounded;
use crate::process::resource::ResourceChild;
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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "synthetic-benchmark-local-credential";
const MODEL: &str = "claude-opus-5-5";
const SELECTED: &str = "claude-sonnet-5";
const PROTOCOL: &str = include_str!("../../parity/local-benchmark-v1.json");
const PROTOCOL_V2: &str = include_str!("../../parity/local-benchmark-v2.json");
const PROTOCOL_V3: &str = include_str!("../../parity/local-benchmark-v3.json");
const PROTOCOL_V4: &str = include_str!("../../parity/local-benchmark-v4.json");
const PROTOCOL_V5: &str = include_str!("../../parity/local-benchmark-v5.json");
const GATES: &str = include_str!("../../parity/performance-gates.json");
const STATUS_INPUT: &[u8] =
    br#"{"session_id":"synthetic-benchmark-session","context_window":{"used_percentage":24.4}}"#;
const STATUS_WIDTHS: [u16; 4] = [40, 80, 100, 120];
const STATUS_WORKLOAD: &str = "startup_status_snapshot";
fn status_snapshot(pid: u32, heartbeat_at: u64) -> Value {
    json!({"version":1,"pid":pid,"heartbeat_at":heartbeat_at,
        "sessions":{"synthetic-benchmark-session":{"phase":"streaming","actual_model":"claude-sonnet-5",
            "source":"jev","latency_ms":243,"reason":"tool_turn_pinned"}},
        "savings":{"synthetic-benchmark-session":{"baseline_model":"claude-opus-5-5",
            "requests":4,"unpriced_requests":0,"actual_usd":0.26,"baseline_usd":0.68,
            "saved_usd":0.42,"percent":61.76470588235294}}})
}
fn expected_status_output(columns: u16) -> Option<&'static str> {
    match columns {
        40 => Some("● AutoRouter · Sonnet 5 · turn pinned\n"),
        80 => Some("● AutoRouter · Sonnet 5 · streaming · turn pinned · est saved 62% vs Opus\n"),
        100 => Some(
            "● AutoRouter · Sonnet 5 · streaming · Jev 243ms · turn pinned · est saved $0.42 (62%) vs Opus\n",
        ),
        120 => Some(
            "● AutoRouter · Sonnet 5 · streaming · Jev 243ms · turn pinned · est saved $0.42 (62%) vs Opus · CLI ctx 24%\n",
        ),
        _ => None,
    }
}
fn retain_status_only_gaps(unmeasured: &mut Vec<&'static str>, status_only: bool) {
    if status_only {
        for metric in ["open_file_descriptors", "true_peak_rss", "streaming"] {
            if !unmeasured.contains(&metric) {
                unmeasured.push(metric);
            }
        }
    }
}
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
    stream: Option<stream::Kind>,
    payload: Payload,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Payload {
    Standard,
    NearLimit,
    TinyStrings,
}
const NEAR_LIMIT_BYTES: usize = 32 * 1024 * 1024 - 4096;
const TINY_STRINGS: usize = 65536;
fn capacity_preflight(memory: Option<u64>, selected: Option<&str>) -> Value {
    let required = selected.is_none_or(|id| id == "near_limit_body_miss");
    let minimum = 32 * 1024 * 1024 * 1024u64;
    json!({"required":required,"physical_memory_bytes":memory,"minimum_physical_memory_bytes":minimum,
        "adequate":!required || memory.is_some_and(|bytes|bytes>=minimum),
        "maximum_concurrency":128,"raw_driver_request_bytes_lower_bound":NEAR_LIMIT_BYTES as u64 *128,
        "basis":"32GiB safety heuristic for concurrent near-limit inputs, not a proven copy factor, peak-memory bound or available-headroom guarantee; do not reduce load or claim acceptance on refusal"})
}
fn workloads(v2: bool, v4: bool) -> Vec<Workload> {
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
                stream: None,
                payload: Payload::Standard,
            });
        }
    }
    if v2 {
        for (id, kind) in [
            ("stream_burst", stream::Kind::Burst),
            ("stream_paced", stream::Kind::Paced),
        ] {
            for concurrency in [1, 8, 32, 128] {
                result.push(Workload {
                    id,
                    tools: false,
                    cached: false,
                    delay_ms: 0,
                    concurrency,
                    stream: Some(kind),
                    payload: Payload::Standard,
                });
            }
        }
    }
    if v4 {
        for (id, payload) in [
            ("near_limit_body_miss", Payload::NearLimit),
            ("tiny_strings_miss", Payload::TinyStrings),
        ] {
            for concurrency in [1, 8, 32, 128] {
                result.push(Workload {
                    id,
                    tools: false,
                    cached: false,
                    delay_ms: 0,
                    concurrency,
                    stream: None,
                    payload,
                });
            }
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
        if workload.stream.is_some() {
            value["stream"] = json!(true);
        }
        match workload.payload {
            Payload::Standard => {}
            Payload::NearLimit => {
                value["synthetic_opaque_padding"] = json!("");
                let overhead = serde_json::to_vec(&value).unwrap().len();
                value["synthetic_opaque_padding"] = json!("x".repeat(NEAR_LIMIT_BYTES - overhead));
            }
            Payload::TinyStrings => {
                value["synthetic_opaque_strings"] = json!(vec!["s"; TINY_STRINGS]);
            }
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
enum GatewayChild {
    Direct(Child),
    Measured(ResourceChild),
}
impl GatewayChild {
    fn id(&self) -> u32 {
        match self {
            Self::Direct(child) => child.id(),
            Self::Measured(child) => child.id(),
        }
    }
    fn exited(&mut self) -> Result<bool, String> {
        match self {
            Self::Direct(child) => child
                .try_wait()
                .map(|value| value.is_some())
                .map_err(|_| "Cannot inspect benchmark gateway".into()),
            Self::Measured(child) => child.exited(),
        }
    }
}
impl Drop for GatewayChild {
    fn drop(&mut self) {
        if let Self::Direct(child) = self {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
struct Gateway {
    child: GatewayChild,
    address: std::net::SocketAddr,
    ready_ms: f64,
    _scratch: Scratch,
    stderr: PathBuf,
    resources: Option<Value>,
}
impl Gateway {
    async fn start(
        executable: &Executable,
        mock: &service::Mock,
        client: &NativeHttpClient,
        resources: bool,
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
        let mut command = executable.command(&scratch.0, "serve");
        command
            .env("AUTOROUTER_PORT", address.port().to_string())
            .env(
                "AUTOROUTER_UPSTREAM_URL",
                format!("http://{}", mock.address),
            )
            .env(
                "AUTOROUTER_JEV_URL",
                format!("http://{}/v1/systemone", mock.address),
            );
        let child = if resources {
            GatewayChild::Measured(ResourceChild::spawn(&command, output).await?)
        } else {
            GatewayChild::Direct(
                command
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::from(output))
                    .spawn()
                    .map_err(|_| "Cannot launch benchmark gateway")?,
            )
        };
        let mut gateway = Self {
            child,
            address,
            ready_ms: 0.0,
            _scratch: scratch,
            stderr,
            resources: None,
        };
        loop {
            if gateway.child.exited()? {
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
        if let GatewayChild::Measured(child) = &mut self.child {
            self.resources = Some(child.stop().await?);
            if std::net::TcpStream::connect_timeout(&self.address, Duration::from_millis(50))
                .is_ok()
            {
                return Err("Benchmark gateway left its listener open".into());
            }
            return Ok(started.elapsed().as_secs_f64() * 1000.0);
        }
        let GatewayChild::Direct(child) = &mut self.child else {
            unreachable!()
        };
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(child.id() as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
        loop {
            if let Some(status) = child
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
    expected: Bytes,
    v2: bool,
) -> Result<RequestObservation, String> {
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
        let mut observation = stream::Observation::default();
        if v2 {
            let mut body = Limited::new(response.into_body(), 8 * 1024 * 1024);
            while let Some(frame) = body.frame().await {
                let frame =
                    frame.map_err(|_| "Benchmark response exceeded limit or was truncated")?;
                if let Some(bytes) = frame.data_ref() {
                    observation.data(bytes, &expected, started, Instant::now())?;
                } else if frame.trailers_ref().is_some() {
                    return Err("Unexpected benchmark response trailers".into());
                }
            }
            observation.finish(&expected)?;
        } else {
            let bytes = Limited::new(response.into_body(), 1024 * 1024)
                .collect()
                .await
                .map_err(|_| "Benchmark response exceeded limit or was truncated")?
                .to_bytes();
            if bytes != expected {
                return Err("Benchmark response bytes mismatch".into());
            }
        }
        Ok(RequestObservation {
            total_ms: started.elapsed().as_secs_f64() * 1000.0,
            headers_ms: first,
            body: observation,
        })
    })
    .await
    .map_err(|_| "Benchmark HTTP request timed out")?
}
struct RequestObservation {
    total_ms: f64,
    headers_ms: f64,
    body: stream::Observation,
}
struct Batch {
    observations: Vec<RequestObservation>,
    seconds: f64,
    peak_in_flight: usize,
}
async fn batch(
    client: &NativeHttpClient,
    address: std::net::SocketAddr,
    fixture: Arc<Fixture>,
    concurrency: usize,
    samples: std::ops::Range<usize>,
    expected: Bytes,
    v2: bool,
) -> Result<Batch, String> {
    let start = samples.start;
    let count = samples.len();
    let index = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut tasks = tokio::task::JoinSet::new();
    let started = Instant::now();
    for _ in 0..concurrency {
        let client = client.clone();
        let index = index.clone();
        let fixture = fixture.clone();
        let expected = expected.clone();
        let active = active.clone();
        let peak = peak.clone();
        tasks.spawn(async move {
            let mut results = Vec::new();
            loop {
                let n = index.fetch_add(1, Ordering::SeqCst);
                if n >= count {
                    break;
                }
                let in_flight = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(in_flight, Ordering::SeqCst);
                let response = request(
                    &client,
                    address,
                    fixture.body(start + n),
                    start + n,
                    concurrency,
                    expected.clone(),
                    v2,
                )
                .await;
                active.fetch_sub(1, Ordering::SeqCst);
                results.push(response?);
            }
            Ok::<_, String>(results)
        });
    }
    let mut observations = Vec::with_capacity(count);
    while let Some(result) = tasks.join_next().await {
        observations.extend(result.map_err(|_| "Benchmark client task failed")??);
    }
    Ok(Batch {
        observations,
        seconds: started.elapsed().as_secs_f64(),
        peak_in_flight: peak.load(Ordering::SeqCst),
    })
}
async fn http(
    executable: Option<&Executable>,
    workload: &Workload,
    round: usize,
    validate: bool,
    v2: bool,
    resources: bool,
    descriptor_collector: Option<&descriptors::Collector>,
) -> Result<Value, String> {
    let v4 = descriptor_collector.is_some();
    let fixture = Arc::new(Fixture::new(workload, round));
    let stream_fixture = workload.stream.map(stream::Fixture::new);
    let expected = stream_fixture
        .as_ref()
        .map(|v| v.expected.clone())
        .unwrap_or_else(|| Bytes::from_static(service::RESPONSE));
    let large = workload.tools || workload.payload != Payload::Standard;
    let expected_model = if executable.is_some() && !large {
        SELECTED
    } else {
        MODEL
    };
    let mock = service::Mock::start(
        workload.delay_ms,
        expected_model,
        stream_fixture,
        v2.then(|| {
            service::ExpectedRequests::new(
                serde_json::from_slice(&fixture.bytes).unwrap(),
                expected_model,
            )
        }),
    )
    .await?;
    let client = NativeHttpClient::new().map_err(|_| "Cannot initialize benchmark HTTP client")?;
    let mut gateway = match executable {
        Some(executable) => Some(Gateway::start(executable, &mock, &client, resources).await?),
        None => None,
    };
    let address = gateway.as_ref().map(|v| v.address).unwrap_or(mock.address);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let idle = match &gateway {
        Some(gateway) => measure::process(gateway.child.id()).await,
        None => Value::Null,
    };
    let idle_descriptors = if v4 {
        match &gateway {
            Some(gateway) => {
                descriptor_collector
                    .unwrap()
                    .snapshot(gateway.child.id())
                    .await
            }
            None => Value::Null,
        }
    } else {
        Value::Null
    };
    let warmup = if validate {
        if v2 { 8.max(workload.concurrency) } else { 8 }
    } else {
        200
    };
    let measured = if validate {
        if v2 { 32.max(workload.concurrency) } else { 32 }
    } else {
        2000
    };
    batch(
        &client,
        address,
        fixture.clone(),
        workload.concurrency,
        0..warmup,
        expected.clone(),
        v2,
    )
    .await
    .map_err(|error| {
        format!(
            "{} {} c{} warmup: {error}; observed calls {:?}, streams {:?}",
            executable
                .map(|value| value.name)
                .unwrap_or("direct_upstream"),
            workload.id,
            workload.concurrency,
            mock.counts.snapshot(),
            mock.streams.snapshot()
        )
    })?;
    let warm_descriptors = if v4 {
        match &gateway {
            Some(gateway) => {
                descriptor_collector
                    .unwrap()
                    .snapshot(gateway.child.id())
                    .await
            }
            None => Value::Null,
        }
    } else {
        Value::Null
    };
    let before = mock.counts.snapshot();
    let streams_before = mock.streams.snapshot();
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
        warmup..warmup + measured,
        expected.clone(),
        v2,
    )
    .await;
    sampling.cancel();
    let peak = sampler
        .await
        .map_err(|_| "Benchmark resource sampler failed")?;
    let batch = measured_result.map_err(|error| {
        format!(
            "{} {} c{} measured: {error}; observed calls {:?}, streams {:?}",
            executable
                .map(|value| value.name)
                .unwrap_or("direct_upstream"),
            workload.id,
            workload.concurrency,
            mock.counts.snapshot(),
            mock.streams.snapshot()
        )
    })?;
    let seconds = batch.seconds;
    let times: Vec<_> = batch.observations.iter().map(|v| v.total_ms).collect();
    let firsts: Vec<_> = batch.observations.iter().map(|v| v.headers_ms).collect();
    let cpu_after = match &gateway {
        Some(gateway) => measure::process(gateway.child.id()).await,
        None => Value::Null,
    };
    let after = mock.counts.snapshot();
    let streams_after = mock.streams.snapshot();
    let stream_delta: Vec<_> = streams_after
        .iter()
        .zip(streams_before)
        .map(|(a, b)| a - b)
        .collect();
    let expected_streams = if workload.stream.is_some() {
        measured as u64
    } else {
        0
    };
    if stream_delta != [expected_streams, expected_streams, 0] {
        return Err("Benchmark stream producer did not yield all expected bodies cleanly".into());
    }
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
    let (recovery, recovery_descriptors) = if v4 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        match &gateway {
            Some(gateway) => (
                measure::process(gateway.child.id()).await,
                descriptor_collector
                    .unwrap()
                    .snapshot(gateway.child.id())
                    .await,
            ),
            None => (Value::Null, Value::Null),
        }
    } else {
        (Value::Null, Value::Null)
    };
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
    let mut row = json!({"implementation":executable.map(|v|v.name).unwrap_or("direct_upstream"),"workload":workload.id,"concurrency":workload.concurrency,"round":round,"fixture_bytes":fixture.bytes.len(),"fixture_sha256":digest(&fixture.bytes),"response_sha256":digest(&expected),"deterministic_counts_passed":true,"requests":measured,"warmup_requests":warmup,"evaluator_calls":delta[0],"upstream_calls":delta[1],"count_token_calls":delta[2],"duration_seconds":seconds,"requests_per_second":measured as f64/seconds,"route_ms":measure::stats(&times),"first_response_headers_ms":measure::stats(&firsts),"raw_route_ms":times,"raw_first_response_headers_ms":firsts,"ready_ms":ready,"cleanup_ms":cleanup,"idle_rss_bytes":idle["rss_bytes"],"sampled_peak_rss_bytes":peak,"cpu_time_per_request_ms":cpu_ms,"cpu_time_delta_ms":cpu_delta_ms,"cpu_time_resolution_ms":resolution_ms,"diagnostic_bytes":stderr_bytes,"allocations":null,"open_file_descriptors":null,"true_peak_rss_bytes":null});
    if resources {
        let lifetime = gateway
            .as_ref()
            .and_then(|gateway| gateway.resources.as_ref());
        row["resource_collection_verified"] = json!(lifetime.is_some());
        row["lifetime_resources"] = json!(lifetime);
        row["true_peak_rss_bytes"] = lifetime
            .map(|value| value["peak_rss_bytes"].clone())
            .unwrap_or(Value::Null);
    }
    if v4 {
        row["open_file_descriptors"] = json!({
            "idle": idle_descriptors,
            "after_warmup": warm_descriptors,
            "postload": recovery_descriptors,
            "postload_minus_warmup": descriptors::delta(&warm_descriptors, &recovery_descriptors),
            "acceptance_qualified": false
        });
        row["postload_rss_bytes"] = recovery["rss_bytes"].clone();
        row["postload_quiescence_ms"] = json!(200);
        row["resource_recovery_scope"] = json!(
            "single fixed-load batch with HTTP pools retained; descriptive snapshots, no long-term retention qualification"
        );
    }
    if v2 {
        row["driver_peak_in_flight_requests"] = json!(batch.peak_in_flight);
        row["upstream_and_count_request_fields_verified"] = json!(true);
        let bodies: Vec<_> = batch
            .observations
            .iter()
            .filter_map(|v| v.body.first_body_ms)
            .collect();
        if bodies.len() != measured {
            return Err("Benchmark response lacked a first body observation".into());
        }
        let gaps: Vec<_> = batch
            .observations
            .iter()
            .map(|v| v.body.maximum_data_gap_ms)
            .collect();
        let observed_gaps: Vec<_> = gaps.iter().copied().flatten().collect();
        let gap_stats = (!observed_gaps.is_empty()).then(|| measure::stats(&observed_gaps));
        let fields = json!({"response_bytes":expected.len(),"response_bytes_verified":true,"first_body_ms":measure::stats(&bodies),"raw_first_body_ms":bodies,"maximum_data_gap_ms":gap_stats,"raw_maximum_data_gap_ms":gaps,"raw_data_frame_counts":batch.observations.iter().map(|v| v.body.data_frames).collect::<Vec<_>>(),"response_bytes_per_second":expected.len() as f64 * measured as f64 / seconds,"mock_streams_started":stream_delta[0],"mock_streams_fully_yielded":stream_delta[1],"mock_streams_abandoned":stream_delta[2]});
        row.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
    }
    Ok(row)
}
async fn startup(
    executable: &Executable,
    argument: &str,
    round: usize,
    validate: bool,
    status_columns: Option<u16>,
) -> Result<Value, String> {
    let scratch = Scratch::new("benchmark-startup")?;
    let stdout = scratch.file("stdout", b"")?;
    let stderr = scratch.file("stderr", b"")?;
    let status_stdin = status_columns
        .map(|_| scratch.file("status-stdin.json", STATUS_INPUT))
        .transpose()?;
    let status_file = status_columns
        .map(|_| scratch.file("status-snapshot.json", b""))
        .transpose()?;
    let warmup = if validate { 1 } else { 10 };
    let count = if validate { 2 } else { 100 };
    let mut times = Vec::with_capacity(count);
    let mut identity = None;
    for index in 0..warmup + count {
        if let Some(path) = &status_file {
            let heartbeat_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "Cannot obtain benchmark snapshot time")?
                .as_millis()
                .min(u128::from(u64::MAX)) as u64;
            let bytes = serde_json::to_vec(&status_snapshot(std::process::id(), heartbeat_at))
                .map_err(|_| "Cannot serialize synthetic status snapshot")?;
            crate::tool_process::write_private(path, &bytes)?;
        }
        let mut command = tokio::process::Command::from(executable.command(&scratch.0, argument));
        command
            .stdin(match &status_stdin {
                Some(path) => {
                    Stdio::from(File::open(path).map_err(|_| "Cannot open synthetic status input")?)
                }
                None => Stdio::null(),
            })
            .stdout(Stdio::from(
                File::create(&stdout).map_err(|_| "Cannot stage benchmark stdout")?,
            ))
            .stderr(Stdio::from(
                File::create(&stderr).map_err(|_| "Cannot stage benchmark stderr")?,
            ))
            .kill_on_drop(true);
        if let (Some(columns), Some(path)) = (status_columns, &status_file) {
            command
                .env("COLUMNS", columns.to_string())
                .env("AUTOROUTER_STATUS_FILE", path);
        }
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
        let output = read_bounded(&stdout, 1024 * 1024)?;
        if status_columns.is_some_and(|columns| {
            expected_status_output(columns).is_none_or(|expected| output != expected.as_bytes())
        }) {
            return Err("Synthetic status output differs from the declared frozen fixture".into());
        }
        let sha = digest(&output);
        if identity.as_ref().is_some_and(|old| old != &sha) {
            return Err("Startup output changed between identical invocations".into());
        }
        identity = Some(sha);
        if index >= warmup {
            times.push(elapsed)
        }
    }
    let workload = status_columns.map_or_else(
        || match argument {
            "--help" => "startup_help".to_owned(),
            "--version" => "startup_version".to_owned(),
            _ => "startup_status".to_owned(),
        },
        |columns| format!("{STATUS_WORKLOAD}_{columns}"),
    );
    let mut row = json!({"implementation":executable.name,"workload":workload,"round":round,"startup_ms":measure::stats(&times),"raw_startup_ms":times,"stdout_sha256":identity,"cold_disk_cache":false});
    if let Some(columns) = status_columns {
        row["columns"] = json!(columns);
        row["stdin_sha256"] = json!(digest(STATUS_INPUT));
        row["snapshot_template_sha256"] = json!(digest(
            &serde_json::to_vec(&status_snapshot(1, 100000)).unwrap()
        ));
        row["snapshot"] = json!("private_synthetic_online");
    }
    Ok(row)
}
fn scalar(row: &Value, path: &[&str]) -> Option<f64> {
    let mut value = row;
    for key in path {
        value = &value[*key];
    }
    value.as_f64()
}
fn comparisons(rows: &[Value], v2: bool) -> Vec<Value> {
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
        let mut paths: Vec<(&str, Vec<&str>, f64, bool)> = if workload.starts_with("startup_") {
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
        if workload == "large_catalog_miss"
            && pairs.iter().any(|(a, b)| {
                a["resource_collection_verified"] == true
                    && b["resource_collection_verified"] == true
            })
        {
            paths.push((
                "lifetime_peak_rss_ratio",
                vec!["true_peak_rss_bytes"],
                0.7,
                false,
            ));
        }
        for (metric, path, threshold, minimum) in paths {
            let ratios = pairs
                .iter()
                .filter_map(|(a, b)| {
                    if metric == "lifetime_peak_rss_ratio"
                        && (a["resource_collection_verified"] != true
                            || b["resource_collection_verified"] != true)
                    {
                        return None;
                    }
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
            if v2 && workload.starts_with("stream_") {
                for (metric, path) in [
                    (
                        "response_byte_throughput_ratio",
                        &["response_bytes_per_second"][..],
                    ),
                    (
                        "maximum_data_gap_p95_ratio",
                        &["maximum_data_gap_ms", "p95"][..],
                    ),
                ] {
                    let ratios: Vec<_> = pairs
                        .iter()
                        .filter_map(|(a, b)| {
                            scalar(a, path)
                                .zip(scalar(b, path))
                                .filter(|(a, _)| *a > 0.0)
                                .map(|(a, b)| b / a)
                        })
                        .collect();
                    results.push(json!({"workload":workload,"concurrency":concurrency,"metric":metric,"paired_rounds":ratios.len(),"interval":measure::interval(&ratios),"status":"descriptive_no_acceptance_threshold","acceptance_qualified":false}));
                }
            }
            let latency_metrics: &[(&str, &str)] = if v2 {
                &[("route_ms", ""), ("first_body_ms", "first_body_")]
            } else {
                &[("route_ms", "")]
            };
            for &(path, prefix) in latency_metrics {
                for percentile in ["p95", "p99"] {
                    let margins = pairs
                        .iter()
                        .filter_map(|(a, b)| {
                            scalar(a, &[path, percentile])
                                .zip(scalar(b, &[path, percentile]))
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
                    results.push(json!({"workload":workload,"concurrency":concurrency,"metric":format!("{prefix}{percentile}_regression_margin_ms"),"interval":interval,"absolute_noise_floor_ms":0.5,"relative_allowance":0.1,"status":status,"acceptance_qualified":false}));
                }
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
struct BenchmarkOptions<'a> {
    validate: bool,
    v2: bool,
    resources: bool,
    v4: bool,
    v5: bool,
    libproc: bool,
    selected_workload: Option<&'a str>,
}
async fn execute(
    root: &Path,
    destination: &Path,
    binary: &Path,
    options: BenchmarkOptions<'_>,
) -> Result<bool, String> {
    let BenchmarkOptions {
        validate,
        v2,
        resources,
        v4,
        v5,
        libproc,
        selected_workload,
    } = options;
    let protocol = if v5 {
        PROTOCOL_V5
    } else if v4 {
        PROTOCOL_V4
    } else if resources {
        PROTOCOL_V3
    } else if v2 {
        PROTOCOL_V2
    } else {
        PROTOCOL
    };
    let schema_version = if v5 {
        5
    } else if v4 {
        4
    } else if resources {
        3
    } else if v2 {
        2
    } else {
        1
    };
    let selected_status_only = v5 && selected_workload == Some(STATUS_WORKLOAD);
    let descriptor_limit = if v4 && !selected_status_only {
        let (soft, hard) =
            nix::sys::resource::getrlimit(nix::sys::resource::Resource::RLIMIT_NOFILE)
                .map_err(|_| "Cannot inspect benchmark descriptor limit")?;
        let limit = json!({"soft":soft,"hard":hard,"required_soft_minimum":1024,"adequate":soft>=1024,"scope":"driver and both inherited engine environments; control sockets require more than256 descriptors at128 clients"});
        write(&destination.join("file-descriptor-preflight.json"), &limit)?;
        if soft < 1024 {
            return Err("Protocol v4 requires a soft file-descriptor limit of at least1024; raise it for this command before running, without reducing declared concurrency".into());
        }
        Some(limit)
    } else {
        None
    };
    let capacity = if v4 {
        let capacity = capacity_preflight(measure::memory_bytes(), selected_workload);
        write(&destination.join("capacity-preflight.json"), &capacity)?;
        if capacity["adequate"] != true {
            return Err("Protocol v4 near-limit128-client work requires at least32GiB known physical RAM by its declared safety heuristic; transfer the full workload to a larger host. This refusal is unmeasured, never a passing or reduced-load run".into());
        }
        Some(capacity)
    } else {
        None
    };
    if libproc {
        write(
            &destination.join("descriptor-collector-declaration.json"),
            &json!({
                "selector":"libproc-v1","design":serde_json::from_str::<Value>(descriptors::LIBPROC_DESIGN).unwrap(),
                "design_sha256":digest(descriptors::LIBPROC_DESIGN.as_bytes()),
                "implementation_companion":serde_json::from_str::<Value>(descriptors::LIBPROC_IMPLEMENTATION).unwrap(),
                "implementation_companion_sha256":digest(descriptors::LIBPROC_IMPLEMENTATION.as_bytes()),
                "numerical_or_hardware_acceptance":false
            }),
        )?;
    }
    let descriptor_collector = if v4 && !selected_status_only {
        Some(descriptors::Collector::prepare(libproc).await?)
    } else {
        None
    };
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
    let mut preflight = json!({"schema_version":schema_version,"kind":"synthetic_native_benchmark_preflight","mode":if validate{"deterministic_validation"}else{"exploratory_measurement"},"baseline_commit":"ea930c247626ce2af5ccdad721b5121417bf4ad8","protocol":serde_json::from_str::<Value>(protocol).unwrap(),"protocol_sha256":digest(protocol.as_bytes()),"gates":serde_json::from_str::<Value>(GATES).unwrap(),"gates_sha256":digest(GATES.as_bytes()),"candidate_sha256":digest(&read_bounded(&native.program,32*1024*1024)?),"node_binary_sha256":digest(&read_bounded(&node.program,256*1024*1024)?),"source_manifest":source,"environment":before,"provider_calls":0,"reference_integrity_verified":true});
    if v4 {
        preflight["inherited_protocol"] = serde_json::from_str(PROTOCOL_V3).unwrap();
        preflight["inherited_protocol_sha256"] = json!(digest(PROTOCOL_V3.as_bytes()));
        preflight["selected_workload"] = json!(selected_workload);
        preflight["complete_protocol_workload_selection"] = json!(selected_workload.is_none());
        preflight["file_descriptor_limit"] = json!(descriptor_limit);
        preflight["capacity"] = json!(capacity);
        if libproc {
            preflight["descriptor_collector"] = json!({"selector":"libproc-v1","identity":descriptor_collector.as_ref().unwrap().identity,
                "design_protocol_sha256":digest(descriptors::LIBPROC_DESIGN.as_bytes()),
                "implementation_protocol_sha256":digest(descriptors::LIBPROC_IMPLEMENTATION.as_bytes())});
        }
    }
    if v5 {
        preflight["inherited_protocol_v4"] = serde_json::from_str(PROTOCOL_V4).unwrap();
        preflight["inherited_protocol_v4_sha256"] = json!(digest(PROTOCOL_V4.as_bytes()));
    }
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
                        | "first_body_ms"
                        | "raw_first_body_ms"
                        | "maximum_data_gap_ms"
                        | "raw_maximum_data_gap_ms"
                        | "raw_data_frame_counts"
                        | "response_bytes_per_second"
                        | "duration_seconds"
                        | "requests_per_second"
                        | "ready_ms"
                        | "cleanup_ms"
                        | "idle_rss_bytes"
                        | "sampled_peak_rss_bytes"
                        | "cpu_time_per_request_ms"
                        | "cpu_time_delta_ms"
                        | "lifetime_resources"
                )
            });
            if resources {
                row.as_object_mut().unwrap().remove("true_peak_rss_bytes");
            }
            if v4 {
                descriptors::validation_only(&mut row);
            }
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
        for argument in ["--help", "--version", "statusline"]
            .into_iter()
            .filter(|_| selected_workload.is_none())
        {
            let mut expected = None;
            for executable in order {
                let row = startup(executable, argument, round, validate, None).await?;
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
        if v5 && selected_workload.is_none_or(|selected| selected == STATUS_WORKLOAD) {
            for columns in STATUS_WIDTHS {
                let mut expected = None;
                for executable in order {
                    let row =
                        startup(executable, "statusline", round, validate, Some(columns)).await?;
                    if expected
                        .as_ref()
                        .is_some_and(|value| value != &row["stdout_sha256"])
                    {
                        return Err("Node/Rust synthetic status output differs".into());
                    }
                    expected = Some(row["stdout_sha256"].clone());
                    save(row)?;
                }
            }
        }
        for workload in workloads(v2, v4)
            .into_iter()
            .filter(|workload| selected_workload.is_none_or(|selected| selected == workload.id))
        {
            save(
                http(
                    None,
                    &workload,
                    round,
                    validate,
                    v2,
                    resources,
                    descriptor_collector.as_ref(),
                )
                .await?,
            )?;
            for executable in order {
                save(
                    http(
                        Some(executable),
                        &workload,
                        round,
                        validate,
                        v2,
                        resources,
                        descriptor_collector.as_ref(),
                    )
                    .await?,
                )?;
            }
        }
        write(
            &destination.join(format!("round-{round}-after.json")),
            &json!({"background_load":measure::load()}),
        )?;
    }
    drop(save);
    let mut unmeasured = vec![
        "cold_disk_cache",
        "npm_dispatcher_timing",
        "gateway_processing_only_latency",
        "allocations",
        "open_file_descriptors",
        "near_limit_body",
        "identical_shared_evaluation",
        "continuity",
        "cancellation",
        "upstream_failures",
        "token_count_workloads",
        "storage_slow_failed",
        "status_polling",
        "history_at_read_limits",
        "resource_soak",
        "real_evaluator_quality",
        "other_hardware_and_platforms",
    ];
    if !resources || validate {
        unmeasured.push("true_peak_rss");
    }
    if !v2 {
        unmeasured.push("streaming");
    } else if validate {
        unmeasured.push("streaming_performance");
    }
    if v2 {
        unmeasured.push("stream_backpressure_failure_cancellation_performance");
    }
    if v4 && !validate {
        if rows
            .iter()
            .any(|row| row["workload"] == "near_limit_body_miss")
        {
            unmeasured.retain(|metric| *metric != "near_limit_body");
        }
        let descriptors_missing = rows.iter().any(|row| {
            row["implementation"] != "direct_upstream"
                && row["concurrency"].is_number()
                && ["idle", "after_warmup", "postload"]
                    .iter()
                    .any(|stage| row["open_file_descriptors"][stage]["verified"] != true)
        });
        if descriptors_missing {
            unmeasured.push("verified_open_file_descriptors");
        } else {
            unmeasured.retain(|metric| *metric != "open_file_descriptors");
        }
    }
    retain_status_only_gaps(&mut unmeasured, selected_status_only);
    if selected_workload.is_some() {
        unmeasured.push("excluded_protocol_workloads");
    }
    if v5 && validate {
        unmeasured.push("status_snapshot_startup_numerical");
    }
    let mut report = json!({"schema_version":schema_version,"kind":if validate{"synthetic_runtime_neutral_validation"}else{"synthetic_runtime_neutral_benchmark"},"completed":true,"deterministic_parity":true,"representative_hardware_acceptance":false,"overall_plan_performance_gate":"incomplete","rounds":rounds,"protocol_sha256":digest(protocol.as_bytes()),"comparisons":if validate{Vec::new()}else{comparisons(&rows,v2)},"rows":rows,"environment_after":measure::environment(root),"unmeasured":unmeasured,"limits":"Uncontrolled developer workstation; complete HTTP latency includes driver/network/mocks. Sampled peak RSS cannot certify the peak-memory target. No live-provider or model-quality evidence."});
    if resources {
        report["limits"] = json!(
            "Uncontrolled developer workstation; complete HTTP latency includes driver/network/mocks. Isolated child getrusage reports full-lifetime maximum individual RSS, including waited descendants, not aggregate process-tree memory or only the measured interval. Ready/cleanup timings include resource collector overhead; startup commands remain direct. Lifetime CPU is descriptive and never substituted for measured-interval CPU. No live-provider or model-quality evidence."
        );
    }
    if v2 {
        report["deterministic_parity_scope"] = json!(
            "Complete inference/count fixture fields, target models, expected stage call counts, exact response bytes through EOF, mock fully-yielded/abandoned bodies and clean gateway exit/listener closure. Evaluator inputs have object-shape/count checks only; rubric/state, cancellation and transport acknowledgement parity require their separate suites."
        );
    }
    if selected_status_only {
        report["deterministic_parity_scope"] = json!(
            "Only synthetic statusline process output at 40, 80, 100 and 120 columns, checked against exact declared bytes for Node and Rust. No gateway, evaluator, streaming or resource workload was run."
        );
        report["limits"] = json!(
            "Uncontrolled developer workstation. This selected startup run excludes HTTP, streaming, descriptor and peak-RSS workloads. No live-provider, model-quality or numerical performance acceptance evidence."
        );
    }
    if v4 {
        report["selected_workload"] = json!(selected_workload);
        report["complete_protocol_workload_selection"] = json!(selected_workload.is_none());
        report["excluded_workloads"] = json!(workloads(v2, v4).into_iter()
            .filter(|workload| selected_workload.is_some_and(|selected| selected != workload.id))
            .map(|workload| workload.id).collect::<std::collections::BTreeSet<_>>());
        if v5 && let Some(selected) = selected_workload {
            let excluded = report["excluded_workloads"].as_array_mut().unwrap();
            for workload in [
                "startup_help",
                "startup_version",
                "startup_status",
                STATUS_WORKLOAD,
            ] {
                if selected != workload {
                    excluded.push(json!(workload));
                }
            }
        }
        report["descriptor_recovery_qualified"] = json!(false);
        if libproc {
            report["descriptor_collector"] = json!({"selector":"libproc-v1","design_protocol_sha256":digest(descriptors::LIBPROC_DESIGN.as_bytes()),
                "implementation_protocol_sha256":digest(descriptors::LIBPROC_IMPLEMENTATION.as_bytes())});
        }
    }
    write(&destination.join("report.json"), &report)?;
    println!(
        "{}",
        json!({"report":destination.join("report.json").to_string_lossy(),"completed":true,"mode":if validate{"deterministic_validation"}else{"exploratory_measurement"},"rows":report["rows"].as_array().unwrap().len(),"overall_plan_performance_gate":"incomplete"})
    );
    Ok(true)
}
pub fn run(args: &[String], root: &Path) -> Result<bool, String> {
    if args == ["__whole-connection-child"] {
        return whole::child();
    }
    if args.first().is_some_and(|arg| arg == "--whole-connection") {
        return whole::run(&args[1..], root);
    }
    if args == ["__processing-child"] {
        return processing::child();
    }
    if args.first().is_some_and(|arg| arg == "--processing") {
        return processing::run(&args[1..], root);
    }
    if args.iter().any(|arg| arg == "--compare-legacy") {
        if args.len() != 2 || args[0] != "--compare-legacy" || args[1].starts_with("--") {
            return Err("Usage: cargo xtask benchmark --compare-legacy REPORT.json".into());
        }
        return legacy::run(&root.join(&args[1]));
    }
    let mut destination = None;
    let mut binary = root.join("rust/target/release/claude-autorouter");
    let mut validate = false;
    let mut v2 = false;
    let mut resources = false;
    let mut v4 = false;
    let mut v5 = false;
    let mut libproc = false;
    let mut selected_workload = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--help" => {
                println!(
                    "cargo xtask benchmark --processing --output NEW_DIRECTORY [--validate]\nUse --processing --help for the matched body-processing component and its explicit whole-gateway limits."
                );
                println!(
                    "cargo xtask benchmark --output NEW_DIRECTORY [--rust EXECUTABLE] [--protocol v1|v2|v3|v4|v5] [--workload ID] [--descriptor-collector libproc-v1] [--validate]\ncargo xtask benchmark --compare-legacy REPORT.json\nSynthetic loopback services only; --validate checks counts/bytes without retaining timing results. Default v1 retains the original workload; v2 adds first-body observations and exact streamed responses; v3 adds isolated lifetime resource collection; v4 adds near-limit/tiny-string requests and descriptor/recovery snapshots; v5 adds private synthetic status snapshots at 40/80/100/120 columns. --workload requires v4 or v5 and selects one HTTP workload at all declared concurrency groups; v5 also accepts startup_status_snapshot to select all four widths. Excluded coverage is recorded. Explicit --descriptor-collector libproc-v1 requires v4 or v5, compiles an identity-checked macOS SDK helper before services, and leaves Linux enumeration unchanged; ordinary v4/v5 retains lsof. Full measurements remain local opt-in evidence; never pool protocol/collector versions.\n--compare-legacy only reads a combined historical baseline/candidate report (at most 16 MiB, 4096 rows each), recomputes the original JS heap/event-loop/latency/coalescing gates and prints JSON. Both reports must be unmarked historical shapes or schema_version 1/type synthetic_router_benchmark; finite nonnegative metrics and safe integer counts are required. Missing candidate maxima serialize as null. No measurements or files are written; these gates do not qualify native paired protocols."
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
            "--protocol" => {
                index += 1;
                (v2, resources, v4, v5) = match args.get(index).map(String::as_str) {
                    Some("v1") => (false, false, false, false),
                    Some("v2") => (true, false, false, false),
                    Some("v3") => (true, true, false, false),
                    Some("v4") => (true, true, true, false),
                    Some("v5") => (true, true, true, true),
                    _ => return Err("--protocol requires v1, v2, v3, v4 or v5".into()),
                };
            }
            "--workload" => {
                index += 1;
                selected_workload =
                    Some(args.get(index).ok_or("--workload requires an ID")?.as_str());
            }
            "--descriptor-collector" => {
                index += 1;
                if args.get(index).map(String::as_str) != Some("libproc-v1") {
                    return Err("--descriptor-collector requires libproc-v1".into());
                }
                libproc = true;
            }
            _ => return Err("Unknown benchmark option; use --help".into()),
        }
        index += 1;
    }
    if libproc && !v4 {
        return Err("--descriptor-collector libproc-v1 requires --protocol v4 or v5; earlier frozen protocols remain unchanged".into());
    }
    if libproc && v5 && selected_workload == Some(STATUS_WORKLOAD) {
        return Err("Status-only startup selection has no gateway descriptor collector".into());
    }
    if let Some(selected) = selected_workload {
        if !v4 {
            return Err(
                "--workload requires --protocol v4 or v5; earlier frozen protocols are unfiltered"
                    .into(),
            );
        }
        if !(v5 && selected == STATUS_WORKLOAD)
            && !workloads(v2, v4)
                .iter()
                .any(|workload| workload.id == selected)
        {
            return Err("Unknown benchmark workload; use a declared HTTP workload ID or v5 startup_status_snapshot".into());
        }
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
    match runtime.block_on(execute(
        root,
        &destination,
        &binary,
        BenchmarkOptions {
            validate,
            v2,
            resources,
            v4,
            v5,
            libproc,
            selected_workload,
        },
    )) {
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
    fn descriptor_selection_cannot_mutate_earlier_protocols_or_admit_other_tools() {
        let root = Path::new("/synthetic");
        for arguments in [
            vec!["--protocol", "v3", "--descriptor-collector", "libproc-v1"],
            vec!["--protocol", "v4", "--descriptor-collector", "unreviewed"],
        ] {
            let arguments = arguments.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert!(run(&arguments, root).is_err());
        }
        let design: Value = serde_json::from_str(descriptors::LIBPROC_DESIGN).unwrap();
        let companion: Value = serde_json::from_str(descriptors::LIBPROC_IMPLEMENTATION).unwrap();
        assert_eq!(
            companion["design_protocol_sha256"],
            digest(descriptors::LIBPROC_DESIGN.as_bytes())
        );
        assert_eq!(
            companion["inherited_protocol_sha256"],
            digest(PROTOCOL_V4.as_bytes())
        );
        assert_eq!(
            design["inherited_protocol_sha256"],
            digest(PROTOCOL_V4.as_bytes())
        );
    }
    #[test]
    fn body_identity_changes_misses_and_retains_cache_input() {
        let workload = Workload {
            id: "fixture",
            tools: true,
            cached: false,
            delay_ms: 0,
            concurrency: 1,
            stream: None,
            payload: Payload::Standard,
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
    fn v4_large_inputs_keep_exact_limits_opaque_data_and_request_identity() {
        for payload in [Payload::NearLimit, Payload::TinyStrings] {
            let workload = Workload {
                id: "synthetic-v4",
                tools: false,
                cached: false,
                delay_ms: 0,
                concurrency: 1,
                stream: None,
                payload,
            };
            let fixture = Fixture::new(&workload, 1);
            assert!(fixture.bytes.len() > 150000);
            assert!(fixture.bytes.len() < 32 * 1024 * 1024);
            let body = fixture.body(21);
            let parsed: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(parsed["model"], MODEL);
            assert!(
                parsed["messages"][0]["content"]
                    .as_str()
                    .unwrap()
                    .ends_with("000000000021")
            );
            match payload {
                Payload::NearLimit => {
                    assert_eq!(body.len(), NEAR_LIMIT_BYTES);
                    assert!(
                        parsed["synthetic_opaque_padding"]
                            .as_str()
                            .unwrap()
                            .bytes()
                            .all(|byte| byte == b'x')
                    );
                }
                Payload::TinyStrings => {
                    let strings = parsed["synthetic_opaque_strings"].as_array().unwrap();
                    assert_eq!(strings.len(), TINY_STRINGS);
                    assert!(strings.iter().all(|value| value == "s"));
                }
                Payload::Standard => unreachable!(),
            }
            assert_eq!(body.len(), fixture.bytes.len());
            assert_ne!(body, fixture.body(22));
            assert_eq!(
                &body[..fixture.index_offset],
                &fixture.bytes[..fixture.index_offset]
            );
            assert_eq!(
                &body[fixture.index_offset + 12..],
                &fixture.bytes[fixture.index_offset + 12..]
            );
        }
    }
    #[test]
    fn v4_adds_full_concurrency_groups_without_changing_frozen_workloads() {
        let v1 = workloads(false, false);
        let v2 = workloads(true, false);
        let v4 = workloads(true, true);
        assert_eq!(v1.len(), 13);
        assert_eq!(v2.len(), 21);
        assert_eq!(v4.len(), 29);
        for (old, new) in v2.iter().zip(&v4) {
            assert_eq!(old.id, new.id);
            assert_eq!(old.concurrency, new.concurrency);
            assert_eq!(old.tools, new.tools);
            assert_eq!(old.cached, new.cached);
            assert_eq!(old.delay_ms, new.delay_ms);
            assert!(new.payload == Payload::Standard);
            assert_eq!(Fixture::new(old, 1).bytes, Fixture::new(new, 1).bytes);
        }
        for id in ["near_limit_body_miss", "tiny_strings_miss"] {
            assert_eq!(
                v4.iter()
                    .filter(|workload| workload.id == id)
                    .map(|workload| workload.concurrency)
                    .collect::<Vec<_>>(),
                [1, 8, 32, 128]
            );
        }
        let protocol: Value = serde_json::from_str(PROTOCOL_V4).unwrap();
        assert_eq!(
            protocol["predecessor_protocol_sha256"],
            digest(PROTOCOL_V3.as_bytes())
        );
    }
    #[test]
    fn v5_status_fixture_and_each_width_match_declared_frozen_output() {
        let protocol: Value = serde_json::from_str(PROTOCOL_V5).unwrap();
        assert_eq!(
            protocol["predecessor_protocol_sha256"],
            digest(PROTOCOL_V4.as_bytes())
        );
        let workload = &protocol["new_workload"];
        assert_eq!(workload["id"], STATUS_WORKLOAD);
        assert_eq!(
            workload["stdin"],
            serde_json::from_slice::<Value>(STATUS_INPUT).unwrap()
        );
        assert_eq!(workload["snapshot_template"], status_snapshot(1, 100000));
        assert_eq!(workload["columns"], json!(STATUS_WIDTHS));
        for columns in STATUS_WIDTHS {
            let output = autorouter_core::statusline::render_status_line(
                &serde_json::from_slice(STATUS_INPUT).unwrap(),
                &status_snapshot(1, 100000),
                &json!({"now":100001,"alive":true,"color":false,"columns":columns}),
            ) + "\n";
            assert_eq!(output, expected_status_output(columns).unwrap());
            assert!(output.trim_end().chars().count() <= usize::from(columns));
        }
    }
    #[test]
    fn v5_status_only_selection_cannot_claim_gateway_resources_or_streaming() {
        let mut numerical_unmeasured = Vec::new();
        retain_status_only_gaps(&mut numerical_unmeasured, true);
        assert_eq!(
            numerical_unmeasured,
            ["open_file_descriptors", "true_peak_rss", "streaming"]
        );
        retain_status_only_gaps(&mut numerical_unmeasured, true);
        assert_eq!(numerical_unmeasured.len(), 3);
        let mut frozen_protocol_unmeasured = Vec::new();
        retain_status_only_gaps(&mut frozen_protocol_unmeasured, false);
        assert!(frozen_protocol_unmeasured.is_empty());
    }
    #[test]
    fn workload_selection_rejects_frozen_or_unknown_protocol_inputs_before_writes() {
        let scratch = Scratch::new("benchmark-selection").unwrap();
        for (protocol, workload) in [
            ("v3", "tiny_strings_miss"),
            ("v4", "not-a-declared-workload"),
            ("v4", STATUS_WORKLOAD),
            ("v5", "not-a-declared-workload"),
        ] {
            let args = [
                "--protocol",
                protocol,
                "--workload",
                workload,
                "--output",
                "new-evidence",
                "--validate",
            ]
            .map(str::to_owned);
            assert!(run(&args, &scratch.0).is_err());
            assert!(!scratch.0.join("new-evidence").exists());
        }
    }
    #[test]
    fn capacity_refusal_never_reduces_workloads_or_excludes_small_hosts_silently() {
        for memory in [
            None,
            Some(16 * 1024 * 1024 * 1024),
            Some(31 * 1024 * 1024 * 1024),
        ] {
            assert_eq!(capacity_preflight(memory, None)["adequate"], false);
            assert_eq!(
                capacity_preflight(memory, Some("near_limit_body_miss"))["adequate"],
                false
            );
            assert_eq!(
                capacity_preflight(memory, Some("tiny_strings_miss"))["adequate"],
                true
            );
            assert_eq!(
                capacity_preflight(memory, Some("small_miss"))["adequate"],
                true
            );
            assert_eq!(
                capacity_preflight(memory, Some(STATUS_WORKLOAD))["adequate"],
                true
            );
        }
        let enough = capacity_preflight(Some(32 * 1024 * 1024 * 1024), None);
        assert_eq!(enough["adequate"], true);
        assert_eq!(
            enough["raw_driver_request_bytes_lower_bound"],
            4294443008u64
        );
        assert_eq!(enough["maximum_concurrency"], 128);
    }
    #[test]
    fn regression_comparison_does_not_pass_missing_rounds() {
        let rows = vec![
            json!({"implementation":"node","workload":"startup_help","round":1,"startup_ms":{"p50":10}}),
            json!({"implementation":"rust","workload":"startup_help","round":1,"startup_ms":{"p50":1}}),
        ];
        assert_eq!(comparisons(&rows, false)[0]["status"], "unmeasured");
    }
    #[test]
    fn peak_memory_gate_uses_lifetime_peaks_and_requires_five_pairs() {
        let mut rows = Vec::new();
        for round in 1..=5 {
            for (name, peak, sampled) in [("node", 100, 100), ("rust", 80, 30)] {
                rows.push(json!({"implementation":name,"workload":"large_catalog_miss","concurrency":128,"round":round,"true_peak_rss_bytes":peak,"sampled_peak_rss_bytes":sampled,"resource_collection_verified":true}));
            }
        }
        let find = |rows: &[Value]| {
            comparisons(rows, true)
                .into_iter()
                .find(|row| row["metric"] == "lifetime_peak_rss_ratio")
                .unwrap()
        };
        let complete = find(&rows);
        assert_eq!(complete["interval"]["median"], 0.8);
        assert_eq!(complete["threshold"], 0.7);
        assert_eq!(complete["status"], "observed_target_not_met");
        assert_eq!(complete["acceptance_qualified"], false);
        // Numeric peaks alone cannot stand in for successful collection. One
        // verified pair must not authorize the other four unverified pairs.
        let mut unverified = rows.clone();
        for row in &mut unverified {
            if row["implementation"] == "rust" {
                row["true_peak_rss_bytes"] = json!(30);
                if row["round"] != 1 {
                    row["resource_collection_verified"] = json!(false);
                }
            }
        }
        assert_eq!(find(&unverified)["paired_rounds"], 1);
        assert_eq!(find(&unverified)["status"], "unmeasured");
        unverified[1]
            .as_object_mut()
            .unwrap()
            .remove("resource_collection_verified");
        assert!(
            !comparisons(&unverified, true)
                .iter()
                .any(|row| row["metric"] == "lifetime_peak_rss_ratio")
        );
        rows.pop();
        assert_eq!(find(&rows)["status"], "unmeasured");
    }
    #[test]
    fn first_body_regression_is_separate_from_headers_and_full_response() {
        let mut rows = Vec::new();
        for round in 1..=5 {
            for (name, body) in [("node", 1.0), ("rust", 5.0)] {
                rows.push(json!({"implementation":name,"workload":"stream_paced","concurrency":1,"round":round,"route_ms":{"p95":50.0,"p99":50.0},"first_response_headers_ms":{"p95":0.1,"p99":0.1},"first_body_ms":{"p95":body,"p99":body},"requests_per_second":10.0}));
            }
        }
        let results = comparisons(&rows, true);
        let status = |metric| {
            results.iter().find(|v| v["metric"] == metric).unwrap()["status"]
                .as_str()
                .unwrap()
        };
        assert_eq!(status("p99_regression_margin_ms"), "no_regression_detected");
        assert_eq!(
            status("first_body_p99_regression_margin_ms"),
            "regression_detected"
        );
        for row in &mut rows {
            row.as_object_mut().unwrap().remove("first_body_ms");
        }
        assert_eq!(
            comparisons(&rows, true)
                .iter()
                .find(|v| v["metric"] == "first_body_p99_regression_margin_ms")
                .unwrap()["status"],
            "unmeasured"
        );
    }
    #[test]
    fn repeated_real_regression_is_not_hidden_by_noise_floor() {
        let mut rows = Vec::new();
        for round in 1..=5 {
            for (name, latency, throughput) in [("node", 1.0, 1000.0), ("rust", 3.0, 700.0)] {
                rows.push(json!({"implementation":name,"workload":"small_miss","concurrency":1,"round":round,"route_ms":{"p95":latency,"p99":latency},"requests_per_second":throughput}));
            }
        }
        let results = comparisons(&rows, false);
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
