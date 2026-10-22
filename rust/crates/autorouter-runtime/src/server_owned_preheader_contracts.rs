//! Deterministic service/error ordering with an injected private transport.
//! The reservation is real vendor ownership metadata, not a socket or TLS proof.
use super::*;
use crate::tls_roots::openssl_spike::gateway_intent::Disconnect;
use crate::tls_roots::openssl_spike::gateway_terminal::Event as TerminalEvent;
use hyper_util::client::legacy::connect::{
    CaptureAssignment, Connected, Http1Reservation, bind_http1_reservation,
    capture_http1_assignment,
};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

#[derive(Default)]
struct OrderingState {
    ready: AtomicBool,
    active_requests: AtomicUsize,
    request_drops: AtomicUsize,
    calls: AtomicUsize,
    rejected_admission: AtomicBool,
    error_before_token: AtomicBool,
    action_finished: AtomicBool,
    claimed: AtomicBool,
    token: Mutex<Option<CancellationToken>>,
    terminal: Mutex<Option<RequestTerminal>>,
    trace: Mutex<Vec<&'static str>>,
    retired_reusable: Mutex<Vec<bool>>,
}

struct TokenRouter {
    original: Arc<TrackedRouter>,
    state: Arc<OrderingState>,
}
impl GatewayRouter for TokenRouter {
    async fn route(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> Result<Value, EvaluationError> {
        assert!(
            self.state
                .token
                .lock()
                .unwrap()
                .replace(cancellation.clone())
                .is_none()
        );
        self.original
            .route(document, options, headers, cancellation, search)
            .await
    }
    fn complete(&self, id: &str, evidence: &Value) -> bool {
        self.original.complete(id, evidence)
    }
    fn complete_exact(&self, id: &str, evidence: &CompletionEvidence) -> bool {
        self.original.complete_exact(id, evidence)
    }
    fn shutdown(&self) {}
    async fn close(&self) {}
}

struct HeldAction {
    entered: tokio::sync::Notify,
    entered_send: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}
struct FailureWorker {
    entered: mpsc::Receiver<()>,
    release: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<bool>>,
}
impl FailureWorker {
    fn new() -> (Arc<HeldAction>, Self) {
        let (entered_send, entered) = mpsc::channel();
        let (release, receive) = mpsc::channel();
        (
            Arc::new(HeldAction {
                entered: tokio::sync::Notify::new(),
                entered_send: Mutex::new(Some(entered_send)),
                release: Mutex::new(Some(receive)),
            }),
            Self {
                entered,
                release: Some(release),
                thread: None,
            },
        )
    }
    fn start(&mut self, terminal: RequestTerminal, cause: FailureCause) {
        assert!(self.thread.is_none());
        self.thread = Some(std::thread::spawn(move || terminal.fail(cause)));
        // The acknowledgment follows Notify publication, so returning also
        // establishes that the injected transport is eligible to resume. No
        // Tokio work is needed to publish either signal under paused time.
        self.entered
            .recv_timeout(BARRIER)
            .expect("first failure did not enter its action");
    }
    fn finish(&mut self) {
        self.release
            .take()
            .unwrap()
            .send(())
            .expect("held action disappeared");
        assert!(
            self.thread
                .take()
                .unwrap()
                .join()
                .expect("failure worker panicked")
        );
    }
}
impl Drop for FailureWorker {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct OwnedPendingRequest {
    request: Option<Request<Full<Bytes>>>,
    reservation: Option<Http1Reservation>,
    state: Arc<OrderingState>,
}
impl Drop for OwnedPendingRequest {
    fn drop(&mut self) {
        if let Some(reservation) = self.reservation.take() {
            let reusable = reservation.retire();
            self.state.retired_reusable.lock().unwrap().push(reusable);
        }
        drop(self.request.take());
        self.state.trace.lock().unwrap().push("request_drop");
        self.state.request_drops.fetch_add(1, Ordering::SeqCst);
        self.state.active_requests.fetch_sub(1, Ordering::SeqCst);
    }
}

struct OrderingTransport {
    capable: bool,
    pending_deadline: bool,
    state: Arc<OrderingState>,
    held: Arc<HeldAction>,
}
impl HttpTransport for OrderingTransport {
    type ResponseBody = Full<Bytes>;
    fn request_terminal_abort(&self) -> bool {
        self.capable
    }
    async fn request(
        &self,
        mut request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        assert_eq!(self.state.calls.fetch_add(1, Ordering::SeqCst), 0);
        assert_eq!(request.method(), Method::POST);
        assert_eq!(request.uri().path(), "/v1/messages");
        let body = request.body().clone().collect().await.unwrap().to_bytes();
        assert_eq!(body.as_ref(), br#"{"model":"claude-sonnet-5","max_tokens":100,"messages":[{"role":"user","content":"replace"}]}"#);
        assert_eq!(
            request.headers()["content-length"].to_str().unwrap(),
            body.len().to_string()
        );
        let terminal = request
            .extensions()
            .get::<RequestTerminal>()
            .unwrap()
            .clone();
        let capture: Option<Arc<CaptureAssignment>> = self
            .pending_deadline
            .then(|| Arc::new(capture_http1_assignment(&mut request)));
        let state = self.state.clone();
        let held = self.held.clone();
        terminal
            .bind_transport_action(move |cause| {
                state.trace.lock().unwrap().push("action_enter");
                // Publish the async wake before acknowledging entry to the
                // driving task. Tokio yields cannot force this OS worker to
                // run again if it is descheduled after the acknowledgment.
                held.entered.notify_one();
                held.entered_send
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap()
                    .send(())
                    .unwrap();
                let release = held.release.lock().unwrap().take().unwrap();
                release
                    .recv_timeout(BARRIER)
                    .expect("test did not release held action");
                if let Some(capture) = capture {
                    assert_eq!(cause, FailureCause::Deadline);
                    let claim = capture.assignment().unwrap().claim_abort();
                    state.claimed.store(claim.is_some(), Ordering::SeqCst);
                    state.trace.lock().unwrap().push("claim");
                    drop(claim);
                }
                state.trace.lock().unwrap().push("action_finish");
                state.action_finished.store(true, Ordering::SeqCst);
            })
            .unwrap();
        let reservation = if self.pending_deadline {
            let reservation = bind_http1_reservation(&mut request, &Connected::new())
                .unwrap()
                .unwrap();
            terminal.admit_transport().unwrap();
            Some(reservation)
        } else {
            None
        };
        self.state.active_requests.fetch_add(1, Ordering::SeqCst);
        let _owned = OwnedPendingRequest {
            request: Some(request),
            reservation,
            state: self.state.clone(),
        };
        *self.state.terminal.lock().unwrap() = Some(terminal.clone());
        self.state.ready.store(true, Ordering::SeqCst);
        if self.pending_deadline {
            std::future::pending::<()>().await;
        } else {
            self.held.entered.notified().await;
            assert_eq!(
                terminal.failure_cause(),
                Some(FailureCause::Downstream(Disconnect::ReadEof))
            );
            self.state
                .rejected_admission
                .store(terminal.admit_transport().is_err(), Ordering::SeqCst);
            let cancelled = self
                .state
                .token
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .is_cancelled();
            self.state
                .error_before_token
                .store(!cancelled, Ordering::SeqCst);
            return Err(HttpError::Network);
        }
        unreachable!("the pending metadata request must be dropped by its caller")
    }
}

struct OrderingFixture {
    state: Arc<OrderingState>,
    transport: Arc<OrderingTransport>,
    router: Arc<TokenRouter>,
    io: ScriptedIo,
    registry: CompletionRegistry,
    probe: TerminalProbe,
    logs: Arc<Mutex<Vec<Value>>>,
}
impl OrderingFixture {
    fn new(capable: bool, pending_deadline: bool) -> (Self, FailureWorker) {
        let state = Arc::new(OrderingState::default());
        let (held, worker) = FailureWorker::new();
        let transport = Arc::new(OrderingTransport {
            capable,
            pending_deadline,
            state: state.clone(),
            held,
        });
        let router = Arc::new(TokenRouter {
            original: TrackedRouter::seeded(),
            state: state.clone(),
        });
        let body = br#"{"model":"claude-opus-5-5","max_tokens":100,"messages":[{"role":"user","content":"replace"}]}"#;
        let mut input = format!(
            "POST /v1/messages HTTP/1.1\r\nHost: localhost\r\nx-api-key: {LOCAL_TOKEN}\r\nx-claude-code-session-id: {SESSION}\r\nx-claude-code-prompt-id: {TASK}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len(),
        ).into_bytes();
        input.extend_from_slice(body);
        (
            Self {
                state,
                transport,
                router,
                io: ScriptedIo(Arc::new(Mutex::new(WriterState {
                    input: input.into(),
                    ..WriterState::default()
                }))),
                registry: CompletionRegistry::new(16),
                probe: TerminalProbe::default(),
                logs: Arc::new(Mutex::new(Vec::new())),
            },
            worker,
        )
    }
    fn gateway(&self) -> Arc<Gateway<OrderingTransport, TokenRouter>> {
        let logs = self.logs.clone();
        let log: EventSink = Arc::new(move |document| {
            let mut logs = logs.lock().unwrap();
            assert!(
                logs.len() < 32,
                "one synthetic request has bounded diagnostics"
            );
            logs.push(serde_json::from_str(&document.stringify()).unwrap());
        });
        let config = buffered_config("127.0.0.1:9".parse().unwrap(), None, Path::new("/tmp"));
        let gateway = Gateway::with_router(
            config,
            self.transport.clone(),
            self.router.clone(),
            EventSinks {
                log: Some(log),
                ..EventSinks::default()
            },
        )
        .unwrap();
        Gateway::with_test_terminal(gateway, self.probe.clone())
    }
    fn terminal(&self) -> RequestTerminal {
        self.state
            .terminal
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .clone()
    }
    fn proxy_errors(&self) -> Vec<Value> {
        self.logs
            .lock()
            .unwrap()
            .iter()
            .filter(|row| row["event"] == "proxy_error")
            .cloned()
            .collect()
    }
    fn assert_cleanup(&self, cause: FailureCause) {
        assert_eq!(self.state.calls.load(Ordering::SeqCst), 1);
        assert_eq!(self.state.active_requests.load(Ordering::SeqCst), 0);
        assert_eq!(self.state.request_drops.load(Ordering::SeqCst), 1);
        assert_eq!(self.registry.pending(), 0);
        assert_eq!(self.probe.tasks(), 0);
        assert_eq!(self.probe.writer_snapshot().live_io, 0);
        assert_eq!(self.probe.writer_snapshot().waiters, 0);
        assert!(!self.probe.observer_failed());
        assert_eq!(
            self.probe
                .events()
                .iter()
                .filter(|event| matches!(event,
            TerminalEvent::Failed(_, observed) if *observed == cause))
                .count(),
            1
        );
        assert_eq!(self.router.original.exact_completions(), 0);
        assert_eq!(self.router.original.releases(), 1);
        let turns = self.router.original.turns.lock().unwrap();
        assert_eq!(turns.attempt_count(), 0);
        assert!(
            turns
                .tool_owner_exact(SESSION, &["replacement-tool".into()])
                .is_none()
        );
        assert!(
            matches!(turns.tool_owner_exact(SESSION, &["original-tool".into()]),
            Some(ToolOwner::Owned { pin, .. }) if pin.model == JsString::from_scalar("claude-opus-5-5"))
        );
    }
}

async fn poll_once<F: Future>(mut future: Pin<&mut F>) -> bool {
    std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_ready())).await
}
async fn reach_pending_request<F: Future>(mut connection: Pin<&mut F>, fixture: &OrderingFixture) {
    // Poll the actual connection driver, not a separate service imitation. This
    // bounded progress loop ends before either clock advancement or failure.
    for _ in 0..64 {
        assert!(
            !poll_once(connection.as_mut()).await,
            "connection completed before request barrier"
        );
        if fixture.state.ready.load(Ordering::SeqCst) {
            assert_eq!(fixture.state.active_requests.load(Ordering::SeqCst), 1);
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("actual connection did not reach its pending request barrier");
}

async fn reach_rejected_admission<F: Future>(
    mut connection: Pin<&mut F>,
    fixture: &OrderingFixture,
) {
    // A wake makes the injected transport eligible; it does not promise that
    // one outer Hyper poll resumes it. Drive the actual connection to its
    // explicit rejection/error barrier without releasing the held action.
    for _ in 0..64 {
        assert!(
            !poll_once(connection.as_mut()).await,
            "connection bypassed held action"
        );
        if fixture.state.rejected_admission.load(Ordering::SeqCst)
            && fixture.state.error_before_token.load(Ordering::SeqCst)
        {
            return;
        }
        assert!(!fixture.state.action_finished.load(Ordering::SeqCst));
        tokio::task::yield_now().await;
    }
    panic!("actual connection did not reach rejection/error barrier");
}

async fn poll_with_exhausted_budget<F: Future>(
    mut connection: Pin<&mut F>,
    fixture: &OrderingFixture,
) {
    std::future::poll_fn(|cx| {
        // Run inside a spawned task: the #[tokio::test] root can be
        // unconstrained. Pinned Tokio starts a task with 128 credits; 256
        // individually polled operations bound exhaustion without an await
        // that could yield and replenish the budget before the driver poll.
        for _ in 0..256 {
            if !tokio::task::coop::has_budget_remaining() {
                break;
            }
            let mut consume = std::pin::pin!(tokio::task::coop::consume_budget());
            let _ = consume.as_mut().poll(cx);
        }
        assert!(!tokio::task::coop::has_budget_remaining());
        assert!(connection.as_mut().poll(cx).is_pending());
        assert!(!fixture.state.rejected_admission.load(Ordering::SeqCst));
        assert!(!fixture.state.error_before_token.load(Ordering::SeqCst));
        assert!(!fixture.state.action_finished.load(Ordering::SeqCst));
        assert_eq!(fixture.state.request_drops.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.state.active_requests.load(Ordering::SeqCst), 1);
        assert!(fixture.io.0.lock().unwrap().output.is_empty());
        assert!(fixture.proxy_errors().is_empty());
        Poll::Ready(())
    })
    .await;
}

#[tokio::test]
async fn held_downstream_action_rejection_suppresses_only_capable_preheader_proxy_error() {
    tokio::spawn(async move {
        for capable in [false, true] {
            let (fixture, mut worker) = OrderingFixture::new(capable, false);
            let gateway = fixture.gateway();
            let mut connection = Box::pin(gateway.serve_connection(
                fixture.io.clone(),
                fixture.registry.clone(),
                CancellationToken::new(),
            ));
            reach_pending_request(connection.as_mut(), &fixture).await;
            let cause = FailureCause::Downstream(Disconnect::ReadEof);
            worker.start(fixture.terminal(), cause);
            assert_eq!(fixture.terminal().failure_cause(), Some(cause));
            assert!(!fixture.state.action_finished.load(Ordering::SeqCst));
            // The physical IO barrier also holds a synthetic non-capable transport
            // which deliberately installed an action. Suppression is checked after
            // settlement; no ordinary-client timing claim is made by this control.
            poll_with_exhausted_budget(connection.as_mut(), &fixture).await;
            reach_rejected_admission(connection.as_mut(), &fixture).await;
            assert!(fixture.state.rejected_admission.load(Ordering::SeqCst));
            assert!(fixture.state.error_before_token.load(Ordering::SeqCst));
            assert!(!fixture.state.action_finished.load(Ordering::SeqCst));
            let output = fixture.io.0.lock().unwrap().output.clone();
            if capable {
                assert!(
                    output.is_empty(),
                    "owned downstream failure wrote response bytes"
                );
                assert_eq!(fixture.proxy_errors(), Vec::<Value>::new());
            }
            worker.finish();
            tokio::time::timeout(BARRIER, connection.as_mut())
                .await
                .unwrap();
            if capable {
                assert!(fixture.io.0.lock().unwrap().output.is_empty());
                assert_eq!(fixture.proxy_errors(), Vec::<Value>::new());
            } else {
                assert!(
                    fixture
                        .io
                        .0
                        .lock()
                        .unwrap()
                        .output
                        .starts_with(b"HTTP/1.1 502 "),
                    "ordinary error path lost its 502"
                );
                assert_eq!(
                    fixture.proxy_errors(),
                    [json!({"event":"proxy_error","status":502})]
                );
            }
            assert_eq!(
                *fixture.state.trace.lock().unwrap(),
                ["action_enter", "request_drop", "action_finish"]
            );
            fixture.assert_cleanup(cause);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn preheader_deadline_waits_for_held_action_to_claim_the_published_reservation() {
    let _clock = PausedClock::new();
    let (fixture, mut worker) = OrderingFixture::new(true, true);
    let gateway = fixture.gateway();
    let mut connection = Box::pin(gateway.serve_connection(
        fixture.io.clone(),
        fixture.registry.clone(),
        CancellationToken::new(),
    ));
    reach_pending_request(connection.as_mut(), &fixture).await;
    let terminal = fixture.terminal();
    let (_, deadline) = terminal.observation();
    let deadline = deadline.unwrap();
    assert_eq!(
        deadline.duration_since(Instant::now()),
        Duration::from_secs(30)
    );
    worker.start(terminal.clone(), FailureCause::Deadline);
    assert_eq!(terminal.failure_cause(), Some(FailureCause::Deadline));
    assert!(
        !fixture
            .state
            .token
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_cancelled()
    );
    tokio::time::advance(Duration::from_secs(30)).await;
    // The server future is owned here, not spawned. This poll must observe its
    // expired preheader timer while the first immutable action remains held.
    // A duplicate fail() returning false is not sufficient settlement evidence.
    assert!(
        !poll_once(connection.as_mut()).await,
        "connection finished before action settlement"
    );
    assert_eq!(fixture.state.active_requests.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.state.request_drops.load(Ordering::SeqCst), 0);
    assert!(fixture.state.retired_reusable.lock().unwrap().is_empty());
    assert!(!fixture.state.claimed.load(Ordering::SeqCst));
    assert!(!fixture.state.action_finished.load(Ordering::SeqCst));
    assert!(fixture.io.0.lock().unwrap().output.is_empty());
    assert_eq!(fixture.proxy_errors(), Vec::<Value>::new());
    worker.finish();
    assert!(
        fixture.state.claimed.load(Ordering::SeqCst),
        "lease retired before qualified claim"
    );
    assert!(
        fixture
            .state
            .token
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_cancelled()
    );
    tokio::time::timeout(BARRIER, connection.as_mut())
        .await
        .unwrap();
    assert_eq!(*fixture.state.retired_reusable.lock().unwrap(), [false]);
    assert_eq!(
        *fixture.state.trace.lock().unwrap(),
        ["action_enter", "claim", "action_finish", "request_drop"]
    );
    assert!(
        fixture
            .io
            .0
            .lock()
            .unwrap()
            .output
            .starts_with(b"HTTP/1.1 502 ")
    );
    assert_eq!(
        fixture.proxy_errors(),
        [json!({"event":"proxy_error","status":502})]
    );
    fixture.assert_cleanup(FailureCause::Deadline);
}
