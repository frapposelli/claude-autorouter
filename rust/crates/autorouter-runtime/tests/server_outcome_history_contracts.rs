//! Complete frozen final server #33–#37 fallback continuity and outcome-history schedules.
#[allow(dead_code)] // Shared peer also supplies streaming interfaces to other targets.
#[path = "support/server_streaming_routing.rs"]
mod peer;
#[path = "support/server_outcome_history.rs"]
mod real;
use autorouter_core::{config::read_config, js_json::JsDocument, router::RouteOptions};
use autorouter_runtime::{
    evaluator::EvaluationError,
    http_client::{HttpTransport, NativeHttpClient},
    observer_tasks::{ObserverFuture, ObserverLimits, ObserverRejected, ObserverTasks},
    response_observer::CompletionEvidence,
    server::{Gateway, GatewayHandle, GatewayRouter},
    server_events::{EventSink, EventSinks, RecordSink},
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{HeaderMap, Request};
use peer::{Observation, Peer, ResponsePlan};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const CORPUS: &str = include_str!("../../../parity/cases/server-outcome-history-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../parity/cases/server-outcome-history-contracts.capture.json");
const LIMIT: usize = 2 * 1024 * 1024;
const BOUND: Duration = Duration::from_secs(5);
type Events = Arc<Mutex<Vec<Value>>>;
fn case(number: usize) -> Value {
    assert_eq!(
        format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
        "17c1ea2763c1477c69dfebc49f7613e8e691931f228b43b83ab34464f49d4223"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(CAPTURE.as_bytes())),
        "fe5253ff62631aba8a64753e5b12211edf93c531de4fc1bfbf1d8461de95abcb"
    );
    let metadata: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(metadata["static_assertions"], 33);
    assert_eq!(metadata["expanded_assertions"], 55);
    assert_eq!(metadata["callbacks_executed"], 5);
    CORPUS
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|row| row["source_test"] == format!("test/server.test.mjs#{number}"))
        .unwrap()
}
fn parsed(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}
fn headers(value: &Value) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (key, value) in value.as_object().unwrap() {
        out.insert(
            hyper::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
            value.as_str().unwrap().parse().unwrap(),
        );
    }
    out
}
fn header_is(map: &HeaderMap, name: &str, value: Option<&str>) -> bool {
    map.get(name).and_then(|value| value.to_str().ok()) == value
}
fn decode(value: &Value) -> Vec<u8> {
    let value = value.as_str().unwrap();
    assert!(value.len() < LIMIT * 2);
    openssl::base64::decode_block(value).unwrap()
}
fn chunks(row: &Value) -> Vec<Bytes> {
    row["response_writes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| Bytes::from(decode(&row["base64"])))
        .collect()
}
fn response_bytes(row: &Value) -> Vec<u8> {
    chunks(row)
        .iter()
        .flat_map(|chunk| chunk.iter().copied())
        .collect()
}
fn upstream_rows(row: &Value) -> Vec<Value> {
    row["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["role"] == "upstream")
        .unwrap()["requests"]
        .as_array()
        .unwrap()
        .clone()
}
fn events(rows: &[Value], name: &str) -> Vec<Value> {
    rows.iter()
        .filter(|row| row["event"] == name)
        .cloned()
        .collect()
}
fn event_names(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .map(|row| row["event"].as_str().unwrap().to_owned())
        .collect()
}
fn sink(active: &Events, history: &Events) -> EventSink {
    let (active, history) = (active.clone(), history.clone());
    Arc::new(move |event| {
        let value = event.to_serde_observation_lossy();
        for rows in [&active, &history] {
            let mut rows = rows.lock().unwrap();
            if rows.len() >= 128 {
                if rows.len() == 128 {
                    rows.push(json!({"event":"fixture_observer_overflow"}));
                }
                continue;
            }
            rows.push(value.clone());
        }
    })
}
fn same_request(observed: &Observation, wanted: &Value) -> bool {
    observed.method == wanted["method"].as_str().unwrap()
        && observed.path == wanted["path"].as_str().unwrap()
        && header_is(
            &observed.headers,
            "x-claude-code-session-id",
            wanted["headers"]["x-claude-code-session-id"].as_str(),
        )
}
fn compare_request(observed: &Observation, wanted: &Value) -> Result<(), String> {
    if !same_request(observed, wanted) {
        return Err("unexpected peer method/path/session".into());
    }
    for name in [
        "authorization",
        "x-api-key",
        "x-autorouter-token",
        "cookie",
        "anthropic-beta",
        "anthropic-version",
        "accept-encoding",
        "x-claude-code-session-id",
        "x-claude-code-agent-id",
        "x-claude-code-prompt-id",
        "x-claude-code-request-class",
    ] {
        if !header_is(
            &observed.headers,
            name,
            wanted["headers"].get(name).and_then(Value::as_str),
        ) {
            return Err(format!("semantic header mismatch: {name}"));
        }
    }
    let expected = wanted["request_body_utf8"].as_str().unwrap().as_bytes();
    if expected.is_empty() {
        if !observed.body.is_empty() {
            return Err("unexpected discovery body".into());
        }
    } else if parsed(&observed.body) != parsed(expected) {
        return Err("complete forwarded request differs".into());
    }
    Ok(())
}
enum RouterImpl {
    Mock(Vec<Value>),
    Real(Box<real::OriginalRouter>),
}
struct AuditRouter {
    inner: RouterImpl,
    calls: Mutex<Vec<Value>>,
    completions: Mutex<Vec<Value>>,
    closed: AtomicBool,
}
impl GatewayRouter for AuditRouter {
    async fn route(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> Result<Value, EvaluationError> {
        let index = {
            let mut calls = self.calls.lock().unwrap();
            let index = calls.len();
            assert!(index < 8);
            calls.push(json!({"document":parsed(document.stringify().as_bytes()),"scope":options.scope,"prompt_id":options.prompt_id,"request_class":options.request_class,"request_id":options.request_id,"count_tokens":options.count_tokens,"signal_live":!cancellation.is_cancelled()}));
            index
        };
        match &self.inner {
            RouterImpl::Mock(decisions) => Ok(decisions[index].clone()),
            RouterImpl::Real(real) => {
                real.route(document, options, headers, cancellation, search)
                    .await
            }
        }
    }
    fn complete(&self, id: &str, evidence: &Value) -> bool {
        let accepted = match &self.inner {
            RouterImpl::Mock(_) => false,
            RouterImpl::Real(real) => real.complete(id, evidence),
        };
        let mut rows = self.completions.lock().unwrap();
        assert!(rows.len() < 32);
        rows.push(json!({"id":id,"evidence":evidence,"accepted":accepted}));
        accepted
    }
    fn complete_exact(&self, id: &str, evidence: &CompletionEvidence) -> bool {
        let accepted = match &self.inner {
            RouterImpl::Mock(_) => false,
            RouterImpl::Real(real) => real.complete_exact(id, evidence),
        };
        let mut rows = self.completions.lock().unwrap();
        assert!(rows.len() < 32);
        rows.push(json!({"id":id,"evidence":evidence,"accepted":accepted}));
        accepted
    }
    fn shutdown(&self) {
        if let RouterImpl::Real(real) = &self.inner {
            real.shutdown();
        }
    }
    async fn close(&self) {
        if let RouterImpl::Real(real) = &self.inner {
            real.close().await;
        }
        self.closed.store(true, Ordering::SeqCst);
    }
}
struct Fixture {
    client: Arc<NativeHttpClient>,
    router: Arc<AuditRouter>,
    gateway: Option<GatewayHandle>,
    peer: Option<Peer>,
    statuses: Events,
    history: Events,
    logs: Events,
    log_history: Events,
    decisions: Events,
    records: Events,
    callback_outcomes: Events,
    real: bool,
    observer_owner: Option<ObserverTasks>,
    never_polled: Arc<std::sync::atomic::AtomicUsize>,
    never_dropped: Arc<std::sync::atomic::AtomicUsize>,
}
struct Reply {
    status: u16,
    headers: HeaderMap,
    bytes: Vec<u8>,
}
struct Seen {
    requests: Vec<Observation>,
    routes: Vec<Value>,
    completions: Vec<Value>,
    statuses: Vec<Value>,
    history: Vec<Value>,
    logs: Vec<Value>,
    decisions: Vec<Value>,
    records: Vec<Value>,
    callback_outcomes: Vec<Value>,
    evaluator_calls: Vec<Value>,
    real: bool,
    continuity_before_close: Option<Value>,
    observer_before_close: Option<Value>,
    observer_after_close: Option<Value>,
    never_polled_before_close: usize,
    never_dropped_after_close: usize,
}
impl Fixture {
    async fn new(number: usize, row: &Value, fixture_index: usize) -> Self {
        let script = upstream_rows(row);
        let peer = Peer::start(move |request, index| {
            let wanted = script.get(index).ok_or("unregistered peer request")?;
            compare_request(request, wanted)?;
            Ok(ResponsePlan::chunks(
                wanted["response_status"].as_u64().unwrap() as u16,
                headers(&wanted["response_headers"]),
                chunks(wanted),
            ))
        })
        .await
        .unwrap();
        let client = Arc::new(NativeHttpClient::new().unwrap());
        let mut config = read_config(
            &row["config_arguments"].as_array().unwrap().last().unwrap()[0],
            false,
            Path::new("/tmp"),
        )
        .unwrap();
        config.local_token = row["gateway_config"]["localToken"]
            .as_str()
            .map(str::to_owned);
        config.upstream = format!("http://{}", peer.address);
        config.session_log_mode = if row["gateway_config"]["sessionLogMode"] == "prompts" {
            autorouter_core::config::SessionLogMode::Prompts
        } else {
            autorouter_core::config::SessionLogMode::Metadata
        };
        let inner = if number == 33 {
            RouterImpl::Real(Box::new(real::OriginalRouter::new(row)))
        } else {
            RouterImpl::Mock(
                row["route_calls"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| r["decision"].clone())
                    .collect(),
            )
        };
        let router = Arc::new(AuditRouter {
            inner,
            calls: Mutex::new(Vec::new()),
            completions: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
        });
        let statuses = Events::default();
        let history = Events::default();
        let logs = Events::default();
        let log_history = Events::default();
        let decisions = Events::default();
        let records = Events::default();
        let callback_outcomes = Events::default();
        let d = decisions.clone();
        let decision: EventSink = Arc::new(move |doc| {
            let mut rows = d.lock().unwrap();
            assert!(rows.len() < 32);
            rows.push(doc.to_serde_observation_lossy());
        });
        let observer_owner =
            (number == 37).then(|| ObserverTasks::new(ObserverLimits::default()).unwrap());
        let never_polled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let never_dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let r = records.clone();
        let outcomes = callback_outcomes.clone();
        let record: RecordSink = if let Some(owner) = &observer_owner {
            let polled = never_polled.clone();
            let dropped = never_dropped.clone();
            owner.handle().record_sink(move |row| -> ObserverFuture {
                let event=row["event"].clone();
                { let mut rows=r.lock().unwrap();assert!(rows.len()<32);rows.push(row); }
                { let mut entries=outcomes.lock().unwrap();assert!(entries.len()<32);let invocation=entries.len()+1;
                    entries.push(json!({"invocation":invocation,"event":event,"outcome":if fixture_index==0{"synchronous_unwind"}else if fixture_index==1{"returned_future_rejection"}else{"returned_never_ready_future"}}));
                }
                match fixture_index {
                    0 => panic!("private sink failure"),
                    1 => Box::pin(async { Err(ObserverRejected) }),
                    2 => Box::pin(real::NeverReady::new(polled.clone(),dropped.clone())),
                    _ => unreachable!(),
                }
            })
        } else {
            Arc::new(move |row| {
                let event = row["event"].clone();
                {
                    let mut rows = r.lock().unwrap();
                    assert!(rows.len() < 32);
                    rows.push(row);
                }
                let mut entries = outcomes.lock().unwrap();
                assert!(entries.len() < 32);
                let invocation = entries.len() + 1;
                entries.push(
                    json!({"invocation":invocation,"event":event,"outcome":"synchronous_return"}),
                );
            })
        };
        let gateway = Gateway::with_router(
            config,
            client.clone(),
            router.clone(),
            EventSinks {
                status: Some(sink(&statuses, &history)),
                log: Some(sink(&logs, &log_history)),
                decision: (number == 35).then_some(decision),
                record: (number != 33).then_some(record),
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
            records,
            callback_outcomes,
            real: number == 33,
            observer_owner,
            never_polled,
            never_dropped,
        }
    }
    async fn send(&self, row: &Value) -> Result<Reply, String> {
        let url = url::Url::parse(row["url"].as_str().unwrap()).unwrap();
        let mut request = Request::builder()
            .method(row["method"].as_str().unwrap())
            .uri(format!(
                "http://{}{}",
                self.gateway.as_ref().unwrap().address,
                url.path()
            ));
        *request.headers_mut().unwrap() = headers(&row["headers"]);
        let response = self
            .client
            .request(
                request
                    .body(Full::new(Bytes::from(
                        row["body"].as_str().unwrap_or_default().to_owned(),
                    )))
                    .unwrap(),
            )
            .await
            .map_err(|e| e.to_string())?;
        let (parts, body) = response.into_parts();
        let bytes = Limited::new(body, LIMIT)
            .collect()
            .await
            .map_err(|e| e.to_string())?
            .to_bytes()
            .to_vec();
        Ok(Reply {
            status: parts.status.as_u16(),
            headers: parts.headers,
            bytes,
        })
    }
    async fn wait_for_status(&self, event: &str) -> Result<(), String> {
        for _ in 0..100 {
            if self
                .statuses
                .lock()
                .unwrap()
                .iter()
                .any(|row| row["event"] == event)
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Err(format!("missing status event: {event}"))
    }
    async fn finish(mut self) -> Result<Seen, String> {
        let continuity_before_close = match &self.router.inner {
            RouterImpl::Real(real) => Some(real.continuity()),
            RouterImpl::Mock(_) => None,
        };
        let observer_before_close = self
            .observer_owner
            .as_ref()
            .map(|owner| real::snapshot(owner.snapshot()));
        let never_polled_before_close = self.never_polled.load(Ordering::SeqCst);
        let closed = tokio::time::timeout(BOUND, self.gateway.take().unwrap().close()).await;
        let requests = self.peer.as_ref().unwrap().observations();
        let peer_closed = self.peer.take().unwrap().close().await;
        closed.map_err(|_| "gateway cleanup timed out")?;
        peer_closed?;
        if let Some(owner) = &self.observer_owner {
            tokio::time::timeout(BOUND, owner.close())
                .await
                .map_err(|_| "observer owner close timed out")?;
        }
        let observer_after_close = self
            .observer_owner
            .as_ref()
            .map(|owner| real::snapshot(owner.snapshot()));
        let never_dropped_after_close = self.never_dropped.load(Ordering::SeqCst);
        if !self.router.closed.load(Ordering::SeqCst) {
            return Err("mock router closure not observed".into());
        }
        for rows in [&self.statuses, &self.history, &self.logs, &self.log_history] {
            if rows
                .lock()
                .unwrap()
                .iter()
                .any(|row| row["event"] == "fixture_observer_overflow")
            {
                return Err("sticky event recorder overflow".into());
            }
        }
        Ok(Seen {
            requests,
            routes: self.router.calls.lock().unwrap().clone(),
            completions: self.router.completions.lock().unwrap().clone(),
            statuses: self.statuses.lock().unwrap().clone(),
            history: self.history.lock().unwrap().clone(),
            logs: self.logs.lock().unwrap().clone(),
            decisions: self.decisions.lock().unwrap().clone(),
            records: self.records.lock().unwrap().clone(),
            callback_outcomes: self.callback_outcomes.lock().unwrap().clone(),
            evaluator_calls: match &self.router.inner {
                RouterImpl::Real(r) => r.calls(),
                RouterImpl::Mock(_) => Vec::new(),
            },
            real: self.real,
            continuity_before_close,
            observer_before_close,
            observer_after_close,
            never_polled_before_close,
            never_dropped_after_close,
        })
    }
}
#[derive(Clone)]
struct Assertions {
    number: usize,
    counts: Arc<Mutex<BTreeMap<usize, usize>>>,
}
impl Assertions {
    fn new(number: usize) -> Self {
        Self {
            number,
            counts: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
    fn check(&self, site: usize, passed: bool) {
        *self.counts.lock().unwrap().entry(site).or_default() += 1;
        assert!(passed, "server#{}:assert-{site}", self.number);
    }
    fn finish(self) {
        let counts = self.counts.lock().unwrap();
        let capture: Value = serde_json::from_str(CAPTURE).unwrap();
        let row = capture["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["number"] == self.number)
            .unwrap();
        for (index, site) in row["assertions"].as_array().unwrap().iter().enumerate() {
            assert_eq!(
                counts.get(&(index + 1)).copied().unwrap_or(0),
                site["executions"].as_u64().unwrap() as usize,
                "server#{}:assert-{} execution count",
                self.number,
                index + 1
            );
        }
        assert_eq!(counts.len(), row["assertions"].as_array().unwrap().len());
    }
}
fn javascript_number_equality(wanted: &Value, got: &Value) -> bool {
    match (wanted, got) {
        (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(a, b)| javascript_number_equality(a, b))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, value)| {
                    right
                        .get(key)
                        .is_some_and(|got| javascript_number_equality(value, got))
                })
        }
        _ => wanted == got,
    }
}
fn compare_metadata(wanted: &Value, got: &Value, real: bool) {
    // This is a semantic comparison, not a rewritten observation. Undefined
    // source fields must be absent in the Rust Value interface. Number values
    // use JavaScript equality; only actual measured timings have no equality
    // oracle. Deterministic injected routing times remain exact.
    let expected = wanted.as_object().unwrap();
    let actual = got.as_object().unwrap();
    if wanted.get("timestamp").is_some() {
        assert!(
            regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$")
                .unwrap()
                .is_match(got["timestamp"].as_str().unwrap())
        );
    } else {
        assert!(got.get("timestamp").is_none());
    }
    let mut excluded = vec![
        "request_id",
        "first_response_ms",
        "total_latency_ms",
        "timestamp",
    ];
    if real {
        excluded.extend(["latency_ms", "evaluation_latency_ms", "routing_latency_ms"]);
    }
    let keys = expected
        .iter()
        .filter(|(key, value)| {
            !excluded.contains(&key.as_str()) && value["$js_type"] != "undefined"
        })
        .map(|(key, _)| key)
        .collect::<BTreeSet<_>>();
    let actual_keys = actual
        .keys()
        .filter(|key| !excluded.contains(&key.as_str()))
        .collect::<BTreeSet<_>>();
    assert_eq!(keys, actual_keys, "complete safe event field set");
    for key in keys {
        assert!(
            javascript_number_equality(&expected[key], &actual[key]),
            "safe event field {key}: expected {:?}, observed {:?}",
            expected[key],
            actual[key]
        );
    }
    let mut timings = vec!["first_response_ms", "total_latency_ms"];
    if real {
        timings.extend(["latency_ms", "evaluation_latency_ms", "routing_latency_ms"]);
    }
    for key in timings {
        if expected
            .get(key)
            .is_some_and(|value| value["$js_type"] != "undefined")
        {
            assert!(
                actual[key]
                    .as_f64()
                    .is_some_and(|value| value.is_finite() && value >= 0.0),
                "validated actual measured timing {key}"
            );
        } else {
            assert!(
                !actual.contains_key(key),
                "source timing absence stays absent: {key}"
            );
        }
    }
}
fn supplemental_transcripts(row: &Value, replies: &[Reply], seen: &Seen) {
    let expected = upstream_rows(row);
    assert_eq!(seen.requests.len(), expected.len());
    for (request, wanted) in seen.requests.iter().zip(&expected) {
        compare_request(request, wanted).unwrap();
    }
    let source_routes = row["route_calls"].as_array().unwrap();
    assert_eq!(seen.routes.len(), source_routes.len());
    for (route, wanted) in seen.routes.iter().zip(source_routes) {
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
        assert!(
            seen.history.iter().any(
                |event| event["event"] == "route" && event["request_id"] == route["request_id"]
            )
        );
    }
    // All four #32 clients are sequential and fully consumed before the next
    // request; preserve original request order even when scopes are identical.
    let source = row["statuses"].as_array().unwrap();
    let source_starts = events(source, "request_start");
    let starts = events(&seen.history, "request_start");
    assert_eq!(starts.len(), source_starts.len());
    assert_eq!(seen.history.len(), source.len());
    for (wanted_start, start) in source_starts.iter().zip(&starts) {
        let wanted = source
            .iter()
            .filter(|e| e["request_id"] == wanted_start["request_id"])
            .collect::<Vec<_>>();
        let actual = seen
            .history
            .iter()
            .filter(|e| e["request_id"] == start["request_id"])
            .collect::<Vec<_>>();
        assert_eq!(actual.len(), wanted.len());
        for (got, wanted) in actual.iter().zip(&wanted) {
            compare_metadata(wanted, got, seen.real);
        }
    }
    for (index, reply) in replies.iter().enumerate() {
        let expected = &row["downstream"][index];
        assert_eq!(reply.status, expected["status"]);
        let body = expected["reads"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|read| read["kind"] == "read" && read["done"] == false)
            .flat_map(|read| decode(&read["base64"]))
            .collect::<Vec<_>>();
        assert_eq!(
            reply.bytes, body,
            "every original delivered byte remains unchanged for text and JSON readers"
        );
    }
    let source_completions = row["completions"].as_array().unwrap();
    let starts = events(&seen.history, "request_start");
    assert_eq!(seen.completions.len(), starts.len());
    for completion in &seen.completions {
        let index = starts
            .iter()
            .position(|event| event["request_id"] == completion["id"])
            .unwrap();
        let source_id = source_starts[index]["request_id"].clone();
        let wanted = source_completions
            .iter()
            .find(|r| r["request_id"] == source_id)
            .unwrap();
        assert_eq!(
            completion["accepted"],
            seen.real && !completion["evidence"].is_null()
        );
        if wanted["evidence"]["$js_type"] == "undefined" {
            assert!(completion["evidence"].is_null());
        } else {
            assert_eq!(completion["evidence"], wanted["evidence"]);
        }
    }
    for secret in [
        "PRIVATE_PROMPT_METADATA_MODE",
        "PRIVATE_RESPONSE_TEXT",
        "PRIVATE_ERROR",
        "Fix a typo",
        "local-test-token-123456789",
        "upstream-secret",
        "classifier-secret",
        "PRIVATE_SPEED",
        "PRIVATE_GEO",
        "PRIVATE_TIER",
        "PRIVATE_TOKEN",
    ] {
        assert!(
            !json!({"logs":seen.logs,"statuses":seen.history})
                .to_string()
                .contains(secret)
        );
    }
    if let Some(directory) = std::env::var_os("AUTOROUTER_NATIVE_PARITY_CAPTURE_DIR") {
        let request = |r: &Observation| json!({"method":r.method,"path":r.path,"headers":r.headers.iter().map(|(k,v)|(k.as_str().to_owned(),v.to_str().unwrap().to_owned())).collect::<BTreeMap<_,_>>(),"body_utf8":std::str::from_utf8(&r.body).unwrap()});
        let raw = json!({"schema_version":1,"status":"passed","source_test":row["source_test"],"upstream_requests":seen.requests.iter().map(request).collect::<Vec<_>>(),"route_calls":seen.routes,"complete_calls":seen.completions,"statuses":seen.statuses,"all_emitted_statuses":seen.history,"logs":seen.logs,"records":seen.records,"decisions":seen.decisions,"callback_outcomes":seen.callback_outcomes,"evaluator_calls":seen.evaluator_calls,"continuity_before_close":seen.continuity_before_close,"observer_before_close":seen.observer_before_close,"observer_after_close":seen.observer_after_close,"never_polled_before_close":seen.never_polled_before_close,"never_dropped_after_close":seen.never_dropped_after_close,"downstream":replies.iter().map(|r|json!({"status":r.status,"headers":r.headers.iter().map(|(k,v)|(k.as_str().to_owned(),v.to_str().unwrap().to_owned())).collect::<BTreeMap<_,_>>(),"body_base64":openssl::base64::encode_block(&r.bytes)})).collect::<Vec<_>>(),"projection":"None. Actual generated IDs, measured times, deterministic routing times, headers and complete bytes retained."});
        let bytes = serde_json::to_vec_pretty(&raw).unwrap();
        assert!(bytes.len() <= LIMIT);
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(
                Path::new(&directory).join(format!("{}.native.json", row["id"].as_str().unwrap())),
            )
            .unwrap();
        file.write_all(&bytes).unwrap();
        file.write_all(b"\n").unwrap();
    }
}
fn fixture_case(row: &Value, index: usize) -> Value {
    let mut out = row.clone();
    let all = row["downstream"].as_array().unwrap();
    let selected = all
        .iter()
        .enumerate()
        .filter(|(_, r)| r["fixture_index"] == index)
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    assert!(!selected.is_empty());
    out["downstream"] = Value::Array(selected.iter().map(|i| all[*i].clone()).collect());
    let starts = events(row["statuses"].as_array().unwrap(), "request_start");
    let ids = selected
        .iter()
        .map(|i| starts[*i]["request_id"].clone())
        .collect::<Vec<_>>();
    out["route_calls"] = Value::Array(
        selected
            .iter()
            .map(|i| row["route_calls"][*i].clone())
            .collect(),
    );
    for name in ["statuses", "records", "decisions"] {
        out[name] = Value::Array(
            row[name]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| ids.contains(&r["request_id"]))
                .cloned()
                .collect(),
        );
    }
    out["completions"] = Value::Array(
        row["completions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| ids.contains(&r["request_id"]))
            .cloned()
            .collect(),
    );
    out["peers"] = Value::Array(
        row["peers"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["fixture_index"] == index)
            .cloned()
            .collect(),
    );
    out["gateway_config"] = row["gateway_configs"][index].clone();
    if row["gateway_configs"].as_array().unwrap().len() > 1 {
        out["id"] = format!("{}-fixture-{}", row["id"].as_str().unwrap(), index + 1).into();
    }
    out
}
async fn wait_complete_count(f: &Fixture, count: usize) -> Result<(), String> {
    for _ in 0..100 {
        if events(&f.statuses.lock().unwrap(), "request_complete").len() >= count {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    Err("exact per-request terminal callback not observed".into())
}
fn compare_record_callbacks(row: &Value, seen: &Seen) {
    assert_eq!(seen.records.len(), row["records"].as_array().unwrap().len());
    assert_eq!(
        seen.decisions.len(),
        row["decisions"].as_array().unwrap().len()
    );
    let source_starts = events(row["statuses"].as_array().unwrap(), "request_start");
    let starts = events(&seen.history, "request_start");
    for (actual, wanted) in seen
        .records
        .iter()
        .zip(row["records"].as_array().unwrap())
        .chain(
            seen.decisions
                .iter()
                .zip(row["decisions"].as_array().unwrap()),
        )
    {
        let index = source_starts
            .iter()
            .position(|r| r["request_id"] == wanted["request_id"])
            .unwrap();
        assert_eq!(actual["request_id"], starts[index]["request_id"]);
        compare_metadata(wanted, actual, seen.real);
    }
    let private = [
        "PRIVATE_RESPONSE_TEXT",
        "PRIVATE_PROMPT_METADATA_MODE",
        "PRIVATE_ERROR",
        "private sink failure",
        "private failure",
    ];
    for secret in private {
        assert!(!json!({"records":seen.records,"decisions":seen.decisions,"logs":seen.logs,"statuses":seen.history}).to_string().contains(secret));
    }
}
#[tokio::test]
async fn original_33_provider_fallback_keeps_confirmed_tool_ownership_after_failed_attempts() {
    let row = fixture_case(&case(33), 0);
    let f = Fixture::new(33, &row, 0).await;
    let a = Assertions::new(33);
    let outcome = tokio::time::timeout(BOUND, async {
        let mut replies = Vec::new();
        for (index, request) in row["downstream"].as_array().unwrap().iter().enumerate() {
            let reply = f.send(request).await?;
            wait_complete_count(&f, index + 1).await?;
            a.check(
                1,
                reply.bytes == response_bytes(&upstream_rows(&row)[index]),
            );
            replies.push(reply);
        }
        Ok::<_, String>(replies)
    })
    .await;
    // Sample actual core state before shutdown can retire any attempt.
    let attempts_before_close = match &f.router.inner {
        RouterImpl::Real(real) => real.attempts(),
        RouterImpl::Mock(_) => unreachable!(),
    };
    let seen = f.finish().await.unwrap();
    let replies = outcome.unwrap().unwrap();
    a.check(
        2,
        seen.requests
            .iter()
            .map(|r| parsed(&r.body)["model"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
            == [
                "claude-haiku-4-5-20251001",
                "claude-opus-5-5",
                "claude-opus-5-5",
            ],
    );
    a.check(
        3,
        events(&seen.history, "upstream_model")
            .iter()
            .map(|r| r["model"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
            == [
                "claude-haiku-4-5-20251001",
                "claude-opus-5-5",
                "claude-sonnet-5",
                "claude-sonnet-5",
            ],
    );
    a.check(4, attempts_before_close == 0);
    assert_eq!(
        seen.continuity_before_close.as_ref().unwrap()["attempts"],
        attempts_before_close
    );
    a.finish();
    supplemental_transcripts(&row, &replies, &seen);
}
#[tokio::test]
async fn original_34_confirmed_outcome_history_preserves_pricing_provenance() {
    let row = fixture_case(&case(34), 0);
    let f = Fixture::new(34, &row, 0).await;
    let outcome = tokio::time::timeout(BOUND, async {
        let reply = f.send(&row["downstream"][0]).await?;
        f.wait_for_status("request_complete").await?;
        Ok::<_, String>(reply)
    })
    .await;
    let seen = f.finish().await.unwrap();
    let reply = outcome.unwrap().unwrap();
    let a = Assertions::new(34);
    a.check(1, event_names(&seen.records) == ["decision", "outcome"]);
    let decision = &seen.records[0];
    let outcome = &seen.records[1];
    a.check(2, decision["schema_version"].as_f64() == Some(2.));
    a.check(3, outcome["request_id"] == decision["request_id"]);
    a.check(4, outcome["status"] == "completed");
    a.check(5, outcome["completion_confirmed"] == true);
    a.check(6, outcome["usage_complete"] == true);
    a.check(7, outcome["pricing_eligible"] == true);
    a.check(8, outcome["selected_model"] == "claude-sonnet-5");
    a.check(9, outcome["confirmed_model"] == "claude-sonnet-5");
    a.check(10, outcome["baseline_model"] == "claude-opus-5-5");
    a.check(11, outcome["pricing_version"].is_string());
    a.check(12, outcome["evaluation_latency_ms"].as_f64() == Some(5.));
    a.check(13, outcome["routing_latency_ms"].as_f64() == Some(7.));
    a.check(
        14,
        outcome["first_response_ms"]
            .as_f64()
            .is_some_and(|ms| ms >= 0.),
    );
    a.check(
        15,
        outcome["total_latency_ms"].as_f64().unwrap()
            >= outcome["first_response_ms"].as_f64().unwrap(),
    );
    a.check(
        16,
        javascript_number_equality(
            &outcome["usage"],
            &json!({"input_tokens":10,"output_tokens":4}),
        ),
    );
    a.check(
        17,
        !json!(seen.records)
            .to_string()
            .contains("PRIVATE_RESPONSE_TEXT"),
    );
    a.finish();
    compare_record_callbacks(&row, &seen);
    supplemental_transcripts(&row, &[reply], &seen);
}
#[tokio::test]
async fn original_35_metadata_history_and_decision_callbacks_omit_prompt_excerpts() {
    let row = fixture_case(&case(35), 0);
    let f = Fixture::new(35, &row, 0).await;
    let outcome = tokio::time::timeout(BOUND, async {
        let reply = f.send(&row["downstream"][0]).await?;
        f.wait_for_status("request_complete").await?;
        Ok::<_, String>(reply)
    })
    .await;
    let seen = f.finish().await.unwrap();
    let reply = outcome.unwrap().unwrap();
    let a = Assertions::new(35);
    a.check(1, seen.records.len() == 2);
    for entry in seen.records.iter().chain(&seen.decisions) {
        a.check(2, entry.get("prompt_excerpt").is_none());
        a.check(3, entry.get("prompt_truncated").is_none());
    }
    a.check(4,!json!({"records":seen.records,"decisions":seen.decisions,"logs":seen.logs,"statuses":seen.statuses}).to_string().contains("PRIVATE_PROMPT_METADATA_MODE"));
    a.finish();
    compare_record_callbacks(&row, &seen);
    supplemental_transcripts(&row, &[reply], &seen);
}
#[tokio::test]
async fn original_36_incomplete_stream_error_and_http_error_history_are_unconfirmed() {
    let whole = case(36);
    let a = Assertions::new(36);
    for (index, scenario) in ["incomplete", "stream_error", "http_error"]
        .iter()
        .enumerate()
    {
        let row = fixture_case(&whole, index);
        let f = Fixture::new(36, &row, index).await;
        let outcome = tokio::time::timeout(BOUND, async {
            let reply = f.send(&row["downstream"][0]).await?;
            f.wait_for_status("request_complete").await?;
            Ok::<_, String>(reply)
        })
        .await;
        let seen = f.finish().await.unwrap();
        let reply = outcome.unwrap().unwrap();
        let outcome = events(&seen.records, "outcome")[0].clone();
        a.check(1, outcome["completion_confirmed"] == false);
        a.check(2, outcome["usage_complete"] == false);
        a.check(3, outcome["pricing_eligible"] == false);
        a.check(
            4,
            outcome["unpriced_reason"]
                == if *scenario == "incomplete" {
                    "unconfirmed_completion"
                } else {
                    "request_failed"
                },
        );
        a.check(
            5,
            outcome["status"]
                == if *scenario == "incomplete" {
                    "completed"
                } else {
                    "error"
                },
        );
        a.check(
            6,
            !json!(seen.records).to_string().contains("PRIVATE_ERROR"),
        );
        compare_record_callbacks(&row, &seen);
        supplemental_transcripts(&row, &[reply], &seen);
    }
    a.finish();
}
#[tokio::test]
async fn original_37_actual_returned_record_failures_and_never_ready_futures_cannot_delay_inference()
 {
    let whole = case(37);
    let a = Assertions::new(37);
    for index in 0..3 {
        let row = fixture_case(&whole, index);
        let f = Fixture::new(37, &row, index).await;
        // Original client AbortSignal.timeout(1000) is a real client deadline.
        let outcome = tokio::time::timeout(Duration::from_millis(1000), async {
            let reply = f.send(&row["downstream"][0]).await?;
            f.wait_for_status("request_complete").await?;
            Ok::<_, String>(reply)
        })
        .await;
        if index == 1 {
            for _ in 0..100 {
                if f.observer_owner.as_ref().unwrap().snapshot().rejected == 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert_eq!(
                f.observer_owner.as_ref().unwrap().snapshot().rejected,
                2,
                "two actual returned futures rejected before close"
            );
        } else if index == 2 {
            for _ in 0..100 {
                if f.never_polled.load(Ordering::SeqCst) == 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert_eq!(
                f.never_polled.load(Ordering::SeqCst),
                2,
                "both actual never-ready futures were independently polled"
            );
            assert_eq!(f.observer_owner.as_ref().unwrap().snapshot().pending, 2);
            assert_eq!(
                f.never_dropped.load(Ordering::SeqCst),
                0,
                "returned pending work stays alive until owner close"
            );
        }
        let seen = f.finish().await.unwrap();
        let reply = outcome.unwrap().unwrap();
        a.check(1, reply.status == 200);
        a.check(2, parsed(&reply.bytes)["model"] == "claude-sonnet-5");
        assert_eq!(seen.callback_outcomes.len(), 2);
        let before = seen.observer_before_close.as_ref().unwrap();
        let after = seen.observer_after_close.as_ref().unwrap();
        assert_eq!(after["pending"], 0);
        assert_eq!(after["charged_bytes"], 0);
        assert_eq!(after["worker_finished"], true);
        assert_eq!(after["closed"], true);
        if index == 0 {
            assert_eq!(before["factory_panicked"], 2);
        } else if index == 1 {
            assert_eq!(before["rejected"], 2);
        } else {
            assert_eq!(before["pending"], 2);
            assert_eq!(seen.never_polled_before_close, 2);
            assert_eq!(seen.never_dropped_after_close, 2);
            assert_eq!(after["cancelled"], 2);
        }

        compare_record_callbacks(&row, &seen);
        supplemental_transcripts(&row, &[reply], &seen);
    }
    a.finish();
}
