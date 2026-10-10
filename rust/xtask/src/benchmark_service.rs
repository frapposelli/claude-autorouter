//! Identical loopback-only services for both complete gateway executables.
use super::stream;
use bytes::Bytes;
use http_body_util::{BodyExt, Either, Full, Limited};
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
pub struct ExpectedRequests {
    inference: Value,
    count: Value,
}
impl ExpectedRequests {
    pub fn new(mut inference: Value, model: &str) -> Self {
        inference["model"] = json!(model);
        let mut count = inference.clone();
        let object = count.as_object_mut().unwrap();
        object.remove("max_tokens");
        object.remove("stream");
        object.insert("model".into(), json!("claude-haiku-4-5-20251001"));
        Self { inference, count }
    }
    fn matches(&self, actual: &mut Option<Value>, count: bool) -> bool {
        let expected = if count { &self.count } else { &self.inference };
        let Some(actual) = actual else {
            return false;
        };
        let Some(content) = actual.pointer_mut("/messages/0/content") else {
            return false;
        };
        let (Some(text), Some(template)) = (
            content.as_str(),
            expected
                .pointer("/messages/0/content")
                .and_then(Value::as_str),
        ) else {
            return false;
        };
        // Only the driver's twelve ASCII decimal request-index bytes vary.
        // Everything else, including all tool schemas/extensions, is exact.
        let Some(prefix) = template.strip_suffix("000000000000") else {
            return false;
        };
        if text.len() != template.len()
            || !text.starts_with(prefix)
            || !text.as_bytes()[prefix.len()..]
                .iter()
                .all(u8::is_ascii_digit)
        {
            return false;
        }
        *content = json!(template);
        actual == expected
    }
}
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
    pub streams: Arc<stream::Counts>,
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
    pub async fn start(
        delay_ms: u64,
        expected_model: &'static str,
        fixture: Option<stream::Fixture>,
        expected_requests: Option<ExpectedRequests>,
    ) -> Result<Self, String> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| "Cannot bind synthetic benchmark service")?;
        let address = listener
            .local_addr()
            .map_err(|_| "Cannot inspect benchmark listener")?;
        let counts = Arc::new(Counts::default());
        let count = counts.clone();
        let streams = Arc::new(stream::Counts::default());
        let stream_counts = streams.clone();
        let expected_requests = expected_requests.map(Arc::new);
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
                let fixture = fixture.clone();
                let stream_counts = stream_counts.clone();
                let expected_requests = expected_requests.clone();
                tasks.spawn(async move {
                    let service = service_fn(move |request| {
                        handle(
                            request,
                            count.clone(),
                            delay_ms,
                            expected_model,
                            fixture.clone(),
                            stream_counts.clone(),
                            expected_requests.clone(),
                        )
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
            streams,
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
    fixture: Option<stream::Fixture>,
    streams: Arc<stream::Counts>,
    expected_requests: Option<Arc<ExpectedRequests>>,
) -> Result<Response<Either<Full<Bytes>, stream::Stream>>, Infallible> {
    let path = request.uri().path().to_owned();
    let bytes = Limited::new(request.into_body(), 32 * 1024 * 1024)
        .collect()
        .await;
    let mut parsed = bytes
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
            if expected_requests
                .as_ref()
                .is_some_and(|expected| !expected.matches(&mut parsed, true))
            {
                counts.invalid.fetch_add(1, Ordering::SeqCst);
            }
            br#"{"input_tokens":1000}"#.to_vec()
        }
        "/v1/messages" => {
            counts.upstream.fetch_add(1, Ordering::SeqCst);
            if parsed.as_ref().is_none_or(|v| {
                v["model"] != expected_model
                    || v["stream"].as_bool().unwrap_or(false) != fixture.is_some()
            }) {
                counts.invalid.fetch_add(1, Ordering::SeqCst);
            }
            if expected_requests
                .as_ref()
                .is_some_and(|expected| !expected.matches(&mut parsed, false))
            {
                counts.invalid.fetch_add(1, Ordering::SeqCst);
            }
            if let Some(fixture) = fixture {
                return Ok(Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(Either::Right(stream::Stream::new(fixture, streams)))
                    .unwrap());
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
        .body(Either::Left(Full::new(Bytes::from(body))))
        .unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workload_verification_rejects_missing_context_and_extra_generation_count_fields() {
        let original = json!({"model":"source","max_tokens":128,"stream":true,"messages":[{"role":"user","content":"Synthetic benchmark round1: 000000000000"}],"tools":[{"name":"synthetic","input_schema":{"type":"object","properties":{"value":{"type":"string"}}}}]});
        let expected = ExpectedRequests::new(original.clone(), "target");
        let mut valid = original;
        valid["model"] = json!("target");
        valid["messages"][0]["content"] = json!("Synthetic benchmark round1: 000000002199");
        assert!(expected.matches(&mut Some(valid.clone()), false));
        for key in ["messages", "tools", "max_tokens", "stream"] {
            let mut bad = valid.clone();
            bad.as_object_mut().unwrap().remove(key);
            assert!(!expected.matches(&mut Some(bad), false), "{key}");
        }
        let mut bad = valid.clone();
        bad["tools"][0]["input_schema"]["properties"] = json!({});
        assert!(!expected.matches(&mut Some(bad), false));
        let mut bad = valid;
        bad["messages"][0]["content"] = json!("Different benchmark round1: 000000002199");
        assert!(!expected.matches(&mut Some(bad), false));
        assert!(expected.matches(&mut Some(expected.count.clone()), true));
        for key in ["max_tokens", "stream"] {
            let mut bad = expected.count.clone();
            bad[key] = json!(true);
            assert!(!expected.matches(&mut Some(bad), true));
        }
        assert!(!expected.matches(&mut None, false));
    }
}
