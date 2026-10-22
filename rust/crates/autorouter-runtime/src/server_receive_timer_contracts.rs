//! Parser-driven Gateway receive timers on controlled IO and a paused clock.
//! Manual scans use the production registry; no handler/timeout is fabricated.
use super::*;
use crate::http_receive::ScanRegistry;
use autorouter_core::config::read_config;
use autorouter_core::turn_state::{
    ContinuationEvidence, Pin as TaskPin, Selection, ToolIdentity, ToolOwner, TurnState,
};
use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Waker;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

const TOKEN: &str = "synthetic-local-token-1234567890";
const SESSION: &str = "timer-original-session";
const TASK: &str = "timer-original-task";
const RAW_TIMEOUT: &[u8] = b"HTTP/1.1 408 Request Timeout\r\nConnection: close\r\n\r\n";
const HEALTH_PART: &[u8] = b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer synthetic-local-token-1234567890\r\nConnection: close\r\n";
const BODY_PART: &[u8] = b"POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer synthetic-local-token-1234567890\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{";

type Driver = Pin<Box<dyn Future<Output = ()> + Send>>;

#[derive(Default)]
struct WireState {
    input: VecDeque<u8>,
    output: Vec<u8>,
    read_bytes: usize,
    pending_reads: usize,
    flushes: usize,
    drops: usize,
    read_waker: Option<Waker>,
}
struct ControlledIo(Arc<Mutex<WireState>>);
impl AsyncRead for ControlledIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut state = self.0.lock().unwrap();
        if out.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        if state.input.is_empty() {
            state.pending_reads += 1;
            state.read_waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let count = state.input.len().min(out.remaining());
        let bytes = state.input.drain(..count).collect::<Vec<_>>();
        out.put_slice(&bytes);
        state.read_bytes += count;
        Poll::Ready(Ok(()))
    }
}
impl AsyncWrite for ControlledIo {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut state = self.0.lock().unwrap();
        if state.output.len() + bytes.len() > 8192 {
            return Poll::Ready(Err(io::Error::other("synthetic response bound")));
        }
        state.output.extend_from_slice(bytes);
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.0.lock().unwrap().flushes += 1;
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
impl Drop for ControlledIo {
    fn drop(&mut self) {
        self.0.lock().unwrap().drops += 1;
    }
}

struct NeverTransport(AtomicUsize);
impl HttpTransport for NeverTransport {
    type ResponseBody = Full<Bytes>;
    async fn request(
        &self,
        _: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(HttpError::Network)
    }
}
struct SeededRouter {
    turns: Mutex<TurnState>,
    routes: AtomicUsize,
    releases: AtomicUsize,
    exact: AtomicUsize,
    closes: AtomicUsize,
}
impl SeededRouter {
    fn new() -> Arc<Self> {
        let mut turns = TurnState::default();
        assert!(turns.select_exact(
            &[TASK.to_owned()],
            TaskPin::new("claude-opus-5-5".into(), "claude-sonnet-5".into()),
            Selection {
                scope: SESSION,
                request_id: Some("original-confirmed"),
                sequence: 1
            },
            0
        ));
        assert!(turns.complete_exact(
            "original-confirmed",
            Some(&ContinuationEvidence {
                model: "claude-opus-5-5".into(),
                tools: vec![ToolIdentity {
                    id: "original-tool".into(),
                    model: "claude-opus-5-5".into()
                }],
            }),
            1
        ));
        Arc::new(Self {
            turns: Mutex::new(turns),
            routes: AtomicUsize::new(0),
            releases: AtomicUsize::new(0),
            exact: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
        })
    }
    fn assert_unchanged(&self) {
        assert_eq!(self.routes.load(Ordering::SeqCst), 0);
        assert_eq!(self.exact.load(Ordering::SeqCst), 0);
        let turns = self.turns.lock().unwrap();
        assert_eq!(turns.attempt_count(), 0);
        match turns.tool_owner_exact(SESSION, &["original-tool".into()]) {
            Some(ToolOwner::Owned { pin, .. }) => {
                assert_eq!(pin.model, JsString::from_scalar("claude-opus-5-5"))
            }
            _ => panic!("receive handling overwrote the confirmed tool owner"),
        }
    }
}
impl GatewayRouter for SeededRouter {
    async fn route(
        &self,
        _: Arc<JsDocument>,
        _: RouteOptions,
        _: &HeaderMap,
        _: &CancellationToken,
        _: &str,
    ) -> Result<Value, EvaluationError> {
        self.routes.fetch_add(1, Ordering::SeqCst);
        Err(EvaluationError::Network)
    }
    fn complete(&self, id: &str, evidence: &Value) -> bool {
        assert!(
            evidence.is_null(),
            "receive failures cannot confirm execution"
        );
        self.releases.fetch_add(1, Ordering::SeqCst);
        self.turns.lock().unwrap().complete(id, None, 2)
    }
    fn complete_exact(&self, _: &str, _: &CompletionEvidence) -> bool {
        self.exact.fetch_add(1, Ordering::SeqCst);
        false
    }
    fn shutdown(&self) {}
    async fn close(&self) {
        self.closes.fetch_add(1, Ordering::SeqCst);
    }
}

struct Fixture {
    driver: Option<Driver>,
    done: bool,
    token: CancellationToken,
    request_parent: CancellationToken,
    scans: Arc<ScanRegistry>,
    registry: CompletionRegistry,
    wire: Arc<Mutex<WireState>>,
    router: Arc<SeededRouter>,
    transport: Arc<NeverTransport>,
    gateway_weak: std::sync::Weak<Gateway<NeverTransport, SeededRouter>>,
    statuses: Arc<Mutex<Vec<Value>>>,
    records: Arc<Mutex<Vec<Value>>>,
    origin: Instant,
}
impl Fixture {
    fn new() -> Self {
        let config = read_config(
            &json!({"AUTOROUTER_EVALUATOR":"ollama",
            "AUTOROUTER_AUTH_MODE":"api-key","AUTOROUTER_TOKEN":TOKEN}),
            false,
            std::path::Path::new("."),
        )
        .unwrap();
        let router = SeededRouter::new();
        let transport = Arc::new(NeverTransport(AtomicUsize::new(0)));
        let statuses = Arc::new(Mutex::new(Vec::new()));
        let records = Arc::new(Mutex::new(Vec::new()));
        let observed = statuses.clone();
        let saved = records.clone();
        let gateway = Gateway::with_router(
            config,
            transport.clone(),
            router.clone(),
            EventSinks {
                status: Some(Arc::new(move |row| {
                    let mut rows = observed.lock().unwrap();
                    assert!(rows.len() < 16);
                    rows.push(row.to_serde_observation_lossy());
                })),
                record: Some(Arc::new(move |row| {
                    let mut rows = saved.lock().unwrap();
                    assert!(rows.len() < 4);
                    rows.push(row);
                })),
                ..Default::default()
            },
        )
        .unwrap();
        let gateway_weak = Arc::downgrade(&gateway);
        let wire = Arc::new(Mutex::new(WireState::default()));
        let token = CancellationToken::new();
        let request_parent = CancellationToken::new();
        let registry = CompletionRegistry::new(16);
        let scans = Arc::new(ScanRegistry::new());
        let origin = Instant::now();
        // Accepted registration precedes the first parser poll and stays owned
        // by the same connection driver that the listener uses.
        let receipt = scans.register(registry.clone(), request_parent.clone());
        let driver = Box::pin(gateway.serve_connection_with_receipt(
            ControlledIo(wire.clone()),
            registry.clone(),
            token.clone(),
            receipt,
            request_parent.clone(),
        ));
        Self {
            driver: Some(driver),
            done: false,
            token,
            request_parent,
            scans,
            registry,
            wire,
            router,
            transport,
            gateway_weak,
            statuses,
            records,
            origin,
        }
    }
    fn poll(&mut self) {
        if self.done {
            return;
        }
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        // Bounded direct polls cannot advance virtual time. Every checkpoint
        // below also verifies actual read/output/status state, not poll count.
        for _ in 0..64 {
            if self
                .driver
                .as_mut()
                .unwrap()
                .as_mut()
                .poll(&mut cx)
                .is_ready()
            {
                self.done = true;
                break;
            }
        }
    }
    fn send(&mut self, bytes: &[u8]) {
        let wake = {
            let mut state = self.wire.lock().unwrap();
            assert!(state.input.is_empty());
            state.input.extend(bytes.iter().copied());
            state.read_waker.take()
        };
        if let Some(waker) = wake {
            waker.wake();
        }
        self.poll();
        assert!(
            self.wire.lock().unwrap().input.is_empty(),
            "actual parser must consume supplied bytes"
        );
    }
    async fn at(&mut self, millis: u64) {
        let target = self.origin + Duration::from_millis(millis);
        assert!(
            Instant::now() <= target,
            "only declared advances are permitted"
        );
        tokio::time::advance(target - Instant::now()).await;
        assert_eq!(Instant::now(), target);
        self.poll();
    }
    fn scan(&mut self) {
        // Scan uses the same coherent paused clock as the parser receipt.
        self.scans.scan(Instant::now());
        self.poll();
    }
    fn output(&self) -> Vec<u8> {
        self.wire.lock().unwrap().output.clone()
    }
    fn assert_held(&self) {
        assert!(!self.done);
        let state = self.wire.lock().unwrap();
        assert!(state.pending_reads > 0);
        assert_eq!(state.drops, 0);
        assert!(state.output.is_empty());
        assert_eq!(self.router.routes.load(Ordering::SeqCst), 0);
        assert_eq!(self.transport.0.load(Ordering::SeqCst), 0);
        self.router.assert_unchanged();
    }
    fn assert_started(&self) {
        let statuses = self.statuses.lock().unwrap();
        assert_eq!(
            statuses
                .iter()
                .filter(|row| row["event"] == "request_start")
                .count(),
            1
        );
    }
    async fn finish(mut self, expected_releases: usize) {
        self.token.cancel();
        self.poll();
        assert!(self.done);
        drop(self.driver.take());
        assert_eq!(self.wire.lock().unwrap().drops, 1);
        assert_eq!(self.registry.pending(), 0);
        // Only live owned connection registrations may remain in this map.
        assert_eq!(self.scans.len(), 0);
        assert!(self.gateway_weak.upgrade().is_none());
        self.router.assert_unchanged();
        assert_eq!(
            self.router.releases.load(Ordering::SeqCst),
            expected_releases
        );
        assert_eq!(self.transport.0.load(Ordering::SeqCst), 0);
        {
            let records = self.records.lock().unwrap();
            assert_eq!(records.len(), expected_releases);
            assert!(
                records
                    .iter()
                    .all(|row| row["completion_confirmed"] == false)
            );
        }
        self.router.close().await;
        assert_eq!(self.router.closes.load(Ordering::SeqCst), 1);
        assert_eq!(Arc::strong_count(&self.router), 1);
        assert_eq!(Arc::strong_count(&self.transport), 1);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.token.cancel();
        self.request_parent.cancel();
        // This fixture spawns no task. Dropping its actual connection future
        // synchronously drops the controlled IO/registration on assertion panic.
        drop(self.driver.take());
    }
}

fn assert_entity(wire: &[u8], status: u16, expected: Value) {
    assert!(wire.len() <= 8192);
    let split = wire
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response head");
    let head = std::str::from_utf8(&wire[..split]).unwrap();
    assert!(head.starts_with(&format!("HTTP/1.1 {status} ")));
    let mut length = None;
    let mut chunked = false;
    for line in head.split("\r\n").skip(1) {
        let (name, value) = line.split_once(':').expect("header field");
        if name.eq_ignore_ascii_case("content-length") {
            assert!(
                length
                    .replace(value.trim().parse::<usize>().unwrap())
                    .is_none()
            );
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            assert!(!chunked);
            assert_eq!(value.trim(), "chunked");
            chunked = true;
        }
    }
    let body = &wire[split + 4..];
    let mut entity = Vec::new();
    if let Some(length) = length {
        assert!(!chunked);
        assert_eq!(body.len(), length);
        entity.extend_from_slice(body);
    } else {
        assert!(chunked);
        let mut rest = body;
        loop {
            let end = rest
                .windows(2)
                .position(|w| w == b"\r\n")
                .expect("chunk size");
            let size =
                usize::from_str_radix(std::str::from_utf8(&rest[..end]).unwrap(), 16).unwrap();
            rest = &rest[end + 2..];
            if size == 0 {
                assert_eq!(rest, b"\r\n");
                break;
            }
            assert!(size <= 8192 && rest.len() >= size + 2);
            entity.extend_from_slice(&rest[..size]);
            assert_eq!(&rest[size..size + 2], b"\r\n");
            rest = &rest[size + 2..];
        }
    }
    assert_eq!(serde_json::from_slice::<Value>(&entity).unwrap(), expected);
}

#[tokio::test(start_paused = true)]
async fn accepted_blank_connection_expires_on_default_first_scan_with_exact_raw408() {
    let mut fixture = Fixture::new();
    fixture.poll();
    fixture.assert_held();
    fixture.at(29999).await;
    fixture.assert_held();
    fixture.at(30000).await;
    fixture.scan();
    assert_eq!(fixture.output(), RAW_TIMEOUT);
    assert!(fixture.done);
    assert!(fixture.statuses.lock().unwrap().is_empty());
    fixture.finish(0).await;
}

#[tokio::test(start_paused = true)]
async fn late_health_header_completes_before_first_scan_instead_of_hard10s_close() {
    let mut fixture = Fixture::new();
    fixture.poll();
    fixture.at(1000).await;
    fixture.send(HEALTH_PART);
    fixture.assert_held();
    fixture.at(11000).await;
    fixture.assert_held();
    fixture.at(20000).await;
    fixture.send(b"\r\n");
    let completed = fixture.output();
    assert_entity(&completed, 200, json!({"status":"ok"}));
    assert!(fixture.done);
    fixture.at(30000).await;
    fixture.scan();
    assert_eq!(fixture.output(), completed);
    fixture.finish(0).await;
}

#[tokio::test(start_paused = true)]
async fn body_completion_at35s_before_second_scan_retains_full_invalid_model400() {
    let mut fixture = Fixture::new();
    fixture.poll();
    fixture.at(1000).await;
    fixture.send(BODY_PART);
    fixture.assert_started();
    fixture.assert_held();
    fixture.at(30000).await;
    fixture.scan();
    fixture.assert_held();
    fixture.at(31000).await;
    fixture.assert_held();
    fixture.at(35000).await;
    fixture.send(b"}");
    assert_entity(
        &fixture.output(),
        400,
        json!({"type":"error","error":{"type":"api_error","message":"Invalid Messages API request shape: model"}}),
    );
    assert!(fixture.done);
    fixture.finish(1).await;
}

#[tokio::test(start_paused = true)]
async fn held_body_expires_at_scan60_with_raw408_and_no_false_continuity() {
    let mut fixture = Fixture::new();
    fixture.poll();
    fixture.at(1000).await;
    fixture.send(BODY_PART);
    fixture.assert_started();
    fixture.assert_held();
    fixture.at(30000).await;
    fixture.scan();
    fixture.assert_held();
    fixture.at(31000).await;
    fixture.assert_held();
    fixture.at(35000).await;
    fixture.assert_held();
    fixture.at(60000).await;
    fixture.scan();
    assert_eq!(fixture.output(), RAW_TIMEOUT);
    assert!(fixture.done);
    fixture.finish(1).await;
}
