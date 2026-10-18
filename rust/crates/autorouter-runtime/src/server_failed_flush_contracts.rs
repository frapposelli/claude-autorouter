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

#[derive(Clone, Copy, Default)]
enum FlushBoundary {
    #[default]
    ChunkEnd,
    Empty200,
    HeaderOnly(u16),
}
impl FlushBoundary {
    fn reached(self, output: &[u8]) -> bool {
        if matches!(self, Self::ChunkEnd) {
            return output.ends_with(b"0\r\n\r\n");
        }
        let Some(end) = output.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
            return false;
        };
        let head = std::str::from_utf8(&output[..end + 4])
            .unwrap()
            .to_ascii_lowercase();
        let payload = &output[end + 4..];
        match self {
            Self::HeaderOnly(status) => {
                head.starts_with(&format!("http/1.1 {status} ")) && payload.is_empty()
            }
            Self::Empty200 => {
                head.starts_with("http/1.1 200 ")
                    && if head
                        .lines()
                        .any(|line| line == "transfer-encoding: chunked")
                    {
                        payload == b"0\r\n\r\n"
                    } else {
                        head.lines().any(|line| line == "content-length: 0")
                            && !head
                                .lines()
                                .any(|line| line.starts_with("transfer-encoding:"))
                            && payload.is_empty()
                    }
            }
            Self::ChunkEnd => unreachable!(),
        }
    }
}

#[derive(Default)]
struct WriterState {
    input: VecDeque<u8>,
    output: Vec<u8>,
    fail_at: Option<usize>,
    write_failed: bool,
    flush_boundary: FlushBoundary,
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
            if state.flush_boundary.reached(&state.output) && state.hold_final_flush {
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

#[derive(Clone, Copy, Debug)]
enum BufferedResponse {
    ToolJson,
    EmptyJson,
    NoContent,
    NotModified,
}
impl BufferedResponse {
    fn bodyless(self) -> bool {
        !matches!(self, Self::ToolJson)
    }
    fn status(self) -> u16 {
        match self {
            Self::ToolJson | Self::EmptyJson => 200,
            Self::NoContent => 204,
            Self::NotModified => 304,
        }
    }
    fn flush_boundary(self) -> FlushBoundary {
        match self {
            Self::ToolJson => FlushBoundary::ChunkEnd,
            Self::EmptyJson => FlushBoundary::Empty200,
            Self::NoContent | Self::NotModified => FlushBoundary::HeaderOnly(self.status()),
        }
    }
    fn assert_output(self, output: &[u8]) {
        assert!(self.flush_boundary().reached(output));
        let end = output
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap()
            + 4;
        let head = std::str::from_utf8(&output[..end])
            .unwrap()
            .to_ascii_lowercase();
        assert!(head.starts_with(&format!("http/1.1 {} ", self.status())));
        if !self.bodyless() {
            assert!(
                output
                    .windows(PROVIDER_BODY.len())
                    .any(|bytes| bytes == PROVIDER_BODY)
            );
            return;
        }
        assert!(head.lines().any(|line| line == "x-safe: retained"));
        assert!(
            head.lines()
                .any(|line| line == "content-type: application/json")
        );
        for name in [
            "keep-alive:",
            "trailer:",
            "x-provider-hop:",
            "x-provider-trailer:",
        ] {
            assert!(!head.lines().any(|line| line.starts_with(name)));
        }
        assert!(!head.contains("content-length: 1000000"));
        // Regenerated framing is allowed; the application payload and any
        // forwarded trailers must be empty, even for a representation length.
        if head
            .lines()
            .any(|line| line == "transfer-encoding: chunked")
        {
            assert!(matches!(self, Self::EmptyJson));
            assert_eq!(&output[end..], b"0\r\n\r\n");
        } else {
            assert!(output[end..].is_empty());
        }
    }
}

struct ProviderHold {
    ready: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

async fn buffered_provider(
    listener: tokio::net::TcpListener,
    tls: Option<SslContext>,
    version: Option<SslVersion>,
    response: BufferedResponse,
    hold: Option<ProviderHold>,
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
        buffered_provider_socket(hyper_util::rt::TokioIo::new(socket), response, hold).await;
    } else {
        assert!(version.is_none());
        buffered_provider_socket(socket, response, hold).await;
    }
}

async fn buffered_provider_socket(
    mut socket: impl AsyncRead + AsyncWrite + Unpin,
    response: BufferedResponse,
    hold: Option<ProviderHold>,
) {
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
    let headers = if response.bodyless() {
        let (status, length) = match response {
            BufferedResponse::EmptyJson => ("200 OK", "Content-Length: 0\r\n"),
            BufferedResponse::NoContent => ("204 No Content", ""),
            BufferedResponse::NotModified => ("304 Not Modified", "Content-Length: 1000000\r\n"),
            BufferedResponse::ToolJson => unreachable!(),
        };
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{length}Connection: keep-alive, x-provider-hop\r\nKeep-Alive: timeout=60\r\nX-Provider-Hop: removed\r\nTrailer: x-provider-trailer\r\nX-Safe: retained\r\n\r\n"
        )
    } else {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            PROVIDER_BODY.len()
        )
    };
    socket.write_all(headers.as_bytes()).await.unwrap();
    if let Some(hold) = hold {
        assert!(response.bodyless());
        socket.flush().await.unwrap();
        hold.ready
            .send(())
            .expect("bodyless provider has a ready waiter");
        hold.release
            .await
            .expect("only owned cleanup releases the provider");
        // The gateway may already have closed its peer after cancellation.
        // Retain the actual socket until this explicit teardown, after the
        // test has separately observed decoder EOF and pending final flush.
        let _ = socket.shutdown().await;
    } else {
        assert!(!response.bodyless());
        socket.write_all(PROVIDER_BODY).await.unwrap();
        socket.shutdown().await.unwrap();
    }
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

async fn buffered_final_flush(
    response: BufferedResponse,
    schedule: FinalFlush,
    terminal: bool,
    version: Option<SslVersion>,
) {
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
        flush_boundary: response.flush_boundary(),
        ..WriterState::default()
    })));
    let registry = CompletionRegistry::new(16);
    let cancellation = CancellationToken::new();
    let (hold, provider_release, provider_ready) = if response.bodyless() {
        let (release, wait) = tokio::sync::oneshot::channel();
        let (ready, notified) = tokio::sync::oneshot::channel();
        (
            Some(ProviderHold {
                ready,
                release: wait,
            }),
            Some(release),
            Some(notified),
        )
    } else {
        (None, None, None)
    };
    let mut provider = OwnedTask(tokio::spawn(buffered_provider(
        listener, tls, version, response, hold,
    )));
    let mut connection = OwnedTask(tokio::spawn(gateway.clone().serve_connection(
        io.clone(),
        registry.clone(),
        cancellation.clone(),
    )));
    until(|| io.0.lock().unwrap().final_flush_pending).await;
    if let Some(ready) = provider_ready {
        tokio::time::timeout(BARRIER, ready)
            .await
            .expect("provider header flush exceeded its bound")
            .expect("provider signals headers before awaiting release");
    } else {
        provider.join().await;
    }
    until(|| snapshot()["buffers"]["clean_sources"] == 1).await;

    // Clean source-body EOF and accepted downstream framing are insufficient:
    // the actual writer still owns the unacknowledged final flush. Bodyless
    // providers remain open behind owned release, so source EOF is not socket EOF.
    let pending_output = io.0.lock().unwrap().output.clone();
    response.assert_output(&pending_output);
    assert_eq!(registry.pending(), 1);
    assert!(!connection.0.is_finished());
    assert_eq!(router.exact_completions.load(Ordering::SeqCst), 0);
    assert_eq!(router.releases.load(Ordering::SeqCst), 0);
    previous_owner_survives(&router, 1);
    {
        let rows = records.lock().unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows.iter().all(|row| row["event"] != "outcome"));
    }
    let observed = snapshot();
    assert_eq!(observed["raw_attempts"], 1);
    assert_eq!(observed["fetch_attempts"], 0);
    assert_eq!(observed["buffers"]["clean_sources"], 1);
    assert_eq!(observed["buffers"]["failed_sources"], 0);
    assert_eq!(observed["buffers"]["cancelled_sources"], 0);
    if response.bodyless() {
        assert!(!provider.0.is_finished());
        assert!(!provider_release.as_ref().unwrap().is_closed());
        assert_eq!(observed["raw_live"], 1);
    } else {
        assert!(observed["buffers"]["consumer_polls"].as_u64().unwrap() > 0);
    }
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
            // Real request/header I/O has finished; any held provider only
            // awaits owned release. Advance the live absolute delivery bound.
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
    let confirms = success && !response.bodyless();
    assert_eq!(
        router.exact_completions.load(Ordering::SeqCst),
        usize::from(confirms)
    );
    assert_eq!(
        router.releases.load(Ordering::SeqCst),
        usize::from(!confirms)
    );
    let final_records = records.lock().unwrap().clone();
    assert_eq!(final_records.len(), 2);
    assert_eq!(outcome(&final_records)["completion_confirmed"], confirms);
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
    if confirms {
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
    if let Some(release) = provider_release {
        assert!(!provider.0.is_finished());
        release
            .send(())
            .expect("owned provider cleanup releases its socket");
        provider.join().await;
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
    assert_eq!(
        router.exact_completions.load(Ordering::SeqCst),
        usize::from(confirms)
    );
    assert_eq!(
        router.releases.load(Ordering::SeqCst),
        usize::from(!confirms)
    );
    assert_eq!(records.lock().unwrap().len(), 2);
    if response.bodyless() {
        previous_owner_survives(&router, 0);
    }
}

#[tokio::test]
async fn buffered_raw_success_confirms_continuity_only_after_final_flush() {
    for version in [None, Some(SslVersion::TLS1_2), Some(SslVersion::TLS1_3)] {
        for terminal in [false, true] {
            buffered_final_flush(
                BufferedResponse::ToolJson,
                FinalFlush::Success,
                terminal,
                version,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn buffered_raw_final_flush_error_preserves_confirmed_tool_owner() {
    for version in [None, Some(SslVersion::TLS1_2), Some(SslVersion::TLS1_3)] {
        for terminal in [false, true] {
            buffered_final_flush(
                BufferedResponse::ToolJson,
                FinalFlush::Error,
                terminal,
                version,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn buffered_raw_cancellation_during_final_flush_preserves_confirmed_tool_owner() {
    for version in [None, Some(SslVersion::TLS1_2), Some(SslVersion::TLS1_3)] {
        for terminal in [false, true] {
            buffered_final_flush(
                BufferedResponse::ToolJson,
                FinalFlush::Cancelled,
                terminal,
                version,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn buffered_raw_delivery_deadline_during_final_flush_preserves_confirmed_tool_owner() {
    for version in [None, Some(SslVersion::TLS1_2), Some(SslVersion::TLS1_3)] {
        for terminal in [false, true] {
            buffered_final_flush(
                BufferedResponse::ToolJson,
                FinalFlush::Deadline,
                terminal,
                version,
            )
            .await;
        }
    }
}

async fn buffered_bodyless_final_flush(schedule: FinalFlush) {
    for response in [
        BufferedResponse::EmptyJson,
        BufferedResponse::NoContent,
        BufferedResponse::NotModified,
    ] {
        for version in [None, Some(SslVersion::TLS1_2), Some(SslVersion::TLS1_3)] {
            for terminal in [false, true] {
                buffered_final_flush(response, schedule, terminal, version).await;
            }
        }
    }
}

#[tokio::test]
async fn buffered_raw_bodyless_success_releases_without_confirming_execution() {
    buffered_bodyless_final_flush(FinalFlush::Success).await;
}

#[tokio::test]
async fn buffered_raw_bodyless_final_flush_error_preserves_confirmed_tool_owner() {
    buffered_bodyless_final_flush(FinalFlush::Error).await;
}

#[tokio::test]
async fn buffered_raw_bodyless_cancellation_during_final_flush_preserves_confirmed_tool_owner() {
    buffered_bodyless_final_flush(FinalFlush::Cancelled).await;
}

#[tokio::test]
async fn buffered_raw_bodyless_delivery_deadline_during_final_flush_preserves_confirmed_tool_owner()
{
    buffered_bodyless_final_flush(FinalFlush::Deadline).await;
}

// Full buffered-constructor pipeline controls. Existing single-response and
// primitive tests above stay unchanged. These remain test-only experiments.
const PIPELINE_TASKS: [&str; 2] = ["pipeline-a", "pipeline-b"];
const PIPELINE_SESSIONS: [&str; 2] = ["pipeline-session-a", "pipeline-session-b"];
const PIPELINE_OLD_TOOLS: [&str; 2] = ["pipeline-old-a", "pipeline-old-b"];
const PIPELINE_NEW_TOOLS: [&str; 2] = ["pipeline-new-a", "pipeline-new-b"];
const PIPELINE_BODIES: [&[u8]; 2] = [
    br#"{"model":"claude-sonnet-5","content":[{"type":"tool_use","id":"pipeline-new-a","name":"Read","input":{}}],"stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":1}}"#,
    br#"{"model":"claude-sonnet-5","content":[{"type":"tool_use","id":"pipeline-new-b","name":"Read","input":{}}],"stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":1}}"#,
];
const PIPELINE_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug)]
enum PipelineCase {
    Success,
    AFlushError,
    BFlushError,
    BCancelled,
    BDeadline,
}
impl PipelineCase {
    fn completed(self) -> [usize; 2] {
        [
            usize::from(!matches!(self, Self::AFlushError)),
            usize::from(matches!(self, Self::Success)),
        ]
    }
    fn released(self) -> [usize; 2] {
        self.completed().map(|count| 1 - count)
    }
    fn b_status(self) -> &'static str {
        match self {
            Self::Success => "completed",
            Self::BDeadline => "error",
            Self::AFlushError | Self::BFlushError | Self::BCancelled => "cancelled",
        }
    }
}

fn pipeline_scope(index: usize) -> String {
    format!("[\"{}\",null]", PIPELINE_SESSIONS[index])
}

struct PipelineRouterState {
    turns: TurnState,
    requests: std::collections::BTreeMap<String, usize>,
    completed: [usize; 2],
    released: [usize; 2],
    cancellation: [Option<CancellationToken>; 2],
}
struct PipelineRouter(Mutex<PipelineRouterState>);
impl PipelineRouter {
    fn seeded() -> Arc<Self> {
        let mut turns = TurnState::default();
        for index in 0..2 {
            let id = format!("pipeline-seed-{index}");
            let scope = pipeline_scope(index);
            assert!(turns.select_exact(
                &[PIPELINE_TASKS[index].to_owned()],
                TaskPin::new("claude-opus-5-5".into(), "claude-haiku-4-5-20251001".into()),
                Selection {
                    scope: &scope,
                    request_id: Some(&id),
                    sequence: 1
                },
                0,
            ));
            assert!(turns.complete_exact(
                &id,
                Some(&ContinuationEvidence {
                    model: "claude-opus-5-5".into(),
                    tools: vec![ToolIdentity {
                        id: PIPELINE_OLD_TOOLS[index].into(),
                        model: "claude-opus-5-5".into(),
                    }],
                }),
                1
            ));
        }
        Arc::new(Self(Mutex::new(PipelineRouterState {
            turns,
            requests: std::collections::BTreeMap::new(),
            completed: [0; 2],
            released: [0; 2],
            cancellation: [None, None],
        })))
    }
    fn check(&self, completed: [usize; 2], pending: usize) {
        self.check_with_releases(completed, [0; 2], pending);
    }
    fn check_with_releases(&self, completed: [usize; 2], released: [usize; 2], pending: usize) {
        let state = self.0.lock().unwrap();
        assert_eq!(state.requests.len(), 2);
        assert_eq!(state.completed, completed);
        assert_eq!(state.released, released);
        assert_eq!(state.turns.attempt_count(), pending);
        for index in 0..2 {
            let scope = pipeline_scope(index);
            if completed[index] == 0 {
                assert!(
                    matches!(state.turns.tool_owner_exact(&scope, &[PIPELINE_OLD_TOOLS[index].into()]),
                    Some(ToolOwner::Owned { pin, .. }) if pin.model == JsString::from_scalar("claude-opus-5-5"))
                );
                assert!(
                    state
                        .turns
                        .tool_owner_exact(&scope, &[PIPELINE_NEW_TOOLS[index].into()])
                        .is_none()
                );
            } else {
                assert!(
                    state
                        .turns
                        .tool_owner_exact(&scope, &[PIPELINE_OLD_TOOLS[index].into()])
                        .is_none()
                );
                assert!(
                    matches!(state.turns.tool_owner_exact(&scope, &[PIPELINE_NEW_TOOLS[index].into()]),
                    Some(ToolOwner::Owned { pin, .. }) if pin.model == JsString::from_scalar("claude-sonnet-5"))
                );
            }
        }
    }
}
impl GatewayRouter for PipelineRouter {
    async fn route(
        &self,
        _: Arc<JsDocument>,
        options: RouteOptions,
        _: &HeaderMap,
        cancellation: &CancellationToken,
        _: &str,
    ) -> Result<Value, EvaluationError> {
        let index = PIPELINE_TASKS
            .iter()
            .position(|task| *task == options.prompt_id)
            .ok_or(EvaluationError::InvalidResponse)?;
        let id = options.request_id.ok_or(EvaluationError::InvalidResponse)?;
        if options.scope != pipeline_scope(index) {
            return Err(EvaluationError::InvalidResponse);
        }
        let mut state = self.0.lock().unwrap();
        if state.requests.len() == 2 || state.requests.contains_key(&id) {
            return Err(EvaluationError::InvalidResponse);
        }
        if !state.turns.select_exact(
            &[PIPELINE_TASKS[index].to_owned()],
            TaskPin::new("claude-sonnet-5".into(), "claude-opus-5-5".into()),
            Selection {
                scope: &options.scope,
                request_id: Some(&id),
                sequence: 2,
            },
            2,
        ) {
            return Err(EvaluationError::InvalidResponse);
        }
        state.requests.insert(id, index);
        state.cancellation[index] = Some(cancellation.clone());
        Ok(
            json!({"model":"claude-sonnet-5","tier":"sonnet","source":"jev","evaluator":"jev",
            "reason":"classified","confidence":1.0,"latency_ms":0}),
        )
    }
    fn complete(&self, id: &str, evidence: &Value) -> bool {
        assert!(evidence.is_null());
        let mut state = self.0.lock().unwrap();
        let Some(index) = state.requests.get(id).copied() else {
            return false;
        };
        state.released[index] += 1;
        state.turns.complete_exact(id, None, 3)
    }
    fn complete_exact(&self, id: &str, evidence: &CompletionEvidence) -> bool {
        let mut state = self.0.lock().unwrap();
        let Some(index) = state.requests.get(id).copied() else {
            return false;
        };
        state.completed[index] += 1;
        state.turns.complete_exact(
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
            3,
        )
    }
    fn shutdown(&self) {}
    async fn close(&self) {}
}

// Observe only the real Gateway-created weak publisher, then delegate exactly
// once to the actual buffered constructor. Never construct a terminal owner or
// substitute a service/response/body for the constructor under test.
struct PipelineTransport<T> {
    inner: Arc<T>,
    seen: Arc<Mutex<std::collections::BTreeMap<usize, RequestTerminal>>>,
}
impl<T: HttpTransport> HttpTransport for PipelineTransport<T> {
    type ResponseBody = T::ResponseBody;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        self.inner.request(request).await
    }
    async fn request_raw(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        let marker = request
            .headers()
            .get("x-claude-code-prompt-id")
            .and_then(|value| value.to_str().ok())
            .ok_or(HttpError::InvalidRequest)?;
        let index = PIPELINE_TASKS
            .iter()
            .position(|task| *task == marker)
            .ok_or(HttpError::InvalidRequest)?;
        let terminal = request
            .extensions()
            .get::<RequestTerminal>()
            .cloned()
            .ok_or(HttpError::InvalidRequest)?;
        {
            let mut seen = self.seen.lock().unwrap();
            if seen.len() == 2 || seen.insert(index, terminal).is_some() {
                return Err(HttpError::InvalidRequest);
            }
        }
        self.inner.request_raw(request).await
    }
}

#[derive(Default)]
struct PipelineFlush {
    allowed: usize,
    pending: usize,
    accepted: usize,
    fail_at: Option<usize>,
    failed: Option<usize>,
    waker: Option<Waker>,
}
#[derive(Clone)]
struct PipelineIo {
    inner: ScriptedIo,
    flush: Arc<Mutex<PipelineFlush>>,
}
impl AsyncRead for PipelineIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, output)
    }
}
impl AsyncWrite for PipelineIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let remaining = PIPELINE_BYTES.saturating_sub(this.inner.0.lock().unwrap().output.len());
        if remaining == 0 {
            return Poll::Ready(Err(io::ErrorKind::FileTooLarge.into()));
        }
        Pin::new(&mut this.inner).poll_write(cx, &bytes[..bytes.len().min(remaining)])
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        // These two fixed JSON fixtures contain no literal CR/LF, so this
        // delimiter can occur only in the terminating chunk of each response.
        // The independent chunk decoder below checks every final wire byte.
        let complete = this
            .inner
            .0
            .lock()
            .unwrap()
            .output
            .windows(7)
            .filter(|part| *part == b"\r\n0\r\n\r\n")
            .count();
        let previous = {
            let mut gate = this.flush.lock().unwrap();
            if complete > gate.allowed {
                gate.pending = complete;
                gate.waker.replace(cx.waker().clone())
            } else if gate.fail_at == Some(complete) {
                gate.failed = Some(complete);
                return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
            } else {
                gate.accepted = complete;
                return Poll::Ready(Ok(()));
            }
        };
        drop(previous);
        Poll::Pending
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
impl PipelineIo {
    fn release(&self, ordinal: usize) {
        let wake = {
            let mut gate = self.flush.lock().unwrap();
            assert_eq!(gate.pending, ordinal);
            assert_eq!(gate.allowed + 1, ordinal);
            gate.allowed = ordinal;
            gate.waker.take().expect("held final flush has a waiter")
        };
        wake.wake();
    }
    fn output(&self) -> Vec<u8> {
        self.inner.0.lock().unwrap().output.clone()
    }
}

struct PipelinePeerLease(Arc<AtomicUsize>);
impl Drop for PipelinePeerLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
async fn pipeline_provider_exchange(
    mut socket: impl AsyncRead + AsyncWrite + Unpin,
    ready: tokio::sync::mpsc::Sender<usize>,
    release_a: CancellationToken,
) -> io::Result<()> {
    let invalid = || io::Error::other("invalid bounded pipeline provider fixture");
    let mut request = Vec::new();
    let (end, length) = loop {
        let mut block = [0; 1024];
        let count = socket.read(&mut block).await?;
        if count == 0 || request.len() + count > 16_384 {
            return Err(invalid());
        }
        request.extend_from_slice(&block[..count]);
        if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&request[..end]).map_err(|_| invalid())?;
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .ok_or_else(invalid)?;
            if request.len() == end + 4 + length {
                break (end + 4, length);
            }
            if request.len() > end + 4 + length {
                return Err(invalid());
            }
        }
    };
    let headers = std::str::from_utf8(&request[..end]).map_err(|_| invalid())?;
    if !headers.starts_with("POST /v1/messages HTTP/1.1\r\n") {
        return Err(invalid());
    }
    let marker = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("x-claude-code-prompt-id")
                .then(|| value.trim())
        })
        .ok_or_else(invalid)?;
    let index = PIPELINE_TASKS
        .iter()
        .position(|task| *task == marker)
        .ok_or_else(invalid)?;
    let document: Value =
        serde_json::from_slice(&request[end..end + length]).map_err(|_| invalid())?;
    if document
        != json!({"model":"claude-sonnet-5","max_tokens":100,
        "messages":[{"role":"user","content":PIPELINE_TASKS[index]}]})
    {
        return Err(invalid());
    }
    ready.send(index).await.map_err(|_| invalid())?;
    if index == 0 {
        release_a.cancelled().await
    }
    let body = PIPELINE_BODIES[index];
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nX-Pipeline-Marker: {}\r\n\r\n",
        body.len(),
        PIPELINE_TASKS[index]
    );
    socket.write_all(headers.as_bytes()).await?;
    socket.write_all(body).await?;
    socket.shutdown().await
}
async fn pipeline_provider(
    listener: tokio::net::TcpListener,
    tls: Option<SslContext>,
    version: Option<SslVersion>,
    ready: tokio::sync::mpsc::Sender<usize>,
    release_a: CancellationToken,
    stop: CancellationToken,
    live: Arc<AtomicUsize>,
) -> io::Result<()> {
    let mut peers = tokio::task::JoinSet::new();
    let mut first_error = None;
    for _ in 0..2 {
        let accepted =
            tokio::select! { biased; _=stop.cancelled()=>break, value=listener.accept()=>value };
        let (socket, _) = match accepted {
            Ok(value) => value,
            Err(error) => {
                first_error = Some(error);
                stop.cancel();
                break;
            }
        };
        live.fetch_add(1, Ordering::SeqCst);
        let lease = PipelinePeerLease(live.clone());
        let context = tls.clone();
        let ready = ready.clone();
        let release_a = release_a.clone();
        let stopping = stop.clone();
        peers.spawn(async move {
            let _lease = lease;
            let exchange = async move {
                if let Some(context) = context {
                    let ssl = Ssl::new(&context)?;
                    let mut socket =
                        hyper_openssl::SslStream::new(ssl, hyper_util::rt::TokioIo::new(socket))?;
                    Pin::new(&mut socket)
                        .accept()
                        .await
                        .map_err(|_| io::Error::other("pipeline fixture TLS handshake failed"))?;
                    let expected = if version == Some(SslVersion::TLS1_2) {
                        "TLSv1.2"
                    } else {
                        "TLSv1.3"
                    };
                    if socket.ssl().version_str() != expected {
                        return Err(io::Error::other(
                            "pipeline negotiated unexpected TLS version",
                        ));
                    }
                    pipeline_provider_exchange(
                        hyper_util::rt::TokioIo::new(socket),
                        ready,
                        release_a,
                    )
                    .await
                } else {
                    pipeline_provider_exchange(socket, ready, release_a).await
                }
            };
            tokio::select! { biased; _=stopping.cancelled()=>Ok(()), result=exchange=>result }
        });
    }
    drop(listener);
    while let Some(result) = peers.join_next().await {
        let error = match result {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error),
            Err(_) => Some(io::Error::other("pipeline provider task panicked")),
        };
        if let Some(error) = error {
            if first_error.is_none() {
                first_error = Some(error)
            }
            stop.cancel();
        }
    }
    first_error.map_or(Ok(()), Err)
}

// Ordinary assertion failures and fixture timeouts reach awaited cleanup before
// being rethrown. Drop remains a cancellation/abort fallback, not a join claim.
struct PipelineTasks {
    stop: CancellationToken,
    tasks: Vec<tokio::task::JoinHandle<io::Result<()>>>,
}
impl PipelineTasks {
    async fn close(&mut self) -> Vec<String> {
        self.stop.cancel();
        let mut errors = Vec::new();
        for mut task in self.tasks.drain(..) {
            match tokio::time::timeout(BARRIER, &mut task).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => errors.push(error.to_string()),
                Ok(Err(_)) => errors.push("pipeline owned task panicked".into()),
                Err(_) => {
                    task.abort();
                    errors.push("pipeline owned cleanup exceeded its bound".into());
                    if tokio::time::timeout(BARRIER, &mut task).await.is_err() {
                        errors.push("pipeline abort join remained unobserved".into());
                    }
                }
            }
        }
        errors
    }
}
impl Drop for PipelineTasks {
    fn drop(&mut self) {
        self.stop.cancel();
        for task in &self.tasks {
            task.abort()
        }
    }
}
async fn pipeline_catch_assertions(
    future: impl std::future::Future<Output = ()>,
) -> Result<(), Box<dyn std::any::Any + Send>> {
    let mut future = Box::pin(future);
    std::future::poll_fn(|cx| {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
            Ok(Poll::Ready(())) => Poll::Ready(Ok(())),
            Ok(Poll::Pending) => Poll::Pending,
            Err(reason) => Poll::Ready(Err(reason)),
        }
    })
    .await
}

// Decode complete downstream chunk framing independently of the producer,
// response observer and delimiter used only to hold the writer.
fn pipeline_wire_bodies(mut wire: &[u8]) -> Vec<Vec<u8>> {
    let mut bodies = Vec::new();
    while !wire.is_empty() {
        assert!(wire.len() <= PIPELINE_BYTES);
        let end = wire
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .unwrap();
        let head = std::str::from_utf8(&wire[..end])
            .unwrap()
            .to_ascii_lowercase();
        assert!(head.starts_with("http/1.1 200 "));
        assert!(
            head.lines()
                .any(|line| line == "content-type: application/json")
        );
        assert!(
            head.lines()
                .any(|line| line == "transfer-encoding: chunked")
        );
        assert!(
            head.lines()
                .any(|line| line == format!("x-pipeline-marker: {}", PIPELINE_TASKS[bodies.len()]))
        );
        assert!(!head.lines().any(|line| line.starts_with("content-length:")));
        wire = &wire[end + 4..];
        let mut body = Vec::new();
        loop {
            let end = wire.windows(2).position(|part| part == b"\r\n").unwrap();
            let length =
                usize::from_str_radix(std::str::from_utf8(&wire[..end]).unwrap(), 16).unwrap();
            wire = &wire[end + 2..];
            if length == 0 {
                assert!(wire.starts_with(b"\r\n"));
                wire = &wire[2..];
                break;
            }
            assert!(length <= PIPELINE_BYTES - body.len());
            body.extend_from_slice(&wire[..length]);
            assert_eq!(&wire[length..length + 2], b"\r\n");
            wire = &wire[length + 2..];
        }
        bodies.push(body);
        assert!(bodies.len() <= 2);
    }
    bodies
}

async fn buffered_pipeline_success(version: Option<SslVersion>) {
    buffered_pipeline_case(version, PipelineCase::Success).await;
}

async fn buffered_pipeline_case(version: Option<SslVersion>, schedule: PipelineCase) {
    use crate::server_events::RecordSink;
    use crate::tls_roots::openssl_spike::gateway_terminal::{Event, FailureCause};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut config = read_config(
        &json!({"ANTHROPIC_API_KEY":"synthetic-provider-key"}),
        false,
        Path::new("/tmp"),
    )
    .unwrap();
    config.local_token = Some(LOCAL_TOKEN.into());
    config.upstream = format!(
        "{}://{address}",
        if version.is_some() { "https" } else { "http" }
    );
    config.upstream_timeout_ms = 30_000;
    let (transport, snapshot, tls) =
        crate::tls_roots::openssl_spike::buffered_delivery_fixture(version).unwrap();
    let transport = Arc::new(transport);
    let seen = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
    let observed_transport = Arc::new(PipelineTransport {
        inner: transport.clone(),
        seen: seen.clone(),
    });
    let router = PipelineRouter::seeded();
    let records = Arc::new(Mutex::new(Vec::new()));
    let captured = records.clone();
    let overflow = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let record_overflow = overflow.clone();
    let record: RecordSink = Arc::new(move |value| {
        let mut rows = captured.lock().unwrap();
        if rows.len() == 8 {
            record_overflow.store(true, Ordering::SeqCst)
        } else {
            rows.push(value)
        }
    });
    let terminals = TerminalProbe::default();
    let gateway = Gateway::with_test_terminal(
        Gateway::with_router(
            config,
            observed_transport.clone(),
            router.clone(),
            EventSinks {
                record: Some(record),
                ..EventSinks::default()
            },
        )
        .unwrap(),
        terminals.clone(),
    );
    let mut input = Vec::new();
    for index in 0..2 {
        let body = format!(
            r#"{{"model":"claude-opus-5-5","max_tokens":100,"messages":[{{"role":"user","content":"{}"}}]}}"#,
            PIPELINE_TASKS[index]
        );
        input.extend_from_slice(format!("POST /v1/messages HTTP/1.1\r\nHost: localhost\r\nx-api-key: {LOCAL_TOKEN}\r\nx-claude-code-session-id: {}\r\nx-claude-code-prompt-id: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: {}\r\n\r\n{body}",
            PIPELINE_SESSIONS[index], PIPELINE_TASKS[index], body.len(), if index == 0 { "keep-alive" } else { "close" }).as_bytes());
    }
    assert!(input.len() < 16_384);
    let io = PipelineIo {
        inner: ScriptedIo(Arc::new(Mutex::new(WriterState {
            input: input.into(),
            ..WriterState::default()
        }))),
        flush: Arc::new(Mutex::new(PipelineFlush::default())),
    };
    let registry = CompletionRegistry::new(16);
    let stop = CancellationToken::new();
    let release_a = CancellationToken::new();
    let live_peers = Arc::new(AtomicUsize::new(0));
    let (ready, mut received) = tokio::sync::mpsc::channel(2);
    let mut tasks = PipelineTasks {
        stop: stop.clone(),
        tasks: Vec::new(),
    };
    tasks.tasks.push(tokio::spawn(pipeline_provider(
        listener,
        tls,
        version,
        ready,
        release_a.clone(),
        stop.clone(),
        live_peers.clone(),
    )));
    let serving = gateway.clone();
    let connection_io = io.clone();
    let connection_registry = registry.clone();
    let connection_stop = stop.clone();
    tasks.tasks.push(tokio::spawn(async move {
        serving
            .serve_connection(connection_io, connection_registry, connection_stop)
            .await;
        Ok(())
    }));

    // The deadline case advances30,001ms of Tokio time after all provider
    // I/O is complete. Declare a60s outer virtual-time watchdog beforehand;
    // the original15s watchdog would otherwise expire before the real B timer.
    let fixture_deadline = if matches!(schedule, PipelineCase::BDeadline) {
        Duration::from_secs(60)
    } else {
        Duration::from_secs(15)
    };
    let result = tokio::time::timeout(
        fixture_deadline,
        pipeline_catch_assertions(async {
            let mut arrivals = Vec::new();
            for _ in 0..2 {
                arrivals.push(
                    tokio::time::timeout(BARRIER, received.recv())
                        .await
                        .unwrap()
                        .unwrap(),
                )
            }
            arrivals.sort_unstable();
            assert_eq!(arrivals, [0, 1]);
            until(|| {
                snapshot()["buffers"]["responses_ready"] == 1
                    && snapshot()["buffers"]["clean_sources"] == 1
                    && registry.pending() == 1
            })
            .await;
            assert!(!release_a.is_cancelled());
            assert!(
                io.output().is_empty(),
                "ready B must remain queued behind held A"
            );
            assert_eq!(seen.lock().unwrap().len(), 2);
            router.check([0, 0], 2);
            assert_eq!(records.lock().unwrap().len(), 2);

            release_a.cancel();
            until(|| {
                io.flush.lock().unwrap().pending == 1 && snapshot()["buffers"]["clean_sources"] == 2
            })
            .await;
            assert_eq!(
                pipeline_wire_bodies(&io.output()),
                [PIPELINE_BODIES[0].to_vec()]
            );
            assert_eq!(registry.pending(), 2);
            assert_eq!(io.flush.lock().unwrap().accepted, 0);
            router.check([0, 0], 2);
            assert_eq!(records.lock().unwrap().len(), 2);

            if matches!(schedule, PipelineCase::AFlushError) {
                // B is complete and queued; neither response has a delivery
                // claim. Fail A's actual final flush before either can retire.
                let before = terminals.events();
                let registered: Vec<_> = before
                    .iter()
                    .filter_map(|event| match event {
                        Event::Registered(id) => Some(*id),
                        _ => None,
                    })
                    .collect();
                assert_eq!(registered.len(), 2);
                assert_ne!(registered[0], registered[1]);
                assert_eq!(before.iter().filter(|event| matches!(event, Event::Attached(_))).count(), 2);
                assert!(!before.iter().any(|event| matches!(event, Event::Failed(..) | Event::DeliveryClaimed(..))));
                let before_output = io.output();
                io.flush.lock().unwrap().fail_at = Some(1);
                io.release(1);
                until(|| {
                    let state = router.0.lock().unwrap();
                    registry.pending() == 0 && state.completed == [0, 0] && state.released == [1, 1]
                }).await;
                router.check_with_releases([0, 0], [1, 1], 0);
                // Full A bytes reached the writer, but its flush failed. B must
                // remain absent from the wire despite its complete clean body.
                assert_eq!(io.output(), before_output);
                let after = terminals.events();
                for id in registered {
                    let failures: Vec<_> = after.iter().filter_map(|event| match event {
                        Event::Failed(found, cause) if *found == id => Some(*cause),
                        _ => None,
                    }).collect();
                    assert_eq!(failures, [FailureCause::Downstream(
                        crate::tls_roots::openssl_spike::gateway_intent::Disconnect::WriteError,
                    )]);
                    let deliveries: Vec<_> = after.iter().filter_map(|event| match event {
                        Event::DeliveryClaimed(found, delivery) if *found == id => Some(*delivery),
                        _ => None,
                    }).collect();
                    assert_eq!(deliveries, [Delivery::Failed(Failure::Io)]);
                    assert_eq!(after.iter().filter(|event| matches!(event, Event::CallbackFinished(found) if *found == id)).count(), 1);
                }
                {
                    let gate = io.flush.lock().unwrap();
                    assert_eq!(gate.allowed, 1);
                    assert_eq!(gate.pending, 1);
                    assert_eq!(gate.accepted, 0);
                    assert_eq!(gate.failed, Some(1));
                }
                let rows = records.lock().unwrap();
                assert_eq!(rows.len(), 4);
                for task in PIPELINE_TASKS {
                    let outcomes: Vec<_> = rows.iter().filter(|row| row["event"] == "outcome" && row["prompt_id"] == task).collect();
                    assert_eq!(outcomes.len(), 1);
                    assert_eq!(outcomes[0]["completion_confirmed"], false);
                    assert_eq!(outcomes[0]["status"], "cancelled");
                }
                assert!(!overflow.load(Ordering::SeqCst));
                return;
            }

            io.release(1);
            until(|| io.flush.lock().unwrap().pending == 2).await;
            assert_eq!(
                pipeline_wire_bodies(&io.output()),
                PIPELINE_BODIES.map(<[u8]>::to_vec)
            );
            assert_eq!(io.flush.lock().unwrap().accepted, 1);
            assert_eq!(registry.pending(), 1);
            router.check([1, 0], 1);
            assert_eq!(records.lock().unwrap().len(), 3);
            let registered: Vec<_> = terminals
                .events()
                .iter()
                .filter_map(|event| match event {
                    Event::Registered(id) => Some(*id),
                    _ => None,
                })
                .collect();
            assert_eq!(registered.len(), 2);
            assert_ne!(registered[0], registered[1]);
            let deliveries: Vec<_> = terminals
                .events()
                .iter()
                .filter_map(|event| match event {
                    Event::DeliveryClaimed(id, delivery) => Some((*id, *delivery)),
                    _ => None,
                })
                .collect();
            assert_eq!(deliveries, [(registered[0], Delivery::Flushed)]);

            // Actual Gateway publisher from A, after its delivery callback retired
            // the owner. These are stale notifications, not a timer-expiry claim.
            let stale_a = seen.lock().unwrap().get(&0).unwrap().clone();
            let (cancel_a, cancel_b) = {
                let state = router.0.lock().unwrap();
                (
                    state.cancellation[0].as_ref().unwrap().clone(),
                    state.cancellation[1].as_ref().unwrap().clone(),
                )
            };
            assert!(!cancel_a.is_cancelled());
            assert!(!cancel_b.is_cancelled());
            let before = terminals.events();
            // These are the actual per-request child tokens supplied by Gateway
            // route(), not a synthetic terminal owner or the shared connection token.
            cancel_a.cancel();
            assert!(cancel_a.is_cancelled());
            assert!(!cancel_b.is_cancelled());
            assert!(!stale_a.fail(FailureCause::Upstream));
            assert!(!stale_a.fail(FailureCause::Deadline));
            tokio::task::yield_now().await;
            assert_eq!(terminals.events(), before);
            assert!(!cancel_b.is_cancelled());
            assert!(!stop.is_cancelled());
            assert_eq!(registry.pending(), 1);
            assert_eq!(io.flush.lock().unwrap().pending, 2);
            router.check([1, 0], 1);
            assert_eq!(records.lock().unwrap().len(), 3);

            match schedule {
                PipelineCase::AFlushError => unreachable!("first-flush branch returned"),
                PipelineCase::Success => io.release(2),
                PipelineCase::BFlushError => {
                    // Fail the actual inner writer's final B flush after every B
                    // payload/framing byte has been accepted. Terminal publication
                    // must come from the real IO error, not a direct fail() call.
                    io.flush.lock().unwrap().fail_at = Some(2);
                    io.release(2);
                }
                PipelineCase::BCancelled => cancel_b.cancel(),
                PipelineCase::BDeadline => {
                    // Both real provider exchanges must be terminal before pausing
                    // the clock. B still owns its real30s terminal timer and held
                    // final flush; A has already successfully retired.
                    until(|| {
                        live_peers.load(Ordering::SeqCst) == 0 && tasks.tasks[0].is_finished()
                    })
                    .await;
                    assert_eq!(snapshot()["buffers"]["clean_sources"], 2);
                    assert_eq!(registry.pending(), 1);
                    router.check([1, 0], 1);
                    assert_eq!(io.flush.lock().unwrap().pending, 2);
                    let before_advance = Instant::now();
                    {
                        let _clock = PausedClock::new();
                        tokio::time::advance(Duration::from_millis(30_001)).await;
                        tokio::task::yield_now().await;
                    } // Resume before bounded observation waits: no frozen-clock spin.
                    assert!(
                        Instant::now().duration_since(before_advance)
                            >= Duration::from_millis(30_001)
                    );
                }
            }
            until(|| {
                let state = router.0.lock().unwrap();
                registry.pending() == 0
                    && state.completed == schedule.completed()
                    && state.released == schedule.released()
            })
            .await;
            router.check_with_releases(schedule.completed(), schedule.released(), 0);
            assert_eq!(
                pipeline_wire_bodies(&io.output()),
                PIPELINE_BODIES.map(<[u8]>::to_vec)
            );
            let failures: Vec<_> = terminals
                .events()
                .iter()
                .filter_map(|event| match event {
                    Event::Failed(id, cause) => Some((*id, *cause)),
                    _ => None,
                })
                .collect();
            let expected_failure = match schedule {
                PipelineCase::AFlushError => unreachable!("first-flush branch returned"),
                PipelineCase::Success => None,
                PipelineCase::BFlushError => Some(FailureCause::Downstream(
                    crate::tls_roots::openssl_spike::gateway_intent::Disconnect::WriteError,
                )),
                PipelineCase::BCancelled => Some(FailureCause::Cancelled),
                PipelineCase::BDeadline => Some(FailureCause::Deadline),
            };
            assert_eq!(
                failures,
                expected_failure
                    .into_iter()
                    .map(|cause| (registered[1], cause))
                    .collect::<Vec<_>>()
            );
            {
                let gate = io.flush.lock().unwrap();
                assert_eq!(
                    gate.accepted,
                    if matches!(schedule, PipelineCase::Success) {
                        2
                    } else {
                        1
                    }
                );
                assert_eq!(
                    gate.failed,
                    if matches!(schedule, PipelineCase::BFlushError) {
                        Some(2)
                    } else {
                        None
                    }
                );
            }
            let rows = records.lock().unwrap();
            assert_eq!(rows.len(), 4);
            for (index, task) in PIPELINE_TASKS.into_iter().enumerate() {
                let outcomes: Vec<_> = rows
                    .iter()
                    .filter(|row| row["event"] == "outcome" && row["prompt_id"] == task)
                    .collect();
                assert_eq!(outcomes.len(), 1);
                assert_eq!(
                    outcomes[0]["completion_confirmed"],
                    index == 0 || matches!(schedule, PipelineCase::Success)
                );
                assert_eq!(
                    outcomes[0]["status"],
                    if index == 0 {
                        "completed"
                    } else {
                        schedule.b_status()
                    }
                );
            }
            assert!(!overflow.load(Ordering::SeqCst));
        }),
    )
    .await;

    // Always join these owners, including when the assertions above panic or
    // the outer fixture deadline expires. No task result is counted as success
    // after abort fallback. Background transport cleanup is observed separately.
    let cleanup_errors = tasks.close().await;
    io.flush.lock().unwrap().waker.take();
    drop(gateway);
    drop(observed_transport);
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
    assert!(cleanup_errors.is_empty(), "{cleanup_errors:?}");
    assert_eq!(live_peers.load(Ordering::SeqCst), 0);
    assert_eq!(Arc::strong_count(&io.inner.0), 1);
    assert_eq!(registry.pending(), 0);
    assert_eq!(terminals.tasks(), 0);
    assert_eq!(terminals.writer_snapshot().live_io, 0);
    assert_eq!(terminals.writer_snapshot().waiters, 0);
    assert!(!terminals.observer_failed());
    match result {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => std::panic::resume_unwind(reason),
        Err(_) => panic!(
            "actual buffered gateway pipeline exceeded its declared fixture deadline: {schedule:?} {version:?}"
        ),
    }
    let events = terminals.events();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Registered(_)))
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Attached(_)))
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::DeliveryClaimed(_, Delivery::Flushed)))
            .count(),
        schedule.completed().into_iter().sum::<usize>()
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::DeliveryClaimed(_, Delivery::Failed(_))))
            .count(),
        schedule.released().into_iter().sum::<usize>()
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::CallbackFinished(_)))
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Closed(_)))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Failed(..)))
            .count(),
        schedule.released().into_iter().sum::<usize>()
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::CallbackAbandoned(_)))
    );
    let state = snapshot();
    assert_eq!(state["raw_attempts"], 2);
    assert_eq!(
        state["raw_tls_handshakes_completed"],
        if version.is_some() { 2 } else { 0 }
    );
    assert_eq!(state["fetch_attempts"], 0);
    assert_eq!(state["buffers"]["clean_sources"], 2);
    assert_eq!(state["buffers"]["failed_sources"], 0);
    assert_eq!(state["buffers"]["cancelled_sources"], 0);
    router.check_with_releases(schedule.completed(), schedule.released(), 0);
    assert_eq!(records.lock().unwrap().len(), 4);
    let rebound = tokio::net::TcpListener::bind(address).await.unwrap();
    drop(rebound);
}

#[tokio::test]
async fn buffered_gateway_http_pipeline_owns_out_of_order_ready_responses() {
    buffered_pipeline_success(None).await;
}
#[tokio::test]
async fn buffered_gateway_tls12_pipeline_owns_out_of_order_ready_responses() {
    buffered_pipeline_success(Some(SslVersion::TLS1_2)).await;
}
#[tokio::test]
async fn buffered_gateway_tls13_pipeline_owns_out_of_order_ready_responses() {
    buffered_pipeline_success(Some(SslVersion::TLS1_3)).await;
}

#[tokio::test]
async fn buffered_gateway_http_pipeline_b_final_flush_error_preserves_retired_a() {
    buffered_pipeline_case(None, PipelineCase::BFlushError).await;
}
#[tokio::test]
async fn buffered_gateway_tls12_pipeline_b_final_flush_error_preserves_retired_a() {
    buffered_pipeline_case(Some(SslVersion::TLS1_2), PipelineCase::BFlushError).await;
}
#[tokio::test]
async fn buffered_gateway_tls13_pipeline_b_final_flush_error_preserves_retired_a() {
    buffered_pipeline_case(Some(SslVersion::TLS1_3), PipelineCase::BFlushError).await;
}
#[tokio::test]
async fn buffered_gateway_http_pipeline_b_request_cancellation_preserves_retired_a() {
    buffered_pipeline_case(None, PipelineCase::BCancelled).await;
}
#[tokio::test]
async fn buffered_gateway_tls12_pipeline_b_request_cancellation_preserves_retired_a() {
    buffered_pipeline_case(Some(SslVersion::TLS1_2), PipelineCase::BCancelled).await;
}
#[tokio::test]
async fn buffered_gateway_tls13_pipeline_b_request_cancellation_preserves_retired_a() {
    buffered_pipeline_case(Some(SslVersion::TLS1_3), PipelineCase::BCancelled).await;
}
#[tokio::test]
async fn buffered_gateway_http_pipeline_b_actual_terminal_deadline_preserves_retired_a() {
    buffered_pipeline_case(None, PipelineCase::BDeadline).await;
}
#[tokio::test]
async fn buffered_gateway_tls12_pipeline_b_actual_terminal_deadline_preserves_retired_a() {
    buffered_pipeline_case(Some(SslVersion::TLS1_2), PipelineCase::BDeadline).await;
}
#[tokio::test]
async fn buffered_gateway_tls13_pipeline_b_actual_terminal_deadline_preserves_retired_a() {
    buffered_pipeline_case(Some(SslVersion::TLS1_3), PipelineCase::BDeadline).await;
}

#[tokio::test]
async fn buffered_gateway_http_pipeline_a_final_flush_error_preserves_both_prior_owners() {
    buffered_pipeline_case(None, PipelineCase::AFlushError).await;
}
#[tokio::test]
async fn buffered_gateway_tls12_pipeline_a_final_flush_error_preserves_both_prior_owners() {
    buffered_pipeline_case(Some(SslVersion::TLS1_2), PipelineCase::AFlushError).await;
}
#[tokio::test]
async fn buffered_gateway_tls13_pipeline_a_final_flush_error_preserves_both_prior_owners() {
    buffered_pipeline_case(Some(SslVersion::TLS1_3), PipelineCase::AFlushError).await;
}
