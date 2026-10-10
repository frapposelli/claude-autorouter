//! Complete frozen server #15/#16/#17 callbacks; synthetic subscription peers.
#[path = "support/server_streaming_routing.rs"]
mod peer;
use autorouter_core::{config::read_config, js_json::JsDocument, router::RouteOptions};
use autorouter_runtime::{
    evaluator::EvaluationError,
    http_client::{HttpError, HttpTransport, NativeHttpClient},
    response_observer::CompletionEvidence,
    router::Router,
    server::{Gateway, GatewayHandle, GatewayRouter},
    server_events::{EventSink, EventSinks},
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited, combinators::UnsyncBoxBody};
use hyper::{HeaderMap, Request, Response};
use peer::{Observation, Peer, ResponsePlan, StreamGate};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

const CORPUS: &str = include_str!("../../../parity/cases/server-identity-safety-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../parity/cases/server-identity-safety-contracts.capture.json");
const BOUND: Duration = Duration::from_secs(5);
const LIMIT: usize = 2 * 1024 * 1024;
type Error = Box<dyn std::error::Error + Send + Sync>;
type Body = UnsyncBoxBody<Bytes, Error>;
type Events = Arc<Mutex<Vec<Value>>>;

fn case(number: usize) -> Value {
    assert_eq!(
        format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
        "ac6c53b34fe8dbb551d5fa6ae135339e93fd45e61008f0407fb2f2e634f805fc"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(CAPTURE.as_bytes())),
        "cd11f8f0704901ad7cd56582daebccefbc24c27e836256f3ad3eaae2b599a813"
    );
    let metadata: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(metadata["static_assertions"], 52);
    assert_eq!(metadata["expanded_assertions"], 82);
    CORPUS
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|row| row["source_test"] == format!("test/server.test.mjs#{number}"))
        .unwrap()
}
fn decode(value: &Value) -> Vec<u8> {
    let value = value.as_str().unwrap();
    assert!(value.len() < LIMIT * 2);
    openssl::base64::decode_block(value).unwrap()
}
fn writes(row: &Value) -> Vec<Bytes> {
    row["response_writes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| Bytes::from(decode(&row["base64"])))
        .collect()
}
fn response_bytes(row: &Value) -> Vec<u8> {
    writes(row)
        .iter()
        .flat_map(|bytes| bytes.iter().copied())
        .collect()
}
fn upstream_rows(row: &Value) -> Vec<Value> {
    row["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|peer| peer["role"] == "upstream")
        .unwrap()["requests"]
        .as_array()
        .unwrap()
        .clone()
}
fn headers(value: &Value) -> HeaderMap {
    let mut result = HeaderMap::new();
    for (key, value) in value.as_object().unwrap() {
        result.insert(
            hyper::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
            value.as_str().unwrap().parse().unwrap(),
        );
    }
    result
}
fn header_is(map: &HeaderMap, name: &str, value: Option<&str>) -> bool {
    map.get(name).and_then(|value| value.to_str().ok()) == value
}
fn parsed(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}
fn events(rows: &[Value], name: &str) -> Vec<Value> {
    rows.iter()
        .filter(|row| row["event"] == name)
        .cloned()
        .collect()
}
fn route_tuple(row: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for key in ["model", "source", "reason", "classified_tier"] {
        if let Some(value) = row.get(key) {
            out.insert(key.into(), value.clone());
        }
    }
    Value::Object(out)
}
fn sink(rows: &Events) -> EventSink {
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
fn compare(observed: &Observation, expected: &Value, exact_text: bool) -> Result<(), String> {
    if observed.method != expected["method"].as_str().unwrap()
        || observed.path != expected["path"].as_str().unwrap()
    {
        return Err("method/path mismatch".into());
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
            expected["headers"].get(name).and_then(Value::as_str),
        ) {
            return Err(format!("semantic header mismatch: {name}"));
        }
    }
    let wanted = expected["request_body_utf8"].as_str().unwrap().as_bytes();
    if exact_text {
        if observed.body != wanted {
            return Err("exact serialized body mismatch".into());
        }
    } else if serde_json::from_slice::<Value>(&observed.body).map_err(|_| "invalid JSON body")?
        != parsed(wanted)
    {
        return Err("complete parsed body mismatch".into());
    }
    Ok(())
}

struct Transport {
    native: Arc<NativeHttpClient>,
    authority: String,
    script: Vec<Value>,
    mocked: Mutex<Vec<Observation>>,
    faults: Mutex<Vec<String>>,
}
impl Transport {
    fn fault(&self, text: &str) -> HttpError {
        let mut faults = self.faults.lock().unwrap();
        if faults.len() < 16 {
            faults.push(text.into());
        }
        HttpError::InvalidRequest
    }
}
impl HttpTransport for Transport {
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
                    != serde_json::from_str::<Value>(row["body"].as_str().unwrap()).unwrap()
            {
                return Err(self.fault("complete evaluator request mismatch"));
            }
            let response_bytes: Vec<u8> = row["reads"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|read| read["kind"] == "read" && read["done"] == false)
                .flat_map(|read| decode(&read["base64"]))
                .collect();
            let body = Full::new(Bytes::from(response_bytes))
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
            return Err(self.fault("unregistered endpoint or unexpected token count"));
        }
        self.native.request(request).await.map(|response| {
            response.map(|body| {
                body.map_err(|error| -> Error { Box::new(error) })
                    .boxed_unsync()
            })
        })
    }
}

struct AuditRouter {
    real: Option<Router<Transport>>,
    calls: Mutex<Vec<Value>>,
    completions: Mutex<Vec<Value>>,
    token: Mutex<Option<CancellationToken>>,
    admitted: CancellationToken,
    hold: bool,
}
impl AuditRouter {
    fn record_completion(&self, row: Value) {
        let mut rows = self.completions.lock().unwrap();
        if rows.len() >= 32 {
            if rows.len() == 32 {
                rows.push(json!({"fixture_observer_overflow":true}));
            }
            return;
        }
        rows.push(row);
    }
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
        {
            let mut calls = self.calls.lock().unwrap();
            assert!(calls.len() < 8);
            calls.push(json!({"document":parsed(document.stringify().as_bytes()),"scope":options.scope,"prompt_id":options.prompt_id,"request_class":options.request_class,"signal_live":!cancellation.is_cancelled()}));
        }
        *self.token.lock().unwrap() = Some(cancellation.clone());
        self.admitted.cancel();
        if self.hold {
            cancellation.cancelled().await;
            return Err(EvaluationError::Cancelled);
        }
        if let Some(router) = &self.real {
            router
                .route(document, options, headers, cancellation, search)
                .await
        } else {
            Ok(json!({"model":"claude-opus-5-5","source":"test"}))
        }
    }
    fn complete(&self, id: &str, evidence: &Value) -> bool {
        let accepted = self
            .real
            .as_ref()
            .is_some_and(|router| router.complete(id, evidence));
        self.record_completion(json!({"id":id,"evidence":evidence,"accepted":accepted}));
        accepted
    }
    fn complete_exact(&self, id: &str, evidence: &CompletionEvidence) -> bool {
        let accepted = self
            .real
            .as_ref()
            .is_some_and(|router| router.complete_exact(id, evidence));
        self.record_completion(json!({"id":id,"evidence":evidence,"accepted":accepted}));
        accepted
    }
    fn shutdown(&self) {
        if let Some(router) = &self.real {
            router.shutdown();
        }
    }
    async fn close(&self) {
        if let Some(router) = &self.real {
            router.close().await;
        }
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Original,
    HoldRoute,
    Truncated,
    ChangedVerdict,
}
struct Fixture {
    client: Arc<NativeHttpClient>,
    transport: Arc<Transport>,
    router: Arc<AuditRouter>,
    gateway: Option<GatewayHandle>,
    peer: Option<Peer>,
    gate: Option<StreamGate>,
    logs: Events,
    statuses: Events,
    records: Events,
}
struct Reply {
    status: u16,
    headers: HeaderMap,
    bytes: Vec<u8>,
    first: Option<Vec<u8>>,
    held: bool,
    no_model_before_release: bool,
}
struct Seen {
    requests: Vec<Observation>,
    mocked: Vec<Observation>,
    routes: Vec<Value>,
    completions: Vec<Value>,
    logs: Vec<Value>,
    statuses: Vec<Value>,
    records: Vec<Value>,
}
impl Fixture {
    async fn new(number: usize, row: &Value, mode: Mode) -> Self {
        let script = upstream_rows(row);
        let mut gate = None;
        let held = if number == 15 {
            let row = &script[0];
            let mut chunks = writes(row);
            let first = chunks.remove(0);
            let (response, witness) =
                ResponsePlan::held_tail(200, headers(&row["response_headers"]), first, chunks);
            gate = Some(witness);
            Some(response)
        } else {
            None
        };
        let held = Mutex::new(held);
        let peer = Peer::start(move |request, index| {
            let row = script.get(index).ok_or("extra upstream call")?;
            if number == 16 {
                let input = parsed(&request.body);
                // The complete original provider rejects invalid prepared requests.
                if input["model"] != "claude-sonnet-5-5" || input["thinking"]["type"] != "between_tools" {
                    return Ok(ResponsePlan::bytes(400, headers(&json!({"content-type":"application/json"})), br#"{"type":"error","error":{"type":"invalid_request_error","message":"Sonnet 5.5 requires between_tools thinking for this request"}}"#.as_slice()));
                }
            }
            compare(request, row, number == 17)?;
            if let Some(response) = held.lock().unwrap().take() { return Ok(response); }
            let mut chunks = writes(row);
            if mode == Mode::Truncated && index == 0 {
                let stop = b"event: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n";
                let last = chunks.last_mut().unwrap();
                if !last.ends_with(stop) { return Err("truncation control marker absent".into()); }
                let final_delta = b"event: message_delta\r\n";
                let cut = last.windows(final_delta.len()).position(|part| part == final_delta)
                    .ok_or("truncation final-delta marker absent")?;
                *last = last.slice(..cut);
            }
            if mode == Mode::ChangedVerdict && index == 1 {
                let full: Vec<_> = chunks.iter().flat_map(|c| c.iter().copied()).collect();
                let text = String::from_utf8(full).map_err(|_| "fixture verdict is not UTF-8")?;
                if text.matches("flagged").count()!=1 {return Err("fixture verdict marker missing".into());}
                let changed=Bytes::from(text.replace("flagged","allowed"));
                chunks=vec![changed.slice(..37),changed.slice(37..)];
            }
            Ok(ResponsePlan::chunks(row["response_status"].as_u64().unwrap() as u16, headers(&row["response_headers"]), chunks))
        }).await.unwrap();
        let client = Arc::new(NativeHttpClient::new().unwrap());
        let transport = Arc::new(Transport {
            native: client.clone(),
            authority: peer.address.to_string(),
            script: row["mocked_fetch"].as_array().unwrap().clone(),
            mocked: Mutex::new(Vec::new()),
            faults: Mutex::new(Vec::new()),
        });
        let original_config =
            read_config(&row["config_arguments"][0][0], false, Path::new("/tmp")).unwrap();
        let mut config = read_config(
            &row["config_arguments"].as_array().unwrap().last().unwrap()[0],
            false,
            Path::new("/tmp"),
        )
        .unwrap();
        config.auth_mode = autorouter_core::config::AuthMode::Subscription;
        config.local_token = row["gateway_config"]["localToken"]
            .as_str()
            .map(str::to_owned);
        config.models = original_config.models.clone();
        config.client_profile = original_config.client_profile;
        config.upstream = format!("http://{}", peer.address);
        let router = Arc::new(AuditRouter {
            real: (number != 15).then(|| Router::new(transport.clone(), original_config)),
            calls: Mutex::new(Vec::new()),
            completions: Mutex::new(Vec::new()),
            token: Mutex::new(None),
            admitted: CancellationToken::new(),
            hold: mode == Mode::HoldRoute,
        });
        let logs = Events::default();
        let statuses = Events::default();
        let records = Events::default();
        let recorded = records.clone();
        let gateway = Gateway::with_router(
            config,
            transport.clone(),
            router.clone(),
            EventSinks {
                log: Some(sink(&logs)),
                status: Some(sink(&statuses)),
                record: Some(Arc::new(move |row| {
                    let mut rows = recorded.lock().unwrap();
                    if rows.len() >= 32 {
                        if rows.len() == 32 {
                            rows.push(json!({"event":"fixture_observer_overflow"}));
                        }
                        return;
                    }
                    rows.push(row);
                })),
                ..Default::default()
            },
        )
        .unwrap()
        .listen(0)
        .await
        .unwrap();
        Self {
            client,
            transport,
            router,
            gateway: Some(gateway),
            peer: Some(peer),
            gate,
            logs,
            statuses,
            records,
        }
    }
    fn request(&self, row: &Value) -> Request<Full<Bytes>> {
        let url = url::Url::parse(row["url"].as_str().unwrap()).unwrap();
        let suffix = format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        let mut request = Request::builder()
            .method(row["method"].as_str().unwrap())
            .uri(format!(
                "http://{}{suffix}",
                self.gateway.as_ref().unwrap().address
            ));
        *request.headers_mut().unwrap() = headers(&row["headers"]);
        request
            .body(Full::new(Bytes::from(
                row["body"].as_str().unwrap().to_owned(),
            )))
            .unwrap()
    }
    async fn send(&self, row: &Value) -> Result<Reply, String> {
        let response = self
            .client
            .request(self.request(row))
            .await
            .map_err(|e| e.to_string())?;
        let (parts, mut body) = response.into_parts();
        let mut first = None;
        let mut held = false;
        let mut no_model_before_release = false;
        if let Some(gate) = &self.gate {
            gate.wait_first_polled().await?;
            let chunk = body
                .frame()
                .await
                .ok_or("missing first frame")?
                .map_err(|e| e.to_string())?
                .into_data()
                .map_err(|_| "first frame is trailers")?;
            first = Some(chunk.to_vec());
            held = gate.first_was_polled() && !gate.final_was_polled() && !gate.released();
            no_model_before_release =
                events(&self.logs.lock().unwrap(), "upstream_model").is_empty();
            gate.release();
        }
        let rest = Limited::new(body, LIMIT)
            .collect()
            .await
            .map_err(|e| e.to_string())?
            .to_bytes();
        let mut bytes = first.clone().unwrap_or_default();
        bytes.extend_from_slice(&rest);
        Ok(Reply {
            status: parts.status.as_u16(),
            headers: parts.headers,
            bytes,
            first,
            held,
            no_model_before_release,
        })
    }
    async fn finish(mut self) -> Result<Seen, String> {
        if let Some(gate) = &self.gate {
            gate.release();
        }
        let closed = tokio::time::timeout(BOUND, self.gateway.take().unwrap().close()).await;
        let requests = self.peer.as_ref().unwrap().observations();
        let peer_closed = self.peer.take().unwrap().close().await;
        closed.map_err(|_| "gateway cleanup timed out")?;
        peer_closed?;
        let faults = self.transport.faults.lock().unwrap().clone();
        if !faults.is_empty() {
            return Err(format!("sticky transport faults: {faults:?}"));
        }
        for rows in [&self.logs, &self.statuses, &self.records] {
            if rows
                .lock()
                .unwrap()
                .iter()
                .any(|row| row["event"] == "fixture_observer_overflow")
            {
                return Err("sticky event recorder overflow".into());
            }
        }
        if self
            .router
            .completions
            .lock()
            .unwrap()
            .iter()
            .any(|row| row["fixture_observer_overflow"] == true)
        {
            return Err("sticky completion recorder overflow".into());
        }
        Ok(Seen {
            requests,
            mocked: self.transport.mocked.lock().unwrap().clone(),
            routes: self.router.calls.lock().unwrap().clone(),
            completions: self.router.completions.lock().unwrap().clone(),
            logs: self.logs.lock().unwrap().clone(),
            statuses: self.statuses.lock().unwrap().clone(),
            records: self.records.lock().unwrap().clone(),
        })
    }
}
async fn run(number: usize, row: &Value) -> (Vec<Reply>, Seen) {
    let f = Fixture::new(number, row, Mode::Original).await;
    let result = tokio::time::timeout(BOUND, async {
        let mut replies = Vec::new();
        for request in row["downstream"].as_array().unwrap() {
            replies.push(f.send(request).await?);
        }
        Ok::<_, String>(replies)
    })
    .await;
    let seen = f.finish().await;
    (
        result
            .expect("bounded original schedule")
            .expect("original I/O"),
        seen.expect("owned cleanup"),
    )
}
struct Assertions {
    number: usize,
    counts: BTreeMap<usize, usize>,
}
impl Assertions {
    fn new(number: usize) -> Self {
        Self {
            number,
            counts: BTreeMap::new(),
        }
    }
    fn check(&mut self, site: usize, passed: bool) {
        *self.counts.entry(site).or_default() += 1;
        assert!(passed, "server#{}:assert-{site}", self.number);
    }
    fn finish(self) {
        let metadata: Value = serde_json::from_str(CAPTURE).unwrap();
        let row = metadata["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["number"] == self.number)
            .unwrap();
        for (index, site) in row["assertions"].as_array().unwrap().iter().enumerate() {
            assert_eq!(
                self.counts.get(&(index + 1)).copied().unwrap_or(0),
                site["executions"].as_u64().unwrap() as usize,
                "server#{}:assert-{} count",
                self.number,
                index + 1
            );
        }
        assert_eq!(
            self.counts.len(),
            row["assertions"].as_array().unwrap().len()
        );
    }
}

#[tokio::test]
async fn original_15_subscription_identity_partial_sse_and_adaptive_opus() {
    let row = case(15);
    let originals = row.clone();
    let (replies, seen) = run(15, &row).await;
    let mut a = Assertions::new(15);
    assert_eq!(seen.requests.len(), 1);
    assert_eq!(seen.routes.len(), 1);
    assert!(seen.mocked.is_empty());
    let request = &seen.requests[0];
    let expected = &upstream_rows(&row)[0];
    let reply = &replies[0];
    let route = &seen.routes[0];
    a.check(
        1,
        parsed(&request.body) == parsed(expected["request_body_utf8"].as_str().unwrap().as_bytes()),
    );
    a.check(
        2,
        header_is(
            &request.headers,
            "authorization",
            Some("Bearer fake-subscription-token"),
        ),
    );
    a.check(3, header_is(&request.headers, "x-autorouter-token", None));
    a.check(
        4,
        header_is(&request.headers, "accept-encoding", Some("identity")),
    );
    a.check(
        5,
        route["document"] == parsed(row["downstream"][0]["body"].as_str().unwrap().as_bytes()),
    );
    a.check(6, route["prompt_id"] == "human-prompt-1");
    a.check(7, route["scope"] == "[\"session-1\",\"agent-1\"]");
    a.check(8, route["request_class"] == "main");
    // Explicit API migration: actual typed Gateway token, not JS prototype identity.
    a.check(9, route["signal_live"] == true);
    a.check(10, reply.status == 200);
    assert!(
        reply.held,
        "actual first wire receipt before final provider release"
    );
    a.check(
        11,
        reply.first.as_deref() == Some(writes(expected)[0].as_ref()),
    );
    a.check(12, reply.no_model_before_release);
    a.check(13, reply.bytes == response_bytes(expected));
    a.check(14, row == originals);
    a.check(
        15,
        events(&seen.logs, "route").first()
            == row["logs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["event"] == "route"),
    );
    a.check(
        16,
        events(&seen.logs, "upstream_model")
            == json!([{"event":"upstream_model","model":"claude-opus-5-5-provider-revision"}])
                .as_array()
                .unwrap()
                .clone(),
    );
    a.check(
        17,
        seen.logs.iter().any(|row| {
            row["event"] == "upstream_response" && row["status"].as_f64() == Some(200.0)
        }),
    );
    assert!(!seen.logs.is_empty());
    for secret in [
        "Fix a typo",
        "fake-subscription-token",
        "local-test-token-123456789",
    ] {
        a.check(18, !json!(seen.logs).to_string().contains(secret));
    }
    a.finish();
}

#[tokio::test]
async fn original_16_low_confidence_signed_sonnet_tool_continuation_and_exact_bytes() {
    let row = case(16);
    let originals = row.clone();
    let (replies, seen) = run(16, &row).await;
    let mut a = Assertions::new(16);
    for call in &seen.mocked {
        a.check(
            1,
            call.path == row["router_config"]["jevEndpoint"].as_str().unwrap(),
        );
        a.check(
            2,
            header_is(
                &call.headers,
                "authorization",
                Some("Bearer classifier-secret"),
            ),
        );
        a.check(
            3,
            parsed(&call.body)["state"]["current_task"] == "Fix a typo",
        );
    }
    let expected = upstream_rows(&row);
    assert!(
        std::str::from_utf8(&writes(&expected[0])[0]).is_err(),
        "original split is inside UTF-8"
    );
    for (index, call) in seen.requests.iter().enumerate() {
        a.check(
            4,
            header_is(
                &call.headers,
                "authorization",
                Some("Bearer fake-subscription-token"),
            ),
        );
        a.check(5, header_is(&call.headers, "x-autorouter-token", None));
        a.check(6, replies[index].status == 200);
        a.check(7, replies[index].bytes == response_bytes(&expected[index]));
        a.check(
            8,
            parsed(&call.body)
                == parsed(
                    expected[index]["request_body_utf8"]
                        .as_str()
                        .unwrap()
                        .as_bytes(),
                ),
        );
    }
    a.check(9, seen.requests.len() == 2);
    a.check(10, seen.mocked.len() == 2);
    a.check(11, row == originals);
    a.check(12,events(&seen.statuses,"route").iter().map(route_tuple).collect::<Vec<_>>()==vec![json!({"model":"claude-sonnet-5-5","source":"jev","reason":"low_confidence","classified_tier":"haiku"}),json!({"model":"claude-sonnet-5-5","source":"jev","reason":"tool_turn_pinned","classified_tier":"haiku"})]);
    a.check(
        13,
        events(&seen.logs, "upstream_model")
            .iter()
            .map(|row| row["model"].clone())
            .collect::<Vec<_>>()
            == vec![json!("claude-sonnet-5-5"); 2],
    );
    a.check(
        14,
        events(&seen.logs, "upstream_response")
            .iter()
            .map(|row| row["status"].as_f64())
            .collect::<Vec<_>>()
            == vec![Some(200.0); 2],
    );
    assert_eq!(
        seen.completions
            .iter()
            .filter(|row| row["accepted"] == true)
            .count(),
        2,
        "only actual completed forwarding confirms owners"
    );
    for secret in [
        "Synthetic private reasoning: inspect → edit.",
        "synthetic-signature+/==",
        "classifier-secret",
        "fake-subscription-token",
    ] {
        a.check(
            15,
            !json!({"logs":seen.logs,"statuses":seen.statuses})
                .to_string()
                .contains(secret),
        );
    }
    a.finish();
}

#[tokio::test]
async fn original_17_auto_auxiliary_and_flagged_safeguards_preserve_exact_wire_contract() {
    let row = case(17);
    let originals = row.clone();
    let (replies, seen) = run(17, &row).await;
    let mut a = Assertions::new(17);
    for call in &seen.mocked {
        a.check(
            1,
            call.path == row["router_config"]["jevEndpoint"].as_str().unwrap(),
        );
        a.check(
            2,
            parsed(&call.body)["state"]["current_task"]
                == "Design a secure cross-process transaction protocol.",
        );
    }
    let expected = upstream_rows(&row);
    for (index, request) in seen.requests.iter().enumerate() {
        a.check(3, request.path == "/v1/messages?beta=true");
        a.check(
            4,
            header_is(
                &request.headers,
                "authorization",
                Some("Bearer fake-subscription-token"),
            ),
        );
        a.check(
            5,
            header_is(
                &request.headers,
                "anthropic-beta",
                Some("oauth-2025-04-20,future-capability,dangerous-tool-use-2026-09-03"),
            ),
        );
        a.check(
            6,
            header_is(&request.headers, "anthropic-version", Some("2023-06-01")),
        );
        a.check(
            7,
            header_is(
                &request.headers,
                "x-claude-code-request-class",
                Some(["auxiliary", "main"][index]),
            ),
        );
        a.check(
            8,
            header_is(
                &request.headers,
                "x-claude-code-session-id",
                Some("safety-session"),
            ),
        );
        a.check(9, header_is(&request.headers, "x-autorouter-token", None));
        // Source compares JSON.stringify text: preserve complete property order and bytes.
        a.check(
            10,
            request.body
                == expected[index]["request_body_utf8"]
                    .as_str()
                    .unwrap()
                    .as_bytes(),
        );
        a.check(11, replies[index].status == 200);
        a.check(
            12,
            header_is(
                &replies[index].headers,
                "request-id",
                Some(&format!("safety-provider-{index}")),
            ),
        );
        a.check(
            13,
            header_is(
                &replies[index].headers,
                "x-safety-fixture",
                Some("retained"),
            ),
        );
        a.check(14, replies[index].bytes == response_bytes(&expected[index]));
    }
    a.check(15, seen.mocked.len() == 1);
    a.check(16, seen.requests.len() == 2);
    a.check(17, row == originals);
    a.check(18,events(&seen.statuses,"route").iter().map(route_tuple).collect::<Vec<_>>()==vec![json!({"model":"claude-haiku-4-5-20251001","source":"passthrough","reason":"internal_request"}),json!({"model":"claude-opus-5-5","source":"jev","reason":"classified","classified_tier":"opus"})]);
    assert!(!seen.logs.is_empty() && !seen.statuses.is_empty());
    for secret in [
        "Synthetic denied action",
        "Synthetic prior reasoning → retained.",
        "synthetic-safety-signature+/==",
        "Preserve this review context.",
        "classifier-secret",
        "fake-subscription-token",
    ] {
        a.check(
            19,
            !json!({"logs":seen.logs,"statuses":seen.statuses})
                .to_string()
                .contains(secret),
        );
    }
    a.finish();
}

#[tokio::test]
async fn gateway_supplied_route_token_cancels_only_after_actual_downstream_disconnect() {
    let row = case(15);
    let f = Fixture::new(15, &row, Mode::HoldRoute).await;
    let result=tokio::time::timeout(BOUND,async{
        let mut socket=tokio::net::TcpStream::connect(f.gateway.as_ref().unwrap().address).await.unwrap();
        let body=row["downstream"][0]["body"].as_str().unwrap();
        let head=format!("POST /v1/messages HTTP/1.1\r\nHost: {}\r\nx-autorouter-token: local-test-token-123456789\r\nauthorization: Bearer fake-subscription-token\r\nanthropic-beta: oauth-2025-04-20,future-capability\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",f.gateway.as_ref().unwrap().address,body.len());
        socket.write_all(head.as_bytes()).await.unwrap();socket.write_all(body.as_bytes()).await.unwrap();
        f.router.admitted.cancelled().await;
        let token=f.router.token.lock().unwrap().clone().unwrap();let live_before=!token.is_cancelled();
        drop(socket);token.cancelled().await;
        (live_before,token.is_cancelled())
    }).await;
    let seen = f.finish().await.expect("cleanup after real disconnect");
    assert_eq!(result.expect("bounded disconnect"), (true, true));
    assert!(seen.requests.is_empty());
    assert!(seen.mocked.is_empty());
    assert_eq!(seen.routes.len(), 1);
}

#[tokio::test]
async fn provider_rejects_invalid_thinking_and_full_comparator_detects_changed_signature() {
    let row = case(16);
    let f = Fixture::new(16, &row, Mode::Original).await;
    let result = tokio::time::timeout(BOUND, async {
        let request = Request::post(format!(
            "http://{}/v1/messages",
            f.peer.as_ref().unwrap().address
        ))
        .body(Full::new(Bytes::from(
            row["downstream"][0]["body"].as_str().unwrap().to_owned(),
        )))
        .unwrap();
        let response = f.client.request(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, bytes)
    })
    .await;
    let seen = f.finish().await.expect("negative control cleanup");
    let (status, bytes) = result.expect("bounded rejection");
    assert_eq!(status, 400);
    assert_eq!(bytes.as_ref(), br#"{"type":"error","error":{"type":"invalid_request_error","message":"Sonnet 5.5 requires between_tools thinking for this request"}}"#);
    assert_eq!(seen.requests.len(), 1);
    assert!(seen.routes.is_empty());
    let expected = upstream_rows(&row)[1].clone();
    let mut changed = Observation {
        method: "POST".into(),
        path: expected["path"].as_str().unwrap().into(),
        headers: headers(&expected["headers"]),
        body: expected["request_body_utf8"]
            .as_str()
            .unwrap()
            .replace("synthetic-signature+/==", "changed-signature+/==")
            .into_bytes(),
    };
    assert!(compare(&changed, &expected, false).is_err());
    changed.body = expected["request_body_utf8"]
        .as_str()
        .unwrap()
        .as_bytes()
        .to_vec();
    assert!(compare(&changed, &expected, false).is_ok());
}

#[tokio::test]
async fn observed_signed_tool_bytes_with_no_terminal_evidence_never_confirm_continuation() {
    let row = case(16);
    let f = Fixture::new(16, &row, Mode::Truncated).await;
    let result = tokio::time::timeout(BOUND, async {
        let first = f.send(&row["downstream"][0]).await?;
        loop {
            if f.records
                .lock()
                .unwrap()
                .iter()
                .any(|row| row["event"] == "outcome")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        let early_completions = f.router.completions.lock().unwrap().clone();
        let early_models = events(&f.logs.lock().unwrap(), "upstream_model");
        let second = f.send(&row["downstream"][1]).await?;
        Ok::<_, String>((first, second, early_completions, early_models))
    })
    .await;
    let seen = f.finish().await.expect("truncated ownership cleanup");
    let (first, second, early, early_models) = result
        .expect("bounded truncated schedule")
        .expect("truncated I/O");
    assert_eq!(first.status, 200);
    let original = response_bytes(&upstream_rows(&row)[0]);
    let terminal = b"event: message_delta\r\n";
    let cut = original
        .windows(terminal.len())
        .position(|part| part == terminal)
        .expect("original terminal delta exists");
    assert_eq!(first.bytes, original[..cut]);
    for preserved in [b"synthetic-signature+/==".as_slice(), b"tool_sonnet_55"] {
        assert!(
            first
                .bytes
                .windows(preserved.len())
                .any(|part| part == preserved)
        );
    }
    assert_eq!(
        early_models,
        vec![json!({"event":"upstream_model","model":"claude-sonnet-5-5"})]
    );
    assert!(
        !first
            .bytes
            .ends_with(b"event: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n")
    );
    assert!(
        !first
            .bytes
            .windows(b"event: message_delta".len())
            .any(|part| part == b"event: message_delta")
    );
    assert!(
        !first
            .bytes
            .windows(b"event: message_stop".len())
            .any(|part| part == b"event: message_stop")
    );
    assert!(early.iter().all(|row| row["accepted"] == false));
    assert_eq!(
        events(&seen.records, "outcome")[0]["completion_confirmed"],
        false
    );
    assert_eq!(
        events(&seen.statuses, "route")[1]["reason"],
        "unknown_continuation"
    );
    assert_eq!(second.status, 400);
    assert_eq!(seen.mocked.len(), 2);
    assert!(seen.completions.iter().all(|row| row["accepted"] == false));
}

#[tokio::test]
async fn changed_synthetic_verdict_is_forwarded_verbatim_and_rejected_by_original_byte_predicate() {
    let row = case(17);
    let f = Fixture::new(17, &row, Mode::ChangedVerdict).await;
    let result = tokio::time::timeout(BOUND, async {
        let first = f.send(&row["downstream"][0]).await?;
        let second = f.send(&row["downstream"][1]).await?;
        Ok::<_, String>((first, second))
    })
    .await;
    let seen = f.finish().await.expect("changed-verdict owned cleanup");
    let (first, second) = result
        .expect("bounded changed-verdict schedule")
        .expect("changed-verdict I/O");
    let original = upstream_rows(&row);
    assert_eq!(first.bytes, response_bytes(&original[0]));
    let unchanged = response_bytes(&original[1]);
    let changed = String::from_utf8(unchanged.clone())
        .unwrap()
        .replace("flagged", "allowed")
        .into_bytes();
    assert_eq!(
        second.bytes, changed,
        "gateway preserves supplied synthetic verdict bytes"
    );
    assert_ne!(
        second.bytes, unchanged,
        "original full-byte predicate rejects the changed verdict"
    );
    assert_eq!(seen.requests.len(), 2);
    assert_eq!(seen.mocked.len(), 1);
}

#[tokio::test]
async fn bounded_chunk_helper_rejects_overflow_and_empty_held_tail_but_allows_empty_body() {
    for variant in 0..3 {
        let peer = Peer::start(move |_, _| {
            let response = match variant {
                0 => ResponsePlan::chunks(200, HeaderMap::new(), vec![]),
                1 => ResponsePlan::chunks(200, HeaderMap::new(), vec![Bytes::new(); 17]),
                _ => {
                    ResponsePlan::held_tail(
                        200,
                        HeaderMap::new(),
                        Bytes::from_static(b"first"),
                        vec![],
                    )
                    .0
                }
            };
            Ok(response)
        })
        .await
        .unwrap();
        let client = NativeHttpClient::new().unwrap();
        let result = tokio::time::timeout(BOUND, async {
            let response = client
                .request(
                    Request::get(format!("http://{}/", peer.address))
                        .body(Full::new(Bytes::new()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (status, bytes)
        })
        .await;
        let closed = peer.close().await;
        let (status, bytes) = result.expect("bounded helper sensitivity control");
        if variant == 0 {
            assert_eq!(status, 200);
            assert!(bytes.is_empty());
            assert!(closed.is_ok());
        } else {
            assert_eq!(status, 500);
            assert!(
                closed.is_err(),
                "sticky constructor rejection survives cleanup"
            );
        }
    }
}
