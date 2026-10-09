//! Exact frozen server inputs through actual downstream and upstream HTTP.
use autorouter_core::config::read_config;
use autorouter_core::js_json::JsDocument;
use autorouter_core::router::RouteOptions;
use autorouter_runtime::evaluator::EvaluationError;
use autorouter_runtime::http_client::{HttpTransport, NativeHttpClient};
use autorouter_runtime::server::{Gateway, GatewayHandle, GatewayRouter};
use autorouter_runtime::server_events::{EventSink, EventSinks};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{HeaderMap, Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::convert::Infallible;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;

const CASES: &str = include_str!("../../../parity/cases/server-response-contracts.jsonl");
const CAPTURE: &str = include_str!("../../../parity/cases/server-response-contracts.capture.json");
const CASES_SHA: &str = "7c158c333972674c503b3a8767fa5077ec775369998894a72bde8609bcd3e217";
const LIMIT: Duration = Duration::from_secs(5);

fn input(number: usize) -> Value {
    assert_eq!(format!("{:x}", Sha256::digest(CASES.as_bytes())), CASES_SHA);
    let report: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(report["cases_sha256"], CASES_SHA);
    assert_eq!(report["static_assertions"], 22);
    CASES
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|row| row["id"] == format!("baseline-server-response-{number}"))
        .unwrap()
}
async fn bounded(future: impl std::future::Future<Output = ()>) {
    tokio::time::timeout(LIMIT, future)
        .await
        .expect("Complete synthetic server schedule exceeded deadline");
}
type Events = Arc<Mutex<Vec<Value>>>;
fn sink(events: &Events) -> EventSink {
    let events = events.clone();
    Arc::new(move |value| {
        let mut rows = events.lock().unwrap();
        assert!(rows.len() < 128);
        rows.push(value.to_serde_observation_lossy());
    })
}
#[derive(Default)]
struct Router {
    calls: AtomicUsize,
    shutdown: AtomicBool,
}
impl GatewayRouter for Router {
    async fn route(
        &self,
        _: Arc<JsDocument>,
        _: RouteOptions,
        _: &HeaderMap,
        _: &CancellationToken,
        _: &str,
    ) -> Result<Value, EvaluationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"model":"claude-haiku-4-5-20251001","source":"test"}))
    }
    fn complete(&self, _: &str, _: &Value) -> bool {
        false
    }
    fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}
struct PeerLease(Arc<AtomicUsize>);
impl Drop for PeerLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
struct Seen {
    method: String,
    path: String,
    headers: HeaderMap,
    body: Bytes,
}
struct Fixture {
    handle: Option<GatewayHandle>,
    peer: Option<JoinHandle<()>>,
    stop: CancellationToken,
    active: Arc<AtomicUsize>,
    seen: Arc<Mutex<Vec<Seen>>>,
    logs: Events,
    statuses: Events,
    decisions: Events,
    router: Arc<Router>,
    client: Option<Arc<NativeHttpClient>>,
    token: String,
}
impl Fixture {
    async fn new(row: &Value, response_bytes: Vec<u8>, encoding: Option<&str>) -> Self {
        assert!(response_bytes.len() <= 4096);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let stop = CancellationToken::new();
        let peer_stop = stop.clone();
        let active = Arc::new(AtomicUsize::new(0));
        let peer_active = active.clone();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let peer_seen = seen.clone();
        let status = row["response_status"].as_u64().unwrap_or(500) as u16;
        let mut headers = HeaderMap::new();
        if let Some(fields) = row["response_headers"].as_object() {
            for (key, value) in fields {
                headers.insert(
                    hyper::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
                    value.as_str().unwrap().parse().unwrap(),
                );
            }
        }
        headers.remove("content-encoding");
        if let Some(encoding) = encoding {
            headers.insert("content-encoding", encoding.parse().unwrap());
        }
        let bytes = Bytes::from(response_bytes);
        let peer = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = peer_stop.cancelled() => break,
                    result = tasks.join_next(), if !tasks.is_empty() => { result.unwrap().unwrap(); },
                    socket = listener.accept() => {
                        let (socket, _) = socket.unwrap();
                        assert!(peer_active.fetch_add(1, Ordering::SeqCst) < 8);
                        let lease = PeerLease(peer_active.clone());
                        let seen = peer_seen.clone();
                        let headers = headers.clone();
                        let bytes = bytes.clone();
                        tasks.spawn(async move {
                            let _lease = lease;
                            let service = service_fn(move |request: Request<Incoming>| {
                                let seen = seen.clone();
                                let headers = headers.clone();
                                let bytes = bytes.clone();
                                async move {
                                    let (parts, body) = request.into_parts();
                                    let body = Limited::new(body, 4096).collect().await.unwrap().to_bytes();
                                    {
                                        let mut calls = seen.lock().unwrap();
                                        assert!(calls.len() < 16);
                                        calls.push(Seen { method: parts.method.to_string(), path: parts.uri.to_string(), headers: parts.headers, body });
                                    }
                                    let mut response = Response::builder().status(status).body(Full::new(bytes)).unwrap();
                                    *response.headers_mut() = headers;
                                    Ok::<_, Infallible>(response)
                                }
                            });
                            hyper::server::conn::http1::Builder::new()
                                .serve_connection(TokioIo::new(socket), service).await.unwrap();
                        });
                    }
                }
            }
            tasks.abort_all();
            while let Some(result) = tasks.join_next().await {
                if let Err(error) = result {
                    assert!(error.is_cancelled(), "Unexpected peer panic: {error}");
                }
            }
        });
        let token = row["token"].as_str().unwrap().to_owned();
        let mut config = read_config(&json!({"AUTOROUTER_EVALUATOR":"jev","ANTHROPIC_API_KEY":"upstream-secret","TYPESAFE_API_KEY":"classifier-secret","AUTOROUTER_TOKEN":token}), false, std::path::Path::new("/tmp")).unwrap();
        config.upstream = format!("http://{address}");
        if let Some(maximum) = row["maximum_body_bytes"].as_u64() {
            config.max_body_bytes = maximum as usize;
        }
        let client = Arc::new(NativeHttpClient::new().unwrap());
        let router = Arc::new(Router::default());
        let logs = Events::default();
        let statuses = Events::default();
        let decisions = Events::default();
        let gateway = Gateway::with_router(
            config,
            client.clone(),
            router.clone(),
            EventSinks {
                log: Some(sink(&logs)),
                status: Some(sink(&statuses)),
                decision: Some(sink(&decisions)),
                ..Default::default()
            },
        )
        .unwrap();
        let handle = gateway.listen(0).await.unwrap();
        Self {
            handle: Some(handle),
            peer: Some(peer),
            stop,
            active,
            seen,
            logs,
            statuses,
            decisions,
            router,
            client: Some(client),
            token,
        }
    }
    async fn send(
        &self,
        path: &str,
        method: &str,
        body: String,
        extra: &[(&str, &str)],
    ) -> (u16, HeaderMap, Bytes) {
        let mut request = Request::builder()
            .method(method)
            .uri(format!(
                "http://{}{path}",
                self.handle.as_ref().unwrap().address
            ))
            .header("x-api-key", &self.token)
            .body(Full::new(Bytes::from(body)))
            .unwrap();
        for (name, value) in extra {
            request.headers_mut().insert(
                hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        let response = self
            .client
            .as_ref()
            .unwrap()
            .request(request)
            .await
            .unwrap();
        let (parts, body) = response.into_parts();
        (
            parts.status.as_u16(),
            parts.headers,
            Limited::new(body, 65536)
                .collect()
                .await
                .unwrap()
                .to_bytes(),
        )
    }
    async fn completed(&self) {
        while !self
            .statuses
            .lock()
            .unwrap()
            .iter()
            .any(|row| row["event"] == "request_complete")
        {
            tokio::task::yield_now().await;
        }
    }
    async fn close(mut self) {
        self.client.take();
        self.handle.take().unwrap().close().await;
        assert!(self.router.shutdown.load(Ordering::SeqCst));
        self.stop.cancel();
        self.peer.take().unwrap().await.unwrap();
        assert_eq!(self.active.load(Ordering::SeqCst), 0);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(task) = self.peer.take() {
            task.abort();
        }
    }
}

#[tokio::test]
async fn frozen_token_count_error_preserves_body_status_headers_and_bypasses_evaluation() {
    bounded(async {
        let row = input(2);
        let expected = row["response_text"].as_str().unwrap().as_bytes();
        let fixture = Fixture::new(&row, expected.to_vec(), None).await;
        let (status, headers, bytes) = fixture
            .send(
                "/v1/messages/count_tokens",
                "POST",
                row["request"].to_string(),
                &[],
            )
            .await;
        assert_eq!(status, 429);
        assert_eq!(headers["retry-after"], "7");
        assert_eq!(bytes.as_ref(), expected);
        assert_eq!(fixture.router.calls.load(Ordering::SeqCst), 0);
        assert!(fixture.statuses.lock().unwrap().is_empty());
        assert!(fixture.decisions.lock().unwrap().is_empty());
        {
            let seen = fixture.seen.lock().unwrap();
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].method, "POST");
            assert_eq!(seen[0].path, "/v1/messages/count_tokens");
            assert_eq!(
                serde_json::from_slice::<Value>(&seen[0].body).unwrap(),
                row["request"]
            );
        }
        fixture.close().await;
    })
    .await;
}

#[tokio::test]
async fn frozen_local_rejections_never_contact_upstream_or_emit_decisions() {
    bounded(async {
        let row = input(3);
        let fixture = Fixture::new(&row, Vec::new(), None).await;
        let mut malformed = row["request"].clone();
        malformed["messages"] = json!([null]);
        for (path, method, body, extra, expected) in [
            (
                "/v1/messages",
                "POST",
                String::new(),
                vec![("x-api-key", "wrong")],
                401,
            ),
            (
                "/health",
                "GET",
                String::new(),
                vec![("origin", "https://example.com")],
                403,
            ),
            ("/v1/messages", "POST", "{bad".into(), vec![], 400),
            ("/v1/messages", "POST", malformed.to_string(), vec![], 400),
            ("/unexpected", "GET", String::new(), vec![], 404),
        ] {
            assert_eq!(
                fixture.send(path, method, body, &extra).await.0,
                expected,
                "{path}"
            );
        }
        let (status, headers, _) = fixture
            .send("/v1/messages", "POST", "x".repeat(1001), &[])
            .await;
        assert_eq!(status, 413);
        assert_eq!(headers["connection"], "close");
        assert_eq!(
            fixture.send("/health", "GET", String::new(), &[]).await.0,
            200
        );
        assert_eq!(fixture.router.calls.load(Ordering::SeqCst), 0);
        assert!(fixture.decisions.lock().unwrap().is_empty());
        assert!(fixture.seen.lock().unwrap().is_empty());
        fixture.close().await;
    })
    .await;
}

#[tokio::test]
async fn frozen_gzip_bytes_are_opaque_and_encoding_controls_observation() {
    bounded(async {
        let row = input(20);
        let gzip: Vec<u8> = row["response_bytes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u8)
            .collect();
        let plain = row["decoded_synthetic_text"]
            .as_str()
            .unwrap()
            .as_bytes()
            .to_vec();
        for (bytes, encoding, observed) in [
            (gzip, Some("gzip"), false),
            // Sensitivity controls: parseable SSE labelled gzip must stay opaque;
            // the same unencoded bytes must produce model observation.
            (plain.clone(), Some("gzip"), false),
            (plain, None, true),
        ] {
            let fixture = Fixture::new(&row, bytes.clone(), encoding).await;
            let (status, headers, received) = fixture
                .send(
                    "/v1/messages",
                    "POST",
                    row["request"].to_string(),
                    &[("content-type", "application/json")],
                )
                .await;
            assert_eq!(status, 200);
            assert_eq!(
                headers.get("content-encoding").map(|v| v.to_str().unwrap()),
                encoding
            );
            assert_eq!(received.as_ref(), bytes);
            fixture.completed().await;
            assert_eq!(
                fixture
                    .logs
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|r| r["event"] == "upstream_model"),
                observed
            );
            {
                let seen = fixture.seen.lock().unwrap();
                assert_eq!(seen.len(), 1);
                assert_eq!(seen[0].headers["accept-encoding"], "identity");
            }
            fixture.close().await;
        }
    })
    .await;
}
