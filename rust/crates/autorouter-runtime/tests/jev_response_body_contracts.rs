//! Real NativeHttpClient/socket evidence for frozen router-concurrency #9.
//!
//! All eight failure calls use the original live 20 ms deadline and 65,537-byte
//! overflow. A separate five-second fixture ceiling is never evaluator policy.
//! Native body Drop + peer EOF are observable here; JavaScript's never-settling
//! reader.cancel() Promise and the private pending Map are not reinterpreted as
//! equivalent observations. Existing classifier units cover the private counts.
//! The source test's whole-definition two-second wrapper is not a native
//! performance claim made by these independently bounded socket fixtures.
use autorouter_core::{
    config::{RouterConfig, read_config},
    contracts::{ClassifierDecision, ClassifierError, DecisionSource},
    js_json::JsDocument,
};
use autorouter_runtime::{
    bounded_json::DECISION_RESPONSE_LIMIT,
    classifier::{Classifier, Limits},
    evaluator::EvaluationError,
    http_client::{HttpError, HttpTransport, NativeHttpClient},
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    Request, Response,
    body::{Body, Frame, Incoming, SizeHint},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    path::Path,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

const CASES: &str = include_str!("../../../parity/cases/router-concurrency-contracts.jsonl");
const CEILING: Duration = Duration::from_secs(5);
const DEADLINE_MS: u64 = 20;
const PRIVATE_MALFORMED: &[u8] = b"PRIVATE malformed JSON";
const ANSWER: &[u8] = br#"{"answers":{"tier":{"choice":"sonnet","confidence":0.9}}}"#;

// Compare only the 28 frozen source/error/privacy/call-count observations.
// #5 (private Map size) and #6 (JS cancel callback/Promise) remain separate.
struct Frozen(BTreeMap<usize, VecDeque<Value>>);
impl Frozen {
    fn new() -> Self {
        assert_eq!(
            format!("{:x}", Sha256::digest(CASES)),
            "ef5ee89a0b9407ff2e632d4355e7c67653fb578ac7f837069e63d986f0f70f0c"
        );
        let case = CASES
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|case| case["number"] == 9)
            .unwrap();
        assert_eq!(case["id"], "test/router-concurrency.test.mjs#9");
        assert_eq!(
            case["source_sha256"],
            "69c64f16214e76975fd5bd34f267e064efef003a5830c9e23119262cb0ccc39d"
        );
        let mut values = BTreeMap::<usize, VecDeque<Value>>::new();
        for observed in case["observations"].as_array().unwrap() {
            let index = observed["assertion_id"]
                .as_str()
                .unwrap()
                .rsplit('-')
                .next()
                .unwrap()
                .parse::<usize>()
                .unwrap();
            if index <= 4 {
                assert_eq!(observed["arguments"][0]["kind"], "json");
                values
                    .entry(index)
                    .or_default()
                    .push_back(observed["arguments"][0]["value"].clone());
            }
        }
        assert_eq!(
            values.values().map(VecDeque::len).collect::<Vec<_>>(),
            [8, 8, 8, 4]
        );
        Self(values)
    }
    fn observe(&mut self, index: usize, actual: Value) {
        let expected = self.0.get_mut(&index).unwrap().pop_front().unwrap();
        assert_eq!(actual, expected, "frozen #9 assertion {index}");
    }
    fn finish(self) {
        assert!(self.0.values().all(VecDeque::is_empty));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Oversize,
    Malformed,
    Stalled,
    HttpError,
    Success,
}
impl Mode {
    fn error(self) -> ClassifierError {
        match self {
            Self::Oversize | Self::Malformed => ClassifierError::InvalidResponse,
            Self::Stalled => ClassifierError::Timeout,
            Self::HttpError => ClassifierError::HttpError,
            Self::Success => panic!("success is not a failure schedule"),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Receipt {
    native_headers: bool,
    wire_headers: bool,
    body_pending: bool,
    body_bytes: usize,
    body_eof: bool,
    body_drops: usize,
    peer_eof: bool,
    warm_connection: bool,
}
struct Observation(watch::Sender<Receipt>);
impl Observation {
    fn new() -> Self {
        Self(watch::channel(Receipt::default()).0)
    }
    fn update(&self, change: impl FnOnce(&mut Receipt)) {
        self.0.send_modify(change);
    }
    fn snapshot(&self) -> Receipt {
        self.0.borrow().clone()
    }
    async fn until(&self, predicate: impl Fn(&Receipt) -> bool) {
        let mut receiver = self.0.subscribe();
        bounded(async {
            loop {
                if predicate(&receiver.borrow_and_update()) {
                    return;
                }
                receiver.changed().await.unwrap();
            }
        })
        .await;
    }
}
struct State {
    modes: Vec<Mode>,
    records: Vec<Arc<Observation>>,
    requests: AtomicUsize,
    wire_requests: AtomicUsize,
    primes: AtomicUsize,
    active_requests: AtomicUsize,
    active_bodies: AtomicUsize,
    active_sockets: AtomicUsize,
}
impl State {
    fn new(modes: Vec<Mode>) -> Self {
        Self {
            records: modes.iter().map(|_| Arc::new(Observation::new())).collect(),
            modes,
            requests: AtomicUsize::new(0),
            wire_requests: AtomicUsize::new(0),
            primes: AtomicUsize::new(0),
            active_requests: AtomicUsize::new(0),
            active_bodies: AtomicUsize::new(0),
            active_sockets: AtomicUsize::new(0),
        }
    }
    fn drained(&self) {
        assert_eq!(self.active_requests.load(Ordering::SeqCst), 0);
        assert_eq!(self.active_bodies.load(Ordering::SeqCst), 0);
    }
}
struct Active<'a>(&'a AtomicUsize);
impl<'a> Active<'a> {
    fn new(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter)
    }
}
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
struct TrackedBody {
    incoming: Incoming,
    state: Arc<State>,
    observation: Arc<Observation>,
}
impl Body for TrackedBody {
    type Data = Bytes;
    type Error = hyper::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, hyper::Error>>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.incoming).poll_frame(cx);
        this.observation.update(|receipt| match &result {
            Poll::Pending => receipt.body_pending = true,
            Poll::Ready(None) => receipt.body_eof = true,
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(bytes) = frame.data_ref() {
                    receipt.body_bytes += bytes.len();
                }
            }
            Poll::Ready(Some(Err(_))) => {}
        });
        result
    }
    fn is_end_stream(&self) -> bool {
        self.incoming.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.incoming.size_hint()
    }
}
impl Drop for TrackedBody {
    fn drop(&mut self) {
        self.observation.update(|receipt| receipt.body_drops += 1);
        self.state.active_bodies.fetch_sub(1, Ordering::SeqCst);
    }
}
struct ObservedNative {
    native: NativeHttpClient,
    state: Arc<State>,
}
impl HttpTransport for ObservedNative {
    type ResponseBody = TrackedBody;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        let _active = Active::new(&self.state.active_requests);
        let index = self.state.requests.fetch_add(1, Ordering::SeqCst);
        let observation = self
            .state
            .records
            .get(index)
            .expect("extra evaluator call")
            .clone();
        // The actual Hyper/Rustls client owns request I/O, body decoding and
        // connection cancellation; the wrapper only observes its real body.
        let response = self.native.request(request).await?;
        observation.update(|receipt| receipt.native_headers = true);
        self.state.active_bodies.fetch_add(1, Ordering::SeqCst);
        Ok(response.map(|incoming| TrackedBody {
            incoming,
            state: self.state.clone(),
            observation,
        }))
    }
}
async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(CEILING, future)
        .await
        .expect("independent loopback fixture ceiling exceeded")
}

// Bounded raw HTTP/1 fixture: request payloads and credentials are discarded,
// never retained in receipts, formatted into assertions or written to logs.
async fn read_request(socket: &mut TcpStream) -> Result<Option<bool>, &'static str> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let count = socket
            .read(&mut chunk)
            .await
            .map_err(|_| "request read failed")?;
        if count == 0 && bytes.is_empty() {
            return Ok(None);
        }
        if count == 0 || bytes.len() + count > 65_536 {
            return Err("request incomplete or over fixture bound");
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers =
        std::str::from_utf8(&bytes[..header_end]).map_err(|_| "invalid request headers")?;
    let prime = headers.starts_with("GET /ready HTTP/1.1\r\n");
    if !prime && !headers.starts_with("POST /v1/systemone HTTP/1.1\r\n") {
        return Err("unexpected fixture endpoint");
    }
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    if length > 65_536 || header_end + length > 65_536 {
        return Err("request body over fixture bound");
    }
    while bytes.len() < header_end + length {
        let remaining = (header_end + length - bytes.len()).min(chunk.len());
        let count = socket
            .read(&mut chunk[..remaining])
            .await
            .map_err(|_| "request body read failed")?;
        if count == 0 {
            return Err("request body incomplete");
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    if bytes.len() != header_end + length {
        return Err("unexpected pipelined fixture request");
    }
    Ok(Some(prime))
}
async fn serve_socket(mut socket: TcpStream, state: Arc<State>) -> Result<(), &'static str> {
    let _active = Active::new(&state.active_sockets);
    socket
        .set_nodelay(true)
        .map_err(|_| "fixture TCP setup failed")?;
    let mut primed = false;
    loop {
        let Some(prime) = read_request(&mut socket).await? else {
            return Ok(());
        };
        if prime {
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .map_err(|_| "readiness response failed")?;
            socket.flush().await.map_err(|_| "readiness flush failed")?;
            state.primes.fetch_add(1, Ordering::SeqCst);
            primed = true;
            continue;
        }
        let index = state.wire_requests.fetch_add(1, Ordering::SeqCst);
        let mode = *state
            .modes
            .get(index)
            .ok_or("extra wire evaluator request")?;
        let observation = &state.records[index];
        let complete = match mode {
            Mode::Malformed => Some(PRIVATE_MALFORMED),
            Mode::Success => Some(ANSWER),
            _ => None,
        };
        let status = if mode == Mode::HttpError {
            "503 Service Unavailable"
        } else {
            "200 OK"
        };
        let framing = complete.map_or_else(
            || "Transfer-Encoding: chunked\r\n".to_owned(),
            |body| format!("Content-Length: {}\r\n", body.len()),
        );
        socket
            .write_all(
                format!("HTTP/1.1 {status}\r\n{framing}Connection: close\r\n\r\n").as_bytes(),
            )
            .await
            .map_err(|_| "response header write failed")?;
        socket
            .flush()
            .await
            .map_err(|_| "response header flush failed")?;
        observation.update(|receipt| {
            receipt.wire_headers = true;
            receipt.warm_connection = primed;
        });
        if let Some(body) = complete {
            socket
                .write_all(body)
                .await
                .map_err(|_| "finite response write failed")?;
        } else if mode == Mode::Oversize {
            // No Content-Length early exit, compressed substitute, or final
            // chunk: the native body reader must actually consume 65,537 bytes.
            let mut chunk = Vec::with_capacity(65_548);
            chunk.extend_from_slice(b"10001\r\n");
            chunk.resize(chunk.len() + 65_537, 0);
            chunk.extend_from_slice(b"\r\n");
            socket
                .write_all(&chunk)
                .await
                .map_err(|_| "overflow response write failed")?;
        }
        socket
            .flush()
            .await
            .map_err(|_| "response body flush failed")?;
        // Incomplete chunked responses stay open until the client drops them.
        // A fixture shutdown is not admitted as the peer-cancellation witness.
        let mut extra = [0_u8; 1];
        match socket.read(&mut extra).await {
            Ok(0) => observation.update(|receipt| receipt.peer_eof = true),
            _ => return Err("expected native peer EOF after response disposal"),
        }
        return Ok(());
    }
}
struct Fixture {
    endpoint: String,
    state: Arc<State>,
    stop: CancellationToken,
    server: Option<JoinHandle<()>>,
}
impl Fixture {
    async fn start(modes: Vec<Mode>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(State::new(modes));
        let stop = CancellationToken::new();
        let server_state = state.clone();
        let server_stop = stop.clone();
        let server = tokio::spawn(async move {
            let mut children = JoinSet::new();
            loop {
                tokio::select! {
                    _ = server_stop.cancelled() => break,
                    peer = listener.accept() => {
                        let (socket, _) = peer.unwrap();
                        let state = server_state.clone();
                        children.spawn(async move { bounded(serve_socket(socket, state)).await });
                    }
                    result = children.join_next(), if !children.is_empty() => {
                        result.unwrap().unwrap().unwrap();
                    }
                }
            }
            drop(listener);
            while let Some(result) = children.join_next().await {
                result.unwrap().unwrap();
            }
        });
        Self {
            endpoint,
            state,
            stop,
            server: Some(server),
        }
    }
    async fn prime(&self, native: &NativeHttpClient) {
        let response = bounded(
            native.request(
                Request::builder()
                    .uri(format!("{}/ready", self.endpoint))
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), 200);
        assert!(
            bounded(response.into_body().collect())
                .await
                .unwrap()
                .to_bytes()
                .is_empty()
        );
    }
    async fn finish(mut self) {
        self.stop.cancel();
        // Keep the abort-on-drop owner installed until the join succeeds: a
        // fixture ceiling/panic must not detach a server with live children.
        bounded(self.server.as_mut().unwrap()).await.unwrap();
        drop(self.server.take());
        self.state.drained();
        assert_eq!(self.state.active_sockets.load(Ordering::SeqCst), 0);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(server) = &self.server {
            server.abort();
        }
    }
}

fn config(endpoint: &str) -> RouterConfig {
    let mut config = read_config(
        &json!({"AUTOROUTER_EVALUATOR":"jev", "TYPESAFE_API_KEY":"synthetic-key", "AUTOROUTER_AUTH_MODE":"subscription"}),
        false,
        Path::new("/synthetic"),
    )
    .unwrap();
    config.jev_endpoint = format!("{endpoint}/v1/systemone");
    config.jev_timeout_ms = DEADLINE_MS;
    config
}
fn document() -> JsDocument {
    JsDocument::parse(
        br#"{"model":"claude-haiku-4-5-20251001","max_tokens":16,"messages":[{"role":"user","content":"Synthetic task"}]}"#,
    )
    .unwrap()
}
fn transport(state: Arc<State>) -> Arc<ObservedNative> {
    Arc::new(ObservedNative {
        native: NativeHttpClient::new().unwrap(),
        state,
    })
}
fn classifier(transport: Arc<ObservedNative>, config: &RouterConfig) -> Classifier<ObservedNative> {
    Classifier::with_limits(
        transport,
        config,
        Limits {
            pending: 1,
            subscribers: 1,
        },
    )
    .unwrap()
}
async fn disposed(state: &State, index: usize, mode: Mode) {
    let observation = &state.records[index];
    observation.until(|receipt| receipt.peer_eof).await;
    let receipt = observation.snapshot();
    assert!(
        receipt.native_headers && receipt.wire_headers,
        "{mode:?}: {receipt:?}"
    );
    assert!(
        receipt.warm_connection,
        "unprimed {mode:?} socket: {receipt:?}"
    );
    assert_eq!(receipt.body_drops, 1, "{mode:?}: {receipt:?}");
    match mode {
        Mode::Oversize => {
            assert_eq!(receipt.body_bytes, 65_537, "{receipt:?}");
            assert!(!receipt.body_eof, "{receipt:?}");
        }
        Mode::Malformed | Mode::Success => {
            assert!(receipt.body_eof, "{receipt:?}");
        }
        Mode::Stalled => {
            assert!(receipt.body_pending, "{receipt:?}");
            assert_eq!(receipt.body_bytes, 0);
            assert!(!receipt.body_eof, "{receipt:?}");
        }
        Mode::HttpError => {
            assert_eq!(receipt.body_bytes, 0);
            assert!(!receipt.body_eof, "{receipt:?}");
        }
    }
    state.drained();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_frozen_jev_body_failures_retry_over_real_native_sockets_with_live_deadlines() {
    assert_eq!(DECISION_RESPONSE_LIMIT, 65_536);
    let mut frozen = Frozen::new();
    for mode in [
        Mode::Oversize,
        Mode::Malformed,
        Mode::Stalled,
        Mode::HttpError,
    ] {
        let fixture = Fixture::start(vec![mode, mode, Mode::Success]).await;
        let state = fixture.state.clone();
        let transport = transport(state.clone());
        let config = config(&fixture.endpoint);
        let classifier = classifier(transport.clone(), &config);
        let body = document();
        for attempt in 0..2 {
            // Establish actual listener/client/socket readiness before the
            // unchanged 20 ms evaluation clock starts. Readiness calls are not
            // evaluator calls and do not consume the two frozen attempts.
            fixture.prime(&transport.native).await;
            let started = Instant::now();
            let result = bounded(classifier.classify(&body, &config, &CancellationToken::new()))
                .await
                .unwrap();
            assert_eq!(
                result.classifier_error,
                Some(mode.error()),
                "{mode:?}, attempt {attempt}, live elapsed {:?}, receipt {:?}",
                started.elapsed(),
                state.records[attempt].snapshot()
            );
            frozen.observe(1, json!(result.source));
            frozen.observe(2, json!(result.classifier_error));
            frozen.observe(
                3,
                json!(!serde_json::to_string(&result).unwrap().contains("PRIVATE")),
            );
            assert_eq!(
                result.classifier_status,
                (mode == Mode::HttpError).then_some(503)
            );
            disposed(&state, attempt, mode).await;
        }
        frozen.observe(4, json!(state.requests.load(Ordering::SeqCst)));
        assert_eq!(state.wire_requests.load(Ordering::SeqCst), 2);
        // A third, successful same-key call is a positive capacity/no-failure-
        // cache control, not a substitute for private pending Map introspection.
        fixture.prime(&transport.native).await;
        let result = bounded(classifier.classify(&body, &config, &CancellationToken::new()))
            .await
            .unwrap();
        assert_eq!(result.source, DecisionSource::Jev);
        assert_eq!(result.classifier_error, None);
        assert!(!serde_json::to_string(&result).unwrap().contains("PRIVATE"));
        disposed(&state, 2, Mode::Success).await;
        assert_eq!(state.requests.load(Ordering::SeqCst), 3);
        assert_eq!(state.wire_requests.load(Ordering::SeqCst), 3);
        assert_eq!(state.primes.load(Ordering::SeqCst), 3);
        bounded(classifier.close()).await;
        drop(classifier);
        drop(transport);
        fixture.finish().await;
    }
    frozen.finish();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caller_cancellation_after_real_headers_drops_body_and_releases_the_single_slot() {
    let fixture = Fixture::start(vec![Mode::Stalled, Mode::Success]).await;
    let state = fixture.state.clone();
    let transport = transport(state.clone());
    let config = config(&fixture.endpoint);
    let classifier = classifier(transport.clone(), &config);
    fixture.prime(&transport.native).await;
    let cancellation = CancellationToken::new();
    let result = {
        let body = document();
        let evaluation = classifier.classify(&body, &config, &cancellation);
        tokio::pin!(evaluation);
        tokio::select! {
            biased;
            result = &mut evaluation => panic!("evaluation ended before cancellation barrier: {:?}", result.err()),
            _ = state.records[0].until(|receipt| receipt.native_headers && receipt.body_pending) => {}
        }
        cancellation.cancel();
        bounded(evaluation).await
    };
    assert!(matches!(result, Err(EvaluationError::Cancelled)));
    disposed(&state, 0, Mode::Stalled).await;
    fixture.prime(&transport.native).await;
    let body = document();
    let result: ClassifierDecision =
        bounded(classifier.classify(&body, &config, &CancellationToken::new()))
            .await
            .unwrap();
    assert_eq!(result.source, DecisionSource::Jev);
    disposed(&state, 1, Mode::Success).await;
    assert_eq!(state.requests.load(Ordering::SeqCst), 2);
    assert_eq!(state.wire_requests.load(Ordering::SeqCst), 2);
    bounded(classifier.close()).await;
    drop(classifier);
    drop(transport);
    fixture.finish().await;
}
