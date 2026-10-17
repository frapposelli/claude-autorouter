//! Exact source transcripts plus owned, segmented synthetic response bodies.
use autorouter_core::config::{
    RouterConfig, read_config, validate_ollama_endpoint, validate_ollama_model,
};
use autorouter_runtime::http_client::{HttpError, HttpTransport};
use autorouter_runtime::ollama_setup::{SetupError, SetupOptions, inspect_ollama, setup_ollama};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame};
use hyper::{Request, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const CORPUS: &str = include_str!("../../../../parity/cases/local-setup-pull-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../../parity/cases/local-setup-pull-contracts.capture.json");
pub const BOUND: Duration = Duration::from_secs(2);
pub const PULL_LIMIT: usize = 16 * 1024 * 1024;

pub fn cases() -> Vec<Value> {
    assert!(CORPUS.len() <= 8 * 1024 * 1024 && CAPTURE.len() <= 1024 * 1024);
    assert_eq!(
        format!("{:x}", Sha256::digest(CORPUS)),
        "ad424067aaa9f9ee8bb941cb5235f2a31a04289b591771ca66dd45d2920c2e42"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(CAPTURE)),
        "6d2642d6786785f068081971d5259a563c83afcf336c8ea88e37aa15fb34a8ae"
    );
    let metadata: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(metadata["selected_definitions"], json!([4, 6, 7, 13]));
    assert_eq!(metadata["counts"]["static_assertions"], 28);
    assert_eq!(metadata["counts"]["executed_assertions"], 62);
    let source = metadata["definitions"].as_array().unwrap();
    let sites: Vec<_> = source
        .iter()
        .filter(|row| row["selected"] == true)
        .flat_map(|row| row["assertions"].as_array().unwrap())
        .collect();
    assert_eq!(sites.len(), 28);
    let untaken: Vec<_> = sites
        .iter()
        .filter(|row| row["expanded_executions"] == 0)
        .collect();
    assert_eq!(untaken.len(), 1);
    assert_eq!(untaken[0]["id"], "test/ollama-setup.test.mjs#4:assert-6");
    let rows: Vec<Value> = CORPUS
        .lines()
        .map(|line| {
            assert!(line.len() <= 1024 * 1024);
            serde_json::from_str(line).unwrap()
        })
        .collect();
    assert_eq!(rows.len(), 24);
    rows
}

#[derive(Default)]
pub struct Lifetime {
    pub polled: AtomicUsize,
    pub chunks: AtomicUsize,
    pub bytes: AtomicUsize,
    pub dropped: AtomicUsize,
}
pub struct SegmentedBody {
    chunks: VecDeque<Bytes>,
    lifetime: Arc<Lifetime>,
}
impl Drop for SegmentedBody {
    fn drop(&mut self) {
        self.lifetime.dropped.fetch_add(1, Ordering::SeqCst);
    }
}
impl Body for SegmentedBody {
    type Data = Bytes;
    type Error = std::io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        self.lifetime.polled.fetch_add(1, Ordering::SeqCst);
        Poll::Ready(self.chunks.pop_front().map(|bytes| {
            self.lifetime.chunks.fetch_add(1, Ordering::SeqCst);
            self.lifetime.bytes.fetch_add(bytes.len(), Ordering::SeqCst);
            Ok(Frame::data(bytes))
        }))
    }
}
fn unhex(text: &str) -> Vec<u8> {
    assert!(text.len() <= 2 * 1024 * 1024 && text.len().is_multiple_of(2));
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |v: u8| match v {
                b'0'..=b'9' => v - b'0',
                b'a'..=b'f' => v - b'a' + 10,
                _ => panic!("invalid fixture hex"),
            };
            digit(pair[0]) * 16 + digit(pair[1])
        })
        .collect()
}
pub fn source_chunks(response: &Value) -> Vec<Bytes> {
    let body = &response["body"];
    if body["kind"] == "json" {
        let text = body["text"].as_str().unwrap();
        assert!(text.len() <= 1024 * 1024);
        vec![Bytes::copy_from_slice(text.as_bytes())]
    } else {
        assert_eq!(body["kind"], "stream");
        assert_eq!(body["starts"], 1);
        assert_eq!(body["closed"], true);
        let rows = body["chunks_hex"].as_array().unwrap();
        assert!(rows.len() <= 64);
        let chunks: Vec<_> = rows
            .iter()
            .map(|v| Bytes::from(unhex(v.as_str().unwrap())))
            .collect();
        assert_eq!(
            chunks.iter().map(Bytes::len).sum::<usize>(),
            body["total_bytes"].as_u64().unwrap() as usize
        );
        assert!(chunks.iter().map(Bytes::len).sum::<usize>() <= 1024 * 1024);
        chunks
    }
}
pub fn request_projection(row: &Value) -> Value {
    json!({"url":row["url"],"method":row["method"],"headers":row["headers"],"body":row["body"]})
}
pub struct Step {
    pub request: Value,
    pub response: Value,
    pub chunks: Vec<Bytes>,
    pub delay: Duration,
    pub lifetime: Arc<Lifetime>,
}
impl Step {
    pub fn captured(request: &Value, delayed: bool) -> Self {
        assert_eq!(request["redirect"], "error");
        assert_eq!(
            request["signal"],
            json!({"present":true,"aborted_at_request":false})
        );
        Self {
            request: request_projection(request),
            response: request["response"].clone(),
            chunks: source_chunks(&request["response"]),
            delay: if delayed && request["url"].as_str().unwrap().ends_with("/v1/systemone") {
                Duration::from_millis(10)
            } else {
                Duration::ZERO
            },
            lifetime: Arc::default(),
        }
    }
}
pub struct Replay {
    remaining: Mutex<VecDeque<Step>>,
    pub observed: Mutex<Vec<Value>>,
    pub lifetimes: Vec<Arc<Lifetime>>,
    pub invalid: AtomicUsize,
    pub delays_started: AtomicUsize,
    pub delays_finished: AtomicUsize,
}
impl Replay {
    pub fn new(steps: Vec<Step>) -> Self {
        assert!(steps.len() <= 16);
        for step in &steps {
            assert!(
                step.chunks.len() <= 4096
                    && step.chunks.iter().map(Bytes::len).sum::<usize>() <= PULL_LIMIT + 8192
            );
        }
        Self {
            lifetimes: steps.iter().map(|s| s.lifetime.clone()).collect(),
            remaining: Mutex::new(steps.into()),
            observed: Mutex::new(Vec::new()),
            invalid: AtomicUsize::new(0),
            delays_started: AtomicUsize::new(0),
            delays_finished: AtomicUsize::new(0),
        }
    }
    pub fn captured(row: &Value) -> Self {
        Self::new(
            row["requests"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| Step::captured(r, row["source_test"] == "test/ollama-setup.test.mjs#7"))
                .collect(),
        )
    }
    pub fn verified(&self) -> bool {
        self.invalid.load(Ordering::SeqCst) == 0
            && self.remaining.lock().unwrap().is_empty()
            && self
                .lifetimes
                .iter()
                .all(|l| l.dropped.load(Ordering::SeqCst) == 1)
    }
    pub fn paths(&self) -> Vec<String> {
        self.observed
            .lock()
            .unwrap()
            .iter()
            .map(|r| {
                url::Url::parse(r["url"].as_str().unwrap())
                    .unwrap()
                    .path()
                    .to_owned()
            })
            .collect()
    }
}
impl HttpTransport for Replay {
    type ResponseBody = SegmentedBody;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        assert!(bytes.len() <= 65536);
        let headers: serde_json::Map<String, Value> = parts
            .headers
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v.to_str().unwrap())))
            .collect();
        let actual = json!({"url":parts.uri.to_string(),"method":parts.method.as_str(),"headers":headers,
            "body":if bytes.is_empty(){Value::Null}else{serde_json::from_slice::<Value>(&bytes).unwrap()}});
        self.observed.lock().unwrap().push(actual.clone());
        let step = self.remaining.lock().unwrap().pop_front();
        let Some(step) = step else {
            self.invalid.fetch_add(1, Ordering::SeqCst);
            return Err(HttpError::Network);
        };
        if actual != step.request {
            self.invalid.fetch_add(1, Ordering::SeqCst);
        }
        if step.delay != Duration::ZERO {
            self.delays_started.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(step.delay).await;
            self.delays_finished.fetch_add(1, Ordering::SeqCst);
        }
        let mut response =
            Response::builder().status(step.response["status"].as_u64().unwrap() as u16);
        for (name, value) in step.response["headers"].as_object().unwrap() {
            response = response.header(name.as_str(), value.as_str().unwrap());
        }
        Ok(response
            .body(SegmentedBody {
                chunks: step.chunks.into(),
                lifetime: step.lifetime,
            })
            .unwrap())
    }
}
pub fn settings(row: &Value) -> RouterConfig {
    let source = &row["config"];
    let mut c = read_config(
        &json!({"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_OLLAMA_MODEL":source["ollamaModel"]}),
        false,
        std::path::Path::new("/synthetic"),
    )
    .unwrap();
    let keys: Vec<_> = source
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert!(keys.iter().all(|k| {
        [
            "ollamaEndpoint",
            "ollamaModel",
            "ollamaKeepAlive",
            "ollamaTimeoutMs",
            "jevKey",
            "anthropicKey",
        ]
        .contains(k)
    }));
    c.ollama_endpoint = source["ollamaEndpoint"].as_str().unwrap().into();
    c.ollama_keep_alive = source["ollamaKeepAlive"].clone();
    if let Some(ms) = source["ollamaTimeoutMs"].as_u64() {
        c.ollama_timeout_ms = ms;
    }
    c.jev_key = source["jevKey"].as_str().map(str::to_owned);
    c.anthropic_key = source["anthropicKey"].as_str().map(str::to_owned);
    c
}
fn failure(message: String, code: Option<&str>) -> Value {
    json!({"ok":false,"error":{"message":message,"code":code,"code_present":code.is_some(),"cause_present":false}})
}
fn setup_failure(error: SetupError, kind: &str) -> Value {
    assert!(std::error::Error::source(&error).is_none());
    if kind == "inspect" {
        // The original invalid endpoint throws before its operation wrapper.
        // Native inspection supplies an additional typed configuration code.
        assert_eq!(error.code, "INVALID_CONFIGURATION");
        failure(error.message, None)
    } else {
        failure(error.message, Some(error.code))
    }
}
pub fn expected(row: &Value) -> Value {
    let mut v = row["node_expected"].clone();
    if v["ok"] == false {
        assert_eq!(v["error"]["name"], "Error");
        v["error"].as_object_mut().unwrap().remove("name");
    }
    v
}
pub async fn execute(row: &Value, replay: &Replay) -> (Value, Vec<String>) {
    let kind = row["kind"].as_str().unwrap();
    let mut progress = Vec::new();
    let result = match kind {
        "endpoint" | "model" => match if kind == "endpoint" {
            validate_ollama_endpoint(row["input"].as_str().unwrap())
        } else {
            validate_ollama_model(row["input"].as_str().unwrap())
        } {
            Ok(value) => json!({"ok":true,"result":value}),
            Err(message) => failure(message, None),
        },
        "inspect" | "setup" => {
            let config = settings(row);
            let before = serde_json::to_value(&config).unwrap();
            let cancel = CancellationToken::new();
            let result = tokio::time::timeout(BOUND, async {
                if kind == "inspect" {
                    inspect_ollama(replay, &config, &cancel, 5000)
                        .await
                        .map(|v| serde_json::to_value(v).unwrap())
                } else {
                    setup_ollama(
                        replay,
                        &config,
                        &cancel,
                        &SetupOptions {
                            pull: row["options"]["pull"].as_bool().unwrap_or(false),
                            ..Default::default()
                        },
                        &mut |s: String| {
                            assert!(progress.len() < 64 && s.len() <= 65536);
                            progress.push(s);
                        },
                    )
                    .await
                    .map(|v| serde_json::to_value(v).unwrap())
                }
            })
            .await
            .expect("bounded synthetic setup must finish");
            assert_eq!(serde_json::to_value(&config).unwrap(), before);
            match result {
                Ok(value) => json!({"ok":true,"result":value}),
                Err(error) => setup_failure(error, kind),
            }
        }
        _ => panic!("unexpected contract operation"),
    };
    assert!(!result.to_string().contains("PRIVATE_") && !progress.join("\n").contains("PRIVATE_"));
    (result, progress)
}
pub fn matches(row: &Value, replay: &Replay, result: &(Value, Vec<String>)) -> bool {
    result.0 == expected(row)
        && result.1
            == row["progress"]
                .as_array()
                .map(|v| {
                    v.iter()
                        .map(|s| s.as_str().unwrap().to_owned())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        && replay.verified()
}
