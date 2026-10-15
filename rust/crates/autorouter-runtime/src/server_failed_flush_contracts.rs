//! An actual gateway response write must establish delivery before replacing
//! a confirmed tool owner. The provider and client bytes here are synthetic.
use super::*;
use autorouter_core::config::read_config;
use autorouter_core::turn_state::{
    ContinuationEvidence, Pin as TaskPin, Selection, ToolIdentity, ToolOwner, TurnState,
};
use http_body_util::BodyExt;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

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

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
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
