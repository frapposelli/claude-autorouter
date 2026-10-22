//! Captured I/O replay with production timers and separately counted owners.
use autorouter_core::config::{RouterConfig, read_config};
use autorouter_core::js_json::JsDocument;
use autorouter_runtime::http_client::{HttpError, HttpTransport};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame};
use hyper::{Request, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::{Instant, Sleep};

pub const CORPUS: &str =
    include_str!("../../../../parity/cases/ollama-evaluator-timed-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../../parity/cases/ollama-evaluator-timed-contracts.capture.json");
const PIN: &str = "76ab47e2e96be3374be2c8d5e981f59d5fa01cd20765caec08bdafac1b7a4c3f";
pub fn cases() -> Vec<Value> {
    assert!(CORPUS.len() < 524288 && CAPTURE.len() < 1048576);
    assert_eq!(
        format!("{:x}", Sha256::digest(CAPTURE.as_bytes())),
        "dd221a7bc3c57544a3c658ac84fb4e22fc710830d8697ba92219120c17e5dc58"
    );
    let capture: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(format!("{:x}", Sha256::digest(CORPUS.as_bytes())), PIN);
    assert_eq!(capture["cases_sha256"], PIN);
    assert_eq!(capture["selected_definitions"], json!([7, 8, 9, 10, 15]));
    assert_eq!(capture["static_assertions"], 20);
    assert_eq!(capture["expanded_assertions"], 36);
    assert_eq!(capture["request_count"], 26);
    let rows: Vec<Value> = CORPUS
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 16);
    rows
}
fn ordinary(value: &Value) -> Value {
    let mut result = value.clone();
    if let Some(map) = result.as_object_mut() {
        map.retain(|_, item| item != &json!({"$js_type":"undefined"}));
        for item in map.values_mut() {
            *item = ordinary(item);
        }
    } else if let Some(items) = result.as_array_mut() {
        for item in items {
            *item = ordinary(item);
        }
    }
    result
}
fn wire(value: &Value) -> Value {
    JsDocument::parse(value.to_string().as_bytes())
        .unwrap()
        .to_serde_observation_lossy()
}
pub fn config(row: &Value) -> RouterConfig {
    let mut config = read_config(
        &json!({"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_AUTH_MODE":"subscription"}),
        false,
        std::path::Path::new("/synthetic"),
    )
    .unwrap();
    config.ollama_timeout_ms = row["config"]["ollamaTimeoutMs"].as_u64().unwrap();
    assert_eq!(
        wire(&serde_json::to_value(&config).unwrap()),
        wire(&ordinary(&row["config"]))
    );
    config
}
pub fn projected(mut output: Value) -> Value {
    if let Some(map) = output.as_object_mut() {
        for key in ["latency_ms", "evaluation_latency_ms"] {
            if let Some(value) = map.remove(key) {
                assert!(
                    value
                        .as_f64()
                        .is_some_and(|value| value.is_finite() && value >= 0.0)
                );
            }
        }
    }
    wire(&output)
}
pub fn matches(output: &Value, row: &Value) -> bool {
    if row["kind"] == "route"
        && ["latency_ms", "evaluation_latency_ms"]
            .into_iter()
            .any(|field| {
                !output[field]
                    .as_f64()
                    .is_some_and(|value| value.is_finite() && value >= 0.0)
                    || !row["output"][field]
                        .as_f64()
                        .is_some_and(|value| value.is_finite() && value >= 0.0)
            })
    {
        return false;
    }
    projected(output.clone()) == projected(row["output"].clone())
}
fn decode(text: &str) -> Vec<u8> {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut value = 0u32;
    let mut bits = 0;
    let mut bytes = Vec::new();
    assert!(text.len() < 180000);
    for ch in text.bytes().take_while(|ch| *ch != b'=') {
        value = (value << 6)
            | u32::try_from(alphabet.iter().position(|item| *item == ch).unwrap()).unwrap();
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((value >> bits) as u8);
            value &= (1 << bits) - 1;
        }
    }
    assert!(bytes.len() <= 65537);
    bytes
}
#[derive(Clone)]
struct Script {
    expected: Value,
    headers_ms: u64,
    body_ms: u64,
    bytes: Option<Bytes>,
    never_end: bool,
    response: Value,
}
#[derive(Default)]
pub struct Owners {
    pub requests: AtomicUsize,
    pub request_live: AtomicUsize,
    pub request_dropped: AtomicUsize,
    pub bodies: AtomicUsize,
    pub body_dropped: AtomicUsize,
    pub pending_reads: AtomicUsize,
    changed: Notify,
}
pub struct Replay {
    queue: Mutex<VecDeque<Script>>,
    invalid: AtomicUsize,
    start: Mutex<Option<Instant>>,
    pub owners: Arc<Owners>,
}
struct RequestOwner(Arc<Owners>);
impl Drop for RequestOwner {
    fn drop(&mut self) {
        self.0.request_live.fetch_sub(1, Ordering::SeqCst);
        self.0.request_dropped.fetch_add(1, Ordering::SeqCst);
        self.0.changed.notify_waiters();
    }
}
pub struct RecordedBody {
    bytes: Option<Bytes>,
    never_end: bool,
    sleep: Option<Pin<Box<Sleep>>>,
    observed_pending: bool,
    owners: Arc<Owners>,
}
impl Drop for RecordedBody {
    fn drop(&mut self) {
        self.owners.bodies.fetch_sub(1, Ordering::SeqCst);
        self.owners.body_dropped.fetch_add(1, Ordering::SeqCst);
        self.owners.changed.notify_waiters();
    }
}
impl Body for RecordedBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let this = self.get_mut();
        let delayed = this
            .sleep
            .as_mut()
            .is_some_and(|sleep| sleep.as_mut().poll(cx).is_pending());
        if delayed || (this.bytes.is_none() && this.never_end) {
            if !this.observed_pending {
                this.observed_pending = true;
                this.owners.pending_reads.fetch_add(1, Ordering::SeqCst);
                this.owners.changed.notify_waiters();
            }
            return Poll::Pending;
        }
        this.sleep = None;
        Poll::Ready(this.bytes.take().map(|bytes| Ok(Frame::data(bytes))))
    }
    fn is_end_stream(&self) -> bool {
        self.bytes.is_none() && !self.never_end
    }
}
impl Replay {
    pub fn new(rows: &[Value]) -> Self {
        let capture: Value = serde_json::from_str(CAPTURE).unwrap();
        let all = cases();
        let successful_inference = all[3]["requests"][1]["response"].clone();
        let mut queue = VecDeque::new();
        for row in rows {
            let definition = row["source_test"].as_str().unwrap();
            for (index, expected) in row["requests"].as_array().unwrap().iter().enumerate() {
                let headers_ms = if definition.ends_with("#8") {
                    15
                } else if definition.ends_with("#7")
                    && row["requests"].as_array().unwrap().len() == 2
                    && index == 0
                {
                    10
                } else {
                    0
                };
                // A broken positive deadline receives the original valid delayed
                // answer, never an injected timeout error or a forever wait.
                let response = if expected.get("error").is_some() {
                    assert!(definition.ends_with("#8") && index == 1);
                    successful_inference.clone()
                } else {
                    expected["response"].clone()
                };
                let (bytes, never_end, body_ms) =
                    if let Some(text) = response["body"]["text"].as_str() {
                        (Some(Bytes::copy_from_slice(text.as_bytes())), false, 0)
                    } else {
                        let stream = response["body"]["stream"].as_str().unwrap();
                        let events = capture["raw"]["events"].as_array().unwrap();
                        let enqueued: Vec<_> = events
                            .iter()
                            .filter(|event| {
                                event["stream"] == stream && event["kind"] == "stream_enqueue"
                            })
                            .collect();
                        assert!(enqueued.len() <= 1);
                        let bytes = enqueued.first().map(|event| {
                            let bytes = decode(event["base64"].as_str().unwrap());
                            assert_eq!(bytes.len() as u64, event["bytes"].as_u64().unwrap());
                            Bytes::from(bytes)
                        });
                        let closed = events.iter().any(|event| {
                            event["stream"] == stream && event["kind"] == "stream_close"
                        });
                        (
                            bytes,
                            !closed,
                            if definition.ends_with("#8") { 30 } else { 0 },
                        )
                    };
                queue.push_back(Script {
                    expected: expected.clone(),
                    headers_ms,
                    body_ms,
                    bytes,
                    never_end,
                    response,
                });
            }
        }
        Self {
            queue: Mutex::new(queue),
            invalid: AtomicUsize::new(0),
            start: Mutex::new(None),
            owners: Arc::new(Owners::default()),
        }
    }
    pub fn clock(&self) -> Instant {
        self.start.lock().unwrap().unwrap()
    }
    pub fn calls(&self) -> usize {
        self.owners.requests.load(Ordering::SeqCst)
    }
    pub fn idle(&self) -> bool {
        self.owners.request_live.load(Ordering::SeqCst) == 0
            && self.owners.bodies.load(Ordering::SeqCst) == 0
    }
    pub fn verified(&self) -> bool {
        self.invalid.load(Ordering::SeqCst) == 0
            && self.queue.lock().unwrap().is_empty()
            && self.idle()
    }
    pub async fn wait_calls(&self, calls: usize) {
        self.wait_for(|| self.calls() >= calls).await;
    }
    pub async fn wait_pending(&self, count: usize) {
        self.wait_for(|| self.owners.pending_reads.load(Ordering::SeqCst) >= count)
            .await;
    }
    async fn wait_for(&self, predicate: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let changed = self.owners.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if predicate() {
                    break;
                }
                changed.await;
            }
        })
        .await
        .expect("positive replay barrier");
    }
}
impl HttpTransport for Replay {
    type ResponseBody = RecordedBody;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<RecordedBody>, HttpError> {
        self.owners.request_live.fetch_add(1, Ordering::SeqCst);
        let _owner = RequestOwner(self.owners.clone());
        let script = self.queue.lock().unwrap().pop_front();
        let Some(script) = script else {
            self.invalid.fetch_add(1, Ordering::SeqCst);
            return Err(HttpError::Network);
        };
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        let headers: serde_json::Map<_, _> = parts
            .headers
            .iter()
            .map(|(key, value)| (key.to_string(), json!(value.to_str().unwrap())))
            .collect();
        let observed = json!({"url":parts.uri.to_string(),"method":parts.method.as_str(),"headers":headers,"parsed_body":serde_json::from_slice::<Value>(&bytes).unwrap()});
        let expected = json!({"url":script.expected["url"],"method":script.expected["options"]["method"],"headers":script.expected["options"]["headers"],"parsed_body":script.expected["parsed_body"]});
        if wire(&observed) != wire(&expected) {
            self.invalid.fetch_add(1, Ordering::SeqCst);
        }
        assert_eq!(script.expected["options"]["redirect"], "error");
        assert_eq!(script.expected["signal_aborted_before"], false);
        assert!(bytes.len() <= 65536);
        self.start.lock().unwrap().get_or_insert_with(Instant::now);
        self.owners.requests.fetch_add(1, Ordering::SeqCst);
        self.owners.changed.notify_waiters();
        if script.headers_ms != 0 {
            tokio::time::sleep(Duration::from_millis(script.headers_ms)).await;
        }
        let mut builder =
            Response::builder().status(script.response["status"].as_u64().unwrap() as u16);
        for pair in script.response["headers"].as_array().unwrap() {
            builder = builder.header(pair[0].as_str().unwrap(), pair[1].as_str().unwrap());
        }
        self.owners.bodies.fetch_add(1, Ordering::SeqCst);
        Ok(builder
            .body(RecordedBody {
                bytes: script.bytes,
                never_end: script.never_end,
                sleep: (script.body_ms != 0)
                    .then(|| Box::pin(tokio::time::sleep(Duration::from_millis(script.body_ms)))),
                observed_pending: false,
                owners: self.owners.clone(),
            })
            .unwrap())
    }
}
