//! Separate timed replay: deadline errors come only from the actual evaluator.
use autorouter_core::config::{RouterConfig, read_config};
use autorouter_core::js_json::JsDocument;
use autorouter_runtime::http_client::{HttpError, HttpTransport};
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
use tokio::sync::Notify;
use tokio::time::Instant;

pub const CORPUS: &str =
    include_str!("../../../../parity/cases/ollama-routing-timeout-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../../parity/cases/ollama-routing-timeout-contracts.capture.json");
const PIN: &str = "fd5cdab37c2d559a1a3fe9242ddf161bc2c58cc4c4de7860c0cde056c26743c3";
pub fn validate(bytes: &str, capture: &Value) -> Result<Value, &'static str> {
    if bytes.len() > 1024 * 1024 {
        return Err("corpus byte bound");
    }
    let digest = format!("{:x}", Sha256::digest(bytes.as_bytes()));
    if digest != PIN || capture["cases_sha256"] != digest {
        return Err("immutable corpus identity");
    }
    if bytes.lines().count() != 1 || bytes.len() > 524288 {
        return Err("one bounded case");
    }
    if capture["selected_definitions"] != json!([3])
        || capture["static_assertions"] != 12
        || capture["helper_static_assertions"] != 9
        || capture["direct_executed_assertions"] != 12
        || capture["helper_executed_assertions"] != 53
        || capture["executed_assertions"].as_array().map(Vec::len) != Some(65)
        || capture["routers"] != 1
        || capture["routes"] != 4
        || capture["requests"] != 8
        || capture["timeout_outcomes"] != 1
    {
        return Err("capture inventory");
    }
    let row: Value = serde_json::from_str(bytes).map_err(|_| "case JSON")?;
    if row["id"] != "baseline-ollama-routing-timeout-3-1"
        || row["source_test"] != "test/ollama-routing.test.mjs#3"
        || row["steps"].as_array().map(Vec::len) != Some(4)
        || row["config"]["ollamaTimeoutMs"] != 5
    {
        return Err("exact timed schedule");
    }
    Ok(row)
}
pub fn captured() -> (Value, Value) {
    let capture = serde_json::from_str(CAPTURE).unwrap();
    (validate(CORPUS, &capture).unwrap(), capture)
}
fn wire(value: &Value) -> Value {
    JsDocument::parse(value.to_string().as_bytes())
        .unwrap()
        .to_serde_observation_lossy()
}
pub fn settings(row: &Value) -> RouterConfig {
    let mut config = read_config(
        &json!({"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_AUTH_MODE":"subscription",
            "AUTOROUTER_OLLAMA_MODEL":row["config"]["ollamaModel"]}),
        false,
        std::path::Path::new("/synthetic"),
    )
    .unwrap();
    // Original callback spreads the model-derived config before overriding this.
    assert_eq!(row["config"]["ollamaTimeoutMs"], 5);
    config.ollama_timeout_ms = 5;
    assert_eq!(
        wire(&serde_json::to_value(&config).unwrap()),
        wire(&row["config"])
    );
    assert_eq!(
        row["config_tags"],
        json!([
            {"path":"$.sessionLogDir","kind":"undefined"},
            {"path":"$.stopHookBlockCap","kind":"undefined"},
            {"path":"$.anthropicKey","kind":"undefined"},
            {"path":"$.jevKey","kind":"undefined"},
            {"path":"$.localToken","kind":"undefined"}
        ])
    );
    config
}

#[derive(Default)]
struct Queue {
    remaining: VecDeque<Value>,
    observed: usize,
    expected: usize,
    first_request_at: Option<Instant>,
    pending_at: Option<Instant>,
}
#[derive(Default)]
pub struct Replay {
    queue: Mutex<Queue>,
    invalid: AtomicUsize,
    calls: AtomicUsize,
    active: AtomicUsize,
    bodies: Arc<AtomicUsize>,
    pending_started: AtomicUsize,
    pending_dropped: AtomicUsize,
    entered: Notify,
}
struct Active<'a> {
    owner: &'a Replay,
    pending: bool,
}
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.owner.active.fetch_sub(1, Ordering::SeqCst);
        if self.pending {
            self.owner.pending_dropped.fetch_add(1, Ordering::SeqCst);
        }
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
    pub fn set(&self, step: &Value) {
        assert!(self.idle());
        let requests = step["requests"].as_array().unwrap();
        assert_eq!(requests.len(), 2);
        for request in requests {
            let outcome = &request["outcome"];
            let mut options = json!({"redirect":"error","signal_kind":"AbortSignal","signal_aborted_before":false,"signal_aborted_after":false});
            if outcome["kind"] == "error" {
                assert_eq!(outcome["name"], "TimeoutError");
                options["signal_aborted_after"] = json!(true);
                options["signal_reason_name"] = json!("TimeoutError");
                options["same_error_reason"] = json!(true);
            } else {
                assert_eq!(outcome["kind"], "response");
                assert_eq!(outcome["input_tags"], json!([]));
                assert!(outcome["body"].as_str().unwrap().len() <= 65536);
            }
            assert_eq!(request["node_options"], options);
            assert_eq!(
                serde_json::from_str::<Value>(request["body"].as_str().unwrap()).unwrap(),
                request["parsed_body"]
            );
        }
        let mut queue = self.queue.lock().unwrap();
        assert!(queue.remaining.is_empty());
        *queue = Queue {
            remaining: requests.clone().into(),
            expected: requests.len(),
            ..Default::default()
        };
    }
    pub fn count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    pub fn verified(&self) -> bool {
        let queue = self.queue.lock().unwrap();
        self.invalid.load(Ordering::SeqCst) == 0
            && queue.remaining.is_empty()
            && queue.observed == queue.expected
            && self.idle()
    }
    pub fn idle(&self) -> bool {
        self.active.load(Ordering::SeqCst) == 0 && self.bodies.load(Ordering::SeqCst) == 0
    }
    pub fn pending_counts(&self) -> (usize, usize, usize) {
        (
            self.pending_started.load(Ordering::SeqCst),
            self.pending_dropped.load(Ordering::SeqCst),
            self.active.load(Ordering::SeqCst),
        )
    }
    pub async fn wait_pending(&self) {
        loop {
            let entered = self.entered.notified();
            tokio::pin!(entered);
            entered.as_mut().enable();
            if self.queue.lock().unwrap().pending_at.is_some() {
                return;
            }
            entered.await;
        }
    }
    pub fn clock_barrier(&self) -> Instant {
        let queue = self.queue.lock().unwrap();
        let start = queue.first_request_at.unwrap();
        assert_eq!(queue.pending_at, Some(start));
        assert_eq!(
            Instant::now(),
            start,
            "No automatic clock advance before barrier"
        );
        start
    }
}
impl HttpTransport for Replay {
    type ResponseBody = FiniteBody;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.active.fetch_add(1, Ordering::SeqCst);
        let mut active = Active {
            owner: self,
            pending: false,
        };
        let expected = {
            let mut queue = self.queue.lock().unwrap();
            queue.first_request_at.get_or_insert_with(Instant::now);
            queue.remaining.pop_front()
        };
        let Some(expected) = expected else {
            self.invalid.fetch_add(1, Ordering::SeqCst);
            return Err(HttpError::Network);
        };
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        assert!(bytes.len() <= 65536);
        let headers: serde_json::Map<String, Value> = parts
            .headers
            .iter()
            .map(|(key, value)| (key.to_string(), json!(value.to_str().unwrap())))
            .collect();
        let observed = json!({"url":parts.uri.to_string(),"method":parts.method.as_str(),"headers":headers,
            "body":std::str::from_utf8(&bytes).unwrap(),"parsed_body":serde_json::from_slice::<Value>(&bytes).unwrap()});
        let required = json!({"url":expected["url"],"method":expected["method"],"headers":expected["headers"],
            "body":expected["body"],"parsed_body":expected["parsed_body"]});
        if observed != required {
            self.invalid.fetch_add(1, Ordering::SeqCst);
        }
        assert!(parts.headers.get("authorization").is_none());
        assert!(parts.headers.get("x-api-key").is_none());
        self.queue.lock().unwrap().observed += 1;
        let outcome = &expected["outcome"];
        if outcome["kind"] == "error" {
            active.pending = true;
            self.queue.lock().unwrap().pending_at = Some(Instant::now());
            self.pending_started.fetch_add(1, Ordering::SeqCst);
            self.entered.notify_waiters();
            // Do not return a synthetic Timeout: production owns the deadline.
            return std::future::pending().await;
        }
        let mut builder = Response::builder().status(outcome["status"].as_u64().unwrap() as u16);
        for (key, value) in outcome["headers"].as_object().unwrap() {
            builder = builder.header(key.as_str(), value.as_str().unwrap());
        }
        self.bodies.fetch_add(1, Ordering::SeqCst);
        Ok(builder
            .body(FiniteBody {
                inner: Full::new(Bytes::from(outcome["body"].as_str().unwrap().to_owned())),
                owners: self.bodies.clone(),
            })
            .unwrap())
    }
}
