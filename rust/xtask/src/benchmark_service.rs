//! Identical loopback-only services for both complete gateway executables.
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub const RESPONSE: &[u8] = br#"{"id":"synthetic-benchmark-message","type":"message","role":"assistant","model":"claude-sonnet-5","content":[{"type":"text","text":"Synthetic benchmark response."}],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":32,"output_tokens":8}}"#;
#[derive(Default)]
pub struct Counts {
    evaluator: AtomicU64,
    upstream: AtomicU64,
    count: AtomicU64,
    invalid: AtomicU64,
}
impl Counts {
    pub fn snapshot(&self) -> [u64; 4] {
        [&self.evaluator, &self.upstream, &self.count, &self.invalid]
            .map(|v| v.load(Ordering::SeqCst))
    }
}
pub struct Mock {
    pub address: std::net::SocketAddr,
    pub counts: Arc<Counts>,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}
impl Mock {
    pub async fn start(delay_ms: u64, expected_model: &'static str) -> Result<Self, String> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| "Cannot bind synthetic benchmark service")?;
        let address = listener
            .local_addr()
            .map_err(|_| "Cannot inspect benchmark listener")?;
        let counts = Arc::new(Counts::default());
        let count = counts.clone();
        let cancel = CancellationToken::new();
        let shutdown = cancel.clone();
        let task = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                let accepted = tokio::select! {
                    _ = shutdown.cancelled() => break,
                    value = listener.accept() => value,
                    _ = tasks.join_next(), if !tasks.is_empty() => continue,
                };
                let Ok((socket, _)) = accepted else { break };
                let count = count.clone();
                tasks.spawn(async move {
                    let service = service_fn(move |request| {
                        handle(request, count.clone(), delay_ms, expected_model)
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(socket), service)
                        .await;
                });
            }
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        });
        Ok(Self {
            address,
            counts,
            cancel,
            task,
        })
    }
}
async fn handle(
    request: Request<Incoming>,
    counts: Arc<Counts>,
    delay_ms: u64,
    expected_model: &str,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let path = request.uri().path().to_owned();
    let bytes = Limited::new(request.into_body(), 32 * 1024 * 1024)
        .collect()
        .await;
    let parsed = bytes
        .ok()
        .and_then(|v| serde_json::from_slice::<Value>(&v.to_bytes()).ok());
    let body = match path.as_str() {
        "/v1/systemone" => {
            counts.evaluator.fetch_add(1, Ordering::SeqCst);
            if parsed.as_ref().is_none_or(|v| !v.is_object()) {
                counts.invalid.fetch_add(1, Ordering::SeqCst);
            }
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            json!({"answers":{"tier":{"choice":"sonnet","confidence":1}}})
                .to_string()
                .into_bytes()
        }
        "/v1/messages/count_tokens" => {
            counts.count.fetch_add(1, Ordering::SeqCst);
            br#"{"input_tokens":1000}"#.to_vec()
        }
        "/v1/messages" => {
            counts.upstream.fetch_add(1, Ordering::SeqCst);
            if parsed.as_ref().is_none_or(|v| v["model"] != expected_model) {
                counts.invalid.fetch_add(1, Ordering::SeqCst);
            }
            RESPONSE.to_vec()
        }
        _ => {
            counts.invalid.fetch_add(1, Ordering::SeqCst);
            b"{}".to_vec()
        }
    };
    Ok(Response::builder()
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap())
}
