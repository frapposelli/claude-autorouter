//! In-memory local HTTP transcript with explicit body-read and release evidence.
use autorouter_core::config::{RouterConfig, read_config};
use autorouter_runtime::http_client::{HttpError, HttpTransport};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame};
use hyper::{Request, Response};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::task::{Context, Poll};
use tokio::sync::Notify;

#[derive(Default)]
pub struct Lifetime {
    pub polled: AtomicBool,
    pub dropped: AtomicBool,
    pub ready: Notify,
}
pub struct MockBody {
    chunks: VecDeque<Bytes>,
    pending: bool,
    lifetime: Arc<Lifetime>,
}
impl Body for MockBody {
    type Data = Bytes;
    type Error = std::io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        self.lifetime.polled.store(true, Ordering::SeqCst);
        self.lifetime.ready.notify_one();
        match self.chunks.pop_front() {
            Some(bytes) => Poll::Ready(Some(Ok(Frame::data(bytes)))),
            None if self.pending => Poll::Pending,
            None => Poll::Ready(None),
        }
    }
}
impl Drop for MockBody {
    fn drop(&mut self) {
        self.lifetime.dropped.store(true, Ordering::SeqCst);
    }
}
pub enum Step {
    Error,
    Reply {
        status: u16,
        headers: Vec<(String, String)>,
        bytes: Vec<u8>,
        pending: bool,
        lifetime: Arc<Lifetime>,
    },
}
impl Step {
    pub fn json(value: Value) -> Self {
        Self::Reply {
            status: 200,
            headers: Vec::new(),
            bytes: value.to_string().into_bytes(),
            pending: false,
            lifetime: Arc::default(),
        }
    }
    pub fn pending(lifetime: Arc<Lifetime>) -> Self {
        Self::Reply {
            status: 200,
            headers: Vec::new(),
            bytes: Vec::new(),
            pending: true,
            lifetime,
        }
    }
    pub fn captured(value: &Value) -> Self {
        if value.get("network_error").is_some() {
            return Self::Error;
        }
        let body = &value["body"];
        let bytes = if let Some(text) = body["text"].as_str() {
            assert!(text.len() <= 2 * 1024 * 1024, "synthetic response bound");
            text.to_owned()
        } else {
            let text = body["repeat"].as_str().unwrap();
            let times = usize::try_from(body["times"].as_u64().unwrap()).unwrap();
            assert!(
                text.len().checked_mul(times).unwrap() <= 2 * 1024 * 1024,
                "synthetic response bound"
            );
            text.repeat(times)
        }
        .into_bytes();
        Self::Reply {
            status: value["status"].as_u64().unwrap() as u16,
            headers: value["headers"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap().into()))
                .collect(),
            bytes,
            pending: false,
            lifetime: Arc::default(),
        }
    }
}
pub struct Mock {
    steps: Mutex<VecDeque<Step>>,
    pub calls: Mutex<Vec<Value>>,
    pub lifetimes: Vec<Arc<Lifetime>>,
}
impl Mock {
    pub fn new(steps: Vec<Step>) -> Arc<Self> {
        let lifetimes = steps
            .iter()
            .filter_map(|step| match step {
                Step::Reply { lifetime, .. } => Some(lifetime.clone()),
                _ => None,
            })
            .collect();
        Arc::new(Self {
            steps: Mutex::new(steps.into()),
            calls: Mutex::new(Vec::new()),
            lifetimes,
        })
    }
    pub fn paths(&self) -> Vec<String> {
        self.calls
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
    pub fn assert_consumed_and_released(&self) {
        assert!(
            self.steps.lock().unwrap().is_empty(),
            "missing expected request"
        );
        assert!(
            self.lifetimes
                .iter()
                .all(|state| state.dropped.load(Ordering::SeqCst)),
            "response body retained"
        );
    }
}
impl HttpTransport for Mock {
    type ResponseBody = MockBody;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<MockBody>, HttpError> {
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        let headers: serde_json::Map<String, Value> = parts
            .headers
            .iter()
            .map(|(k, v)| (k.as_str().into(), json!(v.to_str().unwrap())))
            .collect();
        assert!(!headers.contains_key("authorization") && !headers.contains_key("x-api-key"));
        assert_eq!(parts.uri.scheme_str(), Some("http"));
        assert_eq!(parts.uri.authority().unwrap().as_str(), "127.0.0.1:11434");
        self.calls.lock().unwrap().push(
            json!({"url":parts.uri.to_string(),"method":parts.method.as_str(),"headers":headers,
            "body":if bytes.is_empty(){Value::Null}else{serde_json::from_slice(&bytes).unwrap()}}),
        );
        match self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request/redirect/retry")
        {
            Step::Error => Err(HttpError::Network),
            Step::Reply {
                status,
                headers,
                bytes,
                pending,
                lifetime,
            } => {
                let mut response = Response::builder().status(status);
                for (name, value) in headers {
                    response = response.header(name, value);
                }
                let chunks = if bytes.is_empty() {
                    VecDeque::new()
                } else {
                    VecDeque::from([Bytes::from(bytes)])
                };
                Ok(response
                    .body(MockBody {
                        chunks,
                        pending,
                        lifetime,
                    })
                    .unwrap())
            }
        }
    }
}
pub fn config() -> RouterConfig {
    read_config(
        &json!({"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_OLLAMA_MODEL":"tev1:4b-q4_K_M"}),
        false,
        std::path::Path::new("/synthetic"),
    )
    .unwrap()
}
pub fn version() -> Step {
    Step::json(json!({"version":"0.35.0"}))
}
pub fn tags(installed: bool) -> Step {
    tags_for(installed, "tev1:4b-q4_K_M")
}
pub fn tags_for(installed: bool, model: &str) -> Step {
    Step::json(json!({"models":if installed{json!([{"name":model}])}else{json!([])}}))
}
pub fn details() -> Step {
    Step::json(json!({"details":{"parameter_size":"4B"}}))
}
