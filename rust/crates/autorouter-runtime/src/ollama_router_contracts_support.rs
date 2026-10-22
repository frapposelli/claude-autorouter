//! Finite request replay with independent mismatch/call/body ownership checks.
use crate::http_client::{HttpError, HttpTransport};
use autorouter_core::config::RouterConfig;
use autorouter_core::js_json::JsDocument;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const CORPUS: &str = include_str!("../../../parity/cases/ollama-router-contracts.jsonl");
const CAPTURE: &str = include_str!("../../../parity/cases/ollama-router-contracts.capture.json");
const MAX_RESPONSE: usize = 1_048_704;

pub fn cases() -> Vec<Value> {
    assert!(CORPUS.len() <= 8 * 1024 * 1024);
    let digest = format!("{:x}", Sha256::digest(CORPUS.as_bytes()));
    assert_eq!(
        digest,
        "0859b00d2f91c90d0c278eb89613f40840f6ff3e4a3240050aef975cea7930da"
    );
    let report: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(report["cases_sha256"], digest);
    assert_eq!(report["selected_definitions"], json!([4, 5, 12, 13, 14]));
    assert_eq!(report["static_assertions"], 37);
    assert_eq!(report["executed_assertions"], 194);
    assert_eq!(report["requests"], 79);
    let rows: Vec<Value> = CORPUS
        .lines()
        .map(|line| {
            assert!(line.len() <= 2 * 1024 * 1024);
            serde_json::from_str(line).unwrap()
        })
        .collect();
    assert_eq!(rows.len(), 26);
    rows
}
pub fn wire(value: &Value) -> Value {
    JsDocument::parse(value.to_string().as_bytes())
        .unwrap()
        .to_serde_observation_lossy()
}
pub fn settings(row: &Value) -> RouterConfig {
    let source = &row["config"];
    let mut config = crate::test_config(
        &json!({"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_AUTH_MODE":"subscription"}),
    );
    // Original config() spreads overrides after readConfig's model defaults.
    config.min_confidence = source["minConfidence"].as_f64().unwrap();
    config.ollama_timeout_ms = source["ollamaTimeoutMs"].as_u64().unwrap();
    config.jev_key = source["jevKey"].as_str().map(str::to_owned);
    config.anthropic_key = source["anthropicKey"].as_str().map(str::to_owned);
    assert_eq!(wire(&serde_json::to_value(&config).unwrap()), wire(source));
    let undefined: Vec<Value> = [
        "sessionLogDir",
        "stopHookBlockCap",
        "anthropicKey",
        "jevKey",
        "localToken",
    ]
    .into_iter()
    .filter(|key| source.get(*key).is_none())
    .map(|key| json!({"path":format!("$.{key}"),"kind":"undefined"}))
    .collect();
    assert_eq!(row["config_tags"], json!(undefined));
    config
}
#[allow(dead_code)] // Body-only sibling callers retain the same tracked replay implementation.
#[path = "../tests/support/tracked_http.rs"]
mod tracked;
pub type Replay = tracked::Replay<MAX_RESPONSE, true>;
