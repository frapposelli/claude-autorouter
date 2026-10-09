//! Explicit local diagnostic using six packaged synthetic classifier fixtures.
//! No account data, repository contents, downloads, unloads or service control.
use crate::bounded_json::read_response_document;
use crate::classifier::Classifier;
use crate::evaluator::{EvaluationError, ollama_questions};
use crate::http_client::HttpTransport;
use crate::ollama_setup::inspect_ollama;
use autorouter_core::config::{
    Evaluator, RouterConfig, validate_ollama_endpoint, validate_ollama_model,
};
use autorouter_core::evaluation_report::evaluate_routing_report;
use autorouter_core::js_json::{JsDocument, JsNode};
use autorouter_core::prompt_state::build_ollama_state_document;
use bytes::Bytes;
use http_body_util::Full;
use hyper::Request;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

pub const LOCAL_DIAGNOSTIC_VERSION: u32 = 1;
const STARTUP_TIMEOUT_MS: u64 = 60000;
const METADATA_TIMEOUT_MS: u64 = 5000;
const MAX_METADATA_BYTES: usize = 1024 * 1024;

pub fn local_diagnostic_cases() -> Value {
    json!([
        {"id":"literal","expected":"haiku","prompt":"Print the literal word READY exactly. Do not add any explanation."},
        {"id":"array-length","expected":"haiku","prompt":"What does the JavaScript expression `[].length` evaluate to? Reply with only the integer."},
        {"id":"bounded-feature","expected":"sonnet","prompt":"Add pagination to this REST endpoint using `page` and `pageSize`. Validate the parameters, preserve existing filtering, and add tests for empty results and out-of-range pages."},
        {"id":"distributed-fencing","expected":"opus","prompt":"Review this distributed locking design: worker A's lease expires while paused. Worker B acquires a newer fencing token and writes successfully. A resumes and writes using its old token. The database checks only whether a token was ever issued. Explain the failure sequence and design the minimum atomic database check that prevents stale writes, including duplicate retries."},
        {"id":"new-mechanical-task","expected":"haiku","history":[
            {"role":"user","content":"Prove the safety of a distributed ledger during failover and concurrent retries."},
            {"role":"assistant","content":"The proof and regression tests are complete."}
        ],"prompt":"Separate task: replace the exact text 'recieve' with 'receive' in a label. Make no other changes."},
        {"id":"new-difficult-task","expected":"opus","history":[
            {"role":"user","content":"What is the value of [].length?"},
            {"role":"assistant","content":"0"}
        ],"prompt":"New task: diagnose nondeterministic deadlocks between several processes after a rolling deployment. Reconcile conflicting traces, identify the broken ordering invariant, and prove that the repair cannot introduce message loss."}
    ])
}
fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
fn rounded(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 100000.0).round() / 100.0
}
fn identity(model: &str) -> String {
    let model = model.strip_prefix("registry.ollama.ai/").unwrap_or(model);
    let model = model.strip_prefix("library/").unwrap_or(model);
    if model.rsplit('/').next().unwrap_or_default().contains(':') {
        model.into()
    } else {
        format!("{model}:latest")
    }
}
fn message(code: &str) -> &'static str {
    match code {
        "evaluator_required" => {
            "Local evaluation requires AUTOROUTER_EVALUATOR=ollama; the diagnostic does not change your configuration."
        }
        "invalid_configuration" => {
            "Check the local Ollama endpoint, model, and runtime deadline configuration."
        }
        "positive_keep_alive_required" => {
            "The local diagnostic requires a positive keep-alive, such as AUTOROUTER_OLLAMA_KEEP_ALIVE=5m. Normal routing still supports 0."
        }
        "model_missing" => {
            "The configured local model is not installed. Install it explicitly before rerunning this diagnostic."
        }
        "unrelated_models_resident" => {
            "Other Ollama models are resident. Retry when only the selected model, or no model, is loaded; this diagnostic leaves them running."
        }
        "residency_unavailable" => {
            "Could not safely inspect local Ollama model residency. No further evaluation was attempted."
        }
        "OLLAMA_VERSION" => "Local evaluation requires Ollama 0.35 or newer with /v1/systemone.",
        "OLLAMA_CLOUD" => {
            "The selected Ollama model uses a remote service. Select an installed local model."
        }
        "OLLAMA_TIMEOUT" => "Local Ollama metadata inspection timed out.",
        "OLLAMA_HTTP" => {
            "Local Ollama rejected metadata inspection. Check its version and selected model."
        }
        "OLLAMA_RESPONSE" => "Local Ollama returned invalid or oversized metadata.",
        "startup_failed" => "The initial synthetic evaluation failed; measured cases were not run.",
        _ => "Could not reach local Ollama. Start the existing service and retry.",
    }
}
fn known_code(code: &str) -> &'static str {
    match code {
        "OLLAMA_VERSION" => "OLLAMA_VERSION",
        "OLLAMA_CLOUD" => "OLLAMA_CLOUD",
        "OLLAMA_TIMEOUT" => "OLLAMA_TIMEOUT",
        "OLLAMA_HTTP" => "OLLAMA_HTTP",
        "OLLAMA_RESPONSE" => "OLLAMA_RESPONSE",
        _ => "OLLAMA_UNAVAILABLE",
    }
}
fn request_body(item: &Value) -> JsDocument {
    let mut messages = item["history"].as_array().cloned().unwrap_or_default();
    messages.push(json!({"role":"user","content":[
        {"type":"text","text":"<system-reminder>SYNTHETIC_REMINDER_ONLY: unrelated environment metadata.</system-reminder>"},
        {"type":"text","text":item["prompt"]}
    ]}));
    let body = json!({"model":"claude-haiku-4-5-20251001","max_tokens":32000,"tools":[],
        "system":[{"type":"text","text":"Synthetic coding assistant guidance: inspect relevant code and verify changes. ".repeat(110)}],"messages":messages});
    JsDocument::parse(body.to_string().as_bytes()).expect("packaged fixture JSON")
}

async fn residency<T: HttpTransport>(
    transport: &T,
    config: &RouterConfig,
    cancellation: &CancellationToken,
) -> Result<&'static str, &'static str> {
    let token = cancellation.child_token();
    let _guard = token.clone().drop_guard();
    let action = async {
        let request = Request::get(format!("{}/api/ps", config.ollama_endpoint))
            .body(Full::new(Bytes::new()))
            .map_err(|_| "residency_unavailable")?;
        let response = transport
            .request(request)
            .await
            .map_err(|_| "residency_unavailable")?;
        if !response.status().is_success() {
            return Err("residency_unavailable");
        }
        let (parts, body) = response.into_parts();
        let document = read_response_document(body, &parts.headers, MAX_METADATA_BYTES, &token)
            .await
            .map_err(|_| "residency_unavailable")?;
        let models = document
            .get(document.root(), "models")
            .and_then(|n| document.node(n));
        let Some(JsNode::Array(models)) = models else {
            return Err("residency_unavailable");
        };
        let expected = identity(&config.ollama_model);
        let mut names = Vec::new();
        for &item in models {
            let name = document
                .get(item, "name")
                .filter(|n| !matches!(document.node(*n), Some(JsNode::Null)))
                .or_else(|| document.get(item, "model"));
            let Some(name) = name.and_then(|n| document.string(n)) else {
                return Err("residency_unavailable");
            };
            names.push(name);
        }
        if names.iter().any(|name| {
            name.to_scalar()
                .is_none_or(|name| identity(&name) != expected)
        }) {
            return Err("unrelated_models_resident");
        }
        Ok(if names.is_empty() {
            "not_resident"
        } else {
            "resident"
        })
    };
    tokio::select! { biased;
        _ = cancellation.cancelled() => Err("residency_unavailable"),
        _ = tokio::time::sleep(Duration::from_millis(METADATA_TIMEOUT_MS)) => Err("residency_unavailable"),
        result = action => result,
    }
}
fn finish(mut report: Value) -> Value {
    let evaluation = evaluate_routing_report(
        &report["rows"],
        &json!({"evaluator":"ollama","classifierOnly":true,"policy":{"profile":"compatible"}}),
    )
    .expect("fixed diagnostic evaluation policy");
    let mut gates = evaluation["gates"].clone();
    gates["preflight"] = json!({"passed":report["preflight_passed"]});
    gates["startup"] = json!({"passed":report["startup"]["source"] == "ollama"});
    let rows = report["rows"].as_array().expect("rows array");
    gates["cases"] = json!({"passed":rows.len() == 6 && rows.iter().all(|row| row["current_task_matches"] == true),"expected":6,"completed":rows.len()});
    report["passed"] = json!(
        report.get("error").is_none()
            && gates
                .as_object()
                .unwrap()
                .values()
                .all(|gate| gate["passed"] != false)
    );
    report["gates"] = gates;
    report
}

pub async fn run_local_diagnostic<T: HttpTransport + 'static>(
    transport: Arc<T>,
    config: &RouterConfig,
    cancellation: &CancellationToken,
) -> Result<Value, EvaluationError>
where
    <T::ResponseBody as hyper::body::Body>::Error: Send + Sync + 'static,
{
    run_local_diagnostic_with_progress(transport, config, cancellation, |_| Ok(())).await
}
pub async fn run_local_diagnostic_with_progress<
    T: HttpTransport + 'static,
    F: FnMut(&Value) -> Result<(), ()>,
>(
    transport: Arc<T>,
    config: &RouterConfig,
    cancellation: &CancellationToken,
    mut progress: F,
) -> Result<Value, EvaluationError>
where
    <T::ResponseBody as hyper::body::Body>::Error: Send + Sync + 'static,
{
    if cancellation.is_cancelled() {
        return Err(EvaluationError::Cancelled);
    }
    let cases = local_diagnostic_cases();
    let mut report = json!({"schema_version":1,"type":"local_evaluator_diagnostic","fixture_version":LOCAL_DIAGNOSTIC_VERSION,
        "fixture_sha256":digest(&cases),"questions_sha256":digest(&ollama_questions()),"evaluator":"ollama","startup_timeout_ms":STARTUP_TIMEOUT_MS,
        "preflight_passed":false,"residency_before":"unknown","startup":null,"rows":[],"paid_provider_calls":0,"downloads":0,"models_unloaded":0,"configuration_changed":false});
    let result: Result<(), &'static str> = async {
        if config.evaluator != Evaluator::Ollama {
            return Err("evaluator_required");
        }
        if validate_ollama_endpoint(&config.ollama_endpoint).is_err()
            || validate_ollama_model(&config.ollama_model).is_err()
            || config.ollama_timeout_ms > 30000
        {
            return Err("invalid_configuration");
        }
        let keep_alive = config.ollama_keep_alive.as_str().unwrap_or("");
        let keep_valid = keep_alive.len() >= 2
            && keep_alive.len() <= 5
            && matches!(keep_alive.as_bytes().last(), Some(b's' | b'm' | b'h'))
            && matches!(keep_alive.as_bytes().first(), Some(b'1'..=b'9'))
            && keep_alive.as_bytes()[..keep_alive.len() - 1]
                .iter()
                .all(u8::is_ascii_digit);
        if !keep_valid {
            return Err("positive_keep_alive_required");
        }
        report["evaluator_model"] = json!(config.ollama_model);
        report["runtime_timeout_ms"] = json!(config.ollama_timeout_ms);
        report["keep_alive"] = config.ollama_keep_alive.clone();
        let _ = progress(&json!({"event":"preflight"}));
        let inspection = inspect_ollama(transport.as_ref(), config, cancellation, 5000)
            .await
            .map_err(|e| known_code(e.code))?;
        if !inspection.installed {
            return Err("model_missing");
        }
        let resident = residency(transport.as_ref(), config, cancellation).await?;
        report["residency_before"] = json!(resident);
        report["preflight_passed"] = json!(true);
        let _ = progress(
            &json!({"event":"startup","residency_before":resident,"timeout_ms":STARTUP_TIMEOUT_MS}),
        );
        let mut startup_config = config.clone();
        startup_config.ollama_timeout_ms = STARTUP_TIMEOUT_MS;
        let initial_start = Instant::now();
        let classifier = Classifier::new(transport.clone(), &startup_config);
        let startup = classifier
            .classify(
                &request_body(&json!({"prompt":"Return the literal word ready."})),
                &startup_config,
                cancellation,
            )
            .await
            .map_err(|_| "OLLAMA_UNAVAILABLE")?;
        let startup = serde_json::to_value(startup).expect("decision JSON");
        let mut details =
            json!({"latency_ms":rounded(initial_start.elapsed()),"source":startup["source"]});
        for key in ["classified_tier", "classifier_error", "classifier_status"] {
            if let Some(value) = startup.get(key) {
                details[key] = value.clone();
            }
        }
        details["residency_before"] = json!(resident);
        details["timeout_ms"] = json!(STARTUP_TIMEOUT_MS);
        report["startup"] = details.clone();
        let mut event = json!({"event":"startup_complete"});
        event
            .as_object_mut()
            .unwrap()
            .extend(details.as_object().unwrap().clone());
        let _ = progress(&event);
        if startup["source"] != "ollama" {
            return Err("startup_failed");
        }
        for item in cases.as_array().expect("fixture cases") {
            if cancellation.is_cancelled() {
                return Err("OLLAMA_UNAVAILABLE");
            }
            let resident = residency(transport.as_ref(), config, cancellation).await?;
            let _ = progress(
                &json!({"event":"case_start","case":item["id"],"residency_before":resident}),
            );
            let body = request_body(item);
            let state = build_ollama_state_document(&body, config.ollama_state_chars);
            let start = Instant::now();
            // A new classifier prevents cached answers from posing as inference.
            let classifier = Classifier::new(transport.clone(), config);
            let decision = classifier
                .classify(&body, config, cancellation)
                .await
                .map_err(|_| "OLLAMA_UNAVAILABLE")?;
            let decision = serde_json::to_value(decision).expect("decision JSON");
            let mut row = json!({"case":item["id"],"expected":item["expected"]});
            for key in [
                "classified_tier",
                "tier",
                "source",
                "evaluator",
                "reason",
                "classifier_error",
                "classifier_status",
            ] {
                if let Some(value) = decision.get(key) {
                    row[key] = value.clone();
                }
            }
            row["latency_ms"] = json!(rounded(start.elapsed()));
            row["residency_before"] = json!(resident);
            row["state_bytes"] = json!(state.stringify().len());
            row["current_task_matches"] =
                json!(state.current_task.to_scalar().as_deref() == item["prompt"].as_str());
            report["rows"].as_array_mut().unwrap().push(row.clone());
            let mut event = json!({"event":"case_complete"});
            event
                .as_object_mut()
                .unwrap()
                .extend(row.as_object().unwrap().clone());
            let _ = progress(&event);
        }
        Ok(())
    }
    .await;
    if cancellation.is_cancelled() {
        return Err(EvaluationError::Cancelled);
    }
    if let Err(code) = result {
        report["error"] = json!({"code":code,"message":message(code)});
    }
    Ok(finish(report))
}

fn display(value: &Value) -> String {
    if value.is_number() {
        return JsDocument::parse(value.to_string().as_bytes())
            .expect("numeric JSON")
            .stringify();
    }
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}
fn cause(row: &Value) -> String {
    row["classifier_error"]
        .as_str()
        .map(|error| {
            format!(
                " ({error}{})",
                row.get("classifier_status")
                    .map(|status| format!(" {}", display(status)))
                    .unwrap_or_default()
            )
        })
        .unwrap_or_default()
}
pub fn format_local_diagnostic(report: &Value) -> Vec<String> {
    let mut lines = vec![format!(
        "Local evaluator diagnostic: {}",
        report["evaluator_model"]
            .as_str()
            .unwrap_or("not configured")
    )];
    if let Some(runtime) = report.get("runtime_timeout_ms") {
        lines.push(format!(
            "Runtime deadline: {}; initial preparation bound: {} ms.",
            if runtime == 0 {
                "disabled".into()
            } else {
                format!("{} ms", display(runtime))
            },
            display(&report["startup_timeout_ms"])
        ));
    }
    if !report["startup"].is_null() {
        let startup = &report["startup"];
        lines.push(format!(
            "Initial synthetic call: {} ms; model {} before call; {}{}.",
            display(&startup["latency_ms"]),
            if startup["residency_before"] == "resident" {
                "resident"
            } else {
                "not resident"
            },
            display(&startup["source"]),
            cause(startup)
        ));
    }
    let rows = report["rows"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    for row in rows {
        lines.push(format!(
            "{}: expected {}, got {}; {}{}; {} ms; {} before call.",
            display(&row["case"]),
            display(&row["expected"]),
            row["classified_tier"].as_str().unwrap_or("no verdict"),
            display(&row["source"]),
            cause(row),
            display(&row["latency_ms"]),
            display(&row["residency_before"])
        ));
    }
    if let Some(message) = report["error"]["message"].as_str() {
        lines.push(message.into());
    }
    let correct = rows
        .iter()
        .filter(|row| row["source"] == "ollama" && row["classified_tier"] == row["expected"])
        .count();
    let missing = report["gates"]["coverage"]["missing"]
        .as_array()
        .map(|v| v.iter().map(display).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();
    lines.push(format!(
        "{}: {correct}/6 synthetic cases; missing tiers: {}.",
        if report["passed"] == true {
            "PASS"
        } else {
            "FAIL"
        },
        if missing.is_empty() { "none" } else { &missing }
    ));
    lines.push("Residency is observed, not a guarantee of cold or warm caches. This checks six classifier labels, not Claude task completion or general accuracy.".into());
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_client::HttpError;
    use http_body_util::BodyExt;
    use hyper::Response;
    use std::path::Path;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct MockOptions {
        missing: bool,
        cloud: bool,
        old_version: bool,
        unrelated_after: Option<usize>,
        bad_residency: Option<String>,
        collapsed: bool,
        failed_startup: bool,
        delay_runtime: bool,
        cancel_runtime: Option<CancellationToken>,
    }
    struct Mock {
        model: String,
        options: MockOptions,
        resident: AtomicBool,
        decisions: AtomicUsize,
        inspections: AtomicUsize,
        calls: Mutex<Vec<Value>>,
    }
    impl Mock {
        fn new(config: &RouterConfig, options: MockOptions) -> Arc<Self> {
            Arc::new(Self {
                model: config.ollama_model.clone(),
                options,
                resident: AtomicBool::new(false),
                decisions: AtomicUsize::new(0),
                inspections: AtomicUsize::new(0),
                calls: Mutex::new(Vec::new()),
            })
        }
    }
    impl HttpTransport for Mock {
        type ResponseBody = Full<Bytes>;
        async fn request(
            &self,
            request: Request<Full<Bytes>>,
        ) -> Result<Response<Self::ResponseBody>, HttpError> {
            assert!(!request.headers().contains_key("authorization"));
            assert!(!request.headers().contains_key("x-api-key"));
            let path = request.uri().path().to_owned();
            let bytes = request.into_body().collect().await.unwrap().to_bytes();
            let body = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap()
            };
            self.calls
                .lock()
                .unwrap()
                .push(json!({"path":path,"body":body}));
            let response = match path.as_str() {
                "/api/version" => {
                    json!({"version":if self.options.old_version {"0.34.9"}else{"0.35.0"}})
                }
                "/api/tags" => {
                    json!({"models":if self.options.missing {json!([])}else{json!([{"name":self.model}])}})
                }
                "/api/show" => {
                    if self.options.cloud {
                        json!({"details":{"parameter_size":"4B"},"remote_host":"PRIVATE_HOST"})
                    } else {
                        json!({"details":{"parameter_size":"4B"}})
                    }
                }
                "/api/ps" => {
                    let index = self.inspections.fetch_add(1, Ordering::SeqCst);
                    if let Some(raw) = &self.options.bad_residency {
                        return Ok(Response::new(Full::new(Bytes::from(raw.clone()))));
                    }
                    if self.options.unrelated_after.is_some_and(|n| index >= n) {
                        json!({"models":[{"name":"PRIVATE_OTHER_MODEL:latest"}]})
                    } else if self.resident.load(Ordering::SeqCst) {
                        json!({"models":[{"name":self.model}]})
                    } else {
                        json!({"models":[]})
                    }
                }
                "/v1/systemone" => {
                    assert_eq!(body["model"], self.model);
                    assert_ne!(body["keep_alive"], "0");
                    let state = body["state"].to_string();
                    for forbidden in [
                        "PRIVATE_KEY",
                        "SYNTHETIC_REMINDER_ONLY",
                        "Synthetic coding assistant guidance",
                    ] {
                        assert!(!state.contains(forbidden));
                    }
                    let index = self.decisions.fetch_add(1, Ordering::SeqCst);
                    self.resident.store(true, Ordering::SeqCst);
                    if index == 0 && self.options.failed_startup {
                        return Ok(Response::builder()
                            .status(503)
                            .body(Full::new(Bytes::from_static(b"PRIVATE_PROVIDER_ERROR")))
                            .unwrap());
                    }
                    if index > 0 {
                        if let Some(cancel) = &self.options.cancel_runtime {
                            cancel.cancel();
                            std::future::pending::<()>().await;
                        }
                        if self.options.delay_runtime {
                            tokio::time::sleep(Duration::from_millis(8)).await;
                        }
                    }
                    let selected = if index == 0 {
                        "haiku".to_owned()
                    } else if self.options.collapsed {
                        "sonnet".to_owned()
                    } else {
                        local_diagnostic_cases()
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|case| case["prompt"] == body["state"]["current_task"])
                            .expect("exact current synthetic task")["expected"]
                            .as_str()
                            .unwrap()
                            .to_owned()
                    };
                    json!({"model":self.model,"answers":{"tier":{"type":"choice","choice":selected,"confidence":1,
                        "probabilities":{"haiku":u8::from(selected=="haiku"),"sonnet":u8::from(selected=="sonnet"),"opus":u8::from(selected=="opus")}}},"usage":{"input_tokens":900,"output_tokens":1}})
                }
                _ => panic!("Unexpected non-diagnostic endpoint"),
            };
            Ok(Response::new(Full::new(Bytes::from(response.to_string()))))
        }
    }
    fn config() -> RouterConfig {
        autorouter_core::config::read_config(
            &json!({"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_OLLAMA_MODEL":"tev1:4b-q4_K_M"}),
            false,
            Path::new("/tmp"),
        )
        .unwrap()
    }
    #[test]
    fn packaged_fixtures_and_rubric_retain_recorded_reference_hashes() {
        assert_eq!(
            digest(&local_diagnostic_cases()),
            "256524fd96d0d96c9cc03053f5d520dda3244707e68edb5c492255b7872f8ac6"
        );
        assert_eq!(
            digest(&ollama_questions()),
            "be151cedb4de4b7ef3f7162d751f70ce7d9dd14efc66fae1835f73ffd04027be"
        );
    }
    #[tokio::test]
    async fn synthetic_diagnostic_separates_labels_from_transport_and_task_claims() {
        let mut config = config();
        config.jev_key = Some("PRIVATE_KEY_JEV".into());
        config.anthropic_key = Some("PRIVATE_KEY_ANTHROPIC".into());
        let transport = Mock::new(&config, MockOptions::default());
        let mut events = Vec::new();
        let report = run_local_diagnostic_with_progress(
            transport.clone(),
            &config,
            &CancellationToken::new(),
            |event| {
                events.push(event.clone());
                Err(())
            },
        )
        .await
        .unwrap();
        assert_eq!(report["passed"], true);
        assert_eq!(report["residency_before"], "not_resident");
        assert_eq!(report["startup"]["timeout_ms"], 60000);
        assert_eq!(transport.decisions.load(Ordering::SeqCst), 7);
        assert_eq!(transport.inspections.load(Ordering::SeqCst), 7);
        for gate in ["transport", "policy", "task"] {
            assert!(report["gates"][gate]["passed"].is_null());
        }
        assert_eq!(
            events
                .iter()
                .filter(|e| e["event"] == "case_complete")
                .count(),
            6
        );
        assert!(
            report["rows"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["current_task_matches"] == true
                    && row["state_bytes"].as_u64().unwrap() <= 3000
                    && row["residency_before"] == "resident")
        );
        assert!(!report.to_string().contains("PRIVATE_"));
        assert!(!report.to_string().contains("Print the literal word READY"));
        assert!(
            format_local_diagnostic(&report)
                .join("\n")
                .contains("PASS: 6/6 synthetic cases; missing tiers: none.")
        );
    }
    #[tokio::test]
    async fn unsafe_preflight_conditions_prevent_task_transmission() {
        let config = config();
        for (options, code) in [
            (
                MockOptions {
                    missing: true,
                    ..Default::default()
                },
                "model_missing",
            ),
            (
                MockOptions {
                    cloud: true,
                    ..Default::default()
                },
                "OLLAMA_CLOUD",
            ),
            (
                MockOptions {
                    old_version: true,
                    ..Default::default()
                },
                "OLLAMA_VERSION",
            ),
            (
                MockOptions {
                    unrelated_after: Some(0),
                    ..Default::default()
                },
                "unrelated_models_resident",
            ),
            (
                MockOptions {
                    bad_residency: Some("PRIVATE_BAD_JSON".into()),
                    ..Default::default()
                },
                "residency_unavailable",
            ),
            (
                MockOptions {
                    bad_residency: Some("x".repeat(MAX_METADATA_BYTES + 1)),
                    ..Default::default()
                },
                "residency_unavailable",
            ),
        ] {
            let transport = Mock::new(&config, options);
            let report =
                run_local_diagnostic(transport.clone(), &config, &CancellationToken::new())
                    .await
                    .unwrap();
            assert_eq!(report["error"]["code"], code);
            assert_eq!(report["passed"], false);
            assert_eq!(transport.decisions.load(Ordering::SeqCst), 0);
            assert!(!report.to_string().contains("PRIVATE_"));
        }
    }
    #[tokio::test]
    async fn newly_resident_other_model_stops_next_inference_without_unloading() {
        let config = config();
        let transport = Mock::new(
            &config,
            MockOptions {
                unrelated_after: Some(2),
                ..Default::default()
            },
        );
        let report = run_local_diagnostic(transport.clone(), &config, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(report["error"]["code"], "unrelated_models_resident");
        assert_eq!(report["rows"].as_array().unwrap().len(), 1);
        assert_eq!(transport.decisions.load(Ordering::SeqCst), 2);
        assert_eq!(report["models_unloaded"], 0);
    }
    #[tokio::test]
    async fn wrong_labels_and_startup_http_errors_fail_distinct_gates() {
        let config = config();
        let transport = Mock::new(
            &config,
            MockOptions {
                collapsed: true,
                ..Default::default()
            },
        );
        let report = run_local_diagnostic(transport, &config, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(report["gates"]["evaluator"]["passed"], true);
        assert_eq!(report["gates"]["rubric"]["passed"], false);
        assert_eq!(
            report["gates"]["coverage"]["missing"],
            json!(["haiku", "opus"])
        );
        let transport = Mock::new(
            &config,
            MockOptions {
                failed_startup: true,
                ..Default::default()
            },
        );
        let report = run_local_diagnostic(transport.clone(), &config, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(report["error"]["code"], "startup_failed");
        assert_eq!(report["startup"]["classifier_status"], 503);
        assert_eq!(transport.decisions.load(Ordering::SeqCst), 1);
        assert!(
            format_local_diagnostic(&report)
                .join("\n")
                .contains("http_error 503")
        );
        assert!(!report.to_string().contains("PRIVATE_"));
    }
    #[tokio::test]
    async fn invalid_settings_and_early_cancellation_do_not_perform_io() {
        for (change, code) in [
            (0, "evaluator_required"),
            (1, "invalid_configuration"),
            (2, "positive_keep_alive_required"),
        ] {
            let mut config = config();
            match change {
                0 => config.evaluator = Evaluator::Jev,
                1 => config.ollama_endpoint = "https://PRIVATE_HOST/v1".into(),
                _ => config.ollama_keep_alive = json!("0"),
            }
            let transport = Mock::new(&config, MockOptions::default());
            let report =
                run_local_diagnostic(transport.clone(), &config, &CancellationToken::new())
                    .await
                    .unwrap();
            assert_eq!(report["error"]["code"], code);
            assert!(transport.calls.lock().unwrap().is_empty());
            assert!(!report.to_string().contains("PRIVATE_"));
        }
        let config = config();
        let transport = Mock::new(&config, MockOptions::default());
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            run_local_diagnostic(transport.clone(), &config, &cancel)
                .await
                .unwrap_err(),
            EvaluationError::Cancelled
        );
        assert!(transport.calls.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn disabled_runtime_deadline_preserves_cancellation_and_finite_deadline_fallbacks() {
        let mut config = config();
        config.ollama_timeout_ms = 2;
        let transport = Mock::new(
            &config,
            MockOptions {
                delay_runtime: true,
                ..Default::default()
            },
        );
        let report = run_local_diagnostic(transport, &config, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(report["startup"]["source"], "ollama");
        assert_eq!(report["gates"]["evaluator"]["fallbacks"], 6);
        assert_eq!(
            report["gates"]["coverage"]["missing"],
            json!(["haiku", "sonnet", "opus"])
        );
        config.ollama_timeout_ms = 0;
        let transport = Mock::new(
            &config,
            MockOptions {
                delay_runtime: true,
                ..Default::default()
            },
        );
        let report = run_local_diagnostic(transport, &config, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(report["passed"], true);
        assert!(format_local_diagnostic(&report)[1].contains("Runtime deadline: disabled"));
        let cancel = CancellationToken::new();
        let transport = Mock::new(
            &config,
            MockOptions {
                cancel_runtime: Some(cancel.clone()),
                ..Default::default()
            },
        );
        assert_eq!(
            run_local_diagnostic(transport.clone(), &config, &cancel)
                .await
                .unwrap_err(),
            EvaluationError::Cancelled
        );
        assert_eq!(transport.decisions.load(Ordering::SeqCst), 2);
    }
}
