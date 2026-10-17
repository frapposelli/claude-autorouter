//! Complete frozen server #25–#28 stream failure and cancellation schedules; #26 async-return remains partial.
#[allow(dead_code)] // Shared helpers retain separate policies used by sibling targets.
#[path = "support/server_contract.rs"]
mod contract;
use contract::{
    compare_metadata, compare_request, decode, event_names, events, headers, parsed, same_request,
    sink, upstream_rows,
};
#[path = "support/server_stream_failure.rs"]
mod peer;
use autorouter_core::{config::read_config, js_json::JsDocument, router::RouteOptions};
use autorouter_runtime::{
    evaluator::EvaluationError,
    http_client::{HttpTransport, NativeHttpClient},
    response_observer::CompletionEvidence,
    server::{Gateway, GatewayHandle, GatewayRouter},
    server_events::{EventSink, EventSinks},
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::{HeaderMap, Request};
use peer::{Observation, Peer};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

const CORPUS: &str = include_str!("../../../parity/cases/server-stream-failure-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../parity/cases/server-stream-failure-contracts.capture.json");
const LIMIT: usize = 2 * 1024 * 1024;
const BOUND: Duration = Duration::from_secs(5);
type Events = Arc<Mutex<Vec<Value>>>;
fn case(number: usize) -> Value {
    assert_eq!(
        format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
        "93427d9e2bcb68304b4bd28af30974083a0238b97d305377725029bcef87a21f"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(CAPTURE.as_bytes())),
        "0eb917770f8b49a114ce914d9a31b6c24a987d2c980221b82305d9a3f3427332"
    );
    let metadata: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(metadata["static_assertions"], 13);
    assert_eq!(metadata["expanded_assertions"], 12);
    assert_eq!(metadata["callbacks_executed"], 4);
    CORPUS
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|row| row["source_test"] == format!("test/server.test.mjs#{number}"))
        .unwrap()
}
struct MockRouter {
    decisions: Vec<Value>,
    calls: Mutex<Vec<Value>>,
    completions: Mutex<Vec<Value>>,
    closed: AtomicBool,
    hold: bool,
    entered: CancellationToken,
    cancelled: AtomicBool,
    dropped: AtomicBool,
    observed_token: Mutex<Option<CancellationToken>>,
}
struct RouteDrop<'a>(&'a AtomicBool);
impl Drop for RouteDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
impl GatewayRouter for MockRouter {
    async fn route(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        _: &HeaderMap,
        cancellation: &CancellationToken,
        _: &str,
    ) -> Result<Value, EvaluationError> {
        let index = {
            let mut rows = self.calls.lock().unwrap();
            let index = rows.len();
            assert!(index < self.decisions.len());
            rows.push(json!({"document":parsed(document.stringify().as_bytes()),"scope":options.scope,"prompt_id":options.prompt_id,"request_class":options.request_class,"request_id":options.request_id,"count_tokens":options.count_tokens,"signal_live":!cancellation.is_cancelled()}));
            index
        };
        *self.observed_token.lock().unwrap() = Some(cancellation.clone());
        let _drop = RouteDrop(&self.dropped);
        self.entered.cancel();
        if self.hold {
            cancellation.cancelled().await;
            self.cancelled.store(true, Ordering::SeqCst);
            return Err(EvaluationError::Cancelled);
        }
        Ok(self.decisions[index].clone())
    }
    fn complete(&self, id: &str, evidence: &Value) -> bool {
        let mut rows = self.completions.lock().unwrap();
        assert!(rows.len() < 32);
        rows.push(json!({"id":id,"evidence":evidence,"accepted":false}));
        false
    }
    fn complete_exact(&self, id: &str, evidence: &CompletionEvidence) -> bool {
        let mut rows = self.completions.lock().unwrap();
        assert!(rows.len() < 32);
        rows.push(json!({"id":id,"evidence":evidence,"accepted":false}));
        false
    }
    fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
    async fn close(&self) {
        self.shutdown();
    }
}
struct Fixture {
    client: Arc<NativeHttpClient>,
    router: Arc<MockRouter>,
    gateway: Option<GatewayHandle>,
    peer: Option<Peer>,
    statuses: Events,
    history: Events,
    logs: Events,
    log_history: Events,
    decisions: Events,
    callback_outcomes: Events,
}
struct Reply {
    status: Option<u16>,
    headers: HeaderMap,
    bytes: Vec<u8>,
    body_rejected: bool,
    request_cancelled: bool,
}
struct Seen {
    requests: Vec<Observation>,
    routes: Vec<Value>,
    completions: Vec<Value>,
    statuses: Vec<Value>,
    history: Vec<Value>,
    logs: Vec<Value>,
    decisions: Vec<Value>,
    callback_outcomes: Vec<Value>,
    router_cancelled: bool,
    cancellation_branch_resumed: bool,
    router_dropped: bool,
    provider_destroyed: bool,
    provider_peer_closed: bool,
}
impl Fixture {
    async fn new(number: usize, row: &Value) -> Self {
        let peer = Peer::start(number, upstream_rows(row)).await;
        let client = Arc::new(NativeHttpClient::new().unwrap());
        let mut config =
            read_config(&row["config_arguments"][0][0], false, Path::new("/tmp")).unwrap();
        config.local_token = row["gateway_config"]["localToken"]
            .as_str()
            .map(str::to_owned);
        config.upstream = format!("http://{}", peer.address);
        config.upstream_timeout_ms = row["gateway_config"]["upstreamTimeoutMs"].as_u64().unwrap();
        let router = Arc::new(MockRouter {
            decisions: row["route_calls"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["decision"].clone())
                .collect(),
            calls: Mutex::new(Vec::new()),
            completions: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
            hold: number == 28,
            entered: CancellationToken::new(),
            cancelled: AtomicBool::new(false),
            dropped: AtomicBool::new(false),
            observed_token: Mutex::new(None),
        });
        let statuses = Events::default();
        let history = Events::default();
        let logs = Events::default();
        let log_history = Events::default();
        let decisions = Events::default();
        let callback_outcomes = Events::default();
        let record_status = sink(&statuses, &history);
        let outcomes = callback_outcomes.clone();
        let status: EventSink = Arc::new(move |document| {
            let event = document.to_serde_observation_lossy()["event"].clone();
            record_status(document);
            let mut outcomes = outcomes.lock().unwrap();
            assert!(outcomes.len() < 128);
            let invocation = outcomes.len() + 1;
            let throws = number == 26 && invocation % 2 == 1;
            outcomes.push(json!({"invocation":invocation,"event":event,"outcome":if throws{"synchronous_unwind"}else if number==26{"async_return_not_representable_in_synchronous_EventSink"}else{"synchronous_return"}}));
            drop(outcomes);
            if throws {
                panic!("Status storage unavailable");
            }
        });
        let decision_rows = decisions.clone();
        let decision: EventSink = Arc::new(move |document| {
            let mut rows = decision_rows.lock().unwrap();
            assert!(rows.len() < 8);
            rows.push(document.to_serde_observation_lossy());
        });
        let gateway = Gateway::with_router(
            config,
            client.clone(),
            router.clone(),
            EventSinks {
                status: Some(status),
                log: Some(sink(&logs, &log_history)),
                decision: (number == 28).then_some(decision),
                ..Default::default()
            },
        )
        .unwrap()
        .listen(0)
        .await
        .unwrap();
        Self {
            client,
            router,
            gateway: Some(gateway),
            peer: Some(peer),
            statuses,
            history,
            logs,
            log_history,
            decisions,
            callback_outcomes,
        }
    }
    async fn response(&self, row: &Value) -> Result<hyper::Response<Incoming>, String> {
        let url = url::Url::parse(row["url"].as_str().unwrap()).unwrap();
        let mut request = Request::builder()
            .method(row["method"].as_str().unwrap())
            .uri(format!(
                "http://{}{}",
                self.gateway.as_ref().unwrap().address,
                url.path()
            ));
        *request.headers_mut().unwrap() = headers(&row["headers"]);
        self.client
            .request(
                request
                    .body(Full::new(Bytes::from(
                        row["body"].as_str().unwrap().to_owned(),
                    )))
                    .unwrap(),
            )
            .await
            .map_err(|error| error.to_string())
    }
    async fn wait_for_status(&self, event: &str) -> Result<(), String> {
        contract::wait_for_status(&self.statuses, event).await
    }
    async fn finish(mut self) -> Result<Seen, String> {
        // Preserve the actual pre-close observation boundary. Shutdown itself
        // must not manufacture evidence of request cancellation or retirement.
        let router_cancelled = self
            .router
            .observed_token
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled);
        let cancellation_branch_resumed = self.router.cancelled.load(Ordering::SeqCst);
        let router_dropped = self.router.dropped.load(Ordering::SeqCst);
        let closed = tokio::time::timeout(BOUND, self.gateway.take().unwrap().close()).await;
        let requests = self.peer.as_ref().unwrap().observations();
        let provider_destroyed = self.peer.as_ref().unwrap().destroyed();
        let provider_peer_closed = self.peer.as_ref().unwrap().peer_closed();
        let peer_closed = self.peer.take().unwrap().close().await;
        closed.map_err(|_| "gateway cleanup timed out")?;
        peer_closed?;
        if !self.router.closed.load(Ordering::SeqCst) {
            return Err("router cleanup unobserved".into());
        }
        contract::check_recorders([&self.statuses, &self.history, &self.logs, &self.log_history])?;
        Ok(Seen {
            requests,
            routes: self.router.calls.lock().unwrap().clone(),
            completions: self.router.completions.lock().unwrap().clone(),
            statuses: self.statuses.lock().unwrap().clone(),
            history: self.history.lock().unwrap().clone(),
            logs: self.logs.lock().unwrap().clone(),
            decisions: self.decisions.lock().unwrap().clone(),
            callback_outcomes: self.callback_outcomes.lock().unwrap().clone(),
            router_cancelled,
            cancellation_branch_resumed,
            router_dropped,
            provider_destroyed,
            provider_peer_closed,
        })
    }
}
type Assertions = contract::Assertions<1, true>;
fn supplemental_transcripts(row: &Value, reply: &Reply, seen: &Seen) {
    let expected = upstream_rows(row);
    assert_eq!(seen.requests.len(), expected.len());
    for request in &seen.requests {
        let wanted = expected
            .iter()
            .find(|wanted| same_request(request, wanted))
            .unwrap();
        compare_request(request, wanted).unwrap();
    }
    assert_eq!(
        seen.routes.len(),
        row["route_calls"].as_array().unwrap().len()
    );
    for (route, wanted) in seen
        .routes
        .iter()
        .zip(row["route_calls"].as_array().unwrap())
    {
        assert_eq!(route["document"], wanted["document"]);
        assert_eq!(route["signal_live"], true);
        assert_eq!(route["count_tokens"], true);
        for (native, source) in [
            ("scope", "scope"),
            ("prompt_id", "promptId"),
            ("request_class", "requestClass"),
        ] {
            if wanted["options"][source]["$js_type"] == "undefined" {
                assert_eq!(route[native], "");
            } else {
                assert_eq!(route[native], wanted["options"][source]);
            }
        }
        assert_eq!(route["request_id"], seen.history[0]["request_id"]);
    }
    let source = row["statuses"].as_array().unwrap();
    assert_eq!(seen.history.len(), source.len());
    for (wanted, got) in source.iter().zip(&seen.history) {
        assert_eq!(got["request_id"], seen.history[0]["request_id"]);
        compare_metadata(wanted, got);
    }
    assert!(
        seen.completions
            .iter()
            .all(|row| row["evidence"].is_null() && row["accepted"] == false)
    );
    assert!(!seen.completions.is_empty());
    assert!(
        seen.router_dropped,
        "actual route future destroyed before handoff"
    );
    for secret in [
        "Fix a typo",
        "local-test-token-123456789",
        "upstream-secret",
        "classifier-secret",
        "Status storage unavailable",
        "Asynchronous status failure",
    ] {
        assert!(
            !json!({"logs":seen.logs,"statuses":seen.statuses})
                .to_string()
                .contains(secret)
        );
    }
    let wanted = &row["downstream"][0];
    if let Some(status) = wanted["status"].as_u64() {
        assert_eq!(reply.status, Some(status as u16));
    } else {
        assert!(reply.status.is_none());
    }
    let source_bytes = wanted["reads"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|read| read["kind"] == "read" && read["done"] == false)
        .flat_map(|read| decode(&read["base64"]))
        .collect::<Vec<_>>();
    assert_eq!(
        reply.bytes, source_bytes,
        "every original delivered response byte stays unchanged"
    );
    if let Some(directory) = std::env::var_os("AUTOROUTER_NATIVE_PARITY_CAPTURE_DIR") {
        let raw = json!({"schema_version":1,"source_test":row["source_test"],"status":if row["source_test"]=="test/server.test.mjs#26"{"partial_async_callback_API"}else{"passed"},"upstream_requests":seen.requests.iter().map(contract::capture_request).collect::<Vec<_>>(),"route_calls":seen.routes,"complete_calls":seen.completions,"statuses":seen.statuses,"all_emitted_statuses":seen.history,"logs":seen.logs,"decisions":seen.decisions,"callback_outcomes":seen.callback_outcomes,"route_token_cancelled":seen.router_cancelled,"route_cancellation_branch_resumed":seen.cancellation_branch_resumed,"route_future_destroyed":seen.router_dropped,"provider_destroyed_after_first_read":seen.provider_destroyed,"provider_connection_closed_by_gateway":seen.provider_peer_closed,"downstream":{"status":reply.status,"headers":contract::capture_headers(&reply.headers),"body_base64":openssl::base64::encode_block(&reply.bytes),"body_rejected":reply.body_rejected,"request_cancelled":reply.request_cancelled},"projection":"None. Native raw IDs, numbers, timing values, callback kinds, headers and complete delivered bytes remain unchanged."});
        contract::write_row_capture(&directory, &row["id"], &raw, LIMIT, false);
    }
}
#[tokio::test]
async fn original_25_upstream_disconnect_after_first_read_is_not_client_cancel() {
    let row = case(25);
    let f = Fixture::new(25, &row).await;
    let outcome = tokio::time::timeout(BOUND, async {
        let response = f.response(&row["downstream"][0]).await?;
        let (parts, mut body) = response.into_parts();
        let first = body
            .frame()
            .await
            .ok_or("first frame absent")?
            .map_err(|_| "first frame failed")?
            .into_data()
            .map_err(|_| "first frame is trailers")?;
        assert_eq!(first.as_ref(), b": ping\n\n");
        f.peer.as_ref().unwrap().disconnect();
        let mut bytes = first.to_vec();
        let mut rejected = false;
        while let Some(frame) = body.frame().await {
            match frame {
                Ok(frame) => {
                    if let Some(data) = frame.data_ref() {
                        bytes.extend_from_slice(data);
                    }
                }
                Err(_) => {
                    rejected = true;
                    break;
                }
            }
        }
        f.wait_for_status("request_error").await?;
        Ok::<_, String>(Reply {
            status: Some(parts.status.as_u16()),
            headers: parts.headers,
            bytes,
            body_rejected: rejected,
            request_cancelled: false,
        })
    })
    .await;
    let seen = f.finish().await.unwrap();
    let reply = outcome.unwrap().unwrap();
    let mut a = Assertions::new(25);
    a.check(1, reply.body_rejected);
    a.check(2, events(&seen.statuses, "request_cancelled").is_empty());
    a.check(3, events(&seen.statuses, "request_complete").is_empty());
    a.finish();
    assert!(seen.provider_destroyed);
    supplemental_transcripts(&row, &reply, &seen);
}
#[tokio::test]
async fn original_26_sync_callback_unwinds_are_isolated_async_return_remains_partial() {
    let row = case(26);
    let f = Fixture::new(26, &row).await;
    let outcome = tokio::time::timeout(BOUND, async {
        let response = f.response(&row["downstream"][0]).await?;
        let (parts, body) = response.into_parts();
        let bytes = Limited::new(body, LIMIT)
            .collect()
            .await
            .map_err(|_| "finite provider body failed")?
            .to_bytes()
            .to_vec();
        f.wait_for_status("request_complete").await?;
        Ok::<_, String>(Reply {
            status: Some(parts.status.as_u16()),
            headers: parts.headers,
            bytes,
            body_rejected: false,
            request_cancelled: false,
        })
    })
    .await;
    let seen = f.finish().await.unwrap();
    let reply = outcome.unwrap().unwrap();
    let mut a = Assertions::new(26);
    a.check(1, reply.status == Some(200));
    a.check(
        2,
        parsed(&reply.bytes)["model"] == "claude-haiku-4-5-20251001",
    );
    a.check(3, seen.callback_outcomes.len() == 5);
    a.finish();
    assert_eq!(
        seen.callback_outcomes
            .iter()
            .filter(|row| row["outcome"] == "synchronous_unwind")
            .count(),
        3
    );
    assert_eq!(
        seen.callback_outcomes
            .iter()
            .filter(
                |row| row["outcome"] == "async_return_not_representable_in_synchronous_EventSink"
            )
            .count(),
        2
    );
    supplemental_transcripts(&row, &reply, &seen);
}
#[tokio::test]
async fn original_27_production_upstream_deadline_after_headers_is_error() {
    let row = case(27);
    assert_eq!(row["gateway_config"]["upstreamTimeoutMs"], 40);
    let f = Fixture::new(27, &row).await;
    let outcome = tokio::time::timeout(BOUND, async {
        let response = f.response(&row["downstream"][0]).await?;
        let (parts, mut body) = response.into_parts();
        let mut bytes = Vec::new();
        let mut rejected = false;
        while let Some(frame) = body.frame().await {
            match frame {
                Ok(frame) => {
                    if let Some(data) = frame.data_ref() {
                        bytes.extend_from_slice(data);
                    }
                }
                Err(_) => {
                    rejected = true;
                    break;
                }
            }
        }
        f.wait_for_status("request_error").await?;
        Ok::<_, String>(Reply {
            status: Some(parts.status.as_u16()),
            headers: parts.headers,
            bytes,
            body_rejected: rejected,
            request_cancelled: false,
        })
    })
    .await;
    let seen = f.finish().await.unwrap();
    let reply = outcome.unwrap().unwrap();
    let mut a = Assertions::new(27);
    a.check(1, reply.body_rejected);
    a.check(2, events(&seen.statuses, "request_cancelled").is_empty());
    a.check(3, events(&seen.statuses, "request_error").len() == 1);
    a.finish();
    assert!(
        !seen.provider_destroyed,
        "test must not manufacture the production deadline"
    );
    assert_eq!(reply.bytes, b": ping\n\n");
    supplemental_transcripts(&row, &reply, &seen);
}
async fn cancellable_raw_client(
    address: std::net::SocketAddr,
    row: Value,
    cancellation: CancellationToken,
) -> Result<(), String> {
    let mut socket = tokio::net::TcpStream::connect(address)
        .await
        .map_err(|_| "client connect failed")?;
    let body = row["body"].as_str().unwrap();
    let token = row["headers"]["x-api-key"].as_str().unwrap();
    let request = format!(
        "POST /v1/messages HTTP/1.1\r\nHost: {address}\r\nx-api-key: {token}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    socket
        .write_all(request.as_bytes())
        .await
        .map_err(|_| "client request write failed")?;
    let mut one = [0u8; 1];
    tokio::select! {_ = cancellation.cancelled()=>Err("native_downstream_caller_cancelled".into()),result=socket.read(&mut one)=>{match result{Ok(0)=>Err("response socket closed before caller cancellation".into()),Ok(_)=>Ok(()),Err(_)=>Err("unrequested response read error".into())}}}
}
#[tokio::test]
async fn original_28_real_downstream_disconnect_cancels_classification_without_forwarding() {
    let row = case(28);
    let f = Fixture::new(28, &row).await;
    let caller = CancellationToken::new();
    let client = tokio::spawn(cancellable_raw_client(
        f.gateway.as_ref().unwrap().address,
        row["downstream"][0].clone(),
        caller.clone(),
    ));
    let outcome = tokio::time::timeout(BOUND, async {
        f.router.entered.cancelled().await;
        assert!(!f.router.cancelled.load(Ordering::SeqCst));
        caller.cancel();
        let result = client.await.map_err(|_| "client owner task failed")?;
        f.wait_for_status("request_cancelled").await?;
        Ok::<_, String>(result)
    })
    .await;
    let actual_cancelled = f
        .router
        .observed_token
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled);
    let actual_dropped = f.router.dropped.load(Ordering::SeqCst);
    let seen = f.finish().await.unwrap();
    let result = outcome.unwrap().unwrap();
    let mut a = Assertions::new(28);
    // Original assertion1 is a counterfactual provider guard, executing zero
    // times. The live raw peer fails with that exact site if any request arrives.
    assert!(
        seen.requests.is_empty(),
        "server#28:assert-1 counterfactual guard"
    );
    a.check(
        2,
        result
            .as_ref()
            .is_err_and(|error| error == "native_downstream_caller_cancelled"),
    );
    a.check(
        3,
        event_names(&seen.statuses) == ["request_start", "request_cancelled"],
    );
    a.check(4, seen.decisions.is_empty());
    a.finish();
    assert!(
        actual_cancelled && actual_dropped,
        "real borrowed routing cancellation and future destruction precede owner close"
    );
    let reply = Reply {
        status: None,
        headers: HeaderMap::new(),
        bytes: Vec::new(),
        body_rejected: false,
        request_cancelled: true,
    };
    supplemental_transcripts(&row, &reply, &seen);
}
