//! Whole ordinary HTTP/1 gateway with immediate sockets; validation only first.
#[path = "benchmark_whole_io.rs"]
mod io;
#[path = "benchmark_whole_transport.rs"]
mod transport;
use super::{Fixture, Payload, Workload, digest, measure, node_path, service, source_files, write};
use autorouter_core::config::read_config;
use autorouter_core::js_json::JsDocument;
use autorouter_runtime::http_client::{DevelopmentHttpClient, NativeHttpClient};
use autorouter_runtime::router::Router;
use autorouter_runtime::server::{Gateway, GatewayHandle};
use autorouter_runtime::server_events::{EventSink, EventSinks};
use autorouter_runtime::transport_completion::CompletionRegistry;
use bytes::Bytes;
use io::{Downstream, LoopbackProvider, Message, ProviderState};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use transport::{Calls, InferenceClient, ObservedRouter, Transport};

const PROTOCOL: &str = include_str!("../../parity/whole-connection-benchmark-v1.json");
const REVIEW: &str = include_str!("../../parity/whole-connection-benchmark-review-v1.json");
const POLICY: &str = include_str!("../../parity/whole-connection-policy-v1.json");
const CLOCK_FIELDS: &[&str] = &[
    "timestamp",
    "latency_ms",
    "evaluation_latency_ms",
    "routing_latency_ms",
    "decision_latency_ms",
    "first_response_ms",
    "total_latency_ms",
];
#[derive(Default)]
struct Observations {
    log: Vec<JsDocument>,
    status: Vec<JsDocument>,
    decision: Vec<JsDocument>,
    complete: bool,
    failed: bool,
}
#[derive(Default)]
struct ParsedCount {
    values: Vec<Option<u64>>,
    failed: bool,
}
fn sink(observations: Arc<Mutex<Observations>>, kind: &'static str) -> EventSink {
    Arc::new(move |document| {
        let mut observations = observations.lock().unwrap();
        if kind == "status"
            && document
                .get(document.root(), "event")
                .and_then(|node| document.string(node))
                .and_then(|value| value.to_scalar())
                .as_deref()
                == Some("request_complete")
        {
            observations.complete = true;
        }
        let rows = match kind {
            "log" => &mut observations.log,
            "status" => &mut observations.status,
            _ => &mut observations.decision,
        };
        if rows.len() >= 32 {
            observations.failed = true;
        } else {
            rows.push(document);
        }
    })
}
fn canonical(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
        Value::Object(object) => {
            let sorted: BTreeMap<_, _> = object
                .iter()
                .filter(|(key, _)| !CLOCK_FIELDS.contains(&key.as_str()))
                .collect();
            let mut result = Map::new();
            for (key, value) in sorted {
                result.insert(
                    key.clone(),
                    if key == "request_id" {
                        json!("<synthetic-request-id>")
                    } else {
                        canonical(value)
                    },
                );
            }
            Value::Object(result)
        }
        _ => value.clone(),
    }
}
fn object_hash(value: &Value) -> String {
    let value = canonical(value);
    let text = JsDocument::parse(&serde_json::to_vec(&value).unwrap())
        .unwrap()
        .stringify();
    digest(text.as_bytes())
}
fn observation_value(documents: &[JsDocument]) -> Value {
    Value::Array(
        documents
            .iter()
            .map(|document| serde_json::from_str(&document.stringify()).unwrap())
            .collect(),
    )
}
fn expected_branch() -> Value {
    serde_json::from_str::<Value>(POLICY).unwrap()["expected_each_request"].clone()
}
fn policy_proof(
    decision: &Value,
    inference: &[u8],
    count: &[u8],
    parsed_count: Option<u64>,
    response: &[u8],
) -> Result<Value, String> {
    let inference: Value =
        serde_json::from_slice(inference).map_err(|_| "Invalid prepared inference policy proof")?;
    let count: Value =
        serde_json::from_slice(count).map_err(|_| "Invalid prepared count policy proof")?;
    let response: Value =
        serde_json::from_slice(response).map_err(|_| "Invalid provider model policy proof")?;
    let branch = json!({
        "decision_source":decision["source"],"decision_reason":decision["reason"],
        "classified_tier":decision["classified_tier"],"selected_model":decision["selected_model"],
        "inference_model":inference["model"],"count_target_model":count["model"],
        "count_parsed_input_tokens":parsed_count,
        "count_result_used_for_selected_target":decision.get("context_check").is_some() || decision.get("counted_input_tokens").is_some(),
        "provider_observed_model":response["model"]
    });
    if branch != expected_branch() {
        return Err("Whole-connection frozen policy/count result mismatch".into());
    }
    Ok(branch)
}
fn spec(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > io::MAX_REQUEST {
        return Err("Whole-connection control exceeds bound".into());
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "Invalid whole-connection control")?;
    let numeric = value["mode"] == "measure";
    if value["kind"] != "synthetic_whole_connection_input_v1"
        || !matches!(value["mode"].as_str(), None | Some("validate" | "measure"))
        || value["warmup"] != if numeric { 200 } else { 8 }
        || value["measured"] != if numeric { 2000 } else { 32 }
        || !value["fixture"].is_string()
    {
        return Err("Invalid whole-connection control".into());
    }
    Ok(value)
}
enum Client {
    Immediate {
        stream: Downstream,
        task: Option<tokio::task::JoinHandle<()>>,
        stopping: CancellationToken,
        registry: CompletionRegistry,
    },
    Loopback {
        stream: tokio::net::TcpStream,
        gateway: Option<GatewayHandle>,
    },
}
impl Client {
    async fn feed(&mut self, bytes: Bytes) -> Result<(), String> {
        match self {
            Self::Immediate { stream, .. } => stream
                .feed(bytes)
                .map_err(|_| "Synthetic downstream input failed".into()),
            Self::Loopback { stream, .. } => stream
                .write_all(&bytes)
                .await
                .map_err(|_| "Loopback downstream input failed".into()),
        }
    }
    async fn response(&mut self) -> Result<Message, String> {
        match self {
            Self::Immediate {
                stream, registry, ..
            } => {
                if registry.pending() != 0 || stream.flushes() == 0 {
                    return Err("Synthetic delivery was not flushed".into());
                }
                stream
                    .take_response()
                    .map_err(|_| "Synthetic downstream response incomplete".into())
            }
            Self::Loopback { stream, .. } => {
                let mut wire = Vec::new();
                let mut chunk = [0; 8192];
                loop {
                    let count = stream
                        .read(&mut chunk)
                        .await
                        .map_err(|_| "Loopback downstream response failed")?;
                    if count == 0 || count > io::MAX_RESPONSE.saturating_sub(wire.len()) {
                        return Err("Loopback downstream response incomplete".into());
                    }
                    wire.extend_from_slice(&chunk[..count]);
                    if let Some(response) =
                        io::parse(&wire, false).map_err(|_| "Loopback downstream framing failed")?
                    {
                        if response.consumed != wire.len() {
                            return Err("Loopback downstream response has extra bytes".into());
                        }
                        return Ok(response);
                    }
                }
            }
        }
    }
    async fn close(mut self) {
        match &mut self {
            Self::Immediate { task, stopping, .. } => {
                stopping.cancel();
                if let Some(task) = task.take() {
                    let _ = task.await;
                }
            }
            Self::Loopback { gateway, stream } => {
                let _ = stream.shutdown().await;
                if let Some(gateway) = gateway.take() {
                    gateway.close().await;
                }
            }
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        if let Self::Immediate { stopping, task, .. } = self {
            stopping.cancel();
            if let Some(task) = task {
                task.abort();
            }
        }
    }
}
struct CaseResult {
    rows: Vec<Value>,
    times: Vec<f64>,
}
async fn run_case(
    input: &Value,
    loopback: bool,
    requests: usize,
    held_writer: bool,
    numerical: bool,
) -> Result<CaseResult, String> {
    let raw = input["fixture"]
        .as_str()
        .ok_or("Missing whole-connection fixture")?
        .as_bytes();
    let offsets: Vec<_> = raw
        .windows(12)
        .enumerate()
        .filter_map(|(index, bytes)| (bytes == b"000000000000").then_some(index))
        .collect();
    let [offset] = offsets.as_slice() else {
        return Err("Ambiguous whole-connection fixture index".into());
    };
    if raw.len() > io::MAX_REQUEST.saturating_sub(1024) {
        return Err("Whole-connection fixture exceeds wire bound".into());
    }
    let provider = Arc::new(ProviderState::default());
    let upstream = if loopback {
        Some(
            LoopbackProvider::start(provider.clone())
                .await
                .map_err(|_| "Cannot start synthetic loopback provider")?,
        )
    } else {
        None
    };
    let authority = upstream.as_ref().map_or_else(
        || "127.0.0.1:1".to_owned(),
        |upstream| upstream.address.to_string(),
    );
    let config = read_config(&json!({
        "AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_AUTH_MODE":"api-key","AUTOROUTER_TOKEN":super::TOKEN,
        "TYPESAFE_API_KEY":"synthetic-processing-evaluator-key","ANTHROPIC_API_KEY":"synthetic-processing-provider-key",
        "AUTOROUTER_JEV_URL":"http://127.0.0.1:1/v1/systemone","AUTOROUTER_UPSTREAM_URL":format!("http://{authority}")
    }), false, Path::new("/synthetic")).map_err(|_| "Invalid synthetic whole-connection config")?;
    let inference = if loopback {
        InferenceClient::Loopback(
            NativeHttpClient::new().map_err(|_| "Cannot construct ordinary native HTTP client")?,
        )
    } else {
        InferenceClient::Synthetic(DevelopmentHttpClient::new(io::Connector(provider.clone())))
    };
    let transport = Arc::new(Transport {
        calls: Mutex::new(Calls::default()),
        count_done: Arc::new(AtomicUsize::new(0)),
        count_abandoned: Arc::new(AtomicUsize::new(0)),
        inference,
        authority,
    });
    let parsed_counts = Arc::new(Mutex::new(ParsedCount::default()));
    let count_collector = parsed_counts.clone();
    let router = Arc::new(ObservedRouter {
        router: Router::new(transport.clone(), config.clone()).with_development_count_observer(
            Arc::new(move |count| {
                let mut observed = count_collector.lock().unwrap();
                if !observed.values.is_empty() {
                    observed.failed = true;
                } else {
                    observed.values.push(count);
                }
            }),
        ),
        commits: Mutex::new(Vec::new()),
    });
    let observations = Arc::new(Mutex::new(Observations::default()));
    let gateway = Gateway::with_router(
        config,
        transport.clone(),
        router.clone(),
        EventSinks {
            log: Some(sink(observations.clone(), "log")),
            status: Some(sink(observations.clone(), "status")),
            decision: Some(sink(observations.clone(), "decision")),
            record: None,
        },
    )?;
    let mut client = if loopback {
        let handle = gateway.listen(0).await?;
        let stream = tokio::net::TcpStream::connect(handle.address)
            .await
            .map_err(|_| "Cannot connect ordinary loopback gateway")?;
        Client::Loopback {
            stream,
            gateway: Some(handle),
        }
    } else {
        let stream = Downstream::default();
        let registry = CompletionRegistry::new(16);
        let stopping = CancellationToken::new();
        let task = tokio::spawn(gateway.clone().serve_development_connection(
            stream.clone(),
            registry.clone(),
            stopping.clone(),
        ));
        tokio::task::yield_now().await;
        Client::Immediate {
            stream,
            registry,
            stopping,
            task: Some(task),
        }
    };
    let result = async {
        let mut rows = Vec::with_capacity(requests);
        let mut times = Vec::new();
        for index in 0..requests {
            let mut body = raw.to_vec();
            body[*offset..*offset + 12].copy_from_slice(format!("{index:012}").as_bytes());
            let wire = io::request_wire(&body, index);
            *observations.lock().unwrap() = Observations::default();
            *parsed_counts.lock().unwrap() = ParsedCount::default();
            if held_writer {
                let Client::Immediate { stream, .. } = &client else { return Err("Held-writer control requires immediate IO".into()); };
                stream.hold_flush();
            }
            // Ordinary codecs/handler/delivery and advisory completion all lie
            // between these edges. Numerical sample storage is disabled first.
            let started = Instant::now();
            client.feed(wire.clone()).await?;
            let mut writer_released = !held_writer;
            loop {
                let (complete,model_observed,failed) = {
                    let observed = observations.lock().unwrap();
                    let model_observed = observed.status.iter().any(|document| document.get(document.root(), "event").and_then(|node| document.string(node)).and_then(|value| value.to_scalar()).as_deref() == Some("upstream_model"));
                    (observed.complete,model_observed,observed.failed)
                };
                let (count_parsed, count_failed) = {
                    let observed = parsed_counts.lock().unwrap();
                    (observed.values.len() == 1, observed.failed)
                };
                if failed || count_failed { return Err("Whole-connection observation bound exceeded".into()); }
                if !writer_released && model_observed && count_parsed && transport.count_done.load(Ordering::SeqCst) == index + 1 {
                    let Client::Immediate { stream, registry, .. } = &client else { return Err("Held-writer control lost its owner".into()); };
                    if complete || !router.commits.lock().unwrap().is_empty() || registry.pending() == 0 {
                        return Err("Whole-connection clock/commit ended before writer flush".into());
                    }
                    stream.release_flush();
                    writer_released = true;
                }
                if complete && count_parsed && transport.count_done.load(Ordering::SeqCst) == index + 1 { break; }
                if transport.count_abandoned.load(Ordering::SeqCst) != 0 || started.elapsed() > Duration::from_secs(10) { return Err("Whole-connection delivery/count incomplete".into()); }
                tokio::task::yield_now().await;
            }
            // The real count-result observer fires only after the actual count
            // future returns, including parsing/cache/finally. EOF alone cannot
            // end this clock or establish a successfully parsed token count.
            // Processing clock ends before detailed response parsing/hashing.
            let elapsed = started.elapsed();
            if elapsed > Duration::from_secs(10) { return Err("Whole-connection writer completion deadline".into()); }
            if numerical && index >= input["warmup"].as_u64().unwrap() as usize { times.push(elapsed.as_secs_f64() * 1000.0); }
            let response = client.response().await?;
            let calls = std::mem::take(&mut *transport.calls.lock().unwrap());
            let inference = std::mem::take(&mut *provider.records.lock().unwrap());
            let commits = std::mem::take(&mut *router.commits.lock().unwrap());
            let observed = std::mem::take(&mut *observations.lock().unwrap());
            let log = observation_value(&observed.log);
            let status = observation_value(&observed.status);
            let decision = observation_value(&observed.decision);
            if response.status != 200 || response.body.as_ref() != service::RESPONSE
                || calls.evaluator.len() != 1 || calls.count.len() != 1 || inference.len() != 1 || commits.len() != 1 || observed.decision.len() != 1
                || inference[0].body.as_ref() != body || status.as_array().and_then(|rows| rows.last()).is_none_or(|row| row["event"] != "request_complete" || row["completion_confirmed"] != true)
            { return Err("Whole-connection output/call/commit mismatch".into()); }
            let parsed = std::mem::take(&mut *parsed_counts.lock().unwrap());
            if parsed.failed || parsed.values != [Some(1000)] { return Err("Whole-connection actual parsed count mismatch".into()); }
            let branch = policy_proof(&decision[0], &inference[0].body, &calls.count[0], parsed.values[0], &response.body)?;
            let mut row = json!({"index":index,"raw_request_sha256":digest(&wire),"prepared_sha256":digest(&inference[0].body),"response_sha256":digest(&response.body),"response_status":response.status,
                "response_headers_sha256":object_hash(&serde_json::to_value(&response.headers).unwrap()),"inference_headers_sha256":object_hash(&serde_json::to_value(&inference[0].headers).unwrap()),
                "evaluator_payload_sha256":digest(&calls.evaluator[0]),"count_payload_sha256":digest(&calls.count[0]),"log_sha256":object_hash(&log),"status_sha256":object_hash(&status),"decision_sha256":object_hash(&decision),"commits_sha256":object_hash(&json!(commits)),
                "evaluator_calls":1,"inference_calls":1,"count_calls":1,"count_responses_completed":1,"count_responses_abandoned":0,"delivery_confirmed":true,"handler_complete":true});
            row.as_object_mut().unwrap().extend(branch.as_object().unwrap().clone());
            rows.push(row);
        }
        Ok(CaseResult { rows, times })
    }.await;
    client.close().await;
    router.router.close().await;
    drop(gateway);
    drop(router);
    drop(transport);
    if let Some(upstream) = upstream {
        upstream.close().await;
    }
    for _ in 0..100 {
        if provider.live.load(Ordering::SeqCst) == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    if provider.live.load(Ordering::SeqCst) != 0 {
        return Err("Synthetic HTTP client connections survived cleanup".into());
    }
    result
}
async fn collect(input: &Value) -> Result<Value, String> {
    let numerical = input["mode"] == "measure";
    let warmup = input["warmup"].as_u64().unwrap() as usize;
    let measured = input["measured"].as_u64().unwrap() as usize;
    let result = run_case(input, false, warmup + measured, false, numerical).await?;
    let controls = run_case(input, true, 2, false, false).await?;
    if result.rows[..2] != controls.rows {
        return Err("Native ordinary-loopback control mismatch".into());
    }
    let held = run_case(input, false, 1, true, false).await?;
    if held.rows[0] != result.rows[0] {
        return Err("Native held-writer control changed output/state".into());
    }
    let mut report = json!({"implementation":"rust","fixture_sha256":digest(input["fixture"].as_str().unwrap().as_bytes()),"requests":measured,"warmup_requests":warmup,"deterministic_checks_passed":true,"genuine_inference_client":true,"ordinary_loopback_control":true,"held_writer_control":true,"count_responses_completed":warmup+measured,"count_responses_abandoned":0,"rows":result.rows,"loopback_controls":controls.rows,"numerical_samples_retained":numerical,"clean_close":true});
    if numerical {
        report["raw_processing_ms"] = json!(result.times);
    }
    Ok(report)
}
pub(super) fn child() -> Result<bool, String> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(io::MAX_REQUEST as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read whole-connection control")?;
    let input = spec(&bytes)?;
    if input["mode"] == "measure" && cfg!(debug_assertions) {
        return Err("Whole-connection numerical collection requires a release helper".into());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Cannot create whole-connection runtime")?;
    let report = runtime.block_on(collect(&input))?;
    std::io::stdout()
        .write_all(&serde_json::to_vec(&report).unwrap())
        .map_err(|_| "Cannot write whole-connection result")?;
    Ok(true)
}
fn verified_pair(node: &Value, native: &Value, numerical: bool) -> Result<(), String> {
    let warmup = if numerical { 200 } else { 8 };
    let measured = if numerical { 2000 } else { 32 };
    for row in [node, native] {
        for field in [
            "deterministic_checks_passed",
            "genuine_inference_client",
            "ordinary_loopback_control",
            "held_writer_control",
            "clean_close",
        ] {
            if row[field] != true {
                return Err(format!("Whole-connection missing proof: {field}"));
            }
        }
        if row["requests"] != measured
            || row["warmup_requests"] != warmup
            || row["count_responses_completed"] != warmup + measured
            || row["count_responses_abandoned"] != 0
            || row["numerical_samples_retained"] != numerical
            || row["rows"]
                .as_array()
                .is_none_or(|rows| rows.len() != warmup + measured)
            || row["loopback_controls"]
                .as_array()
                .is_none_or(|rows| rows.len() != 2)
        {
            return Err("Whole-connection missing counts/cleanup".into());
        }
        if numerical {
            if row["raw_processing_ms"].as_array().is_none_or(|values| {
                values.len() != measured
                    || values.iter().any(|value| {
                        value
                            .as_f64()
                            .is_none_or(|number| !number.is_finite() || number < 0.0)
                    })
            }) {
                return Err("Whole-connection numeric samples invalid".into());
            }
        } else if row.get("raw_processing_ms").is_some() || row.get("processing_ms").is_some() {
            return Err("Whole-connection validation must not retain clocks".into());
        }
        let proof = row["rows"].as_array().unwrap();
        if row["loopback_controls"].as_array().unwrap() != &proof[..2] {
            return Err("Whole-connection ordinary-loopback evidence differs".into());
        }
        for (index, sample) in proof.iter().enumerate() {
            if sample["index"] != index
                || sample["response_status"] != 200
                || sample["evaluator_calls"] != 1
                || sample["inference_calls"] != 1
                || sample["count_calls"] != 1
                || sample["count_responses_completed"] != 1
                || sample["count_responses_abandoned"] != 0
                || sample["delivery_confirmed"] != true
                || sample["handler_complete"] != true
            {
                return Err("Whole-connection individual completion proof missing".into());
            }
            for (field, expected) in expected_branch().as_object().unwrap() {
                if sample[field] != *expected {
                    return Err(format!("Whole-connection policy branch mismatch: {field}"));
                }
            }
            for field in [
                "raw_request_sha256",
                "prepared_sha256",
                "response_sha256",
                "response_headers_sha256",
                "inference_headers_sha256",
                "evaluator_payload_sha256",
                "count_payload_sha256",
                "log_sha256",
                "status_sha256",
                "decision_sha256",
                "commits_sha256",
            ] {
                if sample[field].as_str().is_none_or(|value| {
                    value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
                }) {
                    return Err(format!("Whole-connection invalid proof hash: {field}"));
                }
            }
        }
    }
    if node["fixture_sha256"] != native["fixture_sha256"] {
        return Err("Whole-connection fixture identity mismatch".into());
    }
    for (index, (node, native)) in node["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(native["rows"].as_array().unwrap())
        .enumerate()
    {
        if node != native {
            let field = node
                .as_object()
                .ok_or("Invalid whole-connection proof row")?
                .iter()
                .find(|(key, value)| native[*key] != **value)
                .map(|(key, _)| key.as_str())
                .unwrap_or("unexpected_field");
            return Err(format!(
                "Whole-connection Node/native row{index} mismatch: {field}"
            ));
        }
    }
    Ok(())
}
pub(super) fn run(args: &[String], root: &Path) -> Result<bool, String> {
    if args == ["--help"] {
        println!(
            "cargo xtask benchmark --whole-connection [--validate | --measure] --output NEW_DIRECTORY\nDefault deterministic validation retains no clocks. Explicit --measure requires a release helper and runs five alternating pairs,200 warmup/2000 measured samples, with genuine ordinary gateway/client HTTP1 codecs, bounded immediate sockets and independent held-writer/loopback controls. No providers or automatic hardware/shipping-artifact approval."
        );
        return Ok(true);
    }
    let mut destination = None;
    let mut validate = false;
    let mut numerical = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--validate" => validate = true,
            "--measure" => numerical = true,
            "--output" => {
                index += 1;
                destination = args.get(index).map(|value| root.join(value));
            }
            _ => return Err("Unknown whole-connection option; use --help".into()),
        }
        index += 1;
    }
    if validate && numerical {
        return Err("Choose whole-connection --validate or --measure, not both".into());
    }
    if numerical && cfg!(debug_assertions) {
        return Err("Whole-connection numerical collection requires a release helper".into());
    }
    let review: Value =
        serde_json::from_str(REVIEW).map_err(|_| "Invalid whole-connection boundary review")?;
    if numerical
        && (review["protocol_sha256"] != digest(PROTOCOL.as_bytes())
            || review["numerical_collection_enabled"] != true
            || review["threshold"] != 0.5
            || review["representative_hardware_acceptance"] != false
            || review["shipping_artifact_qualification"] != false)
    {
        return Err("Whole-connection boundary review does not authorize this protocol".into());
    }
    let destination = destination.ok_or("--output requires a new directory")?;
    if destination.exists() {
        return Err("Whole-connection evidence destination already exists".into());
    }
    std::fs::create_dir_all(&destination)
        .map_err(|_| "Cannot create whole-connection evidence directory")?;
    let result = run_inner(root, &destination, numerical);
    if let Err(error) = &result {
        let _ = write(
            &destination.join("failure.json"),
            &json!({"completed":false,"error":error,"acceptance_qualified":false}),
        );
    }
    result
}
fn environment(root: &Path) -> Value {
    let mut value = measure::environment(root);
    value["driver_worker_threads"] = Value::Null;
    value["candidate_worker_configuration"] = json!(
        "Synchronous external driver;native helper one current-thread Tokio executor;Node helper one JavaScript event-loop thread. Client/classifier/advisory background task scheduling remains inside each processing sample."
    );
    value["build_profile"] = json!(if cfg!(debug_assertions) {
        "debug validation;source-to-binary provenance not verified"
    } else {
        "release helper;source-to-binary provenance not verified"
    });
    value
}
fn run_inner(root: &Path, destination: &Path, numerical: bool) -> Result<bool, String> {
    let reference = root.join("artifacts/rust-rewrite/reference");
    crate::reference::freeze(root, &reference)?;
    let node = node_path()?;
    let helper = std::env::current_exe().map_err(|_| "Cannot locate whole-connection helper")?;
    let mut sources = Vec::new();
    for directory in ["rust/crates", "rust/xtask", "rust/vendor"] {
        source_files(&root.join(directory), root, &mut sources)?;
    }
    for file in [
        "rust/Cargo.toml",
        "rust/Cargo.lock",
        "rust/rust-toolchain.toml",
    ] {
        let bytes = crate::process::read_bounded(&root.join(file), 16 * 1024 * 1024)?;
        sources.push(json!({"path":file,"sha256":digest(&bytes),"bytes":bytes.len()}));
    }
    let mut adapters = Vec::new();
    for file in [
        "rust/parity/whole-connection-reference.mjs",
        "rust/parity/whole-connection-reference-sockets.mjs",
    ] {
        let bytes = crate::process::read_bounded(&root.join(file), io::MAX_REQUEST as u64)?;
        adapters.push(json!({"path":file,"sha256":digest(&bytes),"bytes":bytes.len()}));
    }
    write(
        &destination.join("preflight.json"),
        &json!({"schema_version":1,"kind":"matched_whole_connection_preflight","protocol":serde_json::from_str::<Value>(PROTOCOL).unwrap(),"protocol_sha256":digest(PROTOCOL.as_bytes()),"boundary_review":serde_json::from_str::<Value>(REVIEW).unwrap(),"boundary_review_sha256":digest(REVIEW.as_bytes()),"policy_companion":serde_json::from_str::<Value>(POLICY).unwrap(),"policy_companion_sha256":digest(POLICY.as_bytes()),"helper_sha256":digest(&crate::process::read_bounded(&helper,256*1024*1024)?),"node_sha256":digest(&crate::process::read_bounded(&node,256*1024*1024)?),"source_manifest":sources,"reference_adapters":adapters,"environment":environment(root),"mode":if numerical{"exploratory_measurement"}else{"deterministic_validation"},"provider_calls":0,"representative_hardware_acceptance":false,"development_feature_enabled":true,"shipping_artifact_qualification":false}),
    )?;
    let rounds = if numerical { 5 } else { 1 };
    let warmup = if numerical { 200 } else { 8 };
    let measured = if numerical { 2000 } else { 32 };
    let mut all_rows = Vec::new();
    let mut ratios = Vec::new();
    for round in 1..=rounds {
        let workload = Workload {
            id: "large_catalog_miss",
            tools: true,
            cached: false,
            delay_ms: 0,
            concurrency: 1,
            stream: None,
            payload: Payload::Standard,
        };
        let fixture = Fixture::new(&workload, round);
        let input = json!({"kind":"synthetic_whole_connection_input_v1","mode":if numerical{"measure"}else{"validate"},"warmup":warmup,"measured":measured,"fixture":std::str::from_utf8(&fixture.bytes).unwrap()});
        let bytes = serde_json::to_vec(&input).unwrap();
        spec(&bytes)?;
        write(
            &destination.join(format!("round-{round}-input.json")),
            &input,
        )?;
        let mut pair = Vec::new();
        let order = if round % 2 == 1 {
            ["node", "rust"]
        } else {
            ["rust", "node"]
        };
        for engine in order {
            let mut command = if engine == "node" {
                let mut command = Command::new(&node);
                command
                    .arg(root.join("rust/parity/whole-connection-reference.mjs"))
                    .arg(&reference);
                command
            } else {
                let mut command = Command::new(&helper);
                command.args(["benchmark", "__whole-connection-child"]);
                command
            };
            command.env_clear().current_dir(root);
            for key in ["PATH", "TMPDIR", "LANG", "LC_ALL"] {
                if let Some(value) = std::env::var_os(key) {
                    command.env(key, value);
                }
            }
            let bytes =
                crate::process::capture(&mut command, &bytes, Duration::from_secs(30 * 60))?;
            let mut row: Value = serde_json::from_slice(&bytes)
                .map_err(|_| "Invalid whole-connection helper response")?;
            if row["implementation"] != engine {
                return Err("Whole-connection helper identity mismatch".into());
            }
            if numerical {
                let samples: Vec<_> = row["raw_processing_ms"]
                    .as_array()
                    .ok_or("Missing whole-connection numeric samples")?
                    .iter()
                    .map(|value| value.as_f64().unwrap_or(f64::NAN))
                    .collect();
                row["processing_ms"] = measure::stats(&samples);
            }
            row["round"] = json!(round);
            write(
                &destination.join(format!("round-{round}-{engine}.json")),
                &row,
            )?;
            pair.push(row);
        }
        let node = pair
            .iter()
            .find(|row| row["implementation"] == "node")
            .unwrap();
        let native = pair
            .iter()
            .find(|row| row["implementation"] == "rust")
            .unwrap();
        verified_pair(node, native, numerical)?;
        if numerical {
            let baseline = node["processing_ms"]["p95"].as_f64().unwrap();
            let candidate = native["processing_ms"]["p95"].as_f64().unwrap();
            if baseline <= 0.0 || !candidate.is_finite() {
                return Err("Whole-connection p95 ratio is not measurable".into());
            }
            ratios.push(candidate / baseline);
        }
        all_rows.extend(pair);
    }
    let interval = if numerical {
        measure::interval(&ratios)
    } else {
        Value::Null
    };
    let status = if !numerical {
        "not_measured"
    } else if interval["high"].as_f64().is_some_and(|value| value <= 0.5) {
        "observed_processing_target_met"
    } else if interval["low"].as_f64().is_some_and(|value| value > 0.5) {
        "observed_processing_target_not_met"
    } else {
        "inconclusive"
    };
    let report = json!({"schema_version":1,"kind":"matched_whole_http1_connection_processing","completed":true,"deterministic_parity":true,"rounds":rounds,"protocol_sha256":digest(PROTOCOL.as_bytes()),"boundary_review_sha256":digest(REVIEW.as_bytes()),"policy_companion_sha256":digest(POLICY.as_bytes()),"metric":"large_catalog_whole_gateway_processing_p95_ratio","threshold":0.5,"interval":interval,"status":status,"acceptance_qualified":false,"whole_gateway_processing_gate":"incomplete","representative_hardware_acceptance":false,"shipping_artifact_qualification":false,"provider_calls":0,"numerical_samples_retained":numerical,"rows":all_rows,"environment_after":environment(root)});
    write(&destination.join("report.json"), &report)?;
    println!(
        "{}",
        json!({"report":destination.join("report.json"),"completed":true,"status":status,"whole_gateway_processing_gate":"incomplete"})
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fake_report(numerical: bool) -> Value {
        let (warmup, measured) = if numerical { (200, 2000) } else { (8, 32) };
        let rows: Vec<_> = (0..warmup+measured).map(|index| {
            let mut row=json!({"index":index,"response_status":200,"evaluator_calls":1,"inference_calls":1,"count_calls":1,"count_responses_completed":1,"count_responses_abandoned":0,"delivery_confirmed":true,"handler_complete":true});
            row.as_object_mut().unwrap().extend(expected_branch().as_object().unwrap().clone());
            for field in ["raw_request_sha256","prepared_sha256","response_sha256","response_headers_sha256","inference_headers_sha256","evaluator_payload_sha256","count_payload_sha256","log_sha256","status_sha256","decision_sha256","commits_sha256"] { row[field]=json!("a".repeat(64)); }
            row
        }).collect();
        let mut value = json!({"fixture_sha256":"a".repeat(64),"requests":measured,"warmup_requests":warmup,"count_responses_completed":warmup+measured,"count_responses_abandoned":0,"deterministic_checks_passed":true,"genuine_inference_client":true,"ordinary_loopback_control":true,"held_writer_control":true,"clean_close":true,"numerical_samples_retained":numerical,"loopback_controls":rows[..2],"rows":rows});
        if numerical {
            value["raw_processing_ms"] = json!(vec![1.0; measured]);
        }
        value
    }
    #[test]
    fn paired_claims_require_individual_delivery_counts_hashes_and_fresh_controls() {
        let valid = fake_report(false);
        assert!(verified_pair(&valid, &valid, false).is_ok());
        for (field, bad) in [
            ("delivery_confirmed", json!(false)),
            ("handler_complete", json!(false)),
            ("count_responses_completed", json!(0)),
            ("count_responses_abandoned", json!(1)),
            ("inference_calls", json!(2)),
            ("response_status", json!(502)),
            ("prepared_sha256", json!("short")),
            ("decision_reason", json!("passthrough")),
            ("classified_tier", json!("opus")),
            ("selected_model", json!("claude-sonnet-5")),
            ("count_target_model", json!("claude-opus-5-5")),
            ("count_parsed_input_tokens", json!(null)),
            ("count_result_used_for_selected_target", json!(true)),
        ] {
            let mut changed = valid.clone();
            changed["rows"][3][field] = bad;
            assert!(verified_pair(&changed, &changed, false).is_err(), "{field}");
        }
        let mut changed = valid.clone();
        changed["rows"][3]["prepared_sha256"] = json!("b".repeat(64));
        assert!(verified_pair(&valid, &changed, false).is_err());
        changed = valid.clone();
        changed["loopback_controls"][0]["commits_sha256"] = json!("b".repeat(64));
        assert!(verified_pair(&changed, &changed, false).is_err());
        changed = valid.clone();
        changed["held_writer_control"] = json!(false);
        assert!(verified_pair(&changed, &changed, false).is_err());
        changed = valid.clone();
        changed["raw_processing_ms"] = json!(vec![1.0; 32]);
        assert!(verified_pair(&changed, &changed, false).is_err());
    }
    #[test]
    fn numerical_schema_requires_declared_samples_without_running_a_benchmark() {
        let valid = fake_report(true);
        assert!(verified_pair(&valid, &valid, true).is_ok());
        for bad in [json!(vec![1.0; 1999]), json!([1.0]), json!(null)] {
            let mut changed = valid.clone();
            changed["raw_processing_ms"] = bad;
            assert!(verified_pair(&changed, &changed, true).is_err());
        }
        let mut changed = valid.clone();
        changed["raw_processing_ms"][0] = json!(-1.0);
        assert!(verified_pair(&changed, &changed, true).is_err());
        changed = valid;
        changed["warmup_requests"] = json!(199);
        assert!(verified_pair(&changed, &changed, true).is_err());
        let review: Value = serde_json::from_str(REVIEW).unwrap();
        assert_eq!(review["protocol_sha256"], digest(PROTOCOL.as_bytes()));
        assert_eq!(review["threshold"], 0.5);
        assert_eq!(review["shipping_artifact_qualification"], false);
    }
    #[test]
    fn frozen_policy_requires_the_actual_count_result_and_declared_guard() {
        let decision = json!({"source":"jev","reason":"large_or_multimodal_request","classified_tier":"sonnet","selected_model":"claude-opus-5-5"});
        let inference = br#"{"model":"claude-opus-5-5"}"#;
        let count = br#"{"model":"claude-haiku-4-5-20251001"}"#;
        assert_eq!(
            policy_proof(&decision, inference, count, Some(1000), service::RESPONSE).unwrap(),
            expected_branch()
        );
        for parsed in [None, Some(0), Some(999)] {
            assert!(policy_proof(&decision, inference, count, parsed, service::RESPONSE).is_err());
        }
        let mut changed = decision;
        changed["counted_input_tokens"] = json!(1000);
        assert!(policy_proof(&changed, inference, count, Some(1000), service::RESPONSE).is_err());
        let declaration: Value = serde_json::from_str(POLICY).unwrap();
        assert_eq!(declaration["protocol_sha256"], digest(PROTOCOL.as_bytes()));
    }
    #[test]
    fn control_cannot_enable_numeric_collection_or_oversized_raw_input() {
        let value = json!({"kind":"synthetic_whole_connection_input_v1","warmup":8,"measured":32,"fixture":"000000000000"});
        assert!(spec(&serde_json::to_vec(&value).unwrap()).is_ok());
        let mut numeric = value;
        numeric["measured"] = json!(2000);
        assert!(spec(&serde_json::to_vec(&numeric).unwrap()).is_err());
        assert!(spec(&vec![b'x'; io::MAX_REQUEST + 1]).is_err());
    }
    #[tokio::test]
    async fn native_real_gateway_and_client_match_ordinary_loopback_control() {
        let workload = Workload {
            id: "large_catalog_miss",
            tools: true,
            cached: false,
            delay_ms: 0,
            concurrency: 1,
            stream: None,
            payload: Payload::Standard,
        };
        let fixture = Fixture::new(&workload, 1);
        let input = json!({"kind":"synthetic_whole_connection_input_v1","warmup":8,"measured":32,"fixture":std::str::from_utf8(&fixture.bytes).unwrap()});
        let native = run_case(&input, false, 2, false, false).await.unwrap();
        let control = run_case(&input, true, 2, false, false).await.unwrap();
        assert_eq!(native.rows, control.rows);
        let held = run_case(&input, false, 1, true, false).await.unwrap();
        assert_eq!(native.rows[0], held.rows[0]);
        assert!(native.times.is_empty() && control.times.is_empty() && held.times.is_empty());
    }
}
