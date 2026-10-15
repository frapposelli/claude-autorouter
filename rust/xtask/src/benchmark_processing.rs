//! Matched, opt-in body-processing component; no shipping gateway hooks.
use super::{Fixture, Payload, Workload, digest, measure, node_path, source_files, write};
use autorouter_core::config::read_config;
use autorouter_core::js_json::JsDocument;
use autorouter_core::model_request::prepare_request_document_exact;
use autorouter_core::request_validation::validate_request_document;
use autorouter_core::router::RouteOptions;
use autorouter_runtime::http_client::{HttpError, HttpTransport};
use autorouter_runtime::router::Router;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Request, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;
use std::pin::Pin;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const PROTOCOL: &str = include_str!("../../parity/processing-benchmark-v1.json");
const MAX_CONTROL: u64 = 1024 * 1024;
const LIMIT: Duration = Duration::from_secs(30 * 60);
const STAGES: [&str; 6] = [
    "parse",
    "validate",
    "router",
    "prepare",
    "serialize",
    "advisory_settle",
];
fn environment(root: &Path) -> Value {
    let mut value = measure::environment(root);
    value["driver_worker_threads"] = Value::Null;
    value["candidate_worker_configuration"] = json!(
        "Synchronous external driver; native helper one current-thread Tokio executor, Node helper one JavaScript event-loop thread; runtime background workers remain included in processing."
    );
    value["build_profile"] = json!(if cfg!(debug_assertions) {
        "debug validation; source-to-binary provenance not verified"
    } else {
        "release driver/helper; source-to-binary provenance not verified"
    });
    value
}
#[derive(Default)]
struct Calls {
    evaluator: Vec<Bytes>,
    count: Vec<Bytes>,
}
#[derive(Default)]
struct Immediate {
    calls: Mutex<Calls>,
    count_done: Arc<AtomicUsize>,
    count_abandoned: Arc<AtomicUsize>,
}
struct ImmediateBody {
    inner: Full<Bytes>,
    count_done: Option<Arc<AtomicUsize>>,
    count_abandoned: Arc<AtomicUsize>,
    ended: bool,
}
impl hyper::body::Body for ImmediateBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
        let result = Pin::new(&mut self.inner).poll_frame(context);
        if matches!(result, Poll::Ready(None)) && !self.ended {
            self.ended = true;
            if let Some(done) = &self.count_done {
                done.fetch_add(1, Ordering::SeqCst);
            }
        }
        result
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}
impl Drop for ImmediateBody {
    fn drop(&mut self) {
        if self.count_done.is_some() && !self.ended {
            self.count_abandoned.fetch_add(1, Ordering::SeqCst);
        }
    }
}
impl HttpTransport for Immediate {
    type ResponseBody = ImmediateBody;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        if request.uri().scheme_str() != Some("http")
            || request.uri().authority().map(|value| value.as_str()) != Some("127.0.0.1:1")
            || request.method() != hyper::Method::POST
        {
            return Err(HttpError::InvalidRequest);
        }
        let path = request.uri().path().to_owned();
        let bytes = request
            .into_body()
            .collect()
            .await
            .map_err(|_| HttpError::InvalidRequest)?
            .to_bytes();
        if bytes.len() as u64 > MAX_CONTROL {
            return Err(HttpError::InvalidRequest);
        }
        let mut calls = self.calls.lock().unwrap();
        let is_count = path == "/v1/messages/count_tokens";
        let response = match path.as_str() {
            "/v1/systemone" => {
                calls.evaluator.push(bytes);
                b"{\"answers\":{\"tier\":{\"choice\":\"sonnet\",\"confidence\":1}}}".as_slice()
            }
            "/v1/messages/count_tokens" => {
                calls.count.push(bytes);
                b"{\"input_tokens\":1000}".as_slice()
            }
            _ => return Err(HttpError::InvalidRequest),
        };
        Ok(Response::builder()
            .header("content-type", "application/json")
            .body(ImmediateBody {
                inner: Full::new(Bytes::from_static(response)),
                count_done: is_count.then(|| self.count_done.clone()),
                count_abandoned: self.count_abandoned.clone(),
                ended: false,
            })
            .unwrap())
    }
}

fn input(bytes: &[u8]) -> Result<Value, String> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "Invalid processing helper input")?;
    if bytes.len() as u64 > MAX_CONTROL
        || value["kind"] != "synthetic_processing_input_v1"
        || !matches!(value["warmup"].as_u64(), Some(8 | 200))
        || !matches!(value["measured"].as_u64(), Some(32 | 2000))
        || !value["fixture"].is_string()
        || !matches!(
            value.get("diagnostic_stages"),
            None | Some(Value::Bool(true))
        )
        || (value["diagnostic_stages"] == true && (value["warmup"] != 8 || value["measured"] != 32))
    {
        return Err("Invalid processing helper input".into());
    }
    Ok(value)
}
async fn collect(spec: &Value) -> Result<Value, String> {
    let config = read_config(&json!({
        "AUTOROUTER_EVALUATOR":"jev", "AUTOROUTER_AUTH_MODE":"api-key",
        "TYPESAFE_API_KEY":"synthetic-processing-evaluator-key", "ANTHROPIC_API_KEY":"synthetic-processing-provider-key",
        "AUTOROUTER_JEV_URL":"http://127.0.0.1:1/v1/systemone", "AUTOROUTER_UPSTREAM_URL":"http://127.0.0.1:1"
    }), false, Path::new("/synthetic")).map_err(|_| "Invalid synthetic processing config")?;
    let transport = Arc::new(Immediate::default());
    let router = Router::new(transport.clone(), config);
    let raw = spec["fixture"]
        .as_str()
        .ok_or("Missing processing fixture")?
        .as_bytes();
    let offsets: Vec<_> = raw
        .windows(12)
        .enumerate()
        .filter_map(|(index, bytes)| (bytes == b"000000000000").then_some(index))
        .collect();
    let [offset] = offsets.as_slice() else {
        return Err("Ambiguous processing fixture index".into());
    };
    let warmup = spec["warmup"].as_u64().unwrap() as usize;
    let measured = spec["measured"].as_u64().unwrap() as usize;
    let diagnostic = spec["diagnostic_stages"] == true;
    let (mut evaluator, mut count, mut prepared_hash, mut decision_hash) =
        (Sha256::new(), Sha256::new(), Sha256::new(), Sha256::new());
    let mut samples = Vec::with_capacity(measured);
    let mut stage_samples =
        diagnostic.then(|| std::array::from_fn::<Vec<f64>, 6, _>(|_| Vec::with_capacity(measured)));
    for index in 0..warmup + measured {
        let mut bytes = raw.to_vec();
        bytes[*offset..*offset + 12].copy_from_slice(format!("{index:012}").as_bytes());
        let request_id = format!("synthetic-processing-request-{index}");
        let options = RouteOptions {
            scope: json!(["synthetic-benchmark-session", "agent-0"]).to_string(),
            prompt_id: format!("synthetic-prompt-{index}"),
            request_id: Some(request_id.clone()),
            count_tokens: true,
            ..RouteOptions::default()
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            "synthetic-processing-provider-key".parse().unwrap(),
        );
        let cancellation = CancellationToken::new();
        let (started, _document, decision, prepared, stage_marks) = if diagnostic {
            let started = Instant::now();
            let document =
                Arc::new(JsDocument::parse(&bytes).map_err(|_| "Processing fixture parse failed")?);
            let parsed_at = Instant::now();
            let valid = validate_request_document(&document);
            if valid["valid"] != true {
                return Err("Processing fixture validation failed".into());
            }
            let validated_at = Instant::now();
            let decision = router
                .route_exact(document.clone(), options, &headers, &cancellation, "")
                .await
                .map_err(|_| "Processing route failed")?;
            let routed_at = Instant::now();
            let (prepared, _) = prepare_request_document_exact(&document, &decision.model);
            let prepared_at = Instant::now();
            let prepared = prepared.stringify();
            let serialized_at = Instant::now();
            (
                started,
                document,
                decision,
                prepared,
                Some([
                    started,
                    parsed_at,
                    validated_at,
                    routed_at,
                    prepared_at,
                    serialized_at,
                ]),
            )
        } else {
            let started = Instant::now();
            let document =
                Arc::new(JsDocument::parse(&bytes).map_err(|_| "Processing fixture parse failed")?);
            let valid = validate_request_document(&document);
            if valid["valid"] != true {
                return Err("Processing fixture validation failed".into());
            }
            let decision = router
                .route_exact(document.clone(), options, &headers, &cancellation, "")
                .await
                .map_err(|_| "Processing route failed")?;
            let (prepared, _) = prepare_request_document_exact(&document, &decision.model);
            let prepared = prepared.stringify();
            (started, document, decision, prepared, None)
        };
        loop {
            let lengths = {
                let calls = transport.calls.lock().unwrap();
                (calls.evaluator.len(), calls.count.len())
            };
            if lengths == (1, 1) && transport.count_done.load(Ordering::SeqCst) == index + 1 {
                break;
            }
            if lengths.0 > 1
                || lengths.1 > 1
                || transport.count_abandoned.load(Ordering::SeqCst) > 0
                || started.elapsed() > Duration::from_secs(10)
            {
                return Err(
                    "Processing mock call-count mismatch or unsettled advisory work".into(),
                );
            }
            tokio::task::yield_now().await;
        }
        let stopped = Instant::now();
        let elapsed = stopped.duration_since(started).as_secs_f64() * 1000.0;
        if decision.model.to_scalar().as_deref() != Some("claude-opus-5-5")
            || prepared.as_bytes() != bytes
        {
            return Err("Processing prepared request or model mismatch".into());
        }
        let calls = std::mem::take(&mut *transport.calls.lock().unwrap());
        evaluator.update(&calls.evaluator[0]);
        evaluator.update(b"\n");
        count.update(&calls.count[0]);
        count.update(b"\n");
        prepared_hash.update(prepared.as_bytes());
        prepared_hash.update(b"\n");
        let metadata: std::collections::BTreeMap<_, _> = decision
            .decision
            .as_object()
            .unwrap()
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "latency_ms" | "evaluation_latency_ms"))
            .collect();
        // Preserve numeric JS semantics, only normalizing declared timing.
        let metadata = JsDocument::parse(&serde_json::to_vec(&metadata).unwrap())
            .unwrap()
            .stringify();
        decision_hash.update(metadata.as_bytes());
        decision_hash.update(b"\n");
        if !router.complete(
            &request_id,
            &json!({"continuation_model":"claude-opus-5-5","tool_uses":[]}),
        ) {
            return Err("Processing simulated completion state mismatch".into());
        }
        if index >= warmup {
            samples.push(elapsed);
            if let (Some(stage_samples), Some(stage_marks)) = (&mut stage_samples, stage_marks) {
                let marks = [
                    stage_marks[0],
                    stage_marks[1],
                    stage_marks[2],
                    stage_marks[3],
                    stage_marks[4],
                    stage_marks[5],
                    stopped,
                ];
                for (stage, samples) in stage_samples.iter_mut().enumerate() {
                    samples
                        .push(marks[stage + 1].duration_since(marks[stage]).as_secs_f64() * 1000.0);
                }
            }
        }
    }
    router.close().await;
    let mut report = json!({"implementation":"rust","fixture_sha256":digest(raw),"requests":measured,"warmup_requests":warmup,"deterministic_checks_passed":true,"evaluator_calls":warmup+measured,"count_token_calls":warmup+measured,"count_responses_completed":transport.count_done.load(Ordering::SeqCst),"count_responses_abandoned":transport.count_abandoned.load(Ordering::SeqCst),"prepared_sha256":format!("{:x}",prepared_hash.finalize()),"decision_metadata_sha256":format!("{:x}",decision_hash.finalize()),"evaluator_payloads_sha256":format!("{:x}",evaluator.finalize()),"count_payloads_sha256":format!("{:x}",count.finalize()),"raw_pipeline_ms":samples});
    if let Some(stage_samples) = stage_samples {
        report["raw_stage_ms"] = json!(
            STAGES
                .into_iter()
                .zip(stage_samples)
                .collect::<std::collections::BTreeMap<_, _>>()
        );
    }
    Ok(report)
}

pub(super) fn child() -> Result<bool, String> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_CONTROL + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read processing helper input")?;
    let spec = input(&bytes)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Cannot create processing helper runtime")?;
    let report = runtime.block_on(collect(&spec))?;
    let bytes = serde_json::to_vec(&report).map_err(|_| "Cannot serialize processing result")?;
    std::io::stdout()
        .write_all(&bytes)
        .map_err(|_| "Cannot write processing result")?;
    Ok(true)
}

fn verified_pair(
    node: &Value,
    native: &Value,
    warmup: usize,
    measured: usize,
) -> Result<(), String> {
    for row in [node, native] {
        if row["requests"] != measured
            || row["warmup_requests"] != warmup
            || row["deterministic_checks_passed"] != true
            || row["evaluator_calls"] != warmup + measured
            || row["count_token_calls"] != warmup + measured
            || row["count_responses_completed"] != warmup + measured
            || row["count_responses_abandoned"] != 0
            || row["raw_pipeline_ms"].as_array().is_none_or(|values| {
                values.len() != measured
                    || values.iter().any(|value| {
                        value
                            .as_f64()
                            .is_none_or(|number| !number.is_finite() || number < 0.0)
                    })
            })
        {
            return Err("Processing helper observations are incomplete".into());
        }
    }
    for field in [
        "fixture_sha256",
        "prepared_sha256",
        "decision_metadata_sha256",
        "evaluator_payloads_sha256",
        "count_payloads_sha256",
    ] {
        if !node[field].as_str().is_some_and(|value| {
            value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) || node[field] != native[field]
        {
            return Err(format!(
                "Processing deterministic Node/native mismatch for {field}"
            ));
        }
    }
    Ok(())
}
fn verified_stages(node: &Value, native: &Value, measured: usize) -> Result<(), String> {
    for row in [node, native] {
        let stages = row["raw_stage_ms"]
            .as_object()
            .ok_or("Missing processing stage observations")?;
        if stages.len() != STAGES.len() {
            return Err("Unexpected processing stage observations".into());
        }
        for name in STAGES {
            let Some(values) = stages.get(name).and_then(Value::as_array) else {
                return Err(format!("Missing processing stage {name}"));
            };
            if values.len() != measured
                || values.iter().any(|value| {
                    value
                        .as_f64()
                        .is_none_or(|number| !number.is_finite() || number < 0.0)
                })
            {
                return Err(format!("Invalid processing stage {name}"));
            }
        }
    }
    Ok(())
}

pub(super) fn run(args: &[String], root: &Path) -> Result<bool, String> {
    if args == ["--help"] {
        println!(
            "cargo xtask benchmark --processing --output NEW_DIRECTORY [--validate | --diagnose-stages]\nMatched parse/validate/real-router/prepare/serialize component in fresh Node/native helpers with immediate in-memory evaluator/count transports. No network or models. Development helper boundary excludes HTTP receive/auth/response/disk operations and does not alone qualify the whole gateway. Numerical runs require release helpers and five new paired rounds; --validate strips timing values. --diagnose-stages runs one fixed 8+32 synthetic pair, retaining non-gating stage wall intervals for diagnosis only."
        );
        return Ok(true);
    }
    let mut destination = None;
    let mut validate = false;
    let mut diagnostic = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--output" => {
                index += 1;
                destination = args.get(index).map(|value| root.join(value));
            }
            "--validate" => validate = true,
            "--diagnose-stages" => diagnostic = true,
            _ => return Err("Unknown processing benchmark option; use --help".into()),
        }
        index += 1;
    }
    if validate && diagnostic {
        return Err("Choose either --validate or --diagnose-stages".into());
    }
    if !validate && !diagnostic && cfg!(debug_assertions) {
        return Err("Numerical processing benchmarks require a release driver/helper".into());
    }
    let destination = destination.ok_or("--output requires a new directory")?;
    if destination.exists() {
        return Err("Processing evidence destination already exists".into());
    }
    std::fs::create_dir_all(&destination)
        .map_err(|_| "Cannot create processing evidence directory")?;
    let result = run_inner(root, &destination, validate, diagnostic);
    if let Err(error) = &result {
        let _ = write(
            &destination.join("failure.json"),
            &json!({"completed":false,"error":error,"acceptance":false}),
        );
    }
    result
}
fn run_inner(
    root: &Path,
    destination: &Path,
    validate: bool,
    diagnostic: bool,
) -> Result<bool, String> {
    let reference = root.join("artifacts/rust-rewrite/reference");
    crate::reference::freeze(root, &reference)?;
    let node = node_path()?;
    let helper = std::env::current_exe().map_err(|_| "Cannot resolve native processing helper")?;
    let rounds = if validate || diagnostic { 1 } else { 5 };
    let warmup = if validate || diagnostic { 8 } else { 200 };
    let measured = if validate || diagnostic { 32 } else { 2000 };
    let before = environment(root);
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
    write(
        &destination.join("preflight.json"),
        &json!({"schema_version":1,"kind":"matched_processing_component_preflight","protocol":serde_json::from_str::<Value>(PROTOCOL).unwrap(),"protocol_sha256":digest(PROTOCOL.as_bytes()),"helper_sha256":digest(&crate::process::read_bounded(&helper,256*1024*1024)?),"node_sha256":digest(&crate::process::read_bounded(&node,256*1024*1024)?),"reference_adapter_sha256":digest(&crate::process::read_bounded(&root.join("rust/parity/processing-reference.mjs"),MAX_CONTROL)?),"source_manifest":sources,"environment":before,"mode":if validate{"deterministic_validation"}else if diagnostic{"stage_diagnostic"}else{"exploratory_measurement"},"provider_calls":0,"representative_hardware_acceptance":false}),
    )?;
    let mut rows = Vec::new();
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
        let mut spec = json!({"kind":"synthetic_processing_input_v1","warmup":warmup,"measured":measured,"fixture":std::str::from_utf8(&fixture.bytes).unwrap()});
        if diagnostic {
            spec["diagnostic_stages"] = json!(true);
        }
        let input =
            serde_json::to_vec(&spec).map_err(|_| "Cannot encode processing helper input")?;
        write(
            &destination.join(format!("round-{round}-input.json")),
            &spec,
        )?;
        if input.len() as u64 > MAX_CONTROL {
            return Err("Processing control exceeds byte bound".into());
        }
        let order = if round % 2 == 1 {
            ["node", "rust"]
        } else {
            ["rust", "node"]
        };
        let mut pair = Vec::new();
        for engine in order {
            let mut command = if engine == "node" {
                let mut command = Command::new(&node);
                command
                    .arg(root.join("rust/parity/processing-reference.mjs"))
                    .arg(&reference);
                command
            } else {
                let mut command = Command::new(&helper);
                command.args(["benchmark", "__processing-child"]);
                command
            };
            command.env_clear().current_dir(root);
            for key in ["PATH", "TMPDIR", "LANG", "LC_ALL"] {
                if let Some(value) = std::env::var_os(key) {
                    command.env(key, value);
                }
            }
            let bytes = crate::process::capture(&mut command, &input, LIMIT)?;
            let mut row: Value =
                serde_json::from_slice(&bytes).map_err(|_| "Invalid processing helper response")?;
            if row["implementation"] != engine {
                return Err("Processing helper identity mismatch".into());
            }
            let times: Vec<_> = row["raw_pipeline_ms"]
                .as_array()
                .ok_or("Missing processing samples")?
                .iter()
                .map(|value| value.as_f64().unwrap_or(f64::NAN))
                .collect();
            row["pipeline_ms"] = measure::stats(&times);
            if diagnostic {
                let mut stages = serde_json::Map::new();
                for name in STAGES {
                    let values = row["raw_stage_ms"][name]
                        .as_array()
                        .ok_or("Missing processing stage samples")?
                        .iter()
                        .map(|value| value.as_f64().unwrap_or(f64::NAN))
                        .collect::<Vec<_>>();
                    stages.insert(name.to_owned(), measure::stats(&values));
                }
                row["stage_ms"] = Value::Object(stages);
            }
            row["round"] = json!(round);
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
        verified_pair(node, native, warmup, measured)?;
        if diagnostic {
            verified_stages(node, native, measured)?;
        } else {
            let baseline = node["pipeline_ms"]["p95"].as_f64().unwrap();
            if baseline > 0.0 {
                ratios.push(native["pipeline_ms"]["p95"].as_f64().unwrap() / baseline);
            }
        }
        for mut row in pair {
            if validate {
                row.as_object_mut().unwrap().remove("pipeline_ms");
                row.as_object_mut().unwrap().remove("raw_pipeline_ms");
            }
            write(
                &destination.join(format!(
                    "round-{round}-{}.json",
                    row["implementation"].as_str().unwrap()
                )),
                &row,
            )?;
            rows.push(row);
        }
    }
    let interval = if validate || diagnostic {
        Value::Null
    } else {
        measure::interval(&ratios)
    };
    let status = if diagnostic {
        "diagnostic_only"
    } else if validate {
        "not_measured"
    } else if interval["high"].as_f64().is_some_and(|value| value <= 0.5) {
        "observed_component_target_met"
    } else if interval["low"].as_f64().is_some_and(|value| value > 0.5) {
        "observed_component_target_not_met"
    } else {
        "inconclusive"
    };
    let report = if diagnostic {
        json!({"schema_version":1,"kind":"matched_processing_stage_diagnostic_v1","completed":true,"deterministic_parity":true,"rounds":rounds,"base_protocol_sha256":digest(PROTOCOL.as_bytes()),"stages":STAGES,"timing_semantics":"Observed wall intervals between pipeline marks. Advisory count can overlap router, prepare, and serialize; these are not exclusive CPU costs or an additive causal decomposition.","status":status,"acceptance_qualified":false,"whole_gateway_processing_gate":"incomplete","representative_hardware_acceptance":false,"provider_calls":0,"rows":rows,"environment_after":environment(root)})
    } else {
        json!({"schema_version":1,"kind":"matched_gateway_body_processing_component","completed":true,"deterministic_parity":true,"rounds":rounds,"protocol_sha256":digest(PROTOCOL.as_bytes()),"metric":"large_catalog_body_processing_p95_ratio","threshold":0.5,"interval":interval,"status":status,"acceptance_qualified":false,"whole_gateway_processing_gate":"incomplete","representative_hardware_acceptance":false,"provider_calls":0,"rows":rows,"environment_after":environment(root)})
    };
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
    #[test]
    fn matched_samples_require_all_payloads_counts_and_finite_times() {
        let valid = json!({"requests":32,"warmup_requests":8,"deterministic_checks_passed":true,"evaluator_calls":40,"count_token_calls":40,"count_responses_completed":40,"count_responses_abandoned":0,"fixture_sha256":"a".repeat(64),"prepared_sha256":"b".repeat(64),"decision_metadata_sha256":"e".repeat(64),"evaluator_payloads_sha256":"c".repeat(64),"count_payloads_sha256":"d".repeat(64),"raw_pipeline_ms":vec![1.0;32]});
        assert!(verified_pair(&valid, &valid, 8, 32).is_ok());
        for key in [
            "prepared_sha256",
            "decision_metadata_sha256",
            "evaluator_payloads_sha256",
            "count_payloads_sha256",
            "evaluator_calls",
            "count_token_calls",
            "count_responses_completed",
            "count_responses_abandoned",
            "raw_pipeline_ms",
        ] {
            let mut changed = valid.clone();
            changed.as_object_mut().unwrap().remove(key);
            assert!(verified_pair(&valid, &changed, 8, 32).is_err(), "{key}");
        }
        let mut changed = valid.clone();
        changed["raw_pipeline_ms"][0] = json!(-1);
        assert!(verified_pair(&valid, &changed, 8, 32).is_err());
    }
    #[test]
    fn stage_diagnostics_require_the_fixed_stage_set_and_finite_samples() {
        let samples = vec![0.25; 32];
        let mut row = json!({"raw_stage_ms": STAGES.into_iter()
            .map(|name| (name, samples.clone()))
            .collect::<std::collections::BTreeMap<_, _>>()});
        assert!(verified_stages(&row, &row, 32).is_ok());
        row["raw_stage_ms"]["parse"][0] = json!(-1);
        assert!(verified_stages(&row, &row, 32).is_err());
        row["raw_stage_ms"]["parse"] = json!(samples);
        row["raw_stage_ms"]["unexpected"] = json!(vec![1.0; 32]);
        assert!(verified_stages(&row, &row, 32).is_err());
    }
    #[tokio::test]
    async fn request_capture_does_not_count_as_reply_completion_and_abandonment_fails() {
        let transport = Immediate::default();
        let response = transport
            .request(
                Request::post("http://127.0.0.1:1/v1/messages/count_tokens")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(transport.calls.lock().unwrap().count.len(), 1);
        assert_eq!(transport.count_done.load(Ordering::SeqCst), 0);
        drop(response);
        assert_eq!(transport.count_done.load(Ordering::SeqCst), 0);
        assert_eq!(transport.count_abandoned.load(Ordering::SeqCst), 1);
        let mut body = transport
            .request(
                Request::post("http://127.0.0.1:1/v1/messages/count_tokens")
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body();
        while body.frame().await.is_some() {}
        drop(body);
        assert_eq!(transport.count_done.load(Ordering::SeqCst), 1);
        assert_eq!(transport.count_abandoned.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn immediate_transport_rejects_every_non_synthetic_target() {
        let transport = Immediate::default();
        for url in [
            "https://api.anthropic.com/v1/messages/count_tokens",
            "http://127.0.0.1:2/v1/systemone",
            "http://127.0.0.1:1/private",
        ] {
            assert!(
                transport
                    .request(Request::post(url).body(Full::new(Bytes::new())).unwrap())
                    .await
                    .is_err()
            );
        }
        assert!(transport.calls.lock().unwrap().evaluator.is_empty());
    }
    #[test]
    fn control_limits_are_not_caller_tunable() {
        let valid = json!({"kind":"synthetic_processing_input_v1","warmup":8,"measured":32,"fixture":"synthetic"});
        assert!(input(&serde_json::to_vec(&valid).unwrap()).is_ok());
        let mut diagnostic = valid.clone();
        diagnostic["diagnostic_stages"] = json!(true);
        assert!(input(&serde_json::to_vec(&diagnostic).unwrap()).is_ok());
        for value in [json!(false), json!("yes")] {
            diagnostic["diagnostic_stages"] = value;
            assert!(input(&serde_json::to_vec(&diagnostic).unwrap()).is_err());
        }
        diagnostic["diagnostic_stages"] = json!(true);
        diagnostic["warmup"] = json!(200);
        assert!(input(&serde_json::to_vec(&diagnostic).unwrap()).is_err());
        for (key, value) in [
            ("warmup", json!(0)),
            ("measured", json!(60)),
            ("kind", json!("other")),
            ("fixture", Value::Null),
        ] {
            let mut changed = valid.clone();
            changed[key] = value;
            assert!(input(&serde_json::to_vec(&changed).unwrap()).is_err());
        }
    }
}
