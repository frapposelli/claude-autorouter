//! Finite, hash-pinned setup transcript. No real transport or environment read.
use autorouter_core::config::{RouterConfig, read_config};
use autorouter_runtime::ollama_setup::{SetupOptions, inspect_ollama, setup_ollama};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const CORPUS: &str = include_str!("../../../../parity/cases/local-finite-contracts.jsonl");

pub fn cases() -> Vec<Value> {
    assert_eq!(
        format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
        "f2071490be6f926e750d95f863f728fc729129357848a846435662b6fadc35a5"
    );
    assert!(CORPUS.len() <= 8 * 1024 * 1024);
    let rows: Vec<Value> = CORPUS
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 24);
    rows
}

#[path = "finite_http.rs"]
mod finite_http;
pub type Replay = finite_http::Replay<16, false>;
pub fn request_observation(request: &Value) -> Value {
    finite_http::request_observation::<false>(request)
}

fn settings(row: &Value) -> RouterConfig {
    let source = row["config"].as_object().unwrap();
    assert_eq!(source.len(), 3);
    assert!(row["config_tags"].as_array().unwrap().is_empty());
    let mut config = read_config(
        &json!({"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_OLLAMA_MODEL":source["ollamaModel"]}),
        false,
        std::path::Path::new("/synthetic"),
    )
    .unwrap();
    config.ollama_endpoint = source["ollamaEndpoint"].as_str().unwrap().into();
    config.ollama_keep_alive = source["ollamaKeepAlive"].clone();
    config
}

pub async fn execute(row: &Value, replay: &Replay) -> (Value, Vec<String>) {
    let config = settings(row);
    assert!(
        row["options"]
            .as_object()
            .unwrap()
            .keys()
            .all(|k| k == "pull")
    );
    for tag in row["option_tags"].as_array().unwrap() {
        assert_eq!(tag["kind"], "function");
        assert!(["$.fetchImpl", "$.write"].contains(&tag["path"].as_str().unwrap()));
    }
    let cancellation = CancellationToken::new();
    let mut progress = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        match row["kind"].as_str().unwrap() {
            "inspect" => inspect_ollama(replay, &config, &cancellation, 5000)
                .await
                .map(|value| serde_json::to_value(value).unwrap()),
            "setup" => setup_ollama(
                replay,
                &config,
                &cancellation,
                &SetupOptions {
                    pull: row["options"]["pull"] == true,
                    ..Default::default()
                },
                &mut |line| progress.push(line),
            )
            .await
            .map(|value| serde_json::to_value(value).unwrap()),
            _ => panic!("unexpected finite operation"),
        }
    })
    .await
    .expect("finite setup replay exceeded fixture deadline");
    let observed = match result {
        Ok(result) => json!({"ok":true,"result":result}),
        Err(error) => {
            assert!(std::error::Error::source(&error).is_none());
            json!({"ok":false,"error":{"code":error.code,"message":error.message,"has_cause":false}})
        }
    };
    assert!(!observed.to_string().contains("PRIVATE_"));
    assert!(!progress.join("\n").contains("PRIVATE_"));
    (observed, progress)
}
