//! Finite original diagnostic transcript replay; no network or injected verdicts.
use autorouter_core::config::{Evaluator, RouterConfig, read_config};
use autorouter_core::js_json::JsDocument;
use autorouter_runtime::http_client::{HttpError, HttpTransport};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame};
use hyper::{Request, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio_util::sync::CancellationToken;

const CORPUS: &str =
    include_str!("../../../../parity/cases/local-diagnostic-boundary-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../../parity/cases/local-diagnostic-boundary-contracts.capture.json");
pub fn captured() -> Vec<Value> {
    assert!(CORPUS.len() < 262144 && CAPTURE.len() < 524288);
    let pin = "e1c1a0da3844383e3a0a965135ba30583a89a1048bd540c44ebb967995481da6";
    assert_eq!(format!("{:x}", Sha256::digest(CORPUS)), pin);
    assert_eq!(
        format!("{:x}", Sha256::digest(CAPTURE)),
        "e9bb29cb50f70124c39ee8e4d908fd1e6a38e627870f6eb507318c77957fd072"
    );
    let capture: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(capture["cases_sha256"], pin);
    assert_eq!(capture["selected_definitions"], json!([6, 12, 13, 14]));
    assert_eq!(capture["static_assertions"], 12);
    assert_eq!(capture["helper_static_assertions"], 11);
    assert_eq!(capture["expanded_assertions"], 333);
    assert_eq!(capture["request_count"], 61);
    assert_eq!(capture["operation_count"], 10);
    let rows: Vec<Value> = CORPUS
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 10);
    assert_eq!(
        rows.iter()
            .map(|row| row["requests"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [0, 0, 0, 0, 0, 0, 9, 4, 24, 24]
    );
    rows
}
fn ordinary(value: &Value) -> Value {
    let mut value = value.clone();
    if let Some(map) = value.as_object_mut() {
        map.retain(|key, item| {
            if item == &json!({"$js_type":"undefined"}) {
                assert!(
                    [
                        "sessionLogDir",
                        "stopHookBlockCap",
                        "localToken",
                        "anthropicKey",
                        "jevKey",
                        "classified_tier",
                        "classifier_error",
                        "classifier_status"
                    ]
                    .contains(&key.as_str())
                );
                false
            } else {
                true
            }
        });
        assert!(
            !map.contains_key("$js_type"),
            "non-JSON value requires explicit adaptation"
        );
        for item in map.values_mut() {
            *item = ordinary(item);
        }
    } else if let Some(items) = value.as_array_mut() {
        for item in items {
            *item = ordinary(item);
        }
    }
    value
}
fn wire(value: &Value) -> Value {
    JsDocument::parse(value.to_string().as_bytes())
        .unwrap()
        .to_serde_observation_lossy()
}
pub fn settings(row: &Value) -> RouterConfig {
    let source = &row["config"];
    let mut config = read_config(
        &json!({"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_OLLAMA_MODEL":"tev1:4b-q4_K_M"}),
        false,
        std::path::Path::new("/synthetic"),
    )
    .unwrap();
    config.evaluator = if source["evaluator"] == "jev" {
        Evaluator::Jev
    } else {
        Evaluator::Ollama
    };
    config.ollama_endpoint = source["ollamaEndpoint"].as_str().unwrap().into();
    config.ollama_model = source["ollamaModel"].as_str().unwrap().into();
    config.ollama_timeout_ms = source["ollamaTimeoutMs"].as_u64().unwrap();
    config.ollama_keep_alive = source["ollamaKeepAlive"].clone();
    assert_eq!(
        wire(&serde_json::to_value(&config).unwrap()),
        wire(&ordinary(source))
    );
    config
}
pub fn projected(value: &Value) -> Value {
    fn timings(value: &mut Value) {
        if let Some(map) = value.as_object_mut() {
            if let Some(timing) = map.remove("latency_ms") {
                assert!(timing.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0));
            }
            for item in map.values_mut() {
                timings(item);
            }
        } else if let Some(items) = value.as_array_mut() {
            for item in items {
                timings(item);
            }
        }
    }
    let mut value = ordinary(value);
    timings(&mut value);
    wire(&value)
}
pub fn expected_progress(row: &Value) -> Value {
    Value::Array(
        row["progress"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["event"].clone())
            .collect(),
    )
}
pub fn check_progress(events: &[Value], row: &Value) {
    assert_eq!(
        projected(&json!(events)),
        projected(&expected_progress(row))
    );
    for event in events {
        if matches!(
            event["event"].as_str(),
            Some("startup_complete" | "case_complete")
        ) {
            assert!(
                event["latency_ms"]
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && n >= 0.0)
            );
        }
    }
}
pub fn check_report(report: &Value, events: &[Value], row: &Value) {
    assert_eq!(projected(report), projected(&row["output"]));
    check_progress(events, row);
    if !report["startup"].is_null() {
        assert!(
            report["startup"]["latency_ms"]
                .as_f64()
                .is_some_and(|n| n.is_finite() && n >= 0.0)
        );
        for item in report["rows"].as_array().unwrap() {
            assert!(
                item["latency_ms"]
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && n >= 0.0)
            );
        }
    }
    for event in events {
        match event["event"].as_str() {
            Some("startup_complete") => {
                assert_eq!(event["latency_ms"], report["startup"]["latency_ms"])
            }
            Some("case_complete") => {
                let item = report["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["case"] == event["case"])
                    .unwrap();
                assert_eq!(event["latency_ms"], item["latency_ms"]);
            }
            _ => {}
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Counts {
    pub requests: usize,
    pub requests_live: usize,
    pub requests_dropped: usize,
    pub bodies: usize,
    pub bodies_live: usize,
    pub bodies_dropped: usize,
    pub stalled_created: usize,
    pub stalled_dropped: usize,
}
#[derive(Default)]
struct State {
    observed: Vec<Value>,
    invalid: bool,
    counts: Counts,
}
pub struct Replay {
    expected: Vec<Value>,
    cancellation: CancellationToken,
    state: Arc<Mutex<State>>,
}
impl Replay {
    pub fn new(row: &Value, cancellation: &CancellationToken) -> Arc<Self> {
        let expected = row["requests"].as_array().unwrap().clone();
        assert!(expected.len() <= 24);
        for request in &expected {
            assert_eq!(request["options"]["redirect"], "error");
            assert_eq!(request["signal_aborted_before"], false);
            if let Some(text) = request["response"]["body"]["text"].as_str() {
                assert!(text.len() < 65536);
            }
        }
        Arc::new(Self {
            expected,
            cancellation: cancellation.clone(),
            state: Arc::new(Mutex::new(State::default())),
        })
    }
    pub fn counts(&self) -> Counts {
        self.state.lock().unwrap().counts
    }
    pub fn idle(&self) -> bool {
        let s = self.counts();
        s.requests_live == 0 && s.bodies_live == 0
    }
    pub fn verified(&self) -> bool {
        let state = self.state.lock().unwrap();
        !state.invalid
            && state.observed.len() == self.expected.len()
            && state.counts.requests_live == 0
            && state.counts.bodies_live == 0
    }
    pub fn calls(&self) -> Vec<Value> {
        self.state.lock().unwrap().observed.clone()
    }
}
struct RequestOwner(Arc<Mutex<State>>);
impl Drop for RequestOwner {
    fn drop(&mut self) {
        let mut s = self.0.lock().unwrap();
        s.counts.requests_live -= 1;
        s.counts.requests_dropped += 1;
    }
}
pub struct ReplayBody {
    bytes: Option<Bytes>,
    stalled: bool,
    state: Arc<Mutex<State>>,
}
impl Drop for ReplayBody {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.counts.bodies_live -= 1;
        state.counts.bodies_dropped += 1;
        if self.stalled {
            state.counts.stalled_dropped += 1;
        }
    }
}
impl Body for ReplayBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let this = self.get_mut();
        if this.stalled {
            Poll::Pending
        } else {
            Poll::Ready(this.bytes.take().map(|b| Ok(Frame::data(b))))
        }
    }
    fn is_end_stream(&self) -> bool {
        !self.stalled && self.bytes.is_none()
    }
}
impl HttpTransport for Replay {
    type ResponseBody = ReplayBody;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<ReplayBody>, HttpError> {
        let index = {
            let mut state = self.state.lock().unwrap();
            let index = state.counts.requests;
            state.counts.requests += 1;
            state.counts.requests_live += 1;
            index
        };
        let _owner = RequestOwner(self.state.clone());
        let Some(expected) = self.expected.get(index) else {
            self.state.lock().unwrap().invalid = true;
            return Err(HttpError::Network);
        };
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        if bytes.len() > 65536 {
            self.state.lock().unwrap().invalid = true;
            return Err(HttpError::Network);
        }
        let headers: serde_json::Map<_, _> = parts
            .headers
            .iter()
            .map(|(key, value)| (key.to_string(), json!(value.to_str().unwrap())))
            .collect();
        let actual = json!({"url":parts.uri.to_string(),"method":parts.method.as_str(),"headers":headers,"body_text":std::str::from_utf8(&bytes).unwrap(),"body":if bytes.is_empty(){Value::Null}else{serde_json::from_slice::<Value>(&bytes).unwrap()}});
        let wanted = json!({"url":expected["url"],"method":expected["options"]["method"].as_str().unwrap_or("GET"),"headers":expected["options"].get("headers").cloned().unwrap_or(json!({})),"body_text":expected["options"]["body"].as_str().unwrap_or(""),"body":expected["parsed_body"]});
        {
            let mut state = self.state.lock().unwrap();
            state.invalid |= wire(&actual) != wire(&wanted);
            state.observed.push(actual);
        }
        if expected.get("error").is_some() {
            assert_eq!(expected["source_test"], "test/local-diagnostic.test.mjs#12");
            assert_eq!(index, 8);
            // Original mock aborts on the second inference dispatch. The real
            // classifier must observe cancellation and drop this owned request.
            self.cancellation.cancel();
            return std::future::pending().await;
        }
        let response = &expected["response"];
        let stalled = response["body"].get("stream").is_some();
        if stalled {
            assert_eq!(expected["source_test"], "test/local-diagnostic.test.mjs#13");
            assert_eq!(index, 3);
        }
        let bytes = if stalled {
            None
        } else {
            Some(Bytes::copy_from_slice(
                response["body"]["text"].as_str().unwrap().as_bytes(),
            ))
        };
        {
            let mut state = self.state.lock().unwrap();
            state.counts.bodies += 1;
            state.counts.bodies_live += 1;
            if stalled {
                state.counts.stalled_created += 1;
            }
        }
        let body = ReplayBody {
            bytes,
            stalled,
            state: self.state.clone(),
        };
        let mut builder = Response::builder().status(response["status"].as_u64().unwrap() as u16);
        for header in response["headers"].as_array().unwrap() {
            builder = builder.header(header[0].as_str().unwrap(), header[1].as_str().unwrap());
        }
        let response = builder.body(body).unwrap();
        // Source queueMicrotask aborts after stream creation and before the
        // fetch facade observes its return. Retain that ownership boundary.
        if stalled {
            self.cancellation.cancel();
        }
        Ok(response)
    }
}
