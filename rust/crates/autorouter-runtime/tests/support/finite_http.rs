//! Shared exact finite HTTP replay; each caller retains its call and wire bounds.
use autorouter_runtime::http_client::{HttpError, HttpTransport};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

pub struct Replay<const MAX_CALLS: usize, const BODY_TEXT: bool> {
    remaining: Mutex<VecDeque<Value>>,
    observed: Mutex<Vec<Value>>,
    invalid: AtomicUsize,
    expected_count: usize,
}
impl<const MAX_CALLS: usize, const BODY_TEXT: bool> Replay<MAX_CALLS, BODY_TEXT> {
    pub fn new(requests: Vec<Value>) -> Arc<Self> {
        assert!(requests.len() <= MAX_CALLS);
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
pub fn request_observation<const BODY_TEXT: bool>(request: &Value) -> Value {
    let mut observed = json!({"url":request["url"],"method":request["method"],"headers":request["headers"],"body":request["body"]});
    if BODY_TEXT {
        observed["body_text"] = request["body_text"].clone();
    }
    observed
}
impl<const MAX_CALLS: usize, const BODY_TEXT: bool> HttpTransport for Replay<MAX_CALLS, BODY_TEXT> {
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
        let mut observed = json!({"url":parts.uri.to_string(),"method":parts.method.as_str(),"headers":headers,
            "body":if bytes.is_empty(){Value::Null}else{serde_json::from_slice::<Value>(&bytes).unwrap()}});
        if BODY_TEXT {
            observed["body_text"] = json!(std::str::from_utf8(&bytes).unwrap());
        }
        if observed != request_observation::<BODY_TEXT>(&expected) {
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
