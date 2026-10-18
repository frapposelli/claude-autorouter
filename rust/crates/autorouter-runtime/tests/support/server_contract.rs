//! Exact shared server-contract helpers; distinct original metadata policies stay separate.
use super::peer::Observation;
use autorouter_core::{
    config::{RouterConfig, SessionLogMode, read_config},
    js_json::JsDocument,
    router::RouteOptions,
};
use autorouter_runtime::{
    evaluator::EvaluationError,
    http_client::{HttpError, HttpTransport, NativeHttpClient},
    response_observer::CompletionEvidence,
    server::GatewayRouter,
    server_events::EventSink,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited, combinators::UnsyncBoxBody};
use hyper::{HeaderMap, Request, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use tokio_util::sync::CancellationToken;

type Error = Box<dyn std::error::Error + Send + Sync>;
type Body = UnsyncBoxBody<Bytes, Error>;

const LIMIT: usize = 2 * 1024 * 1024;
type Events = Arc<Mutex<Vec<Value>>>;

// Admit immutable captured server cases before looking up the requested row.
pub fn captured_server_case(
    number: usize,
    corpus: &str,
    capture: &str,
    hashes: [&str; 2],
    counts: (u64, u64, Option<u64>),
) -> Value {
    assert_eq!(
        format!("{:x}", Sha256::digest(corpus.as_bytes())),
        hashes[0]
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(capture.as_bytes())),
        hashes[1]
    );
    let metadata: Value = serde_json::from_str(capture).unwrap();
    assert_eq!(metadata["static_assertions"], counts.0);
    assert_eq!(metadata["expanded_assertions"], counts.1);
    if let Some(callbacks) = counts.2 {
        assert_eq!(metadata["callbacks_executed"], callbacks);
    }
    corpus
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|row| row["source_test"] == format!("test/server.test.mjs#{number}"))
        .unwrap()
}

// The caller supplies its peer's ordinary chunk constructor. This stays valid
// in targets with a different peer API; no peer implementation is imported here.
pub fn indexed_responder<P: 'static>(
    row: &Value,
    response: fn(u16, HeaderMap, Vec<Bytes>) -> P,
) -> impl Fn(&Observation, usize) -> Result<P, String> + Send + Sync + 'static {
    let script = upstream_rows(row);
    move |request, index| {
        let wanted = script.get(index).ok_or("unregistered peer request")?;
        compare_request(request, wanted)?;
        Ok(response(
            wanted["response_status"].as_u64().unwrap() as u16,
            headers(&wanted["response_headers"]),
            chunks(wanted),
        ))
    }
}

pub fn gateway_config(row: &Value, arguments: &Value, address: SocketAddr) -> RouterConfig {
    let mut config = read_config(arguments, false, Path::new("/tmp")).unwrap();
    config.local_token = row["gateway_config"]["localToken"]
        .as_str()
        .map(str::to_owned);
    config.upstream = format!("http://{address}");
    config
}

pub fn history_gateway_config(row: &Value, address: SocketAddr) -> RouterConfig {
    let mut config = gateway_config(
        row,
        &row["config_arguments"].as_array().unwrap().last().unwrap()[0],
        address,
    );
    config.session_log_mode = if row["gateway_config"]["sessionLogMode"] == "prompts" {
        SessionLogMode::Prompts
    } else {
        SessionLogMode::Metadata
    };
    config
}

pub fn parsed(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

pub fn headers(value: &Value) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (key, value) in value.as_object().unwrap() {
        out.insert(
            hyper::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
            value.as_str().unwrap().parse().unwrap(),
        );
    }
    out
}

pub fn header_is(map: &HeaderMap, name: &str, value: Option<&str>) -> bool {
    map.get(name).and_then(|value| value.to_str().ok()) == value
}

pub fn decode(value: &Value) -> Vec<u8> {
    let value = value.as_str().unwrap();
    assert!(value.len() < LIMIT * 2);
    openssl::base64::decode_block(value).unwrap()
}

pub fn chunks(row: &Value) -> Vec<Bytes> {
    row["response_writes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| Bytes::from(decode(&row["base64"])))
        .collect()
}

pub fn response_bytes(row: &Value) -> Vec<u8> {
    chunks(row)
        .iter()
        .flat_map(|chunk| chunk.iter().copied())
        .collect()
}

pub fn upstream_rows(row: &Value) -> Vec<Value> {
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

pub fn events(rows: &[Value], name: &str) -> Vec<Value> {
    rows.iter()
        .filter(|row| row["event"] == name)
        .cloned()
        .collect()
}

pub fn event_names(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .map(|row| row["event"].as_str().unwrap().to_owned())
        .collect()
}

pub fn sink(active: &Events, history: &Events) -> EventSink {
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

pub fn same_request(observed: &Observation, wanted: &Value) -> bool {
    observed.method == wanted["method"].as_str().unwrap()
        && observed.path == wanted["path"].as_str().unwrap()
        && header_is(
            &observed.headers,
            "x-claude-code-session-id",
            wanted["headers"]["x-claude-code-session-id"].as_str(),
        )
}

pub fn compare_request(observed: &Observation, wanted: &Value) -> Result<(), String> {
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

include!("../../../autorouter-core/src/json_compare_macros.rs");
contract_json_equal!(pub javascript_number_equality, a, b => a.as_f64() == b.as_f64());

pub fn compare_metadata(wanted: &Value, got: &Value) {
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

pub fn fixture_case(row: &Value, index: usize) -> Value {
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

pub fn compare_real_metadata(wanted: &Value, got: &Value, real: bool) {
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

pub fn identity_headers(value: &Value) -> HeaderMap {
    let mut result = HeaderMap::new();
    for (key, value) in value.as_object().unwrap() {
        result.insert(
            hyper::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
            value.as_str().unwrap().parse().unwrap(),
        );
    }
    result
}

pub fn single_sink(rows: &Events) -> EventSink {
    let rows = rows.clone();
    Arc::new(move |event| {
        let mut rows = rows.lock().unwrap();
        if rows.len() >= 128 {
            if rows.len() == 128 {
                rows.push(json!({"event":"fixture_observer_overflow"}));
            }
            return;
        }
        rows.push(event.to_serde_observation_lossy());
    })
}

pub fn finish_counts(number: usize, counts: &BTreeMap<usize, usize>, capture: &str) {
    let capture: Value = serde_json::from_str(capture).unwrap();
    let row = capture["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["number"] == number)
        .unwrap();
    for (index, site) in row["assertions"].as_array().unwrap().iter().enumerate() {
        assert_eq!(
            counts.get(&(index + 1)).copied().unwrap_or(0),
            site["executions"].as_u64().unwrap() as usize,
            "server#{}:assert-{} execution count",
            number,
            index + 1
        );
    }
    assert_eq!(counts.len(), row["assertions"].as_array().unwrap().len());
}

pub struct Reply {
    pub status: u16,
    pub headers: HeaderMap,
    pub bytes: Vec<u8>,
}

pub async fn send_buffered(
    client: &autorouter_runtime::http_client::NativeHttpClient,
    gateway: Option<&autorouter_runtime::server::GatewayHandle>,
    row: &Value,
) -> Result<Reply, String> {
    send_configured_buffered::<false, true>(client, gateway, row).await
}

pub async fn send_configured_buffered<const QUERY: bool, const DEFAULT_BODY: bool>(
    client: &autorouter_runtime::http_client::NativeHttpClient,
    gateway: Option<&autorouter_runtime::server::GatewayHandle>,
    row: &Value,
) -> Result<Reply, String> {
    let url = url::Url::parse(row["url"].as_str().unwrap()).unwrap();
    let suffix = if QUERY {
        format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        )
    } else {
        url.path().to_owned()
    };
    let mut request = Request::builder()
        .method(row["method"].as_str().unwrap())
        .uri(format!("http://{}{}", gateway.unwrap().address, suffix));
    *request.headers_mut().unwrap() = headers(&row["headers"]);
    let response = client
        .request(
            request
                .body(Full::new(Bytes::from(
                    if DEFAULT_BODY {
                        row["body"].as_str().unwrap_or_default()
                    } else {
                        row["body"].as_str().unwrap()
                    }
                    .to_owned(),
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

pub async fn wait_for_status(statuses: &Events, event: &str) -> Result<(), String> {
    for _ in 0..100 {
        if statuses
            .lock()
            .unwrap()
            .iter()
            .any(|row| row["event"] == event)
        {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    Err(format!("missing status event: {event}"))
}

pub struct Transcript<'a> {
    pub requests: &'a [Observation],
    pub routes: &'a [Value],
    pub completions: &'a [Value],
    pub history: &'a [Value],
    pub logs: &'a [Value],
    pub real: bool,
}

pub fn verify_transcript(row: &Value, replies: &[Reply], seen: &Transcript<'_>) {
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
    let starts = events(seen.history, "request_start");
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
            compare_real_metadata(wanted, got, seen.real);
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
    let starts = events(seen.history, "request_start");
    assert_eq!(seen.completions.len(), starts.len());
    for completion in seen.completions {
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
}

pub fn write_row_capture(
    directory: &std::ffi::OsStr,
    id: &Value,
    raw: &Value,
    limit: usize,
    inclusive: bool,
) {
    let bytes = serde_json::to_vec_pretty(raw).unwrap();
    if inclusive {
        assert!(bytes.len() <= limit);
    } else {
        assert!(bytes.len() < limit);
    }
    let path =
        std::path::Path::new(directory).join(format!("{}.native.json", id.as_str().unwrap()));
    write_capture_file(&path, &bytes);
}

pub fn write_capture_file(path: &std::path::Path, bytes: &[u8]) {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).unwrap();
    file.write_all(bytes).unwrap();
    file.write_all(b"\n").unwrap();
}

pub struct MockRouter {
    pub decisions: Vec<Value>,
    pub calls: Mutex<Vec<Value>>,
    pub completions: Mutex<Vec<Value>>,
    pub closed: AtomicBool,
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
#[derive(Clone)]
pub struct SharedAssertions {
    number: usize,
    counts: Arc<Mutex<BTreeMap<usize, usize>>>,
}
impl SharedAssertions {
    pub fn new(number: usize) -> Self {
        Self {
            number,
            counts: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
    pub fn check(&self, site: usize, passed: bool) {
        *self.counts.lock().unwrap().entry(site).or_default() += 1;
        assert!(passed, "server#{}:assert-{site}", self.number);
    }
    pub fn finish(self) {
        let counts = self.counts.lock().unwrap();
        finish_counts(self.number, &counts, super::CAPTURE);
    }
}
pub struct Assertions<const VARIANTS: usize, const ACTIVE_ONLY: bool> {
    number: usize,
    counts: BTreeMap<usize, usize>,
}
impl<const VARIANTS: usize, const ACTIVE_ONLY: bool> Assertions<VARIANTS, ACTIVE_ONLY> {
    pub fn new(number: usize) -> Self {
        Self {
            number,
            counts: BTreeMap::new(),
        }
    }
    pub fn check(&mut self, site: usize, passed: bool) {
        *self.counts.entry(site).or_default() += 1;
        assert!(passed, "server#{}:assert-{site}", self.number);
    }
    pub fn finish(self) {
        let capture: Value = serde_json::from_str(super::CAPTURE).unwrap();
        let row = capture["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["number"] == self.number)
            .unwrap();
        for (index, site) in row["assertions"].as_array().unwrap().iter().enumerate() {
            let executions = site["executions"].as_u64().unwrap() as usize;
            if VARIANTS == 2 {
                assert_eq!(
                    executions % 2,
                    0,
                    "both original variants execute each site equally"
                );
                assert_eq!(
                    self.counts.get(&(index + 1)).copied().unwrap_or(0),
                    executions / 2,
                    "server#{}:assert-{} per-variant count",
                    self.number,
                    index + 1
                );
            } else {
                assert_eq!(
                    self.counts.get(&(index + 1)).copied().unwrap_or(0),
                    executions,
                    "server#{}:assert-{} execution count",
                    self.number,
                    index + 1
                );
            }
        }
        let sites = row["assertions"].as_array().unwrap();
        assert_eq!(
            self.counts.len(),
            if ACTIVE_ONLY {
                sites.iter().filter(|site| site["executions"] != 0).count()
            } else {
                sites.len()
            }
        );
    }
}

pub async fn close_gateway(
    gateway: &mut Option<autorouter_runtime::server::GatewayHandle>,
    peer: &mut Option<super::peer::Peer>,
    bound: std::time::Duration,
) -> Result<Vec<Observation>, String> {
    let closed = tokio::time::timeout(bound, gateway.take().unwrap().close()).await;
    let requests = peer.as_ref().unwrap().observations();
    let peer_closed = peer.take().unwrap().close().await;
    closed.map_err(|_| "gateway cleanup timed out")?;
    peer_closed?;
    Ok(requests)
}

pub fn check_recorders<const N: usize>(rows: [&Events; N]) -> Result<(), String> {
    for rows in rows {
        if rows
            .lock()
            .unwrap()
            .iter()
            .any(|row| row["event"] == "fixture_observer_overflow")
        {
            return Err("sticky event recorder overflow".into());
        }
    }
    Ok(())
}
pub fn capture_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap().to_owned()))
        .collect()
}

pub fn capture_request(r: &Observation) -> Value {
    json!({"method":r.method,"path":r.path,"headers":capture_headers(&r.headers),"body_utf8":std::str::from_utf8(&r.body).unwrap()})
}

pub fn capture_reply(r: &Reply) -> Value {
    json!({"status":r.status,"headers":capture_headers(&r.headers),"body_base64":openssl::base64::encode_block(&r.bytes)})
}

pub struct EvaluatorTransport<const FROM_SLICE: bool> {
    pub native: Arc<NativeHttpClient>,
    pub authority: String,
    pub script: Vec<Value>,
    pub mocked: Mutex<Vec<Observation>>,
    pub faults: Mutex<Vec<String>>,
    pub mismatch_message: &'static str,
    pub endpoint_message: &'static str,
}
impl<const FROM_SLICE: bool> EvaluatorTransport<FROM_SLICE> {
    fn fault(&self, text: &str) -> HttpError {
        let mut faults = self.faults.lock().unwrap();
        if faults.len() < 16 {
            faults.push(text.into());
        }
        HttpError::InvalidRequest
    }
}
impl<const FROM_SLICE: bool> HttpTransport for EvaluatorTransport<FROM_SLICE> {
    type ResponseBody = Body;
    async fn request(&self, request: Request<Full<Bytes>>) -> Result<Response<Body>, HttpError> {
        if self
            .script
            .first()
            .is_some_and(|row| request.uri() == row["url"].as_str().unwrap())
        {
            let (parts, body) = request.into_parts();
            let observation = Observation {
                method: parts.method.to_string(),
                path: parts.uri.to_string(),
                headers: parts.headers,
                body: body.collect().await.unwrap().to_bytes().to_vec(),
            };
            let index = {
                let mut calls = self.mocked.lock().unwrap();
                if calls.len() >= self.script.len() {
                    return Err(self.fault("extra evaluator request"));
                }
                let index = calls.len();
                calls.push(observation.clone());
                index
            };
            let row = &self.script[index];
            if observation.method != row["method"].as_str().unwrap()
                || !header_is(
                    &observation.headers,
                    "authorization",
                    row["headers"]["authorization"].as_str(),
                )
                || parsed(&observation.body)
                    != if FROM_SLICE {
                        parsed(row["body"].as_str().unwrap().as_bytes())
                    } else {
                        serde_json::from_str::<Value>(row["body"].as_str().unwrap()).unwrap()
                    }
            {
                return Err(self.fault(self.mismatch_message));
            }
            let bytes: Vec<u8> = row["reads"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|read| read["kind"] == "read" && read["done"] == false)
                .flat_map(|read| decode(&read["base64"]))
                .collect();
            let body = Full::new(Bytes::from(bytes))
                .map_err(|never| -> Error { match never {} })
                .boxed_unsync();
            return Ok(Response::builder()
                .status(row["status"].as_u64().unwrap() as u16)
                .header("content-type", "application/json")
                .body(body)
                .unwrap());
        }
        if request.uri().scheme_str() != Some("http")
            || request
                .uri()
                .authority()
                .is_none_or(|a| a.as_str() != self.authority)
        {
            return Err(self.fault(self.endpoint_message));
        }
        self.native.request(request).await.map(|response| {
            response.map(|body| {
                body.map_err(|error| -> Error { Box::new(error) })
                    .boxed_unsync()
            })
        })
    }
}
pub struct RecordCallbacks<'a> {
    pub records: &'a [Value],
    pub decisions: &'a [Value],
    pub history: &'a [Value],
    pub logs: &'a [Value],
    pub real: bool,
}
pub fn compare_record_callbacks(row: &Value, seen: &RecordCallbacks<'_>) {
    assert_eq!(seen.records.len(), row["records"].as_array().unwrap().len());
    assert_eq!(
        seen.decisions.len(),
        row["decisions"].as_array().unwrap().len()
    );
    let source_starts = events(row["statuses"].as_array().unwrap(), "request_start");
    let starts = events(seen.history, "request_start");
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
        compare_real_metadata(wanted, actual, seen.real);
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
pub async fn wait_complete_count(statuses: &Events, count: usize) -> Result<(), String> {
    for _ in 0..100 {
        if events(&statuses.lock().unwrap(), "request_complete").len() >= count {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    Err("exact per-request terminal callback not observed".into())
}
