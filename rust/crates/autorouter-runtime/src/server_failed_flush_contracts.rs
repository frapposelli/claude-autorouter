//! An actual gateway response write must establish delivery before replacing
//! a confirmed tool owner. The provider and client bytes here are synthetic.
use super::*;
use autorouter_core::config::read_config;
use autorouter_core::turn_state::{
    ContinuationEvidence, Pin as TaskPin, Selection, ToolIdentity, ToolOwner, TurnState,
};
use http_body_util::BodyExt;
use openssl::ssl::{Ssl, SslContext, SslVersion};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Waker;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

const LOCAL_TOKEN: &str = "synthetic-local-token-123456";
const SESSION: &str = "synthetic-session";
const TASK: &str = "synthetic-task";
const PROVIDER_BODY: &[u8] = br#"{"model":"claude-sonnet-5","content":[{"type":"tool_use","id":"replacement-tool","name":"Read","input":{}}],"stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":1}}"#;

struct Provider {
    calls: AtomicUsize,
}
impl HttpTransport for Provider {
    type ResponseBody = Full<Bytes>;

    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        assert_eq!(request.method(), Method::POST);
        assert_eq!(request.uri().path(), "/v1/messages");
        let body = request.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["model"],
            "claude-sonnet-5"
        );
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from_static(PROVIDER_BODY)))
            .unwrap())
    }
}

struct TrackedRouter {
    turns: Mutex<TurnState>,
    exact_completions: AtomicUsize,
    releases: AtomicUsize,
}
impl TrackedRouter {
    fn seeded() -> Arc<Self> {
        let mut turns = TurnState::default();
        let key = [TASK.to_owned()];
        assert!(turns.select_exact(
            &key,
            TaskPin::new("claude-opus-5-5".into(), "claude-haiku-4-5-20251001".into()),
            Selection {
                scope: SESSION,
                request_id: Some("confirmed-first"),
                sequence: 1,
            },
            0,
        ));
        assert!(turns.complete_exact(
            "confirmed-first",
            Some(&ContinuationEvidence {
                model: "claude-opus-5-5".into(),
                tools: vec![ToolIdentity {
                    id: "original-tool".into(),
                    model: "claude-opus-5-5".into(),
                }],
            }),
            1,
        ));
        Arc::new(Self {
            turns: Mutex::new(turns),
            exact_completions: AtomicUsize::new(0),
            releases: AtomicUsize::new(0),
        })
    }
}
impl GatewayRouter for TrackedRouter {
    async fn route(
        &self,
        _: Arc<JsDocument>,
        options: RouteOptions,
        _: &HeaderMap,
        _: &CancellationToken,
        _: &str,
    ) -> Result<Value, EvaluationError> {
        assert_eq!(options.prompt_id, TASK);
        assert_eq!(options.scope, format!("[\"{SESSION}\",null]"));
        assert!(self.turns.lock().unwrap().select_exact(
            &[TASK.to_owned()],
            TaskPin::new("claude-sonnet-5".into(), "claude-opus-5-5".into()),
            Selection {
                scope: SESSION,
                request_id: options.request_id.as_deref(),
                sequence: 2,
            },
            1,
        ));
        Ok(json!({
            "model": "claude-sonnet-5",
            "tier": "sonnet",
            "source": "jev",
            "evaluator": "jev",
            "reason": "classified",
            "confidence": 1.0,
            "latency_ms": 0,
        }))
    }

    fn complete(&self, id: &str, evidence: &Value) -> bool {
        assert!(evidence.is_null());
        self.releases.fetch_add(1, Ordering::SeqCst);
        self.turns.lock().unwrap().complete(id, None, 2)
    }

    fn complete_exact(&self, id: &str, evidence: &CompletionEvidence) -> bool {
        self.exact_completions.fetch_add(1, Ordering::SeqCst);
        self.turns.lock().unwrap().complete_exact(
            id,
            Some(&ContinuationEvidence {
                model: evidence
                    .continuation_model
                    .clone()
                    .unwrap_or_else(|| evidence.model.clone()),
                tools: evidence
                    .tool_uses
                    .iter()
                    .map(|tool| ToolIdentity {
                        id: tool.id.as_str().into(),
                        model: tool.model.clone(),
                    })
                    .collect(),
            }),
            2,
        )
    }

    fn shutdown(&self) {}
    async fn close(&self) {}
}

#[derive(Default)]
struct WriterState {
    input: VecDeque<u8>,
    output: Vec<u8>,
    fail_at: Option<usize>,
    write_failed: bool,
    hold_final_flush: bool,
    fail_final_flush: bool,
    flush_failed: bool,
    final_flush_pending: bool,
    flush_waker: Option<Waker>,
}

#[derive(Clone)]
struct ScriptedIo(Arc<Mutex<WriterState>>);
impl AsyncRead for ScriptedIo {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut state = self.0.lock().unwrap();
        if state.input.is_empty() {
            return Poll::Pending;
        }
        let length = state.input.len().min(buffer.remaining());
        let data: Vec<_> = state.input.drain(..length).collect();
        buffer.put_slice(&data);
        Poll::Ready(Ok(()))
    }
}
impl AsyncWrite for ScriptedIo {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut state = self.0.lock().unwrap();
        if state.fail_at.is_some_and(|at| state.output.len() >= at) {
            state.write_failed = true;
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        let remaining = state
            .fail_at
            .map_or(usize::MAX, |at| at.saturating_sub(state.output.len()));
        let length = bytes.len().min(5).min(remaining);
        state.output.extend_from_slice(&bytes[..length]);
        Poll::Ready(Ok(length))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut waker = Some(cx.waker().clone());
        let (result, previous) = {
            let mut state = self.0.lock().unwrap();
            if state.output.ends_with(b"0\r\n\r\n") && state.hold_final_flush {
                state.final_flush_pending = true;
                let previous = state.flush_waker.replace(waker.take().unwrap());
                (Poll::Pending, previous)
            } else if state.fail_final_flush {
                state.flush_failed = true;
                (Poll::Ready(Err(io::ErrorKind::BrokenPipe.into())), None)
            } else {
                (Poll::Ready(Ok(())), None)
            }
        };
        drop(previous);
        result
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

struct ResultRow {
    output: Vec<u8>,
    write_failed: bool,
    router: Arc<TrackedRouter>,
    records: Vec<Value>,
}

fn outcome(records: &[Value]) -> &Value {
    let rows: Vec<_> = records
        .iter()
        .filter(|row| row["event"] == "outcome")
        .collect();
    assert_eq!(rows.len(), 1, "one outcome follows the decision record");
    rows[0]
}

async fn run(fail_at: Option<usize>) -> ResultRow {
    use crate::server_events::RecordSink;

    let body = br#"{"model":"claude-opus-5-5","max_tokens":100,"messages":[{"role":"user","content":"replace"}]}"#;
    let request = format!(
        "POST /v1/messages HTTP/1.1\r\nHost: localhost\r\nx-api-key: {LOCAL_TOKEN}\r\nx-claude-code-session-id: {SESSION}\r\nx-claude-code-prompt-id: {TASK}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len(),
    );
    let mut input = request.into_bytes();
    input.extend_from_slice(body);
    let io = ScriptedIo(Arc::new(Mutex::new(WriterState {
        input: input.into(),
        fail_at,
        ..WriterState::default()
    })));
    let router = TrackedRouter::seeded();
    let provider = Arc::new(Provider {
        calls: AtomicUsize::new(0),
    });
    let mut config = read_config(
        &json!({"ANTHROPIC_API_KEY":"synthetic-provider-key"}),
        false,
        Path::new("/tmp"),
    )
    .unwrap();
    config.local_token = Some(LOCAL_TOKEN.into());
    config.upstream = "http://127.0.0.1:9".into();
    let records = Arc::new(Mutex::new(Vec::new()));
    let captured = records.clone();
    let record: RecordSink = Arc::new(move |value| captured.lock().unwrap().push(value));
    let gateway = Gateway::with_router(
        config,
        provider.clone(),
        router.clone(),
        EventSinks {
            record: Some(record),
            ..EventSinks::default()
        },
    )
    .unwrap();
    let registry = CompletionRegistry::new(16);
    tokio::time::timeout(
        Duration::from_secs(5),
        gateway.serve_connection(io.clone(), registry.clone(), CancellationToken::new()),
    )
    .await
    .expect("gateway connection did not finish");
    assert_eq!(registry.pending(), 0);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let state = io.0.lock().unwrap();
    let output = state.output.clone();
    let write_failed = state.write_failed;
    drop(state);
    let records = records.lock().unwrap().clone();
    ResultRow {
        output,
        write_failed,
        router,
        records,
    }
}

#[tokio::test]
async fn final_http_framing_failure_cannot_confirm_clean_tool_response() {
    let clean = run(None).await;
    assert!(!clean.write_failed);
    assert!(clean.output.ends_with(b"0\r\n\r\n"));
    assert!(
        clean
            .output
            .windows(PROVIDER_BODY.len())
            .any(|w| w == PROVIDER_BODY)
    );
    assert_eq!(clean.router.exact_completions.load(Ordering::SeqCst), 1);
    assert_eq!(clean.router.releases.load(Ordering::SeqCst), 0);
    assert_eq!(clean.records.len(), 2);
    assert_eq!(outcome(&clean.records)["completion_confirmed"], true);
    assert_eq!(outcome(&clean.records)["status"], "completed");
    {
        let turns = clean.router.turns.lock().unwrap();
        assert_eq!(turns.attempt_count(), 0);
        assert!(
            turns
                .tool_owner_exact(SESSION, &["original-tool".into()])
                .is_none()
        );
        assert!(matches!(
            turns.tool_owner_exact(SESSION, &["replacement-tool".into()]),
            Some(ToolOwner::Owned { pin, .. }) if pin.model == JsString::from_scalar("claude-sonnet-5")
        ));
    }

    // The provider response reaches clean observer EOF, but this writer fails
    // after accepting only the first two bytes of the final HTTP chunk.
    let broken = run(Some(clean.output.len() - 3)).await;
    assert!(broken.write_failed);
    assert_eq!(broken.output.len(), clean.output.len() - 3);
    assert!(broken.output.ends_with(b"0\r"));
    assert!(
        broken
            .output
            .windows(PROVIDER_BODY.len())
            .any(|w| w == PROVIDER_BODY)
    );
    assert_eq!(broken.router.exact_completions.load(Ordering::SeqCst), 0);
    assert_eq!(broken.router.releases.load(Ordering::SeqCst), 1);
    assert_eq!(broken.records.len(), 2);
    assert_eq!(outcome(&broken.records)["completion_confirmed"], false);
    assert_eq!(outcome(&broken.records)["status"], "cancelled");
    let turns = broken.router.turns.lock().unwrap();
    assert_eq!(turns.attempt_count(), 0);
    assert!(
        turns
            .tool_owner_exact(SESSION, &["replacement-tool".into()])
            .is_none()
    );
    assert!(matches!(
        turns.tool_owner_exact(SESSION, &["original-tool".into()]),
        Some(ToolOwner::Owned { pin, .. }) if pin.model == JsString::from_scalar("claude-opus-5-5")
    ));
}

// These are isolated final-flush controls for the experimental buffered raw
// constructor over HTTP and pinned TLS versions, not broader TLS qualification.
const BARRIER: Duration = Duration::from_secs(5);

struct OwnedTask<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for OwnedTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl<T> OwnedTask<T> {
    async fn join(&mut self) -> T {
        tokio::time::timeout(BARRIER, &mut self.0)
            .await
            .expect("owned synthetic task exceeded its bound")
            .expect("owned synthetic task failed")
    }
}

struct PausedClock;
impl PausedClock {
    fn new() -> Self {
        tokio::time::pause();
        Self
    }
}
impl Drop for PausedClock {
    fn drop(&mut self) {
        tokio::time::resume();
    }
}

async fn until(predicate: impl Fn() -> bool) {
    tokio::time::timeout(BARRIER, async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("synthetic buffered delivery barrier exceeded its bound");
}

async fn buffered_provider(
    listener: tokio::net::TcpListener,
    tls: Option<SslContext>,
    version: Option<SslVersion>,
) {
    let (socket, _) = listener.accept().await.unwrap();
    if let Some(context) = tls {
        let mut socket = hyper_openssl::SslStream::new(
            Ssl::new(&context).unwrap(),
            hyper_util::rt::TokioIo::new(socket),
        )
        .unwrap();
        Pin::new(&mut socket).accept().await.unwrap();
        assert_eq!(
            socket.ssl().version_str(),
            if version == Some(SslVersion::TLS1_2) {
                "TLSv1.2"
            } else {
                "TLSv1.3"
            }
        );
        buffered_provider_socket(hyper_util::rt::TokioIo::new(socket)).await;
    } else {
        assert!(version.is_none());
        buffered_provider_socket(socket).await;
    }
}

async fn buffered_provider_socket(mut socket: impl AsyncRead + AsyncWrite + Unpin) {
    let mut request = Vec::new();
    loop {
        let mut block = [0; 1024];
        let count = socket.read(&mut block).await.unwrap();
        assert_ne!(count, 0, "provider must receive a complete request");
        assert!(request.len() + count <= 16_384);
        request.extend_from_slice(&block[..count]);
        let Some(head) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&request[..head]);
        assert!(headers.starts_with("POST /v1/messages HTTP/1.1\r\n"));
        let length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .expect("provider request declares its body length");
        if request.len() < head + 4 + length {
            continue;
        }
        assert_eq!(request.len(), head + 4 + length);
        assert_eq!(
            serde_json::from_slice::<Value>(&request[head + 4..]).unwrap()["model"],
            "claude-sonnet-5"
        );
        break;
    }
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        PROVIDER_BODY.len()
    );
    socket.write_all(headers.as_bytes()).await.unwrap();
    socket.write_all(PROVIDER_BODY).await.unwrap();
    socket.shutdown().await.unwrap();
}

fn previous_owner_survives(router: &TrackedRouter, attempts: usize) {
    let turns = router.turns.lock().unwrap();
    assert_eq!(turns.attempt_count(), attempts);
    assert!(matches!(
        turns.tool_owner_exact(SESSION, &["original-tool".into()]),
        Some(ToolOwner::Owned { pin, .. }) if pin.model == JsString::from_scalar("claude-opus-5-5")
    ));
    assert!(
        turns
            .tool_owner_exact(SESSION, &["replacement-tool".into()])
            .is_none()
    );
}

#[derive(Clone, Copy, Debug)]
enum FinalFlush {
    Success,
    Error,
    Cancelled,
    Deadline,
}

async fn buffered_final_flush(schedule: FinalFlush, terminal: bool, version: Option<SslVersion>) {
    use crate::server_events::RecordSink;
    use crate::tls_roots::openssl_spike::gateway_intent::Disconnect;
    use crate::tls_roots::openssl_spike::gateway_terminal::{Event, FailureCause};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = read_config(
        &json!({"ANTHROPIC_API_KEY":"synthetic-provider-key"}),
        false,
        Path::new("/tmp"),
    )
    .unwrap();
    config.local_token = Some(LOCAL_TOKEN.into());
    config.upstream = format!(
        "{}://{}",
        if version.is_some() { "https" } else { "http" },
        listener.local_addr().unwrap()
    );
    config.upstream_timeout_ms = 30_000;
    let (transport, snapshot, tls) =
        crate::tls_roots::openssl_spike::buffered_delivery_fixture(version).unwrap();
    let transport = Arc::new(transport);
    let router = TrackedRouter::seeded();
    let records = Arc::new(Mutex::new(Vec::new()));
    let captured = records.clone();
    let record: RecordSink = Arc::new(move |value| {
        let mut rows = captured.lock().unwrap();
        assert!(rows.len() < 4, "one synthetic request has bounded records");
        rows.push(value);
    });
    let gateway = Gateway::with_router(
        config,
        transport.clone(),
        router.clone(),
        EventSinks {
            record: Some(record),
            ..EventSinks::default()
        },
    )
    .unwrap();
    let terminals = TerminalProbe::default();
    let gateway = if terminal {
        Gateway::with_test_terminal(gateway, terminals.clone())
    } else {
        gateway
    };
    let body = br#"{"model":"claude-opus-5-5","max_tokens":100,"messages":[{"role":"user","content":"replace"}]}"#;
    let mut input = format!(
        "POST /v1/messages HTTP/1.1\r\nHost: localhost\r\nx-api-key: {LOCAL_TOKEN}\r\nx-claude-code-session-id: {SESSION}\r\nx-claude-code-prompt-id: {TASK}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    input.extend_from_slice(body);
    let io = ScriptedIo(Arc::new(Mutex::new(WriterState {
        input: input.into(),
        hold_final_flush: true,
        ..WriterState::default()
    })));
    let registry = CompletionRegistry::new(16);
    let cancellation = CancellationToken::new();
    let mut provider = OwnedTask(tokio::spawn(buffered_provider(listener, tls, version)));
    let mut connection = OwnedTask(tokio::spawn(gateway.clone().serve_connection(
        io.clone(),
        registry.clone(),
        cancellation.clone(),
    )));
    until(|| io.0.lock().unwrap().final_flush_pending).await;
    provider.join().await;
    until(|| snapshot()["buffers"]["clean_sources"] == 1).await;

    // Upstream clean EOF and all downstream response bytes are insufficient:
    // the actual HTTP writer still owns the unacknowledged final flush.
    let pending_output = io.0.lock().unwrap().output.clone();
    assert!(pending_output.ends_with(b"0\r\n\r\n"));
    assert!(
        pending_output
            .windows(PROVIDER_BODY.len())
            .any(|v| v == PROVIDER_BODY)
    );
    assert_eq!(registry.pending(), 1);
    assert!(!connection.0.is_finished());
    assert_eq!(router.exact_completions.load(Ordering::SeqCst), 0);
    assert_eq!(router.releases.load(Ordering::SeqCst), 0);
    previous_owner_survives(&router, 1);
    assert_eq!(records.lock().unwrap().len(), 1);
    let observed = snapshot();
    assert_eq!(observed["raw_attempts"], 1);
    assert_eq!(observed["fetch_attempts"], 0);
    assert!(observed["buffers"]["consumer_polls"].as_u64().unwrap() > 0);
    if terminal {
        let events = terminals.events();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::Registered(_)))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::Attached(_)))
                .count(),
            1
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::Failed(..) | Event::DeliveryClaimed(..)))
        );
        assert_eq!(terminals.tasks(), 1);
    }

    match schedule {
        FinalFlush::Success | FinalFlush::Error => {
            let wake = {
                let mut state = io.0.lock().unwrap();
                state.hold_final_flush = false;
                state.fail_final_flush = matches!(schedule, FinalFlush::Error);
                state
                    .flush_waker
                    .take()
                    .expect("held final flush has a waiter")
            };
            wake.wake();
        }
        FinalFlush::Cancelled => cancellation.cancel(),
        FinalFlush::Deadline => {
            // Real TCP and provider work have finished before virtual time is
            // paused. Advance only the live request's absolute delivery bound.
            let _clock = PausedClock::new();
            tokio::time::advance(Duration::from_millis(30_001)).await;
            tokio::task::yield_now().await;
        }
    }
    connection.join().await;
    let retained = io.0.lock().unwrap().flush_waker.take();
    drop(retained);
    assert_eq!(registry.pending(), 0);
    assert_eq!(Arc::strong_count(&io.0), 1, "connection released its IO");
    let success = matches!(schedule, FinalFlush::Success);
    assert_eq!(
        router.exact_completions.load(Ordering::SeqCst),
        usize::from(success)
    );
    assert_eq!(
        router.releases.load(Ordering::SeqCst),
        usize::from(!success)
    );
    let final_records = records.lock().unwrap().clone();
    assert_eq!(final_records.len(), 2);
    assert_eq!(outcome(&final_records)["completion_confirmed"], success);
    assert_eq!(
        outcome(&final_records)["status"],
        match schedule {
            FinalFlush::Success => "completed",
            FinalFlush::Deadline => "error",
            FinalFlush::Error | FinalFlush::Cancelled => "cancelled",
        }
    );
    {
        let state = io.0.lock().unwrap();
        assert_eq!(state.output, pending_output);
        assert_eq!(state.flush_failed, matches!(schedule, FinalFlush::Error));
        assert_eq!(
            state.hold_final_flush,
            matches!(schedule, FinalFlush::Cancelled | FinalFlush::Deadline)
        );
    }
    if success {
        let turns = router.turns.lock().unwrap();
        assert_eq!(turns.attempt_count(), 0);
        assert!(
            turns
                .tool_owner_exact(SESSION, &["original-tool".into()])
                .is_none()
        );
        assert!(matches!(
            turns.tool_owner_exact(SESSION, &["replacement-tool".into()]),
            Some(ToolOwner::Owned { pin, .. }) if pin.model == JsString::from_scalar("claude-sonnet-5")
        ));
    } else {
        previous_owner_survives(&router, 0);
    }
    if terminal {
        let events = terminals.events();
        let deliveries: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::DeliveryClaimed(_, delivery) => Some(*delivery),
                _ => None,
            })
            .collect();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0] == Delivery::Flushed, success);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::CallbackFinished(_)))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::Closed(_)))
                .count(),
            1
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::CallbackAbandoned(_)))
        );
        let failures: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::Failed(_, cause) => Some(*cause),
                _ => None,
            })
            .collect();
        match schedule {
            FinalFlush::Success => assert!(failures.is_empty()),
            FinalFlush::Error => {
                assert_eq!(failures, [FailureCause::Downstream(Disconnect::WriteError)])
            }
            FinalFlush::Deadline => assert_eq!(failures, [FailureCause::Deadline]),
            FinalFlush::Cancelled => {
                // Either the cancellation monitor or the outer connection
                // cancellation wins; neither may publish an unrelated cause.
                assert!(failures.len() <= 1);
                assert!(
                    failures
                        .iter()
                        .all(|cause| *cause == FailureCause::Cancelled)
                );
            }
        }
        assert_eq!(terminals.tasks(), 0);
        assert_eq!(terminals.writer_snapshot().live_io, 0);
        assert_eq!(terminals.writer_snapshot().waiters, 0);
        assert!(!terminals.observer_failed());
    }
    drop(gateway);
    drop(transport);
    until(|| {
        let state = snapshot();
        [
            "raw_live",
            "raw_tasks",
            "raw_shutdown_handles",
            "fetch_live",
            "fetch_tasks",
            "fetch_shutdown_handles",
        ]
        .iter()
        .all(|field| state[*field] == 0)
            && [
                "live_pools",
                "allocated_blocks",
                "outstanding_blocks",
                "queued_bytes",
                "producer_tasks",
            ]
            .iter()
            .all(|field| state["buffers"][*field] == 0)
            && state["cache_released"] == true
    })
    .await;
    let cleaned = snapshot();
    assert_eq!(
        cleaned["raw_tls_handshakes_completed"],
        usize::from(version.is_some())
    );
    assert_eq!(cleaned["buffers"]["clean_sources"], 1);
    assert_eq!(cleaned["buffers"]["failed_sources"], 0);
    assert_eq!(cleaned["buffers"]["cancelled_sources"], 0);
}

#[tokio::test]
async fn buffered_raw_success_confirms_continuity_only_after_final_flush() {
    for version in [None, Some(SslVersion::TLS1_2), Some(SslVersion::TLS1_3)] {
        for terminal in [false, true] {
            buffered_final_flush(FinalFlush::Success, terminal, version).await;
        }
    }
}

#[tokio::test]
async fn buffered_raw_final_flush_error_preserves_confirmed_tool_owner() {
    for version in [None, Some(SslVersion::TLS1_2), Some(SslVersion::TLS1_3)] {
        for terminal in [false, true] {
            buffered_final_flush(FinalFlush::Error, terminal, version).await;
        }
    }
}

#[tokio::test]
async fn buffered_raw_cancellation_during_final_flush_preserves_confirmed_tool_owner() {
    for version in [None, Some(SslVersion::TLS1_2), Some(SslVersion::TLS1_3)] {
        for terminal in [false, true] {
            buffered_final_flush(FinalFlush::Cancelled, terminal, version).await;
        }
    }
}

#[tokio::test]
async fn buffered_raw_delivery_deadline_during_final_flush_preserves_confirmed_tool_owner() {
    for version in [None, Some(SslVersion::TLS1_2), Some(SslVersion::TLS1_3)] {
        for terminal in [false, true] {
            buffered_final_flush(FinalFlush::Deadline, terminal, version).await;
        }
    }
}
