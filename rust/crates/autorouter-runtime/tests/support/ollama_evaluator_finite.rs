//! Exact finite Ollama transcript; synthetic transport, no environment or network.
use autorouter_core::config::{RouterConfig, read_config};
use autorouter_core::js_json::JsDocument;
use autorouter_core::prompt_state::{build_ollama_state_document, build_state_document};
use autorouter_runtime::evaluator::{EvaluationError, evaluate_serialized_answer};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const CORPUS: &str =
    include_str!("../../../../parity/cases/ollama-evaluator-finite-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../../parity/cases/ollama-evaluator-finite-contracts.capture.json");

pub fn cases() -> Vec<Value> {
    let digest = format!("{:x}", Sha256::digest(CORPUS.as_bytes()));
    assert_eq!(
        digest,
        "d8706e0e48de75f116c70be550105ed6b607451af15eb61454ef62637dcf462e"
    );
    assert!(CORPUS.len() <= 4 * 1024 * 1024);
    let report: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(report["cases_sha256"], digest);
    assert_eq!(report["selected_definitions"], json!([1, 2, 3, 6, 11]));
    assert_eq!(report["static_assertions"], 23);
    assert_eq!(report["executed_assertions"], 70);
    let rows: Vec<Value> = CORPUS
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 15);
    rows
}

#[path = "finite_http.rs"]
mod finite_http;
pub type Replay = finite_http::Replay<2, true>;
pub fn request_observation(request: &Value) -> Value {
    finite_http::request_observation::<true>(request)
}

// JS JSON Number equality: normalize the representational1 versus1.0
// distinction, without changing any key, string, magnitude or array position.
pub fn wire(value: &Value) -> Value {
    JsDocument::parse(value.to_string().as_bytes())
        .unwrap()
        .to_serde_observation_lossy()
}

pub fn state(call: &Value) -> String {
    assert_eq!(call["input_tags"], json!([]));
    assert_eq!(call["output_tags"], json!([]));
    assert_eq!(call["limit_argument_present"], false);
    let bytes = call["input_json"].as_str().unwrap().as_bytes();
    assert!(bytes.len() <= 128 * 1024);
    let document = JsDocument::parse(bytes).unwrap();
    match call["operation"].as_str().unwrap() {
        "buildState" => {
            assert_eq!(call["limit"], 12000);
            build_state_document(&document, 12000).stringify()
        }
        "buildOllamaState" => {
            assert_eq!(call["limit"], 3000);
            build_ollama_state_document(&document, 3000).stringify()
        }
        _ => panic!("Unknown finite state operation"),
    }
}

fn settings(row: &Value) -> RouterConfig {
    let source = &row["config"];
    let mut config = read_config(
        &json!({
            "AUTOROUTER_EVALUATOR":"ollama",
            "AUTOROUTER_AUTH_MODE":"subscription",
        }),
        false,
        std::path::Path::new("/synthetic"),
    )
    .unwrap();
    // The original helper spreads its model override after readConfig, so
    // model-dependent defaults retain the original default-model values.
    config.ollama_model = source["ollamaModel"].as_str().unwrap().to_owned();
    assert_eq!(
        wire(&serde_json::to_value(&config).unwrap()),
        wire(source),
        "Complete evaluator config input"
    );
    assert_eq!(
        row["config_tags"],
        json!([
            {"path":"$.sessionLogDir","kind":"undefined"},
            {"path":"$.stopHookBlockCap","kind":"undefined"},
            {"path":"$.anthropicKey","kind":"undefined"},
            {"path":"$.jevKey","kind":"undefined"},
            {"path":"$.localToken","kind":"undefined"},
        ])
    );
    for field in [
        "sessionLogDir",
        "stopHookBlockCap",
        "anthropicKey",
        "jevKey",
        "localToken",
    ] {
        assert!(
            source.get(field).is_none(),
            "Undefined property must remain absent, not null"
        );
    }
    config
}

// Typed native error projection at the existing Rust embedding boundary. Every
// source-observed diagnostic field is preserved; JS prototype/stack identity is
// not claimed. The original callback receives its own unchanged Error object.
fn error_observation(error: EvaluationError) -> Value {
    let mut value = json!({"name":"Error", "message":error.to_string()});
    let mut properties = vec!["message"];
    let mut tags = Vec::new();
    match error {
        EvaluationError::InvalidResponse => {
            tags.push(json!({"path":"$.code","kind":"undefined"}));
            tags.push(json!({"path":"$.classifierStatus","kind":"undefined"}));
        }
        EvaluationError::Http {
            status,
            missing_systemone,
        } => {
            if missing_systemone {
                value["code"] = json!("OLLAMA_VERSION");
                properties.push("code");
            } else {
                tags.push(json!({"path":"$.code","kind":"undefined"}));
            }
            value["classifierStatus"] = json!(status);
            properties.push("classifierStatus");
        }
        _ => panic!("Unexpected finite evaluator error: {error}"),
    }
    json!({"ok":false, "value":value, "tags":tags, "own_properties":properties})
}

pub async fn evaluate(row: &Value, replay: &Replay, state: &str) -> Value {
    assert_eq!(row["kind"], "evaluation");
    assert_eq!(row["options"], json!({}));
    assert_eq!(
        row["option_tags"],
        json!([{"path":"$.fetchImpl","kind":"function"}])
    );
    let config = settings(row);
    let cancellation = CancellationToken::new();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        evaluate_serialized_answer(replay, &config, state, &cancellation),
    )
    .await
    .expect("Complete finite evaluator replay deadline");
    match result {
        Ok(answer) => json!({"ok":true, "value":answer, "tags":[]}),
        Err(error) => error_observation(error),
    }
}
