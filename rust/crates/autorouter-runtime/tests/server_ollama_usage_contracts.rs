//! Complete frozen server #29–#32 Ollama cancellation and usage/pricing schedules.
#[path = "support/server_ollama_usage.rs"]
mod ollama;
#[allow(dead_code)] // Shared peer also supplies streaming interfaces to other targets.
#[path = "support/server_streaming_routing.rs"]
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

const CORPUS: &str = include_str!("../../../parity/cases/server-ollama-usage-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../parity/cases/server-ollama-usage-contracts.capture.json");
const LIMIT: usize = 2 * 1024 * 1024;
const BOUND: Duration = Duration::from_secs(5);
type Events = Arc<Mutex<Vec<Value>>>;
fn case(number: usize) -> Value {
    assert_eq!(
        format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
        "4fb38321dcbf5b071841529ca25bdb3af0b077d888b4359718697885c4b02b9a"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(CAPTURE.as_bytes())),
        "360be4af55d128b570698731c9653f01e9e158548c966694942e154056d9905e"
    );
    let metadata: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(metadata["static_assertions"], 26);
    assert_eq!(metadata["expanded_assertions"], 36);
    assert_eq!(metadata["callbacks_executed"], 4);
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
struct MockRouter {
    decisions: Vec<Value>,
    calls: Mutex<Vec<Value>>,
    completions: Mutex<Vec<Value>>,
    closed: AtomicBool,
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
        let mut rows = self.calls.lock().unwrap();
        let index = rows.len();
        assert!(index < self.decisions.len());
        rows.push(json!({"document":parsed(document.stringify().as_bytes()),"scope":options.scope,"prompt_id":options.prompt_id,"request_class":options.request_class,"request_id":options.request_id,"count_tokens":options.count_tokens,"signal_live":!cancellation.is_cancelled()}));
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
}
impl Fixture {
    async fn new(row: &Value) -> Self {
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
        let mut config =
            read_config(&row["config_arguments"][0][0], false, Path::new("/tmp")).unwrap();
        config.local_token = row["gateway_config"]["localToken"]
            .as_str()
            .map(str::to_owned);
        config.upstream = format!("http://{}", peer.address);
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
        });
        let statuses = Events::default();
        let history = Events::default();
        let logs = Events::default();
        let log_history = Events::default();
        let gateway = Gateway::with_router(
            config,
            client.clone(),
            router.clone(),
            EventSinks {
                status: Some(sink(&statuses, &history)),
                log: Some(sink(&logs, &log_history)),
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
        let closed = tokio::time::timeout(BOUND, self.gateway.take().unwrap().close()).await;
        let requests = self.peer.as_ref().unwrap().observations();
        let peer_closed = self.peer.take().unwrap().close().await;
        closed.map_err(|_| "gateway cleanup timed out")?;
        peer_closed?;
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
fn compare_metadata(wanted: &Value, got: &Value) {
    // This is a semantic comparison, not a rewritten observation. Undefined
    // source fields must be absent in the Rust Value interface. Number values
    // use JavaScript equality; only actual measured timings have no equality
    // oracle. Deterministic injected routing times remain exact.
    let expected = wanted.as_object().unwrap();
    let actual = got.as_object().unwrap();
    let excluded = ["request_id", "first_response_ms", "total_latency_ms"];
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
    for key in ["first_response_ms", "total_latency_ms"] {
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
            compare_metadata(wanted, got);
        }
    }
    for (index, reply) in replies.iter().enumerate() {
        let expected = &row["downstream"][index];
        assert_eq!(reply.status, expected["status"]);
        let body = expected["reads"]
            .as_array()
            .unwrap()
            .iter()
            .find(|read| read["kind"] == "text")
            .unwrap()["value"]
            .as_str()
            .unwrap();
        assert_eq!(
            reply.bytes,
            body.as_bytes(),
            "complete original downstream bytes"
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
        assert_eq!(completion["accepted"], false);
        if wanted["evidence"]["$js_type"] == "undefined" {
            assert!(completion["evidence"].is_null());
        } else {
            assert_eq!(completion["evidence"], wanted["evidence"]);
        }
    }
    for secret in [
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
        let raw = json!({"schema_version":1,"status":"passed","source_test":row["source_test"],"upstream_requests":seen.requests.iter().map(request).collect::<Vec<_>>(),"route_calls":seen.routes,"complete_calls":seen.completions,"statuses":seen.statuses,"all_emitted_statuses":seen.history,"logs":seen.logs,"downstream":replies.iter().map(|r|json!({"status":r.status,"headers":r.headers.iter().map(|(k,v)|(k.as_str().to_owned(),v.to_str().unwrap().to_owned())).collect::<BTreeMap<_,_>>(),"body_base64":openssl::base64::encode_block(&r.bytes)})).collect::<Vec<_>>(),"projection":"None. Actual generated IDs, measured times, deterministic routing times, headers and complete bytes retained."});
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
#[tokio::test]
async fn original_29_unlimited_real_ollama_router_waits_until_downstream_cancel() {
    let row = case(29);
    let a = Assertions::new(29);
    let evaluator = Arc::new(ollama::Evaluator::new(&row, a.clone()));
    let mut router_config =
        read_config(&row["config_arguments"][0][0], false, Path::new("/tmp")).unwrap();
    router_config.ollama_timeout_ms = 0;
    assert_eq!(row["router_config"]["ollamaTimeoutMs"], 0);
    let router = Arc::new(ollama::RealRouter::new(
        autorouter_runtime::router::Router::new(evaluator.clone(), router_config),
    ));
    let peer = Peer::start(|_, _| {
        Ok(ResponsePlan::bytes(
            200,
            headers(&json!({"content-type":"application/json"})),
            Bytes::from_static(b"{}"),
        ))
    })
    .await
    .unwrap();
    let client = Arc::new(NativeHttpClient::new().unwrap());
    let mut config = read_config(&row["config_arguments"][1][0], false, Path::new("/tmp")).unwrap();
    config.evaluator = autorouter_core::config::Evaluator::Ollama;
    config.ollama_timeout_ms = 0;
    config.local_token = row["gateway_config"]["localToken"]
        .as_str()
        .map(str::to_owned);
    config.upstream = format!("http://{}", peer.address);
    let statuses = Events::default();
    let history = Events::default();
    let logs = Events::default();
    let log_history = Events::default();
    let gateway = Gateway::with_router(
        config,
        client,
        router.clone(),
        EventSinks {
            status: Some(sink(&statuses, &history)),
            log: Some(sink(&logs, &log_history)),
            ..Default::default()
        },
    )
    .unwrap()
    .listen(0)
    .await
    .unwrap();
    let caller = CancellationToken::new();
    let client_task = tokio::spawn(ollama::cancellable_client(
        gateway.address,
        row["downstream"][0].clone(),
        caller.clone(),
    ));
    let result = tokio::time::timeout(BOUND, async {
        evaluator.wait_until_polled().await;
        // Preserve the original 30 ms observation with the product deadline 0.
        tokio::time::sleep(Duration::from_millis(30)).await;
        a.check(5, !router.token_cancelled());
        a.check(6, !evaluator.dropped.load(Ordering::SeqCst));
        a.check(
            7,
            event_names(&statuses.lock().unwrap()) == ["request_start"],
        );
        caller.cancel();
        let rejected = client_task.await.expect("caller HTTP task must join");
        a.check(4, rejected == Err(ollama::ClientFailure::Cancelled));
        for _ in 0..100 {
            if statuses
                .lock()
                .unwrap()
                .iter()
                .any(|r| r["event"] == "request_cancelled")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        evaluator.wait_until_dropped().await;
        a.check(8, router.token_cancelled());
        a.check(9, evaluator.dropped.load(Ordering::SeqCst));
        a.check(10, peer.observations().is_empty());
        let paths = evaluator
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|r| {
                url::Url::parse(r["url"].as_str().unwrap())
                    .unwrap()
                    .path()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        a.check(11, paths == ["/api/show", "/v1/systemone"]);
        a.check(
            12,
            event_names(&statuses.lock().unwrap()) == ["request_start", "request_cancelled"],
        );
        a.check(13, events(&logs.lock().unwrap(), "route").is_empty());
        a.finish();
    })
    .await;
    // Snapshot before close: owner shutdown cannot manufacture cancellation.
    let observed_token_cancelled = router.token_cancelled();
    let evaluator_body_dropped = evaluator.dropped.load(Ordering::SeqCst);
    let route_future_destroyed = router.dropped.load(Ordering::SeqCst);
    let route_returned_cancelled = router.returned_cancelled.load(Ordering::SeqCst);
    let requests = peer.observations();
    let raw_routes = router.calls.lock().unwrap().clone();
    let raw_complete = router.completions.lock().unwrap().clone();
    let raw_statuses = statuses.lock().unwrap().clone();
    let raw_history = history.lock().unwrap().clone();
    let raw_logs = logs.lock().unwrap().clone();
    let evaluator_calls = evaluator.calls.lock().unwrap().clone();
    let closed = tokio::time::timeout(BOUND, gateway.close()).await;
    let peer_closed = peer.close().await;
    result.unwrap();
    closed.unwrap();
    peer_closed.unwrap();
    assert!(observed_token_cancelled && evaluator_body_dropped && route_future_destroyed);
    assert!(router.closed.load(Ordering::SeqCst));
    assert!(requests.is_empty());
    assert_eq!(raw_routes.len(), 1);
    let actual = &raw_routes[0];
    let wanted = &row["route_calls"][0];
    assert_eq!(actual["document"], wanted["document"]);
    assert_eq!(actual["scope"], wanted["options"]["scope"]);
    assert_eq!(actual["prompt_id"], "");
    assert_eq!(actual["request_class"], "");
    assert_eq!(actual["count_tokens"], true);
    assert_eq!(actual["signal_live"], true);
    assert_eq!(actual["request_id"], raw_history[0]["request_id"]);
    assert_eq!(raw_history.len(), row["statuses"].as_array().unwrap().len());
    for (wanted, got) in row["statuses"].as_array().unwrap().iter().zip(&raw_history) {
        assert_eq!(got["request_id"], raw_history[0]["request_id"]);
        compare_metadata(wanted, got);
    }
    assert_eq!(raw_complete.len(), 1);
    assert_eq!(raw_complete[0]["id"], raw_history[0]["request_id"]);
    assert!(raw_complete[0]["evidence"].is_null());
    assert_eq!(raw_complete[0]["accepted"], false);
    for secret in [
        "Fix a typo",
        "local-test-token-123456789",
        "upstream-secret",
        "classifier-secret",
    ] {
        assert!(
            !json!({"statuses":raw_history,"logs":raw_logs})
                .to_string()
                .contains(secret)
        );
    }
    let raw = json!({"schema_version":1,"status":"passed","source_test":row["source_test"],"upstream_requests":[],"route_calls":raw_routes,"complete_calls":raw_complete,"statuses":raw_statuses,"all_emitted_statuses":raw_history,"logs":raw_logs,"evaluator_calls":evaluator_calls,"downstream":{"status":null,"body_base64":"","error_kind":"Cancelled"},"observed_routing_token_cancelled_before_close":observed_token_cancelled,"evaluator_body_dropped_before_close":evaluator_body_dropped,"route_future_destroyed_before_close":route_future_destroyed,"route_returned_cancelled":route_returned_cancelled,"projection":"None. Actual requests, raw IDs, timing values and native owned-body cancellation observations are retained; Rust transport has no AbortSignal property."});
    persist_raw(&row, &raw);
}
fn persist_raw(row: &Value, raw: &Value) {
    if let Some(directory) = std::env::var_os("AUTOROUTER_NATIVE_PARITY_CAPTURE_DIR") {
        let bytes = serde_json::to_vec_pretty(raw).unwrap();
        assert!(bytes.len() < LIMIT);
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
#[tokio::test]
async fn original_30_streamed_usage_is_correlated_before_completion_and_private_in_logs() {
    let row = case(30);
    let f = Fixture::new(&row).await;
    let outcome = tokio::time::timeout(BOUND, async {
        let reply = f.send(&row["downstream"][0]).await?;
        f.wait_for_status("request_complete").await?;
        Ok::<_, String>(reply)
    })
    .await;
    let seen = f.finish().await.unwrap();
    let reply = outcome.unwrap().unwrap();
    let a = Assertions::new(30);
    let sse = response_bytes(&upstream_rows(&row)[0]);
    let usage = json!({"input_tokens":100,"output_tokens":20,"cache_creation_input_tokens":60,"cache_read_input_tokens":200,"cache_creation":{"ephemeral_5m_input_tokens":40,"ephemeral_1h_input_tokens":20},"inference_geo":"us","speed":"fast","service_tier":"standard"});
    a.check(1, reply.bytes == sse);
    a.check(
        2,
        event_names(&seen.statuses)
            == [
                "request_start",
                "route",
                "upstream_response",
                "upstream_model",
                "upstream_usage",
                "request_complete",
            ],
    );
    let entry = events(&seen.statuses, "upstream_usage")[0].clone();
    a.check(3, entry["session_id"] == "usage-session");
    a.check(4, entry["request_id"] == seen.statuses[0]["request_id"]);
    a.check(5, javascript_number_equality(&entry["usage"], &usage));
    a.check(
        6,
        events(&seen.statuses, "route")[0]["pricing_context"]
            == json!({"speed":"fast","inference_geo":"us","service_tier":"standard_only"}),
    );
    a.check(7, events(&seen.logs, "upstream_usage").is_empty());
    a.finish();
    supplemental_transcripts(&row, &[reply], &seen);
}
#[tokio::test]
async fn original_31_json_http_error_usage_is_ignored_and_unknown_pricing_is_private() {
    let row = case(31);
    let f = Fixture::new(&row).await;
    let outcome = tokio::time::timeout(BOUND, async {
        let reply = f.send(&row["downstream"][0]).await?;
        f.wait_for_status("request_complete").await?;
        Ok::<_, String>(reply)
    })
    .await;
    let seen = f.finish().await.unwrap();
    let reply = outcome.unwrap().unwrap();
    let a = Assertions::new(31);
    a.check(1, events(&seen.statuses, "upstream_usage").is_empty());
    a.check(
        2,
        events(&seen.statuses, "route")[0]["pricing_context"]
            == json!({"speed":"unknown","inference_geo":"unknown","service_tier":"unknown"}),
    );
    a.check(3, !json!(seen.statuses).to_string().contains("PRIVATE_"));
    a.finish();
    supplemental_transcripts(&row, &[reply], &seen);
}
#[tokio::test]
async fn original_32_all_json_usage_and_unsupported_pricing_marker_variants() {
    let row = case(32);
    let f = Fixture::new(&row).await;
    let a = Assertions::new(32);
    let outcome = tokio::time::timeout(BOUND, async {
        let mut replies = Vec::new();
        for (index, request) in row["downstream"].as_array().unwrap().iter().enumerate() {
            f.statuses.lock().unwrap().clear();
            replies.push(f.send(request).await?);
            f.wait_for_status("request_complete").await?;
            let mut expected =
                json!({"speed":"standard","inference_geo":"global","service_tier":"auto"});
            if index != 0 {
                expected["pricing_unsupported"] = true.into();
            }
            let statuses = f.statuses.lock().unwrap().clone();
            a.check(
                1,
                events(&statuses, "route")[0]["pricing_context"] == expected,
            );
            a.check(
                2,
                javascript_number_equality(
                    &events(&statuses, "upstream_usage")[0]["usage"],
                    &json!({"input_tokens":10,"output_tokens":20}),
                ),
            );
            a.check(3, !json!(statuses).to_string().contains("PRIVATE_TOKEN"));
        }
        Ok::<_, String>(replies)
    })
    .await;
    let seen = f.finish().await.unwrap();
    let replies = outcome.unwrap().unwrap();
    a.finish();
    assert_eq!(replies.len(), 4);
    assert_eq!(events(&seen.history, "request_start").len(), 4);
    supplemental_transcripts(&row, &replies, &seen);
}
