//! Ordinary NativeHttpClient + Gateway, with only the downstream writer held.
use super::*;
use crate::http_client::NativeHttpClient;
use autorouter_core::config::read_config;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::Notify;

const TOKEN: &str = "synthetic-deadline-local-token";
const REQUEST: &str = r#"{"model":"claude-sonnet-5-5","messages":[{"role":"user","content":"synthetic deadline fixture"}],"max_tokens":128}"#;
const BOUND: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Router {
    completed: Mutex<Vec<Value>>,
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
        Ok(json!({"model":"claude-haiku-4-5-20251001","source":"test"}))
    }
    fn complete(&self, _: &str, evidence: &Value) -> bool {
        self.completed.lock().unwrap().push(evidence.clone());
        !evidence.is_null()
    }
    fn shutdown(&self) {}
    async fn close(&self) {}
}

#[derive(Default)]
struct WriterState {
    blocked: AtomicBool,
    pending: AtomicBool,
    dropped: AtomicBool,
    changed: Notify,
}
struct Writer {
    io: tokio::io::DuplexStream,
    state: Arc<WriterState>,
}
impl AsyncRead for Writer {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_read(cx, output)
    }
}
impl AsyncWrite for Writer {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.state.blocked.load(Ordering::SeqCst) {
            this.state.pending.store(true, Ordering::SeqCst);
            this.state.changed.notify_one();
            // The fixture never opens this gate: only connection destruction
            // can release the writer, independent of demand for response data.
            return Poll::Pending;
        }
        Pin::new(&mut this.io).poll_write(cx, bytes)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_shutdown(cx)
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        self.state.dropped.store(true, Ordering::SeqCst);
        self.state.changed.notify_one();
    }
}

async fn send_request(io: &mut tokio::io::DuplexStream) -> Result<(), String> {
    let request = format!(
        "POST /v1/messages HTTP/1.1\r\nHost: localhost\r\nx-api-key: {TOKEN}\r\nContent-Length: {}\r\n\r\n{REQUEST}",
        REQUEST.len(),
    );
    io.write_all(request.as_bytes())
        .await
        .map_err(|e| e.to_string())
}

async fn read_complete(io: &mut tokio::io::DuplexStream) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n0\r\n\r\n") {
        let mut buffer = [0; 1024];
        let count = io.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if count == 0 || bytes.len() + count > 32 * 1024 {
            return Err("incomplete or oversized fixture response".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(bytes)
}

async fn provider(listener: tokio::net::TcpListener) -> Result<(), String> {
    for model in ["claude-haiku-4-5-20251001", "claude-sonnet-5-5"] {
        let (mut socket, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let mut request = Vec::new();
        loop {
            let mut buffer = [0; 1024];
            let count = socket.read(&mut buffer).await.map_err(|e| e.to_string())?;
            if count == 0 || request.len() + count > 32 * 1024 {
                return Err("incomplete or oversized fixture request".into());
            }
            request.extend_from_slice(&buffer[..count]);
            if let Some(head) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..head]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .ok_or("missing fixture request length")?;
                if request.len() >= head + 4 + length {
                    break;
                }
            }
        }
        let body = json!({"type":"message","model":model,"stop_reason":"end_turn","content":[],"usage":{"input_tokens":1,"output_tokens":2}}).to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket
            .write_all(response.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        socket.shutdown().await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tokio::test]
async fn stalled_final_write_expires_without_reviving_old_deadlines_or_confirming_continuity() {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let mut config = read_config(&json!({"AUTOROUTER_EVALUATOR":"jev","ANTHROPIC_API_KEY":"synthetic-upstream-key","TYPESAFE_API_KEY":"synthetic-evaluator-key","AUTOROUTER_TOKEN":TOKEN}), false, std::path::Path::new("/tmp")).unwrap();
    config.upstream = format!("http://{}", listener.local_addr().unwrap());
    config.upstream_timeout_ms = 3000;
    let events = Arc::new(Mutex::new(Vec::<(Instant, Value)>::new()));
    let changed = Arc::new(Notify::new());
    let observed = events.clone();
    let wake = changed.clone();
    let router = Arc::new(Router::default());
    let gateway = Gateway::with_router(
        config,
        Arc::new(NativeHttpClient::new().unwrap()),
        router.clone(),
        EventSinks {
            status: Some(Arc::new(move |event| {
                observed
                    .lock()
                    .unwrap()
                    .push((Instant::now(), event.to_serde_observation_lossy()));
                wake.notify_one();
            })),
            ..Default::default()
        },
    )
    .unwrap();
    let registry = CompletionRegistry::new(16);
    let cancellation = CancellationToken::new();
    let writer = Arc::new(WriterState::default());
    let (mut peer, io) = tokio::io::duplex(1024);
    let mut provider = tokio::spawn(provider(listener));
    let mut connection = tokio::spawn(gateway.serve_connection(
        Writer {
            io,
            state: writer.clone(),
        },
        registry.clone(),
        cancellation.clone(),
    ));
    let mut paused = false;
    let mut connection_joined = false;
    let result = tokio::time::timeout(BOUND, async {
        send_request(&mut peer).await?;
        let first = read_complete(&mut peer).await?;
        if !first.starts_with(b"HTTP/1.1 200")
            || router.completed.lock().unwrap().len() != 1
            || registry.pending() != 0
        {
            return Err("A was not successfully delivered and retired".to_owned());
        }
        let a_head = events
            .lock()
            .unwrap()
            .iter()
            .find(|(_, e)| e["event"] == "upstream_response")
            .unwrap()
            .0;
        // Leave a distinct interval between deadlines while staying below the
        // ordinary 10-second HTTP header timeout. No transport event is inferred
        // from this interval: both response and writer barriers are explicit.
        tokio::time::sleep(Duration::from_millis(200)).await;
        writer.blocked.store(true, Ordering::SeqCst);
        let b_start = Instant::now();
        send_request(&mut peer).await?;
        loop {
            let notification = changed.notified();
            let clean = events
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, e)| e["event"] == "upstream_usage")
                .count()
                == 2;
            if clean {
                break;
            }
            notification.await;
        }
        while !writer.pending.load(Ordering::SeqCst) {
            writer.changed.notified().await;
        }
        let b_head = events
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(_, e)| e["event"] == "upstream_response")
            .unwrap()
            .0;
        if registry.pending() != 1
            || connection.is_finished()
            || router.completed.lock().unwrap().len() != 1
        {
            return Err("B did not reach clean source EOF with delivery still pending".to_owned());
        }
        tokio::time::pause();
        paused = true;
        let after_a = a_head + Duration::from_millis(3001);
        if after_a >= b_start + Duration::from_secs(3) || after_a <= Instant::now() {
            return Err("fixture did not establish separated deadlines".to_owned());
        }
        tokio::time::advance(after_a - Instant::now()).await;
        tokio::task::yield_now().await;
        if connection.is_finished()
            || writer.dropped.load(Ordering::SeqCst)
            || registry.pending() != 1
        {
            return Err("retired A deadline closed active B".to_owned());
        }
        tokio::time::advance(b_head + Duration::from_millis(3001) - Instant::now()).await;
        match tokio::time::timeout(Duration::from_millis(10), &mut connection).await {
            Ok(result) => {
                connection_joined = true;
                result.map_err(|error| format!("connection task failed: {error}"))?;
            }
            Err(_) => return Err("B retained the stalled writer after its deadline".to_owned()),
        }
        if !writer.dropped.load(Ordering::SeqCst)
            || registry.pending() != 0
            || !writer.blocked.load(Ordering::SeqCst)
        {
            return Err("deadline failed to release ownership with writer gate held".to_owned());
        }
        let completed = router.completed.lock().unwrap();
        if completed.len() != 2
            || completed[0]["continuation_model"] != "claude-haiku-4-5-20251001"
            || !completed[1].is_null()
        {
            return Err("deadline changed A continuity or confirmed undelivered B".to_owned());
        }
        let events = events.lock().unwrap();
        let count = |kind: &str| events.iter().filter(|(_, e)| e["event"] == kind).count();
        if count("request_complete") != 1
            || count("request_error") != 1
            || count("request_cancelled") != 0
        {
            return Err("timeout terminal telemetry was not exactly one request_error".to_owned());
        }
        Ok::<_, String>(())
    })
    .await;
    // Failure capture must not depend on the test's eventual forced cleanup.
    // All owned tasks are reaped before reporting the original assertion.
    if paused {
        tokio::time::resume();
    }
    cancellation.cancel();
    if !connection_joined && tokio::time::timeout(BOUND, &mut connection).await.is_err() {
        connection.abort();
        let _ = connection.await;
    }
    drop(peer);
    let provider_result = tokio::time::timeout(BOUND, &mut provider).await;
    if provider_result.is_err() {
        provider.abort();
        let _ = provider.await;
    }
    assert_eq!(result, Ok(Ok(())));
    assert!(matches!(provider_result, Ok(Ok(Ok(())))));
}

// Default upstream budget: real Gateway listener, synthetic owned transport,
// real HTTP parser/writer, and only the Tokio clock advanced by this fixture.
const DEFAULT_UPSTREAM_REQUEST: &str = r#"{"model":"claude-sonnet-5-5","messages":[{"role":"user","content":"synthetic default upstream deadline"}],"max_tokens":128}"#;
const DEFAULT_UPSTREAM_SUCCESS: &[u8] = br#"{"type":"message","model":"claude-sonnet-5-5","stop_reason":"end_turn","content":[],"usage":{"input_tokens":1,"output_tokens":2}}"#;
const DEFAULT_UPSTREAM_ERROR: &[u8] = br#"{"type":"error","error":{"type":"api_error","message":"Router could not complete the upstream request"}}"#;
const DEFAULT_UPSTREAM_TOKEN: &str = "synthetic-upstream-default-local-token";

fn upstream_sha(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Default)]
struct DefaultUpstreamState {
    requests: Vec<Value>,
    head_active: usize,
    head_drops: usize,
    head_polls: usize,
    head_waker: Option<std::task::Waker>,
    body_polls: usize,
    body_drops: usize,
    body_eof: usize,
    progress: bool,
    body_waker: Option<std::task::Waker>,
}
struct DefaultHeadLease(Arc<Mutex<DefaultUpstreamState>>);
impl Drop for DefaultHeadLease {
    fn drop(&mut self) {
        let mut state = self.0.lock().unwrap();
        state.head_active -= 1;
        state.head_drops += 1;
        state.head_waker = None;
    }
}
struct DefaultUpstreamBody {
    state: Arc<Mutex<DefaultUpstreamState>>,
    frames: std::collections::VecDeque<Bytes>,
    held: bool,
    eof: bool,
}
impl Body for DefaultUpstreamBody {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, io::Error>>> {
        let this = self.get_mut();
        if let Some(bytes) = this.frames.pop_front() {
            return Poll::Ready(Some(Ok(hyper::body::Frame::data(bytes))));
        }
        let mut state = this.state.lock().unwrap();
        if this.held {
            if std::mem::take(&mut state.progress) {
                return Poll::Ready(Some(Ok(hyper::body::Frame::data(Bytes::from_static(
                    b": progress\n\n",
                )))));
            }
            state.body_polls += 1;
            state.body_waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        if !this.eof {
            this.eof = true;
            state.body_eof += 1;
        }
        Poll::Ready(None)
    }
}
impl Drop for DefaultUpstreamBody {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.body_drops += 1;
        state.body_waker = None;
    }
}
struct DefaultUpstreamTransport {
    held_head: bool,
    state: Arc<Mutex<DefaultUpstreamState>>,
}
impl HttpTransport for DefaultUpstreamTransport {
    type ResponseBody = DefaultUpstreamBody;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        let (parts, body) = request.into_parts();
        let bytes = body
            .collect()
            .await
            .map_err(|_| HttpError::Network)?
            .to_bytes();
        let mut headers = serde_json::Map::new();
        for (name, value) in &parts.headers {
            headers.insert(
                name.to_string(),
                json!(value.to_str().map_err(|_| HttpError::InvalidRequest)?),
            );
        }
        let first = {
            let mut state = self.state.lock().unwrap();
            if state.requests.len() >= 2 || bytes.len() > 16384 {
                return Err(HttpError::InvalidRequest);
            }
            let first = state.requests.is_empty();
            state.requests.push(json!({"method":parts.method.as_str(),"url":parts.uri.to_string(),
                "headers":headers,"body_text":std::str::from_utf8(&bytes).map_err(|_| HttpError::InvalidRequest)?,
                "body_bytes":bytes.len(),"body_sha256":upstream_sha(&bytes)}));
            first
        };
        if first && self.held_head {
            self.state.lock().unwrap().head_active += 1;
            let _lease = DefaultHeadLease(self.state.clone());
            return std::future::poll_fn(|cx| {
                let mut state = self.state.lock().unwrap();
                state.head_polls += 1;
                state.head_waker = Some(cx.waker().clone());
                Poll::Pending
            })
            .await;
        }
        Response::builder()
            .status(200)
            .header(
                "content-type",
                if first {
                    "text/event-stream"
                } else {
                    "application/json"
                },
            )
            .body(DefaultUpstreamBody {
                state: self.state.clone(),
                frames: [Bytes::from_static(if first {
                    b": ping\n\n"
                } else {
                    DEFAULT_UPSTREAM_SUCCESS
                })]
                .into(),
                held: first,
                eof: false,
            })
            .map_err(|_| HttpError::InvalidRequest)
    }
}

#[derive(Default)]
struct DefaultUpstreamRouter {
    entered: CancellationToken,
    release: CancellationToken,
    routes: std::sync::atomic::AtomicUsize,
    completed: Mutex<Vec<Value>>,
    tokens: Mutex<Vec<CancellationToken>>,
    closes: std::sync::atomic::AtomicUsize,
}
impl GatewayRouter for DefaultUpstreamRouter {
    async fn route(
        &self,
        _: Arc<JsDocument>,
        _: RouteOptions,
        _: &HeaderMap,
        cancellation: &CancellationToken,
        _: &str,
    ) -> Result<Value, EvaluationError> {
        self.tokens.lock().unwrap().push(cancellation.clone());
        if self.routes.fetch_add(1, Ordering::SeqCst) == 0 {
            self.entered.cancel();
            self.release.cancelled().await;
        }
        Ok(json!({"model":"claude-sonnet-5-5","source":"test"}))
    }
    fn complete(&self, _: &str, evidence: &Value) -> bool {
        self.completed.lock().unwrap().push(evidence.clone());
        !evidence.is_null()
    }
    fn shutdown(&self) {}
    async fn close(&self) {
        self.closes.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct DefaultPeerState {
    wire: Vec<u8>,
    ended: bool,
    read_error: bool,
}
struct DefaultPeer {
    state: Arc<Mutex<DefaultPeerState>>,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<Result<(), String>>,
}
impl DefaultPeer {
    async fn open(address: SocketAddr) -> Result<Self, String> {
        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .map_err(|e| e.to_string())?;
        let packet = format!(
            "POST /v1/messages HTTP/1.1\r\nHost: localhost\r\nx-api-key: {DEFAULT_UPSTREAM_TOKEN}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{DEFAULT_UPSTREAM_REQUEST}",
            DEFAULT_UPSTREAM_REQUEST.len()
        );
        stream
            .write_all(packet.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        let state = Arc::new(Mutex::new(DefaultPeerState::default()));
        let observed = state.clone();
        let stop = CancellationToken::new();
        let stopping = stop.clone();
        let task = tokio::spawn(async move {
            let mut buffer = [0; 1024];
            loop {
                let next = tokio::select! { biased; _=stopping.cancelled()=>return Ok(()), n=stream.read(&mut buffer)=>n };
                let mut state = observed.lock().unwrap();
                match next {
                    Ok(0) => {
                        state.ended = true;
                        return Ok(());
                    }
                    Ok(n) => {
                        if state.wire.len() + n > 16384 {
                            return Err("downstream wire bound".into());
                        }
                        state.wire.extend_from_slice(&buffer[..n]);
                    }
                    Err(_) => {
                        state.read_error = true;
                        state.ended = true;
                        return Ok(());
                    }
                }
            }
        });
        Ok(Self { state, stop, task })
    }
}

// Keep this future runnable while paused: unlike a pending sleep/timeout, these
// yields cannot auto-advance the clock to the deadline being measured.
async fn upstream_until(label: &str, mut predicate: impl FnMut() -> bool) -> Result<(), String> {
    for _ in 0..100_000 {
        if predicate() {
            return Ok(());
        }
        tokio::task::yield_now().await;
    }
    Err(format!("positive fixture barrier not reached: {label}"))
}

// Bounded entity decoder, not a complete HTTP implementation. In the body
// timeout case incomplete chunk framing is retained as incomplete, never EOF.
fn upstream_entity(wire: &[u8]) -> Result<(u16, Vec<u8>, bool), String> {
    let head = wire
        .windows(4)
        .position(|b| b == b"\r\n\r\n")
        .ok_or("no response head")?;
    let text = std::str::from_utf8(&wire[..head]).map_err(|_| "nonASCII head")?;
    let status = text
        .split_whitespace()
        .nth(1)
        .ok_or("no status")?
        .parse()
        .map_err(|_| "status")?;
    let mut length = None;
    let mut chunked = false;
    for line in text.split("\r\n").skip(1) {
        let (key, value) = line.split_once(':').ok_or("header")?;
        if key.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err("duplicate length".into());
            }
            length = Some(value.trim().parse::<usize>().map_err(|_| "length")?);
        }
        if key.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !value.trim().eq_ignore_ascii_case("chunked") {
                return Err("transfer coding".into());
            }
            chunked = true;
        }
    }
    let bytes = &wire[head + 4..];
    if let Some(n) = length {
        if chunked || bytes.len() > n {
            return Err("framing".into());
        }
        return Ok((status, bytes.to_vec(), bytes.len() == n));
    }
    if !chunked {
        return Err("fixture expects explicit framing".into());
    }
    let mut at = 0;
    let mut entity = Vec::new();
    loop {
        let Some(end) = bytes[at..].windows(2).position(|b| b == b"\r\n") else {
            return Ok((status, entity, false));
        };
        let token = &bytes[at..at + end];
        if token.is_empty() || token.len() > 8 || !token.iter().all(u8::is_ascii_hexdigit) {
            return Err("chunk token".into());
        }
        let n = usize::from_str_radix(std::str::from_utf8(token).map_err(|_| "chunk ASCII")?, 16)
            .map_err(|_| "chunk size")?;
        at += end + 2;
        if n == 0 {
            return Ok((status, entity, bytes.get(at..) == Some(b"\r\n")));
        }
        if n > 16384 || entity.len() + n > 16384 {
            return Err("entity bound".into());
        }
        if bytes.len() - at < n + 2 {
            return Ok((status, entity, false));
        }
        if &bytes[at + n..at + n + 2] != b"\r\n" {
            return Err("chunk CRLF".into());
        }
        entity.extend_from_slice(&bytes[at..at + n]);
        at += n + 2;
    }
}

// Poll real IO while the test remains runnable, before the first explicit
// advance. Runtime::start_paused establishes the timer driver and frozen Clock
// at one epoch; these polls/yields prevent idle auto-advance during TCP setup.
async fn upstream_runnable<F: Future>(label: &str, future: F) -> Result<F::Output, String> {
    tokio::pin!(future);
    for _ in 0..100_000 {
        if let Poll::Ready(value) =
            std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
        {
            return Ok(value);
        }
        tokio::task::yield_now().await;
    }
    Err(format!(
        "real IO setup did not reach positive readiness: {label}"
    ))
}

async fn upstream_default_case(held_head: bool) {
    let fixture_epoch = Instant::now();
    let env = json!({"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_OLLAMA_TIMEOUT_MS":"0",
        "AUTOROUTER_AUTH_MODE":"api-key","AUTOROUTER_TOKEN":DEFAULT_UPSTREAM_TOKEN,
        "ANTHROPIC_API_KEY":"synthetic-upstream-default-provider-key"});
    let env_before = serde_json::to_vec(&env).unwrap();
    let config = read_config(&env, false, std::path::Path::new(".")).unwrap();
    assert_eq!(config.upstream_timeout_ms, 600_000);
    assert_eq!(config.ollama_timeout_ms, 0);
    let config_before = serde_json::to_value(&config).unwrap();
    let state = Arc::new(Mutex::new(DefaultUpstreamState::default()));
    let transport = Arc::new(DefaultUpstreamTransport {
        held_head,
        state: state.clone(),
    });
    let transport_weak = Arc::downgrade(&transport);
    let router = Arc::new(DefaultUpstreamRouter::default());
    let events = Arc::new(Mutex::new(Vec::<Value>::new()));
    let observed = events.clone();
    let gateway = Gateway::with_router(
        config,
        transport.clone(),
        router.clone(),
        EventSinks {
            status: Some(Arc::new(move |e| {
                let mut rows = observed.lock().unwrap();
                assert!(rows.len() < 32);
                rows.push(e.to_serde_observation_lossy());
            })),
            ..Default::default()
        },
    )
    .unwrap();
    let mut handle = Some(gateway.listen(0).await.unwrap());
    let address = handle.as_ref().unwrap().address;
    let mut peers = Vec::<DefaultPeer>::new();
    let mut paused = true;
    let mut head_boundary_repoll = false;
    let result: Result<Value, String> = async {
        peers.push(upstream_runnable("initial connect/write", DefaultPeer::open(address)).await??);
        upstream_until("actual route entry at frozen epoch", || router.entered.is_cancelled()).await?;
        if Instant::now() != fixture_epoch {return Err("setup auto-advanced the runtime epoch".into());}
        let origin = Instant::now();
        router.release.cancel();
        upstream_until("actual first request/held poll", || {
            let s = state.lock().unwrap(); s.requests.len()==1 && if held_head {s.head_active==1} else {s.body_polls>0}
        }).await?;
        if !held_head { upstream_until("physically forwarded first prefix", || upstream_entity(&peers[0].state.lock().unwrap().wire).is_ok_and(|r| r.0==200 && r.1==b": ping\n\n" && !r.2)).await?; }
        if Instant::now()!=origin {return Err("paused forwarding phase changed".into());}
        tokio::time::advance(Duration::from_millis(599_999)).await;
        if held_head {
            let (before, wake) = {
                let mut state = state.lock().unwrap();
                (state.head_polls, state.head_waker.take())
            };
            wake.ok_or("missing actual held-head waker")?.wake();
            upstream_until("positive held-head repoll at599999", || {
                let state = state.lock().unwrap();
                state.head_polls > before || state.head_active == 0
            }).await?;
            if state.lock().unwrap().head_polls <= before {
                return Err("head deadline won before its required599999repoll".into());
            }
            head_boundary_repoll = true;
        }
        if peers[0].state.lock().unwrap().ended || !router.completed.lock().unwrap().is_empty() {return Err("expired before default equality".into());}
        if held_head {
            if state.lock().unwrap().head_active!=1 || !peers[0].state.lock().unwrap().wire.is_empty() {return Err("head was not held at 599999".into());}
        } else {
            let wake={let mut s=state.lock().unwrap();s.progress=true;s.body_waker.take()};
            if let Some(w)=wake {w.wake();}
            upstream_until("599999 progress physically forwarded", || upstream_entity(&peers[0].state.lock().unwrap().wire).is_ok_and(|r| r.1==b": ping\n\n: progress\n\n" && !r.2)).await?;
            if state.lock().unwrap().body_eof!=0 {return Err("held provider invented EOF".into());}
        }
        if Instant::now()!=origin+Duration::from_millis(599_999) {return Err("predeadline clock drift".into());}
        tokio::time::advance(Duration::from_millis(1)).await;
        upstream_until("actual equality expiry/first receiver closure", || peers[0].state.lock().unwrap().ended && router.completed.lock().unwrap().len()==1).await?;
        let first_wire = peers[0].state.lock().unwrap().wire.clone();
        let first = upstream_entity(&first_wire)?;
        if held_head {
            if first!=(502,DEFAULT_UPSTREAM_ERROR.to_vec(),true) || state.lock().unwrap().head_active!=0 || state.lock().unwrap().head_drops!=1 {return Err("default head expiry result or ownership".into());}
        } else if first!=(200,b": ping\n\n: progress\n\n".to_vec(),false) || state.lock().unwrap().body_drops!=1 || state.lock().unwrap().body_eof!=0 {return Err("default body expiry changed forwarded bytes/framing or EOF".into());}
        if !router.completed.lock().unwrap()[0].is_null() || state.lock().unwrap().requests.len()!=1 {return Err("failure confirmed or automatically retried".into());}
        // A fresh, independent request uses the SAME still-owned listener.
        // Connection/request cancellation from the failed attempt is allowed.
        // Root remains runnable while this new real loopback exchange progresses.
        tokio::time::resume(); paused=false;
        peers.push(tokio::time::timeout(BOUND, DefaultPeer::open(address)).await.map_err(|_| "retry connect bound")??);
        tokio::time::timeout(BOUND, upstream_until("retry clean receiver close", || peers[1].state.lock().unwrap().ended && router.completed.lock().unwrap().len()==2)).await.map_err(|_| "retry bound")??;
        let retry_wire=peers[1].state.lock().unwrap().wire.clone();
        if upstream_entity(&retry_wire)?!=(200,DEFAULT_UPSTREAM_SUCCESS.to_vec(),true)
            || router.completed.lock().unwrap()[1]!=json!({"model":"claude-sonnet-5-5","continuation_model":"claude-sonnet-5-5","stop_reason":"end_turn","tool_uses":[]}) {
            return Err("fresh independent request failed or confirmed wrong execution evidence".into());
        }
        let s=state.lock().unwrap();
        if s.requests.len()!=2 || s.requests[0]!=s.requests[1] || s.body_eof!=1 {return Err("request replay/EOF count".into());}
        for request in &s.requests {
            if request["method"]!="POST" || request["url"]!="https://api.anthropic.com/v1/messages"
                || request["body_text"]!=DEFAULT_UPSTREAM_REQUEST || request["body_bytes"]!=DEFAULT_UPSTREAM_REQUEST.len()
                || request["body_sha256"]!=upstream_sha(DEFAULT_UPSTREAM_REQUEST.as_bytes())
                || request["headers"]!=json!({"content-type":"application/json","x-api-key":"synthetic-upstream-default-provider-key",
                    "accept-encoding":"identity","content-length":"123"}) {
                return Err("complete expected forwarded request differed".into());
            }
        }
        if serde_json::to_value(&gateway.config).unwrap()!=config_before || serde_json::to_vec(&env).unwrap()!=env_before {return Err("input/config mutation".into());}
        Ok(json!({"id":if held_head {"default600000-held-head"} else {"default600000-held-body"},
            "config":config_before,"default_ms":600000,"evaluator_timeout_ms":0,"pending_at_ms":599999,"expired_at_ms":600000,
            "first_status":first.0,"first_entity_text":String::from_utf8(first.1).map_err(|_| "first entity UTF8")?,"first_framing_complete":first.2,
            "first_wire_hex":first_wire.iter().map(|b|format!("{b:02x}")).collect::<String>(),
            "retry_status":200,"retry_entity_text":std::str::from_utf8(DEFAULT_UPSTREAM_SUCCESS).unwrap(),"retry_framing_complete":true,
            "retry_wire_hex":retry_wire.iter().map(|b|format!("{b:02x}")).collect::<String>(),
            "request_counts":[1,2],"requests":s.requests,"completed":*router.completed.lock().unwrap(),
            "native":{"runtime_started_paused":true,"setup_clock_unchanged":origin==fixture_epoch,
                "head_drops":s.head_drops,"head_polls":s.head_polls,
                "positive_head_repoll_at_599999":if held_head {Some(head_boundary_repoll)}else{None},
                "body_drops":s.body_drops,"provider_eof":s.body_eof,
                "request_tokens_cancelled":router.tokens.lock().unwrap().iter().map(CancellationToken::is_cancelled).collect::<Vec<_>>()},
            "input_unchanged":true,"config_unchanged":true,"events":*events.lock().unwrap()}))
    }.await;
    // Snapshot failure state BEFORE harness cancellation can release providers
    // or close receivers. This is diagnostic evidence, never a success oracle.
    let failure_observation = result.as_ref().err().map(|error| {
        let state = state.lock().unwrap();
        json!({"error":error,"elapsed_ms":Instant::now().duration_since(fixture_epoch).as_millis().to_string(),
            "head_active":state.head_active,"head_polls":state.head_polls,"head_drops":state.head_drops,
            "body_polls":state.body_polls,"body_drops":state.body_drops,"provider_eof":state.body_eof,
            "requests":state.requests,"completed":*router.completed.lock().unwrap(),"events":*events.lock().unwrap(),
            "peers":peers.iter().map(|peer| {let state=peer.state.lock().unwrap();json!({"ended":state.ended,
                "read_error":state.read_error,"wire_hex":state.wire.iter().map(|b|format!("{b:02x}")).collect::<String>()})}).collect::<Vec<_>>()})
    });
    // Always resume first. This child test module retains the private listener
    // JoinHandle so a timeout cannot detach it by dropping public close().
    // Cancellation + awaited task is the same ordinary GatewayHandle::close
    // teardown; a forced abort is reported as invalid fixture cleanup.
    if paused {
        tokio::time::resume();
    }
    router.release.cancel();
    let mut listener = handle.take().unwrap();
    listener.cancellation.cancel();
    let mut listener_task = listener.task.take().unwrap();
    let close = matches!(
        tokio::time::timeout(BOUND, &mut listener_task).await,
        Ok(Ok(()))
    );
    if !close {
        listener_task.abort();
        let _ = listener_task.await;
    }
    drop(listener);
    let mut joined = true;
    for mut peer in peers {
        peer.stop.cancel();
        match tokio::time::timeout(BOUND, &mut peer.task).await {
            Ok(Ok(Ok(()))) => {}
            _ => {
                joined = false;
                peer.task.abort();
                let _ = peer.task.await;
            }
        }
    }
    drop(gateway);
    drop(transport);
    assert!(close, "listener close was not observed complete");
    assert!(joined, "a receiver failed or required forced cleanup");
    assert_eq!(router.closes.load(Ordering::SeqCst), 1);
    assert!(
        transport_weak.upgrade().is_none(),
        "transport owner retained after close"
    );
    match result {
        Ok(mut row) => {
            row["native"]["listener_close_completed"] = json!(true);
            row["native"]["receiver_tasks_joined"] = json!(true);
            row["native"]["transport_released"] = json!(true);
            println!("UPSTREAM_DEFAULT_BOUNDARY {}", row);
        }
        Err(error) => {
            println!("UPSTREAM_DEFAULT_FAILURE {}", failure_observation.unwrap());
            panic!("default upstream fixture failed after owned cleanup: {error}");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn default_upstream_600000_held_head_expires_at_equality_and_listener_retries() {
    upstream_default_case(true).await;
}

#[tokio::test(start_paused = true)]
async fn default_upstream_600000_held_body_keeps_forwarded_200_and_listener_retries() {
    upstream_default_case(false).await;
}
