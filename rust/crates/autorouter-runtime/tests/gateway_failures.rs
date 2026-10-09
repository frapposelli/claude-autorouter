//! Auth, cancellation, error and streaming boundaries over a real downstream
//! socket. Upstream bytes are injected so failures are deterministic and local.
use autorouter_core::config::{AuthMode, RouterConfig, read_config};
use autorouter_core::js_json::JsDocument;
use autorouter_core::router::RouteOptions;
use autorouter_runtime::evaluator::EvaluationError;
use autorouter_runtime::http_client::{HttpError, HttpTransport, NativeHttpClient};
use autorouter_runtime::server::{Gateway, GatewayHandle, GatewayRouter};
use autorouter_runtime::server_events::EventSinks;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame};
use hyper::{HeaderMap, Request, Response};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
const TOKEN: &str = "synthetic-local-router-token";
const REQUEST: &str = r#"{"model":"claude-sonnet-5-5","messages":[{"role":"user","content":"synthetic private task"}],"max_tokens":128}"#;
type CapturedRequest = (String, HeaderMap, Bytes);
struct Stream {
    frames: VecDeque<Bytes>,
    hang: bool,
    fail: bool,
    dropped: Arc<AtomicBool>,
}
impl Body for Stream {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let this = self.get_mut();
        if let Some(frame) = this.frames.pop_front() {
            return Poll::Ready(Some(Ok(Frame::data(frame))));
        }
        if this.hang {
            return Poll::Pending;
        }
        if this.fail {
            this.fail = false;
            return Poll::Ready(Some(Err(io::Error::other(
                "synthetic private upstream text",
            ))));
        }
        Poll::Ready(None)
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}
struct Transport {
    received: Mutex<Vec<CapturedRequest>>,
    reply: Mutex<(u16, HeaderMap, Vec<Bytes>, bool, bool)>,
    dropped: Arc<AtomicBool>,
}
impl HttpTransport for Transport {
    type ResponseBody = Stream;
    async fn request(&self, request: Request<Full<Bytes>>) -> Result<Response<Stream>, HttpError> {
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        self.received
            .lock()
            .unwrap()
            .push((parts.uri.to_string(), parts.headers, bytes));
        let (status, headers, frames, hang, fail) = self.reply.lock().unwrap().clone();
        let mut response = Response::builder()
            .status(status)
            .body(Stream {
                frames: frames.into(),
                hang,
                fail,
                dropped: self.dropped.clone(),
            })
            .unwrap();
        *response.headers_mut() = headers;
        Ok(response)
    }
}
#[derive(Default)]
struct Router {
    calls: AtomicUsize,
    completed: Mutex<Vec<Value>>,
    hang: AtomicBool,
    entered: CancellationToken,
    shutdown: AtomicBool,
}
impl GatewayRouter for Router {
    async fn route(
        &self,
        _: Arc<JsDocument>,
        _: RouteOptions,
        _: &HeaderMap,
        cancellation: &CancellationToken,
        _: &str,
    ) -> Result<Value, EvaluationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.cancel();
        if self.hang.load(Ordering::SeqCst) {
            cancellation.cancelled().await;
            return Err(EvaluationError::Cancelled);
        }
        Ok(json!({"model":"claude-haiku-4-5-20251001","source":"test"}))
    }
    fn complete(&self, _: &str, evidence: &Value) -> bool {
        self.completed.lock().unwrap().push(evidence.clone());
        !evidence.is_null()
    }
    fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}
struct Fixture {
    handle: Option<GatewayHandle>,
    transport: Arc<Transport>,
    router: Arc<Router>,
    statuses: Arc<Mutex<Vec<Value>>>,
    logs: Arc<Mutex<Vec<Value>>>,
    client: NativeHttpClient,
}
impl Fixture {
    async fn new(adapt: impl FnOnce(&mut RouterConfig)) -> Self {
        let mut config=read_config(&json!({"AUTOROUTER_EVALUATOR":"jev","ANTHROPIC_API_KEY":"synthetic-upstream-key","TYPESAFE_API_KEY":"synthetic-evaluator-key","AUTOROUTER_TOKEN":TOKEN}),false,std::path::Path::new("/tmp")).unwrap();
        config.upstream = "http://fixed.example.invalid".into();
        adapt(&mut config);
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        let transport=Arc::new(Transport{received:Mutex::new(Vec::new()),reply:Mutex::new((200,headers,vec![Bytes::from_static(br#"{"type":"message","model":"claude-haiku-4-5-20251001","stop_reason":"end_turn","content":[]}"#)],false,false)),dropped:Arc::new(AtomicBool::new(false))});
        let router = Arc::new(Router::default());
        let statuses = Arc::new(Mutex::new(Vec::new()));
        let logs = Arc::new(Mutex::new(Vec::new()));
        let sink = statuses.clone();
        let log = logs.clone();
        let gateway = Gateway::with_router(
            config,
            transport.clone(),
            router.clone(),
            EventSinks {
                status: Some(Arc::new(move |event| {
                    sink.lock()
                        .unwrap()
                        .push(event.to_serde_observation_lossy())
                })),
                log: Some(Arc::new(move |event| {
                    log.lock().unwrap().push(event.to_serde_observation_lossy())
                })),
                ..Default::default()
            },
        )
        .unwrap();
        let handle = gateway.listen(0).await.unwrap();
        Self {
            handle: Some(handle),
            transport,
            router,
            statuses,
            logs,
            client: NativeHttpClient::new().unwrap(),
        }
    }
    fn request(&self, method: &str, path: &str, body: &str) -> Request<Full<Bytes>> {
        Request::builder()
            .method(method)
            .uri(format!(
                "http://{}{path}",
                self.handle.as_ref().unwrap().address
            ))
            .header("x-api-key", TOKEN)
            .body(Full::new(Bytes::copy_from_slice(body.as_bytes())))
            .unwrap()
    }
    async fn status(&self, event: &str) {
        wait_for(|| {
            self.statuses
                .lock()
                .unwrap()
                .iter()
                .any(|row| row["event"] == event)
        })
        .await;
    }
    async fn close(mut self) {
        self.handle.take().unwrap().close().await;
        assert!(self.router.shutdown.load(Ordering::SeqCst));
    }
}
async fn wait_for(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("Bounded resource cleanup did not finish");
}
#[tokio::test]
async fn local_rejections_never_call_router_or_upstream_and_hide_payloads() {
    let f = Fixture::new(|config| config.max_body_bytes = 1000).await;
    for (method, path, body, header, status) in [
        (
            "POST",
            "/v1/messages",
            REQUEST,
            Some(("x-api-key", "wrong")),
            401,
        ),
        (
            "GET",
            "/health",
            "",
            Some(("origin", "https://example.invalid")),
            403,
        ),
        (
            "POST",
            "/v1/messages",
            "{synthetic private invalid",
            None,
            400,
        ),
        (
            "POST",
            "/v1/messages",
            r#"{"model":"x","messages":[null]}"#,
            None,
            400,
        ),
        (
            "POST",
            "/v1/messages",
            r#"{"model":"x","messages":[{"role":"user","content":"x","output_config":1e400}]}"#,
            None,
            400,
        ),
        ("GET", "/unexpected", "", None, 404),
        ("GET", "/v1/models/a/b", "", None, 404),
        (
            "POST",
            "/v1/messages",
            REQUEST,
            Some(("content-encoding", "gzip")),
            415,
        ),
    ] {
        let mut request = f.request(method, path, body);
        if let Some((key, value)) = header {
            request.headers_mut().insert(key, value.parse().unwrap());
        }
        let response = f.client.request(request).await.unwrap();
        assert_eq!(response.status(), status);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(!String::from_utf8_lossy(&bytes).contains("synthetic private"));
    }
    let response = f
        .client
        .request(f.request("POST", "/v1/messages", &"x".repeat(1001)))
        .await
        .unwrap();
    assert_eq!(response.status(), 413);
    assert_eq!(response.headers()["connection"], "close");
    let _ = response.into_body().collect().await;
    let response = f
        .client
        .request(f.request("HEAD", "/api/hello", ""))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
    assert_eq!(f.router.calls.load(Ordering::SeqCst), 0);
    assert!(f.transport.received.lock().unwrap().is_empty());
    assert!(!format!("{:?}", f.logs.lock().unwrap()).contains("synthetic private"));
    f.close().await;
}
#[tokio::test]
async fn subscription_refresh_discovery_and_counting_keep_current_auth_without_routing() {
    let f = Fixture::new(|config| config.auth_mode = AuthMode::Subscription).await;
    for (path, method, credential) in [
        ("/v1/models?limit=2", "GET", "Bearer synthetic-first"),
        (
            "/v1/messages/count_tokens?beta=true",
            "POST",
            "Bearer synthetic-refreshed",
        ),
    ] {
        let raw = if method == "POST" {
            format!("  {REQUEST}\n")
        } else {
            String::new()
        };
        let mut request = f.request(method, path, &raw);
        request.headers_mut().remove("x-api-key");
        for (key, value) in [
            ("x-autorouter-token", TOKEN),
            ("authorization", credential),
            ("anthropic-beta", "other, oauth-2025-04-20"),
            ("cookie", "synthetic-cookie"),
            ("connection", "x-remove"),
            ("x-remove", "synthetic-hop"),
            ("anthropic-version", "2023-06-01"),
        ] {
            request.headers_mut().insert(key, value.parse().unwrap());
        }
        let response = f.client.request(request).await.unwrap();
        assert_eq!(response.status(), 200);
        let _ = response.into_body().collect().await.unwrap();
        let received = f.transport.received.lock().unwrap();
        let (uri, headers, body) = received.last().unwrap();
        assert_eq!(uri, &format!("http://fixed.example.invalid{path}"));
        assert_eq!(headers["authorization"], credential);
        assert_eq!(headers["anthropic-beta"], "other, oauth-2025-04-20");
        assert_eq!(headers["accept-encoding"], "identity");
        assert_eq!(body.as_ref(), raw.as_bytes());
        for key in [
            "cookie",
            "x-autorouter-token",
            "x-api-key",
            "x-remove",
            "connection",
        ] {
            assert!(!headers.contains_key(key), "{key}");
        }
    }
    assert_eq!(f.router.calls.load(Ordering::SeqCst), 0);
    assert!(f.statuses.lock().unwrap().is_empty());
    f.close().await;
}
#[tokio::test]
async fn response_bytes_and_failure_statuses_are_preserved_without_false_confirmation() {
    for (status,encoding,bytes)in [
        (429,None,br#"{"type":"error","error":{"type":"rate_limit_error","message":"synthetic private provider text"}}"#.as_slice()),
        (401,None,br#"{"model":"observed-only","type":"message","stop_reason":"end_turn","content":[]}"#.as_slice()),
        (200,Some("gzip"),&[31u8,139,8,0,255,14,0,90]),
        (200,None,br#"{"model":"observed-only"}"#.as_slice()),
    ]{
        let f=Fixture::new(|_|{}).await;{let mut reply=f.transport.reply.lock().unwrap();reply.0=status;reply.2=vec![Bytes::copy_from_slice(bytes)];reply.1.insert("retry-after","7".parse().unwrap());if let Some(encoding)=encoding{reply.1.insert("content-encoding",encoding.parse().unwrap());}}
        let response=f.client.request(f.request("POST","/v1/messages",REQUEST)).await.unwrap();assert_eq!(response.status(),status);assert_eq!(response.headers()["retry-after"],"7");assert_eq!(response.into_body().collect().await.unwrap().to_bytes().as_ref(),bytes);f.status("request_complete").await;assert!(f.router.completed.lock().unwrap().iter().all(Value::is_null));assert!(!format!("{:?}",f.logs.lock().unwrap()).contains("synthetic private"));assert_eq!(f.transport.received.lock().unwrap().len(),1);f.close().await;
    }
}
#[tokio::test]
async fn streaming_timeout_is_error_and_client_disconnect_cancels_owned_body() {
    for disconnect in [false, true] {
        let f =
            Fixture::new(|config| config.upstream_timeout_ms = if disconnect { 3000 } else { 60 })
                .await;
        {
            let mut reply = f.transport.reply.lock().unwrap();
            reply
                .1
                .insert("content-type", "text/event-stream".parse().unwrap());
            reply.2 = vec![Bytes::from_static(b": ping\n\n")];
            reply.3 = true;
        }
        let response = f
            .client
            .request(f.request("POST", "/v1/messages", REQUEST))
            .await
            .unwrap();
        let mut body = response.into_body();
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            ": ping\n\n"
        );
        if disconnect {
            drop(body);
            f.status("request_cancelled").await;
        } else {
            assert!(body.collect().await.is_err());
            f.status("request_error").await;
            assert!(
                !f.statuses
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|row| row["event"] == "request_cancelled")
            );
        }
        wait_for(|| f.transport.dropped.load(Ordering::SeqCst)).await;
        assert!(
            f.router
                .completed
                .lock()
                .unwrap()
                .iter()
                .all(Value::is_null)
        );
        f.close().await;
    }
}
#[tokio::test]
async fn classification_disconnect_releases_attempt_without_forwarding_or_route_event() {
    let f = Fixture::new(|_| {}).await;
    f.router.hang.store(true, Ordering::SeqCst);
    let mut socket = tokio::net::TcpStream::connect(f.handle.as_ref().unwrap().address)
        .await
        .unwrap();
    socket.write_all(format!("POST /v1/messages HTTP/1.1\r\nHost: localhost\r\nx-api-key: {TOKEN}\r\nContent-Length: {}\r\n\r\n{REQUEST}",REQUEST.len()).as_bytes()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), f.router.entered.cancelled())
        .await
        .unwrap();
    drop(socket);
    f.status("request_cancelled").await;
    assert!(f.transport.received.lock().unwrap().is_empty());
    assert_eq!(
        f.statuses
            .lock()
            .unwrap()
            .iter()
            .map(|row| row["event"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["request_start", "request_cancelled"]
    );
    assert!(
        f.router
            .completed
            .lock()
            .unwrap()
            .iter()
            .all(Value::is_null)
    );
    f.close().await;
}
