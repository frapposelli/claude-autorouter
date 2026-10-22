//! Owned mock evaluator transport for the original unlimited-body cancellation schedule.
//! The product Router is real; the transport injection is the original source test seam.
use super::{Assertions, BOUND, Events, LIMIT, parsed};
use autorouter_core::{js_json::JsDocument, router::RouteOptions};
use autorouter_runtime::{
    evaluator::EvaluationError,
    http_client::{HttpError, HttpTransport},
    response_observer::CompletionEvidence,
    router::Router,
    server::GatewayRouter,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    HeaderMap, Request, Response,
    body::{Body, Frame, SizeHint},
};
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

pub struct EvaluatorBody {
    bytes: Option<Bytes>,
    held: bool,
    polled: CancellationToken,
    dropped: Arc<AtomicBool>,
}
impl Body for EvaluatorBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let this = self.get_mut();
        if this.held {
            this.polled.cancel();
            Poll::Pending
        } else {
            Poll::Ready(this.bytes.take().map(|bytes| Ok(Frame::data(bytes))))
        }
    }
    fn is_end_stream(&self) -> bool {
        !self.held && self.bytes.is_none()
    }
    fn size_hint(&self) -> SizeHint {
        if self.held {
            SizeHint::default()
        } else {
            SizeHint::with_exact(self.bytes.as_ref().map_or(0, Bytes::len) as u64)
        }
    }
}
impl Drop for EvaluatorBody {
    fn drop(&mut self) {
        if self.held {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }
}
pub struct Evaluator {
    pub calls: Events,
    pub polled: CancellationToken,
    pub dropped: Arc<AtomicBool>,
    expected: Vec<Value>,
    origin: String,
    assertions: Assertions,
}
impl Evaluator {
    pub fn new(row: &Value, assertions: Assertions) -> Self {
        Self {
            calls: Events::default(),
            polled: CancellationToken::new(),
            dropped: Arc::new(AtomicBool::new(false)),
            expected: row["mocked_fetch"].as_array().unwrap().clone(),
            origin: row["router_config"]["ollamaEndpoint"]
                .as_str()
                .unwrap()
                .into(),
            assertions,
        }
    }
    pub async fn wait_until_polled(&self) {
        tokio::time::timeout(BOUND, self.polled.cancelled())
            .await
            .expect("actual evaluator body poll bound");
    }
    pub async fn wait_until_dropped(&self) {
        for _ in 0..100 {
            if self.dropped.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!(
            "actual evaluator body must be destroyed on last-waiter cancellation before shutdown"
        );
    }
}
impl HttpTransport for Evaluator {
    type ResponseBody = EvaluatorBody;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<EvaluatorBody>, HttpError> {
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        assert!(bytes.len() < LIMIT);
        let uri = parts.uri.to_string();
        let origin = format!(
            "{}://{}",
            parts.uri.scheme_str().unwrap(),
            parts.uri.authority().unwrap()
        );
        self.assertions.check(1, origin == self.origin);
        let path = parts.uri.path();
        let mut calls = self.calls.lock().unwrap();
        let index = calls.len();
        assert!(
            index < self.expected.len(),
            "no unexpected evaluator request"
        );
        let expected = &self.expected[index];
        assert_eq!(uri, expected["url"]);
        assert_eq!(parts.method.as_str(), expected["method"]);
        assert_eq!(
            parsed(&bytes),
            parsed(expected["body"].as_str().unwrap().as_bytes())
        );
        assert_eq!(
            parts.headers.len(),
            expected["headers"].as_object().unwrap().len()
        );
        for (key, value) in expected["headers"].as_object().unwrap() {
            assert_eq!(
                parts.headers.get(key.as_str()).unwrap().to_str().unwrap(),
                value
            );
        }
        calls.push(json!({"url":uri,"method":parts.method.as_str(),"headers":parts.headers.iter().map(|(k,v)|(k.as_str().to_owned(),v.to_str().unwrap().to_owned())).collect::<std::collections::BTreeMap<_,_>>(),"body_utf8":std::str::from_utf8(&bytes).unwrap(),"native_body_pending":path=="/v1/systemone"}));
        drop(calls);
        if path == "/api/show" {
            let bytes = expected["reads"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| r["kind"] == "read" && r["done"] == false)
                .flat_map(|r| super::decode(&r["base64"]))
                .collect::<Vec<_>>();
            Ok(Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(EvaluatorBody {
                    bytes: Some(Bytes::from(bytes)),
                    held: false,
                    polled: self.polled.clone(),
                    dropped: self.dropped.clone(),
                })
                .unwrap())
        } else {
            self.assertions.check(2, path == "/v1/systemone");
            self.assertions
                .check(3, parsed(&bytes)["state"]["current_task"] == "Fix a typo");
            Ok(Response::new(EvaluatorBody {
                bytes: None,
                held: true,
                polled: self.polled.clone(),
                dropped: self.dropped.clone(),
            }))
        }
    }
}
struct RouteDrop<'a>(&'a AtomicBool);
impl Drop for RouteDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
pub struct RealRouter {
    pub inner: Router<Evaluator>,
    pub calls: Events,
    pub completions: Events,
    pub observed_token: Mutex<Option<CancellationToken>>,
    pub dropped: AtomicBool,
    pub returned_cancelled: AtomicBool,
    pub closed: AtomicBool,
}
impl RealRouter {
    pub fn new(inner: Router<Evaluator>) -> Self {
        Self {
            inner,
            calls: Events::default(),
            completions: Events::default(),
            observed_token: Mutex::new(None),
            dropped: AtomicBool::new(false),
            returned_cancelled: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }
    }
    pub fn token_cancelled(&self) -> bool {
        self.observed_token
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    }
}
impl GatewayRouter for RealRouter {
    async fn route(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> Result<Value, EvaluationError> {
        {
            let mut calls = self.calls.lock().unwrap();
            assert!(calls.len() < 8);
            calls.push(json!({"document":parsed(document.stringify().as_bytes()),"scope":options.scope,"prompt_id":options.prompt_id,"request_class":options.request_class,"request_id":options.request_id,"count_tokens":options.count_tokens,"signal_live":!cancellation.is_cancelled()}));
        }
        *self.observed_token.lock().unwrap() = Some(cancellation.clone());
        let _drop = RouteDrop(&self.dropped);
        let result = self
            .inner
            .route(document, options, headers, cancellation, search)
            .await;
        if matches!(result, Err(EvaluationError::Cancelled)) {
            self.returned_cancelled.store(true, Ordering::SeqCst);
        }
        result
    }
    fn complete(&self, id: &str, evidence: &Value) -> bool {
        let accepted = self.inner.complete(id, evidence);
        let mut rows = self.completions.lock().unwrap();
        assert!(rows.len() < 16);
        rows.push(json!({"id":id,"evidence":evidence,"accepted":accepted}));
        accepted
    }
    fn complete_exact(&self, id: &str, evidence: &CompletionEvidence) -> bool {
        let accepted = self.inner.complete_exact(id, evidence);
        let mut rows = self.completions.lock().unwrap();
        assert!(rows.len() < 16);
        rows.push(json!({"id":id,"evidence":evidence,"accepted":accepted}));
        accepted
    }
    fn shutdown(&self) {
        self.inner.shutdown();
    }
    async fn close(&self) {
        self.inner.close().await;
        self.closed.store(true, Ordering::SeqCst);
    }
}
#[derive(Debug, PartialEq)]
pub enum ClientFailure {
    Cancelled,
    Connect,
    Write,
    UnexpectedResponse,
}
pub async fn cancellable_client(
    address: std::net::SocketAddr,
    row: Value,
    cancellation: CancellationToken,
) -> Result<(), ClientFailure> {
    let mut socket = tokio::net::TcpStream::connect(address)
        .await
        .map_err(|_| ClientFailure::Connect)?;
    let body = row["body"].as_str().unwrap();
    let token = row["headers"]["x-api-key"].as_str().unwrap();
    let request = format!(
        "POST /v1/messages HTTP/1.1\r\nHost: {address}\r\nx-api-key: {token}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    socket
        .write_all(request.as_bytes())
        .await
        .map_err(|_| ClientFailure::Write)?;
    let mut byte = [0u8; 1];
    tokio::select! { _=cancellation.cancelled()=>Err(ClientFailure::Cancelled),_=socket.read(&mut byte)=>Err(ClientFailure::UnexpectedResponse) }
}
