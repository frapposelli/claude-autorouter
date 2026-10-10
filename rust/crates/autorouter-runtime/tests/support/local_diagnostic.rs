//! Finite, hash-pinned diagnostic transcript. No real transport or environment read.
use autorouter_core::config::{RouterConfig, read_config};
use autorouter_core::js_json::JsDocument;
use autorouter_runtime::http_client::{HttpError, HttpTransport};
use autorouter_runtime::local_diagnostic::run_local_diagnostic_with_progress;
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

const CORPUS: &str = include_str!("../../../../parity/cases/local-diagnostic-contracts.jsonl");
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

pub fn cases() -> Vec<Value> {
    assert_eq!(
        format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
        "b4c78704f4808880492f0aeff63bb4c40ce34b4074013a0cb3fd3c8c04b53fcb"
    );
    assert!(CORPUS.len() <= 8 * 1024 * 1024);
    let rows: Vec<Value> = CORPUS
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 10);
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
        assert!(requests.len() <= 64);
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
    json!({"url":request["url"],"method":request["method"],"headers":request["headers"],"body":request["body"],"body_text":request["body_text"]})
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
            "body":if bytes.is_empty(){Value::Null}else{serde_json::from_slice::<Value>(&bytes).unwrap()},
            "body_text":std::str::from_utf8(&bytes).unwrap()});
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

// JS JSON Number equality: normalize the representational1 versus1.0
// distinction, without changing any key, string, magnitude or array position.
pub fn wire(value: &Value) -> Value {
    JsDocument::parse(value.to_string().as_bytes())
        .unwrap()
        .to_serde_observation_lossy()
}

fn settings(row: &Value) -> RouterConfig {
    let source = &row["config"];
    let mut env = json!({
        "AUTOROUTER_EVALUATOR":source["evaluator"],
        "AUTOROUTER_OLLAMA_MODEL":source["ollamaModel"],
        "AUTOROUTER_CLIENT_PROFILE":source["clientProfile"]
    });
    for (field, key) in [
        ("jevKey", "TYPESAFE_API_KEY"),
        ("anthropicKey", "ANTHROPIC_API_KEY"),
    ] {
        if let Some(value) = source.get(field) {
            env[key] = value.clone();
        }
    }
    let config = read_config(&env, false, std::path::Path::new("/synthetic")).unwrap();
    assert_eq!(
        wire(&serde_json::to_value(&config).unwrap()),
        wire(source),
        "Complete captured config must match native input"
    );
    verify_undefined(
        source,
        &row["config_tags"],
        &[
            "sessionLogDir",
            "stopHookBlockCap",
            "localToken",
            "anthropicKey",
            "jevKey",
        ],
    );
    config
}

pub fn verify_undefined(value: &Value, tags: &Value, allowed_fields: &[&str]) {
    for tag in tags.as_array().unwrap() {
        assert_eq!(tag["kind"], "undefined");
        let path = tag["path"].as_str().unwrap();
        let last = path.rsplit('.').next().unwrap();
        assert!(
            allowed_fields.contains(&last),
            "Unexpected omitted field {path}"
        );
        let pointer = format!("/{}", path.strip_prefix("$.").unwrap().replace('.', "/"));
        assert!(
            value.pointer(&pointer).is_none(),
            "Own-undefined must not become explicit null at {path}"
        );
    }
}

pub async fn execute(row: &Value, replay: Arc<Replay>) -> (Value, Vec<Value>) {
    let config = settings(row);
    assert_eq!(row["options"], json!({}));
    for tag in row["option_tags"].as_array().unwrap() {
        assert_eq!(tag["kind"], "function");
        assert!(["$.fetchImpl", "$.onProgress"].contains(&tag["path"].as_str().unwrap()));
    }
    let cancellation = CancellationToken::new();
    let mut progress = Vec::new();
    let report = tokio::time::timeout(
        Duration::from_secs(2),
        run_local_diagnostic_with_progress(replay, &config, &cancellation, |event| {
            assert!(progress.len() < 64);
            progress.push(event.clone());
            Ok(())
        }),
    )
    .await
    .expect("finite diagnostic replay deadline")
    .unwrap();
    assert!(!report.to_string().contains("PRIVATE_"));
    assert!(!json!(progress).to_string().contains("PRIVATE_"));
    (report, progress)
}

fn remove_timing(object: &mut Value, path: &str, paths: &mut Vec<String>) -> Result<(), String> {
    let value = object
        .get("latency_ms")
        .and_then(Value::as_f64)
        .ok_or("Missing/non-numeric elapsed time")?;
    if !value.is_finite() || value < 0.0 {
        return Err("Invalid elapsed time".into());
    }
    object
        .as_object_mut()
        .ok_or("Timing owner is not object")?
        .remove("latency_ms");
    paths.push(format!("{path}.latency_ms"));
    Ok(())
}

/// Only source-unasserted elapsed clock observations are excluded. Presence,
/// type and nonnegative value are mandatory; all other fields compare exactly.
pub fn comparison(report: &Value, progress: &[Value]) -> Result<(Value, Vec<String>), String> {
    let mut report = report.clone();
    let mut progress = progress.to_vec();
    let mut paths = Vec::new();
    let startup = report.get_mut("startup").ok_or("Missing startup")?;
    if !startup.is_null() {
        remove_timing(startup, "$.report.value.startup", &mut paths)?;
    }
    for (index, row) in report
        .get_mut("rows")
        .and_then(Value::as_array_mut)
        .ok_or("Missing rows")?
        .iter_mut()
        .enumerate()
    {
        remove_timing(row, &format!("$.report.value.rows[{index}]"), &mut paths)?;
    }
    for (index, event) in progress.iter_mut().enumerate() {
        if matches!(
            event["event"].as_str(),
            Some("startup_complete" | "case_complete")
        ) {
            remove_timing(event, &format!("$.progress[{index}].value"), &mut paths)?;
        }
    }
    Ok((wire(&json!({"report":report,"progress":progress})), paths))
}

pub fn expected_progress(row: &Value) -> Vec<Value> {
    row["progress"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["value"].clone())
        .collect()
}
