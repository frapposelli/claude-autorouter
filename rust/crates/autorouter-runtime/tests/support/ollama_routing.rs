//! Finite request replay with independent mismatch/call/body ownership checks.
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
            let response = &request["response"];
            assert!(response["body"]["text"].as_str().unwrap().len() <= MAX_RESPONSE);
            assert_eq!(response["json_input_tags"], json!([]));
            assert!(request.get("error").is_none());
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
