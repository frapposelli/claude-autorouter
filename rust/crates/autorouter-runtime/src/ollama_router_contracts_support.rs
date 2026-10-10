//! Finite request replay with independent mismatch/call/body ownership checks.
use crate::http_client::{HttpError, HttpTransport};
use autorouter_core::config::{RouterConfig, read_config};
use autorouter_core::js_json::JsDocument;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame, SizeHint};
use hyper::{Request, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::task::{Context, Poll};

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
    let mut config = read_config(
        &json!({"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_AUTH_MODE":"subscription"}),
        false,
        std::path::Path::new("/synthetic"),
    )
    .unwrap();
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
#[derive(Default)]
struct Queue {
    remaining: VecDeque<Value>,
    observed: Vec<Value>,
    expected: usize,
}
#[derive(Default)]
pub struct Replay {
    queue: Mutex<Queue>,
    invalid: AtomicUsize,
    calls: AtomicUsize,
    active: Arc<AtomicUsize>,
    bodies: Arc<AtomicUsize>,
}
struct Active(Arc<AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
pub struct FiniteBody {
    inner: Full<Bytes>,
    owners: Arc<AtomicUsize>,
}
impl Drop for FiniteBody {
    fn drop(&mut self) {
        self.owners.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Body for FiniteBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        Pin::new(&mut self.get_mut().inner).poll_frame(cx)
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
impl Replay {
    pub fn set(&self, requests: Vec<Value>) {
        assert!(requests.len() <= 2);
        assert_eq!(self.active.load(Ordering::SeqCst), 0);
        assert_eq!(self.bodies.load(Ordering::SeqCst), 0);
        for request in &requests {
            assert_eq!(
                request["node_options"],
                json!({"redirect":"error","signal_kind":"AbortSignal","signal_aborted_before":false,"omitted_fields":[],"signal_aborted_after":false})
            );
            if let Some(response) = request.get("response") {
                assert!(response["body"]["text"].as_str().unwrap().len() <= MAX_RESPONSE);
                assert_eq!(response["json_input_tags"], json!([]));
            } else {
                assert_eq!(
                    request["error"],
                    json!({"name":"Error","message":"private upstream detail","own_properties":["message"]})
                );
            }
        }
        let mut queue = self.queue.lock().unwrap();
        assert!(queue.remaining.is_empty());
        *queue = Queue {
            expected: requests.len(),
            remaining: requests.into(),
            observed: Vec::new(),
        };
    }
    pub fn count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    pub fn verified(&self) -> bool {
        let queue = self.queue.lock().unwrap();
        self.invalid.load(Ordering::SeqCst) == 0
            && queue.remaining.is_empty()
            && queue.observed.len() == queue.expected
            && self.active.load(Ordering::SeqCst) == 0
            && self.bodies.load(Ordering::SeqCst) == 0
    }
    pub fn idle(&self) -> bool {
        self.active.load(Ordering::SeqCst) == 0 && self.bodies.load(Ordering::SeqCst) == 0
    }
}
fn request_value(request: &Value) -> Value {
    json!({"url":request["url"],"method":request["method"],"headers":request["headers"],"body":request["body"],"body_text":request["body_text"]})
}
impl HttpTransport for Replay {
    type ResponseBody = FiniteBody;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.active.fetch_add(1, Ordering::SeqCst);
        let _active = Active(self.active.clone());
        let expected = self.queue.lock().unwrap().remaining.pop_front();
        let Some(expected) = expected else {
            self.invalid.fetch_add(1, Ordering::SeqCst);
            return Err(HttpError::Network);
        };
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        assert!(bytes.len() <= 64 * 1024);
        let headers: serde_json::Map<String, Value> = parts
            .headers
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v.to_str().unwrap())))
            .collect();
        let observed = json!({"url":parts.uri.to_string(),"method":parts.method.as_str(),"headers":headers,"body":serde_json::from_slice::<Value>(&bytes).unwrap(),"body_text":std::str::from_utf8(&bytes).unwrap()});
        if observed != request_value(&expected) {
            self.invalid.fetch_add(1, Ordering::SeqCst);
        }
        self.queue.lock().unwrap().observed.push(observed);
        // Return the original scripted outcome even when request comparison fails.
        // This independent flag prevents a matching safe fallback hiding bad I/O.
        if expected.get("error").is_some() {
            return Err(HttpError::Network);
        }
        let response = &expected["response"];
        let mut builder = Response::builder().status(response["status"].as_u64().unwrap() as u16);
        for (k, v) in response["headers"].as_object().unwrap() {
            builder = builder.header(k.as_str(), v.as_str().unwrap());
        }
        self.bodies.fetch_add(1, Ordering::SeqCst);
        Ok(builder
            .body(FiniteBody {
                inner: Full::new(Bytes::from(
                    response["body"]["text"].as_str().unwrap().to_owned(),
                )),
                owners: self.bodies.clone(),
            })
            .unwrap())
    }
}
