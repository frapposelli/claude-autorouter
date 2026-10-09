//! Finite, hash-pinned setup transcript. No real transport or environment read.
use autorouter_core::config::{RouterConfig, read_config};
use autorouter_runtime::http_client::{HttpError, HttpTransport};
use autorouter_runtime::ollama_setup::{SetupOptions, inspect_ollama, setup_ollama};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const CORPUS: &str = include_str!("../../../../parity/cases/local-finite-contracts.jsonl");
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

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

pub struct Replay {
    remaining: Mutex<VecDeque<Value>>,
    observed: Mutex<Vec<Value>>,
    invalid: AtomicUsize,
    expected_count: usize,
}
impl Replay {
    pub fn new(requests: Vec<Value>) -> Arc<Self> {
        assert!(requests.len() <= 16);
        for request in &requests {
            assert!(
                request["response"]["body"]["text"].as_str().unwrap().len() <= MAX_RESPONSE_BYTES
            );
        }
        Arc::new(Self {
            expected_count: requests.len(),
            remaining: Mutex::new(requests.into()),
            observed: Mutex::new(Vec::new()),
            invalid: AtomicUsize::new(0),
        })
    }
    pub fn observed(&self) -> Vec<Value> {
        self.observed.lock().unwrap().clone()
    }
    pub fn verified(&self) -> bool {
        self.invalid.load(Ordering::SeqCst) == 0
            && self.remaining.lock().unwrap().is_empty()
            && self.observed.lock().unwrap().len() == self.expected_count
    }
}
pub fn request_observation(request: &Value) -> Value {
    json!({"url":request["url"],"method":request["method"],"headers":request["headers"],"body":request["body"]})
}
impl HttpTransport for Replay {
    type ResponseBody = Full<Bytes>;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        let expected = self.remaining.lock().unwrap().pop_front();
        let Some(expected) = expected else {
            self.invalid.fetch_add(1, Ordering::SeqCst);
            return Err(HttpError::Network);
        };
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        assert!(bytes.len() <= MAX_REQUEST_BYTES, "synthetic request bound");
        let headers: serde_json::Map<String, Value> = parts
            .headers
            .iter()
            .map(|(key, value)| (key.to_string(), json!(value.to_str().unwrap())))
            .collect();
        let observed = json!({"url":parts.uri.to_string(),"method":parts.method.as_str(),"headers":headers,
            "body":if bytes.is_empty(){Value::Null}else{serde_json::from_slice::<Value>(&bytes).unwrap()}});
        if observed != request_observation(&expected) {
            self.invalid.fetch_add(1, Ordering::SeqCst);
        }
        self.observed.lock().unwrap().push(observed);
        // Return the scripted response even for a mismatched request: the
        // independent invalid counter must reject otherwise matching errors.
        let response = &expected["response"];
        let mut builder = Response::builder().status(response["status"].as_u64().unwrap() as u16);
        for (key, value) in response["headers"].as_object().unwrap() {
            builder = builder.header(key.as_str(), value.as_str().unwrap());
        }
        Ok(builder
            .body(Full::new(Bytes::from(
                response["body"]["text"].as_str().unwrap().to_owned(),
            )))
            .unwrap())
    }
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
