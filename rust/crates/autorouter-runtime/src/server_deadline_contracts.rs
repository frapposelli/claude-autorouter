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
