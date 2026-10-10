//! Deterministic lifecycle load, not a latency/RSS benchmark. All provider and
//! evaluator responses are synthetic; downstream HTTP uses loopback sockets.
use autorouter_core::config::read_config;
use autorouter_runtime::http_client::{HttpError, HttpTransport, NativeHttpClient};
use autorouter_runtime::server::{Gateway, GatewayHandle};
use autorouter_runtime::server_events::EventSinks;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame};
use hyper::{Request, Response};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOKEN: &str = "synthetic-resource-local-token";
const LIMIT: usize = 32 * 1024 * 1024;
const REQUEST: &[u8] = br#"{"model":"claude-sonnet-5-5","messages":[{"role":"user","content":"synthetic resource fixture"}],"max_tokens":128}"#;
const RESPONSE: &[u8] = br#"{"type":"message","model":"claude-haiku-4-5-20251001","stop_reason":"end_turn","content":[]}"#;

#[derive(Default)]
struct Counters {
    created: AtomicUsize,
    dropped: AtomicUsize,
    active: AtomicUsize,
    produced: AtomicUsize,
    evaluations: AtomicUsize,
    count_requests: Mutex<Vec<(usize, [u8; 32])>>,
}
struct GeneratedBody {
    chunks: VecDeque<Bytes>,
    repeat: usize,
    hang: bool,
    fail: bool,
    yielded: bool,
    stats: Arc<Counters>,
}
impl Body for GeneratedBody {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let this = self.get_mut();
        // Alternate Pending/data to expose cancellation between every frame.
        if !this.yielded {
            this.yielded = true;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        this.yielded = false;
        let chunk = this.chunks.pop_front().or_else(|| {
            if this.repeat == 0 {
                return None;
            }
            this.repeat -= 1;
            Some(Bytes::from_static(&[b'x'; 16 * 1024]))
        });
        if let Some(chunk) = chunk {
            this.stats.produced.fetch_add(chunk.len(), Ordering::SeqCst);
            return Poll::Ready(Some(Ok(Frame::data(chunk))));
        }
        if this.hang {
            return Poll::Pending;
        }
        if this.fail {
            this.fail = false;
            return Poll::Ready(Some(Err(io::Error::other("synthetic stream failure"))));
        }
        Poll::Ready(None)
    }
}
impl Drop for GeneratedBody {
    fn drop(&mut self) {
        self.stats.active.fetch_sub(1, Ordering::SeqCst);
        self.stats.dropped.fetch_add(1, Ordering::SeqCst);
    }
}
struct Transport(Arc<Counters>);
impl HttpTransport for Transport {
    type ResponseBody = GeneratedBody;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<GeneratedBody>, HttpError> {
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        let mode = parts
            .headers
            .get("x-synthetic-mode")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("clean");
        let mut status = 200;
        let mut content_type = "application/json";
        let mut payload = RESPONSE;
        let mut repeat = 0;
        let mut hang = false;
        let mut fail = false;
        if parts.uri.path() == "/v1/systemone" {
            self.0.evaluations.fetch_add(1, Ordering::SeqCst);
            payload = br#"{"answers":{"tier":{"choice":"haiku","confidence":0.99}}}"#;
        } else if parts.uri.path() == "/v1/messages/count_tokens" {
            self.0
                .count_requests
                .lock()
                .unwrap()
                .push((bytes.len(), Sha256::digest(&bytes).into()));
            payload = br#"{"input_tokens":1}"#;
        } else {
            match mode {
                "rate-limit" => {
                    status = 429;
                    payload = br#"{"type":"error","error":{"type":"rate_limit_error"}}"#;
                }
                "malformed" => payload = b"{synthetic incomplete JSON",
                "truncated" => {
                    payload = b"{synthetic truncated JSON";
                    fail = true;
                }
                "terminal-hang" => {
                    content_type = "text/event-stream";
                    payload = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"synthetic\",\"type\":\"message\",\"model\":\"claude-haiku-4-5-20251001\",\"content\":[]}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
                    hang = true;
                }
                "long" => {
                    content_type = "application/octet-stream";
                    payload = b"";
                    repeat = 4096;
                }
                _ => {}
            }
        }
        let chunks = payload.chunks(7).map(Bytes::copy_from_slice).collect();
        self.0.created.fetch_add(1, Ordering::SeqCst);
        self.0.active.fetch_add(1, Ordering::SeqCst);
        Ok(Response::builder()
            .status(status)
            .header("content-type", content_type)
            .body(GeneratedBody {
                chunks,
                repeat,
                hang,
                fail,
                yielded: false,
                stats: self.0.clone(),
            })
            .unwrap())
    }
}
#[derive(Default)]
struct Events {
    active: HashSet<String>,
    terminals: HashMap<String, usize>,
    started: usize,
    completed: usize,
}
struct Fixture {
    handle: Option<GatewayHandle>,
    client: Arc<NativeHttpClient>,
    stats: Arc<Counters>,
    events: Arc<Mutex<Events>>,
}
impl Fixture {
    async fn new() -> Self {
        let mut config = read_config(&json!({"AUTOROUTER_EVALUATOR":"jev","ANTHROPIC_API_KEY":"synthetic-provider","TYPESAFE_API_KEY":"synthetic-evaluator","AUTOROUTER_TOKEN":TOKEN}), false, std::path::Path::new("/synthetic")).unwrap();
        config.upstream = "http://synthetic.invalid".into();
        config.jev_endpoint = "http://synthetic.invalid/v1/systemone".into();
        config.max_body_bytes = LIMIT;
        config.cache_entries = 16;
        let stats = Arc::new(Counters::default());
        let events = Arc::new(Mutex::new(Events::default()));
        let sink = events.clone();
        let gateway = Gateway::new(
            config,
            Arc::new(Transport(stats.clone())),
            EventSinks {
                status: Some(Arc::new(move |event| {
                    let event = event.to_serde_observation_lossy();
                    let Some(id) = event["request_id"].as_str() else {
                        return;
                    };
                    let mut state = sink.lock().unwrap();
                    match event["event"].as_str() {
                        Some("request_start") => {
                            assert!(state.active.insert(id.into()));
                            state.started += 1;
                        }
                        Some(
                            name @ ("request_complete" | "request_error" | "request_cancelled"),
                        ) => {
                            assert!(
                                state.active.remove(id),
                                "terminal event must own one active request"
                            );
                            *state.terminals.entry(name.into()).or_default() += 1;
                            state.completed += 1;
                        }
                        _ => {}
                    }
                })),
                ..Default::default()
            },
        )
        .unwrap();
        let handle = gateway.listen(0).await.unwrap();
        Self {
            handle: Some(handle),
            client: Arc::new(NativeHttpClient::new().unwrap()),
            stats,
            events,
        }
    }
    fn address(&self) -> std::net::SocketAddr {
        self.handle.as_ref().unwrap().address
    }
    fn request(&self, path: &str, bytes: Bytes, mode: &str) -> Request<Full<Bytes>> {
        Request::post(format!("http://{}{path}", self.address()))
            .header("x-api-key", TOKEN)
            .header("x-synthetic-mode", mode)
            .body(Full::new(bytes))
            .unwrap()
    }
    async fn idle(&self) {
        wait_for(|| {
            self.stats.active.load(Ordering::SeqCst) == 0
                && self.events.lock().unwrap().active.is_empty()
        })
        .await;
        assert_eq!(
            self.stats.created.load(Ordering::SeqCst),
            self.stats.dropped.load(Ordering::SeqCst)
        );
        let events = self.events.lock().unwrap();
        assert_eq!(events.started, events.completed);
    }
    async fn close(mut self) {
        let address = self.address();
        tokio::time::timeout(Duration::from_secs(5), self.handle.take().unwrap().close())
            .await
            .unwrap();
        self.idle().await;
        // Successful close releases the listener and every connection task.
        let rebound = tokio::net::TcpListener::bind(address).await.unwrap();
        drop(rebound);
    }
}
async fn wait_for(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bounded lifecycle cleanup");
}

#[tokio::test]
async fn exact_32_mib_count_body_is_forwarded_and_one_extra_streamed_byte_is_rejected() {
    let f = Fixture::new().await;
    let mut bytes = REQUEST.to_vec();
    bytes.resize(LIMIT, b' ');
    let expected: [u8; 32] = Sha256::digest(&bytes).into();
    let bytes = Bytes::from(bytes);
    let response = f
        .client
        .request(f.request("/v1/messages/count_tokens", bytes.clone(), "clean"))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.into_body().collect().await.unwrap();
    assert_eq!(
        *f.stats.count_requests.lock().unwrap(),
        vec![(LIMIT, expected)]
    );
    let mut too_large = bytes.to_vec();
    too_large.push(b' ');
    let mut request = f.request("/v1/messages/count_tokens", too_large.into(), "clean");
    request
        .headers_mut()
        .insert("transfer-encoding", "chunked".parse().unwrap());
    let response = f.client.request(request).await.unwrap();
    assert_eq!(response.status(), 413);
    response.into_body().collect().await.unwrap();
    assert_eq!(f.stats.count_requests.lock().unwrap().len(), 1);
    assert_eq!(f.stats.evaluations.load(Ordering::SeqCst), 0);
    f.close().await;
}

#[tokio::test]
async fn repeated_mixed_requests_release_bodies_and_request_leases_after_each_wave() {
    let f = Fixture::new().await;
    for wave in 0..8 {
        let mut work = tokio::task::JoinSet::new();
        for index in 0..32 {
            let mode = [
                "clean",
                "rate-limit",
                "malformed",
                "truncated",
                "terminal-hang",
                "invalid",
                "clean",
                "clean",
            ][index % 8];
            let payload = if mode == "invalid" {
                Bytes::from_static(b"{")
            } else {
                Bytes::from(format!(
                    "{{\"model\":\"claude-sonnet-5-5\",\"messages\":[{{\"role\":\"user\",\"content\":\"synthetic wave {wave} request {index}\"}}],\"max_tokens\":128}}"
                ))
            };
            let request = f.request("/v1/messages", payload, mode);
            let client = f.client.clone();
            work.spawn(async move {
                let response = client.request(request).await.unwrap();
                let status = response.status().as_u16();
                let mut body = response.into_body();
                match mode {
                    "terminal-hang" => {
                        assert_eq!(status, 200);
                        assert!(body.frame().await.unwrap().is_ok());
                        drop(body);
                    }
                    "truncated" => {
                        assert_eq!(status, 200);
                        assert!(body.collect().await.is_err());
                    }
                    _ => {
                        assert_eq!(
                            status,
                            match mode {
                                "rate-limit" => 429,
                                "invalid" => 400,
                                _ => 200,
                            }
                        );
                        body.collect().await.unwrap();
                    }
                }
            });
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(result) = work.join_next().await {
                result.unwrap();
            }
        })
        .await
        .unwrap();
        f.idle().await;
        assert_eq!(f.events.lock().unwrap().completed, (wave + 1) * 32);
        // The same listener remains usable after failure/cancellation cleanup.
        let response = f
            .client
            .request(f.request(
                "/v1/messages/count_tokens",
                Bytes::from_static(REQUEST),
                "clean",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        response.into_body().collect().await.unwrap();
    }
    let terminal_counts = f.events.lock().unwrap().terminals.clone();
    assert_eq!(terminal_counts["request_cancelled"], 32);
    assert_eq!(terminal_counts["request_error"], 64);
    assert_eq!(terminal_counts["request_complete"], 160);
    f.close().await;
}

#[tokio::test]
async fn long_responses_stream_past_input_limit_and_blocked_clients_cancel_without_draining() {
    let f = Fixture::new().await;
    let response = f
        .client
        .request(f.request("/v1/messages", Bytes::from_static(REQUEST), "long"))
        .await
        .unwrap();
    let mut body = response.into_body();
    let mut received = 0;
    while let Some(frame) = body.frame().await {
        let data = frame.unwrap().into_data().unwrap();
        assert!(data.iter().all(|byte| *byte == b'x'));
        received += data.len();
    }
    assert_eq!(received, 64 * 1024 * 1024);
    f.idle().await;
    let before = f.stats.produced.load(Ordering::SeqCst);
    let mut socket = tokio::net::TcpStream::connect(f.address()).await.unwrap();
    socket.write_all(format!("POST /v1/messages HTTP/1.1\r\nHost: localhost\r\nx-api-key: {TOKEN}\r\nx-synthetic-mode: long\r\nContent-Length: {}\r\n\r\n", REQUEST.len()).as_bytes()).await.unwrap();
    socket.write_all(REQUEST).await.unwrap();
    let mut first = [0; 512];
    assert!(socket.read(&mut first).await.unwrap() > 0);
    // Give the producer repeated scheduling opportunities while the reader is
    // stopped. Kernel buffering is platform-specific; the exact hard bound is
    // proved separately with a 128-byte duplex transport in unit tests.
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert!(f.stats.produced.load(Ordering::SeqCst) - before < 64 * 1024 * 1024);
    drop(socket);
    f.idle().await;
    assert_eq!(f.events.lock().unwrap().terminals["request_cancelled"], 1);
    f.close().await;
}

#[tokio::test]
async fn repeated_shutdown_joins_partial_receipts_and_streams_and_releases_listener() {
    for _ in 0..8 {
        let f = Fixture::new().await;
        let mut partial = tokio::net::TcpStream::connect(f.address()).await.unwrap();
        partial.write_all(format!("POST /v1/messages HTTP/1.1\r\nHost: localhost\r\nx-api-key: {TOKEN}\r\nContent-Length: {}\r\n\r\n{{", REQUEST.len()).as_bytes()).await.unwrap();
        let response = f
            .client
            .request(f.request("/v1/messages", Bytes::from_static(REQUEST), "terminal-hang"))
            .await
            .unwrap();
        wait_for(|| f.events.lock().unwrap().active.len() == 2).await;
        f.close().await;
        drop(partial);
        drop(response);
    }
}

#[tokio::test]
async fn stopped_128_byte_writer_bounds_stream_polling_and_drop_releases_ownership() {
    use autorouter_runtime::transport_completion::{CompletionRegistry, Delivery, serve_http1};
    use hyper::body::Incoming;
    use hyper::service::service_fn;
    let stats = Arc::new(Counters::default());
    let registry = CompletionRegistry::new(1);
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let (mut reader, writer) = tokio::io::duplex(128);
    let service_stats = stats.clone();
    let service_registry = registry.clone();
    let service_outcomes = outcomes.clone();
    let connection = tokio::spawn(serve_http1(
        writer,
        registry.clone(),
        service_fn(move |_: Request<Incoming>| {
            service_stats.created.fetch_add(1, Ordering::SeqCst);
            service_stats.active.fetch_add(1, Ordering::SeqCst);
            let body = GeneratedBody {
                chunks: VecDeque::new(),
                repeat: 4096,
                hang: false,
                fail: false,
                yielded: false,
                stats: service_stats.clone(),
            };
            let outcomes = service_outcomes.clone();
            let response = service_registry
                .track(Response::new(body), move |delivery| {
                    outcomes.lock().unwrap().push(delivery)
                })
                .unwrap();
            async { Ok::<_, std::convert::Infallible>(response) }
        }),
    ));
    reader
        .write_all(b"GET /synthetic HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    wait_for(|| stats.produced.load(Ordering::SeqCst) > 0).await;
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    // Pinned Hyper's output buffer is 400 KiB; at most one 16 KiB source frame
    // can exceed its soft admission threshold. Allow 512 KiB independent of OS.
    assert!(stats.produced.load(Ordering::SeqCst) <= 512 * 1024);
    assert_eq!(registry.pending(), 1);
    assert!(outcomes.lock().unwrap().is_empty());
    assert_eq!(stats.active.load(Ordering::SeqCst), 1);
    connection.abort();
    assert!(connection.await.unwrap_err().is_cancelled());
    assert_eq!(registry.pending(), 0);
    assert_eq!(stats.active.load(Ordering::SeqCst), 0);
    assert_eq!(stats.dropped.load(Ordering::SeqCst), 1);
    let outcomes = outcomes.lock().unwrap();
    assert_eq!(outcomes.len(), 1);
    assert!(matches!(outcomes[0], Delivery::Failed(_)));
}

#[tokio::test]
async fn production_classifier_capacity_recovers_after_1024_independent_cancellations() {
    use autorouter_core::js_json::JsDocument;
    use autorouter_runtime::classifier::Classifier;
    use autorouter_runtime::evaluator::EvaluationError;
    use tokio::sync::Semaphore;
    use tokio_util::sync::CancellationToken;
    struct Evaluator {
        active: Arc<AtomicUsize>,
        calls: AtomicUsize,
        permits: Semaphore,
    }
    struct Active(Arc<AtomicUsize>);
    impl Drop for Active {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    impl HttpTransport for Evaluator {
        type ResponseBody = Full<Bytes>;
        async fn request(
            &self,
            _: Request<Full<Bytes>>,
        ) -> Result<Response<Full<Bytes>>, HttpError> {
            self.active.fetch_add(1, Ordering::SeqCst);
            self.calls.fetch_add(1, Ordering::SeqCst);
            let _active = Active(self.active.clone());
            self.permits.acquire().await.unwrap().forget();
            Ok(Response::new(Full::new(Bytes::from_static(
                br#"{"answers":{"tier":{"choice":"haiku","confidence":0.99}}}"#,
            ))))
        }
    }
    let mut config = read_config(
        &json!({"AUTOROUTER_EVALUATOR":"jev","TYPESAFE_API_KEY":"synthetic-key"}),
        false,
        std::path::Path::new("/synthetic"),
    )
    .unwrap();
    config.jev_timeout_ms = 0;
    config.cache_entries = 4;
    let evaluator = Arc::new(Evaluator {
        active: Arc::new(AtomicUsize::new(0)),
        calls: AtomicUsize::new(0),
        permits: Semaphore::new(0),
    });
    let classifier = Classifier::new(evaluator.clone(), &config);
    let documents: Vec<_> = (0..257).map(|index| Arc::new(JsDocument::parse(format!("{{\"model\":\"claude-sonnet-5-5\",\"messages\":[{{\"role\":\"user\",\"content\":\"synthetic capacity {index}\"}}]}}").as_bytes()).unwrap())).collect();
    let entered = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    let mut tokens = Vec::new();
    for index in 0..1024 {
        let document = documents[index % 256].clone();
        let config = config.clone();
        let classifier = classifier.clone();
        let entered = entered.clone();
        let token = CancellationToken::new();
        tokens.push(token.clone());
        tasks.push(tokio::spawn(async move {
            entered.fetch_add(1, Ordering::SeqCst);
            classifier.classify(&document, &config, &token).await
        }));
    }
    wait_for(|| {
        entered.load(Ordering::SeqCst) == 1024 && evaluator.active.load(Ordering::SeqCst) == 256
    })
    .await;
    for document in [&documents[0], &documents[256]] {
        let decision = classifier
            .classify(document, &config, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(decision.classifier_error, Some("capacity_exhausted"));
    }
    tokens[0].cancel();
    let first = tasks.remove(0).await.unwrap();
    assert!(matches!(first, Err(EvaluationError::Cancelled)));
    assert_eq!(
        evaluator.active.load(Ordering::SeqCst),
        256,
        "one waiter cannot cancel three peers"
    );
    for token in &tokens {
        token.cancel();
    }
    for task in tasks {
        assert!(matches!(
            task.await.unwrap(),
            Err(EvaluationError::Cancelled)
        ));
    }
    wait_for(|| evaluator.active.load(Ordering::SeqCst) == 0).await;
    assert_eq!(evaluator.calls.load(Ordering::SeqCst), 256);
    // Abandoned jobs must leave neither pending entries nor a cached result.
    evaluator.permits.add_permits(1);
    let decision = classifier
        .classify(&documents[0], &config, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(decision.source, "jev");
    assert_eq!(evaluator.calls.load(Ordering::SeqCst), 257);
    classifier.shutdown();
    wait_for(|| evaluator.active.load(Ordering::SeqCst) == 0).await;
}

#[test]
fn continuity_capacity_preserves_1000_active_tool_owners_then_recovers_retired_records() {
    use autorouter_core::turn_state::{
        ContinuationEvidence, Pin as TurnPin, Selection, ToolIdentity, TurnState,
    };
    let mut turns = TurnState::new(1000, 10);
    let model = autorouter_core::js_json::JsString::from("claude-sonnet-5-5");
    for index in 0..1000 {
        let key = format!("task-{index}");
        let keys = vec![key.clone(), format!("alias-{index}")];
        assert!(turns.select_exact(
            &keys,
            TurnPin::new(model.clone(), model.clone()),
            Selection {
                scope: "shared",
                request_id: Some(&key),
                sequence: index
            },
            0
        ));
        assert!(turns.complete_exact(
            &key,
            Some(&ContinuationEvidence {
                model: model.clone(),
                tools: vec![ToolIdentity {
                    id: format!("tool-{index}").into(),
                    model: model.clone()
                }]
            }),
            0
        ));
    }
    assert_eq!(turns.record_count(), 1000);
    assert_eq!(turns.alias_count(), 2000);
    for index in 0..1000 {
        let key = format!("task-{index}");
        let request = format!("pending-{index}");
        assert!(turns.select_exact(
            &[key],
            TurnPin::new(model.clone(), model.clone()),
            Selection {
                scope: "shared",
                request_id: Some(&request),
                sequence: index + 1000
            },
            100
        ));
    }
    assert_eq!(turns.attempt_count(), 1000);
    assert!(!turns.select_exact(
        &["task-0".into(), "refused-alias".into()],
        TurnPin::new(model.clone(), model.clone()),
        Selection {
            scope: "shared",
            request_id: Some("overflow"),
            sequence: 3000
        },
        100
    ));
    assert!(!turns.select_exact(
        &["new-task".into()],
        TurnPin::new(model.clone(), model.clone()),
        Selection {
            scope: "shared",
            request_id: Some("overflow"),
            sequence: 3000
        },
        100
    ));
    assert!(turns.get_exact("refused-alias", 100).is_none());
    for index in 0..1000 {
        let request = format!("pending-{index}");
        if index % 2 == 0 {
            assert!(!turns.complete_exact(&request, None, 100));
            assert_eq!(
                turns
                    .get_exact(&format!("task-{index}"), 100)
                    .unwrap()
                    .tools
                    .len(),
                1
            );
        } else {
            assert!(turns.complete_exact(
                &request,
                Some(&ContinuationEvidence {
                    model: model.clone(),
                    tools: Vec::new()
                }),
                100
            ));
        }
    }
    assert_eq!(turns.attempt_count(), 0);
    for index in 0..1000 {
        let key = format!("replacement-{index}");
        assert!(turns.select_exact(
            std::slice::from_ref(&key),
            TurnPin::new(model.clone(), model.clone()),
            Selection {
                scope: "shared",
                request_id: Some(&key),
                sequence: index + 3000
            },
            111
        ));
        assert!(turns.complete_exact(
            &key,
            Some(&ContinuationEvidence {
                model: model.clone(),
                tools: Vec::new()
            }),
            111
        ));
        assert!(turns.record_count() <= 1000);
        assert!(turns.alias_count() <= 2000);
    }
    for index in (0..1000).step_by(2) {
        assert_eq!(
            turns
                .get_exact(&format!("alias-{index}"), 111)
                .unwrap()
                .tools
                .len(),
            1,
            "active tools are independent of disposable cache/idle pressure"
        );
    }
    assert_eq!(turns.attempt_count(), 0);
}
