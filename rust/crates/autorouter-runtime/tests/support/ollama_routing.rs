//! Finite request replay with independent mismatch/call/body ownership checks.
use autorouter_core::config::{RouterConfig, read_config};
use autorouter_core::js_json::JsDocument;
use autorouter_runtime::http_client::{HttpError, HttpTransport};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const CORPUS: &str = include_str!("../../../../parity/cases/ollama-routing-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../../parity/cases/ollama-routing-contracts.capture.json");
const PIN: &str = "88ea9d384ae10c4106c3a5843a72bdd65f66faf4312002b8b4a5864b36abf12a";
const MAX_RESPONSE: usize = 64 * 1024;

pub fn validate_corpus(corpus: &str, capture: &Value) -> Result<Vec<Value>, &'static str> {
    if corpus.len() > 8 * 1024 * 1024 {
        return Err("Corpus byte bound");
    }
    let digest = format!("{:x}", Sha256::digest(corpus.as_bytes()));
    if digest != PIN || capture["cases_sha256"] != digest {
        return Err("Canonical corpus identity");
    }
    if capture["selected_definitions"] != json!([1, 2, 4])
        || capture["static_assertions"] != 12
        || capture["helper_static_assertions"] != 9
        || capture["direct_executed_assertions"] != 138
        || capture["helper_executed_assertions"] != 406
        || capture["requests"] != 58
    {
        return Err("Capture inventory");
    }
    let rows: Vec<Value> = corpus
        .lines()
        .map(|line| {
            if line.len() > 256 * 1024 {
                return Err("Case byte bound");
            }
            serde_json::from_str(line).map_err(|_| "Case JSON")
        })
        .collect::<Result<_, _>>()?;
    let expected: Vec<String> = (1..=24)
        .map(|n| format!("baseline-ollama-routing-1-{n}"))
        .chain([
            "baseline-ollama-routing-2-1".into(),
            "baseline-ollama-routing-4-1".into(),
        ])
        .collect();
    if rows.len() != 26 {
        return Err("Case count");
    }
    let captured: Vec<&str> = capture["tests"]
        .as_array()
        .ok_or("Capture definitions")?
        .iter()
        .flat_map(|test| test["case_ids"].as_array().into_iter().flatten())
        .map(|id| id.as_str().ok_or("Case ID"))
        .collect::<Result<_, _>>()?;
    if captured != expected {
        return Err("Capture IDs");
    }
    for (index, (row, id)) in rows.iter().zip(&expected).enumerate() {
        let number = if index < 24 {
            1
        } else if index == 24 {
            2
        } else {
            4
        };
        if row["id"] != *id
            || row["source_test"] != format!("test/ollama-routing.test.mjs#{number}")
            || row["kind"] != "router"
            || row["steps"].as_array().map(Vec::len)
                != Some(if number == 2 {
                    3
                } else if number == 4 {
                    2
                } else {
                    1
                })
        {
            return Err("Per-instance schedule");
        }
    }
    Ok(rows)
}
pub fn corpus() -> (&'static str, Value) {
    (CORPUS, serde_json::from_str(CAPTURE).unwrap())
}
pub fn cases() -> Vec<Value> {
    let (text, report) = corpus();
    validate_corpus(text, &report).unwrap()
}
pub fn wire(value: &Value) -> Value {
    JsDocument::parse(value.to_string().as_bytes())
        .unwrap()
        .to_serde_observation_lossy()
}
pub fn settings(row: &Value) -> RouterConfig {
    let source = &row["config"];
    let config = read_config(
        &json!({"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_AUTH_MODE":"subscription",
            "AUTOROUTER_OLLAMA_MODEL":source["ollamaModel"]}),
        false,
        std::path::Path::new("/synthetic"),
    )
    .unwrap();
    // Original configFor passes model into readConfig, preserving model defaults.
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
#[path = "tracked_http.rs"]
mod tracked;
pub type Replay = tracked::Replay<MAX_RESPONSE, false>;
