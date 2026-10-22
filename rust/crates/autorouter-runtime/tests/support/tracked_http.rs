//! Shared tracked finite test replay; each caller declares its original response/error policy.
use super::{HttpError, HttpTransport};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame, SizeHint};
use hyper::{Request, Response};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::task::{Context, Poll};

pub struct FiniteBody {
    pub inner: Full<Bytes>,
    pub owners: Arc<AtomicUsize>,
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
#[derive(Default)]
struct Queue {
    remaining: VecDeque<Value>,
    observed: Vec<Value>,
    expected: usize,
}
#[derive(Default)]
pub struct Replay<const MAX_RESPONSE: usize, const ERRORS_ALLOWED: bool> {
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
impl<const MAX_RESPONSE: usize, const ERRORS_ALLOWED: bool> Replay<MAX_RESPONSE, ERRORS_ALLOWED> {
    pub fn set(&self, requests: Vec<Value>) {
        assert!(requests.len() <= 2);
        assert_eq!(self.active.load(Ordering::SeqCst), 0);
        assert_eq!(self.bodies.load(Ordering::SeqCst), 0);
        for request in &requests {
            assert_eq!(
                request["node_options"],
                json!({"redirect":"error","signal_kind":"AbortSignal","signal_aborted_before":false,"omitted_fields":[],"signal_aborted_after":false})
            );
            if ERRORS_ALLOWED {
                if let Some(response) = request.get("response") {
                    assert!(response["body"]["text"].as_str().unwrap().len() <= MAX_RESPONSE);
                    assert_eq!(response["json_input_tags"], json!([]));
                } else {
                    assert_eq!(
                        request["error"],
                        json!({"name":"Error","message":"private upstream detail","own_properties":["message"]})
                    );
                }
            } else {
                let response = &request["response"];
                assert!(response["body"]["text"].as_str().unwrap().len() <= MAX_RESPONSE);
                assert_eq!(response["json_input_tags"], json!([]));
                assert!(request.get("error").is_none());
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
impl<const MAX_RESPONSE: usize, const ERRORS_ALLOWED: bool> HttpTransport
    for Replay<MAX_RESPONSE, ERRORS_ALLOWED>
{
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
        if ERRORS_ALLOWED && expected.get("error").is_some() {
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
