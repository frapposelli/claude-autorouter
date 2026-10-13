//! Compile controls for the original static-check definitions, plus complete
//! frozen policy schedules through the new typed production operations.
use autorouter_core::config::Evaluator;
use autorouter_core::contracts::{ClassifierDecision, ClassifierError, DecisionSource, Tier};
use autorouter_core::router::{RouteOptions, Router};
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[path = "support/router_contract.rs"]
mod support;
use support::*;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "autorouter-contract-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn compile(directory: &Path, library: &Path, scratch: &Path, source: &str) -> (bool, Vec<Value>) {
    let input = scratch.join("consumer.rs");
    fs::write(&input, source).unwrap();
    let errors = scratch.join("errors.jsonl");
    let output = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&errors)
        .unwrap();
    let mut child = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .arg("--crate-type=lib")
        .arg("--edition=2024")
        .arg("--emit=metadata")
        .arg("--error-format=json")
        .arg("--extern")
        .arg(format!("autorouter_core={}", library.display()))
        .arg("-L")
        .arg(format!("dependency={}", directory.display()))
        .arg("--out-dir")
        .arg(scratch)
        .arg(&input)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(output)
        .spawn()
        .unwrap();
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(30)
            || fs::metadata(&errors).unwrap().len() > 1024 * 1024
        {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Synthetic compile control exceeded its deadline or output cap");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let bytes = fs::read(errors).unwrap();
    assert!(bytes.len() <= 1024 * 1024);
    let diagnostics = std::str::from_utf8(&bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (status.success(), diagnostics)
}

#[test]
fn documented_producers_compile_and_each_invalid_contract_has_its_own_diagnostic() {
    let directory = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let mut libraries: Vec<_> = fs::read_dir(&directory)
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| {
            entry.file_name().to_str().is_some_and(|name| {
                name.starts_with("libautorouter_core-") && name.ends_with(".rlib")
            })
        })
        .filter(|entry| entry.file_type().unwrap().is_file())
        .collect();
    assert!(!libraries.is_empty() && libraries.len() <= 32);
    libraries.sort_by_key(|entry| entry.metadata().unwrap().modified().unwrap());
    let library = libraries.last().unwrap().path();
    let scratch = Scratch::new();
    let prefix = r#"use autorouter_core::contracts::*;
use autorouter_core::config::{Evaluator, RouterConfig};
fn checked(config: &RouterConfig) {
let classification = ClassifierDecision { tier: Tier::Opus, classified_tier: None, confidence: None,
evaluator: Evaluator::Jev, source: DecisionSource::Jev, reason: "classified", classifier_error: None, classifier_status: None };
let mut route = RoutingDecision::classified(&classification);
route.model = config.models.opus.clone(); route.latency_ms = 1.0; route.evaluation_latency_ms = 1.0;
let context = RequestContext { request_id: "example".into(), ..RequestContext::default() };
let fields = TelemetryFields { model: Some(route.model.clone()), source: Some(DecisionSource::Jev),
latency_ms: Some(1.0), completion_confirmed: Some(true), ..TelemetryFields::default() };
let event = LifecycleEvent { context: &context, event: LifecycleName::Route, fields: &fields };
let _ = (&event, config.jev_timeout_ms);
"#;
    let (accepted, diagnostics) = compile(&directory, &library, &scratch.0, &format!("{prefix}}}"));
    assert!(
        accepted,
        "Positive typed consumer must compile: {diagnostics:?}"
    );
    let mut recorded = Vec::new();
    // The rejected source cases intentionally isolate each fault. Otherwise an
    // unrelated syntax/import error could make every negative control pass.
    for (fault, code, field) in [
        ("let _ = LifecycleName::Answered;", "E0599", "Answered"),
        (
            "let _ = TelemetryFields { selected_modle: Some(\"opus\".into()), ..TelemetryFields::default() };",
            "E0560",
            "selected_modle",
        ),
        (
            "let _ = TelemetryFields { first_response_ms: Some(\"1ms\"), ..TelemetryFields::default() };",
            "E0308",
            "f64",
        ),
        ("let _ = DecisionSource::Random;", "E0599", "Random"),
        (
            "let _ = ClassifierDecision { source: \"random\", ..classification };",
            "E0308",
            "DecisionSource",
        ),
        (
            "let _ = TelemetryFields { body: (), ..TelemetryFields::default() };",
            "E0560",
            "body",
        ),
        ("let _ = config.timout_ms;", "E0609", "timout_ms"),
        ("route.latency_ms = \"1ms\";", "E0308", "f64"),
        ("let _ = Tier::Cheap;", "E0599", "Cheap"),
        (
            "let _ = ClassifierDecision { tier: \"cheap\", ..classification };",
            "E0308",
            "Tier",
        ),
        ("let _ = ClassifierError::Other;", "E0599", "Other"),
        (
            "let _ = DecisionSource::Jev; let _ = RoutingDecision { selected_modle: \"opus\".into(), ..route };",
            "E0560",
            "selected_modle",
        ),
        (
            "let _ = RequestContext { body: \"payload\".into(), ..context };",
            "E0560",
            "body",
        ),
        (
            "let _ = RouterConfig { timeout: 1, ..config.clone() };",
            "E0560",
            "timeout",
        ),
    ] {
        let (accepted, diagnostics) = compile(
            &directory,
            &library,
            &scratch.0,
            &format!("{prefix}{fault}}}"),
        );
        assert!(!accepted, "Invalid producer unexpectedly compiled: {field}");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic["level"] == "error"
                    && diagnostic["code"]["code"] == code
                    && diagnostic["rendered"]
                        .as_str()
                        .is_some_and(|message| message.contains(field))),
            "Expected specific {code}/{field} diagnostic: {diagnostics:?}"
        );
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic["level"] == "error"
                    && diagnostic["code"]["code"] == code
                    && diagnostic["rendered"]
                        .as_str()
                        .is_some_and(|message| message.contains(field))
            })
            .unwrap();
        recorded.push(json!({"source":fault,"expected_code":code,"expected_field":field,
            "actual_code":diagnostic["code"]["code"],"actual_message":diagnostic["message"],"rejected":true}));
    }
    if let Some(path) = std::env::var_os("AUTOROUTER_TYPED_CONTRACT_REPORT") {
        use std::io::Write;
        let report = json!({"schema_version":1,"kind":"native_typed_contract_compile_controls","passed":true,
            "positive_consumers_accepted":1,"negative_consumers_rejected":recorded.len(),"controls":recorded,
            "scope":"Only synthetic Rust consumers are compiled; no inference or inspected source execution. Diagnostics omit rendered output, source locations and machine paths."});
        let bytes = serde_json::to_vec_pretty(&report).unwrap();
        assert!(bytes.len() < 64 * 1024);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(&bytes).unwrap();
        file.write_all(b"\n").unwrap();
    }
}

#[test]
fn frozen_original_static_sources_match_reviewed_compile_controls() {
    let capture: Value = serde_json::from_str(include_str!(
        "../../../parity/cases/static-contracts.capture.json"
    ))
    .unwrap();
    assert_eq!(
        capture["baseline_commit"],
        "ea930c247626ce2af5ccdad721b5121417bf4ad8"
    );
    assert_eq!(
        capture["original_definitions"],
        json!([
            "test/static-checks.test.mjs#1",
            "test/static-checks.test.mjs#2"
        ])
    );
    let expected = [
        (
            "test/static-checks.test.mjs",
            "7338e1f0582bfd557c553de875d251d2334c40aec547874a647afef509393b6e",
        ),
        (
            "test/static-contracts.mts",
            "b72e0d89c8de5ff7d2636e50be9214214a45421fd2c0c63b9478a211e0de2dc0",
        ),
        (
            "src/contracts.mjs",
            "cf7ddd5d5cd5fcdead4c8ba49a4f51755a5c0eca1f0976b3a70bfac7a6278d65",
        ),
    ];
    assert_eq!(capture["sources"].as_array().unwrap().len(), expected.len());
    for (source, (path, checksum)) in capture["sources"].as_array().unwrap().iter().zip(expected) {
        assert_eq!(source["path"], path);
        assert_eq!(source["sha256"], checksum);
        assert_eq!(
            digest(source["source"].as_str().unwrap().as_bytes()),
            checksum
        );
    }
}

#[test]
fn every_closed_literal_survives_the_existing_legacy_normalizer() {
    use autorouter_core::contracts::{
        ContextCheck, ContinuityState, LifecycleName, TelemetryFields,
    };
    use autorouter_core::telemetry_event::normalize_telemetry_event;
    for event in LifecycleName::VARIANTS {
        let row = json!({"event":event,"request_id":"synthetic"});
        assert_eq!(
            normalize_telemetry_event(&row, "2026-10-13T00:00:00.000Z").unwrap()["event"],
            event.as_str()
        );
    }
    for source in DecisionSource::VARIANTS {
        let fields = TelemetryFields {
            source: Some(*source),
            ..TelemetryFields::default()
        };
        let mut row = fields.observation();
        row["event"] = json!("route");
        row["request_id"] = json!("synthetic");
        assert_eq!(
            normalize_telemetry_event(&row, "2026-10-13T00:00:00.000Z").unwrap()["source"],
            source.as_str()
        );
    }
    for (field, values) in [
        (
            "classifier_error",
            ClassifierError::VARIANTS
                .iter()
                .map(|value| serde_json::to_value(value).unwrap())
                .collect::<Vec<_>>(),
        ),
        (
            "continuity_state",
            ContinuityState::VARIANTS
                .iter()
                .map(|value| serde_json::to_value(value).unwrap())
                .collect(),
        ),
        (
            "context_check",
            ContextCheck::VARIANTS
                .iter()
                .map(|value| serde_json::to_value(value).unwrap())
                .collect(),
        ),
        (
            "classified_tier",
            Tier::VARIANTS
                .iter()
                .map(|value| serde_json::to_value(value).unwrap())
                .collect(),
        ),
    ] {
        for value in values {
            let mut row = json!({"event":"route","request_id":"synthetic"});
            row[field] = value.clone();
            assert_eq!(
                normalize_telemetry_event(&row, "2026-10-13T00:00:00.000Z").unwrap()[field],
                value
            );
        }
    }
}

fn classification(value: &Value) -> ClassifierDecision {
    ClassifierDecision {
        tier: Tier::from_value(value.get("tier")).unwrap(),
        classified_tier: Tier::from_value(value.get("classified_tier")),
        confidence: value["confidence"].as_f64(),
        evaluator: match text(value, "evaluator") {
            "jev" => Evaluator::Jev,
            "ollama" => Evaluator::Ollama,
            _ => panic!("Unknown frozen evaluator"),
        },
        source: DecisionSource::parse(text(value, "source")).unwrap(),
        reason: match text(value, "reason") {
            "classified" => "classified",
            "classifier_unavailable" => "classifier_unavailable",
            "low_confidence" => "low_confidence",
            _ => panic!("Unknown frozen reason"),
        },
        classifier_error: value["classifier_error"]
            .as_str()
            .map(|error| ClassifierError::parse(error).unwrap()),
        classifier_status: value["classifier_status"]
            .as_u64()
            .map(|status| u16::try_from(status).unwrap()),
    }
}
#[test]
fn typed_policy_preserves_every_frozen_route_metadata_shape_and_omission() {
    assert_eq!(digest(CASES.as_bytes()), CASES_SHA256);
    let mut lines = CASES.lines();
    let dictionary: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let mut routes = 0;
    for line in lines {
        let row: Value = serde_json::from_str(line).unwrap();
        if row["kind"] != "router" {
            continue;
        }
        let input = &row["input"];
        let mut router = Router::new(configuration(&input["config"]));
        for step in input["steps"].as_array().unwrap() {
            routes += 1;
            let document = document(step, &dictionary);
            let options = &step["options"];
            let options = RouteOptions {
                scope: text(options, "scope").into(),
                prompt_id: text(options, "promptId").into(),
                request_class: text(options, "requestClass").into(),
                request_id: options["requestId"].as_str().map(str::to_owned),
                count_tokens: step["count_present"] == true,
            };
            let now = step["now"].as_u64().unwrap();
            let start = router.begin_route(&document, options, now);
            let actual = if let Some(metadata) = start.passthrough_contract {
                metadata.policy_observation()
            } else {
                let pending = router.classified_typed(
                    &document,
                    start,
                    classification(&step["classification"]),
                    now,
                );
                let count = if pending.count_model.is_some() && step["count_present"] == true {
                    let outcome =
                        &step["count_calls"].as_array().unwrap().last().unwrap()["outcome"];
                    if outcome["kind"] == "json" {
                        outcome["value"].as_u64()
                    } else {
                        None
                    }
                } else {
                    None
                };
                router
                    .finish_route_typed_exact(&document, pending, count, now)
                    .metadata
                    .policy_observation()
            };
            assert_eq!(actual, step["node_expected"], "Frozen typed route {routes}");
        }
    }
    assert_eq!(routes, 263);
}

#[test]
fn legacy_policy_adapter_keeps_unknown_and_invalid_observation_fields() {
    let config = autorouter_core::config::read_config(
        &json!({"AUTOROUTER_EVALUATOR":"jev"}),
        false,
        Path::new("/synthetic"),
    )
    .unwrap();
    let mut router = Router::new(config);
    let document = autorouter_core::js_json::JsDocument::parse(
        br#"{"model":"claude-sonnet-5","messages":[{"role":"user","content":"Synthetic"}]}"#,
    )
    .unwrap();
    let start = router.begin_route(&document, RouteOptions::default(), 0);
    let original = json!({"tier":"future","source":"custom","reason":"custom_reason","future_metadata":{"n":1},"confidence":"invalid","context_check":"custom"});
    let pending = router.classified(&document, start, original.clone(), 0);
    let result = router.finish_route(&document, pending, None, 0);
    for (key, value) in original.as_object().unwrap() {
        if key != "reason" {
            assert_eq!(result[key], *value);
        }
    }
}
