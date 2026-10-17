//! Four complete original HTTP callbacks through ordinary native Gateway/Router.
#[path = "support/server_streaming_routing.rs"]
mod peer;
use autorouter_core::{config::read_config, js_json::JsDocument, router::RouteOptions};
use autorouter_runtime::{
    evaluator::EvaluationError,
    http_client::{HttpError, HttpTransport, NativeHttpClient},
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
    convert::Infallible,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const CORPUS: &str = include_str!("../../../parity/cases/server-streaming-routing-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../parity/cases/server-streaming-routing-contracts.capture.json");
const SHA: &str = "3f92130625b6e04789fd802e5322fbaba64098f6318bbd2e2b4e0d6a18151e33";
const BOUND: Duration = Duration::from_secs(5);
const LIMIT: usize = 2 * 1024 * 1024;
type Error = Box<dyn std::error::Error + Send + Sync>;
type Body = UnsyncBoxBody<Bytes, Error>;
type Events = Arc<Mutex<Vec<Value>>>;

fn case(number: usize) -> Value {
    assert_eq!(format!("{:x}", Sha256::digest(CORPUS.as_bytes())), SHA);
    assert_eq!(
        format!("{:x}", Sha256::digest(CAPTURE.as_bytes())),
        "92b3330053517195ee16dd5d5ac1edce811e7a8920a8391fa354aa491e6a1100"
    );
    let capture: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(capture["cases_sha256"], SHA);
    assert_eq!(capture["static_assertions"], 57);
    assert_eq!(capture["expanded_assertions"], 99);
    CORPUS
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|row| row["source_test"] == format!("test/server.test.mjs#{number}"))
        .unwrap()
}
fn headers(value: &Value) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (key, value) in value.as_object().unwrap() {
        map.insert(
            hyper::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
            value.as_str().unwrap().parse().unwrap(),
        );
    }
    map
}
fn body_json(observation: &Observation) -> Value {
    serde_json::from_slice(&observation.body).unwrap()
}
fn expected_peer(row: &Value, role: &str) -> Vec<Value> {
    row["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|peer| peer["role"] == role)
        .unwrap()["requests"]
        .as_array()
        .unwrap()
        .clone()
}
fn response_bytes(row: &Value) -> Vec<u8> {
    row["response_writes"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|write| write["utf8"].as_str().unwrap().as_bytes().to_vec())
        .collect()
}
// Compare the complete JSON body and six original semantic header fields.
// Other captured headers, including content-type and HTTP-generated transport
// headers, remain reference observations, not claims of equality in this helper.
fn compare(observed: &Observation, expected: &Value) -> Result<(), String> {
    if observed.method != expected["method"].as_str().unwrap()
        || observed.path != expected["path"].as_str().unwrap()
    {
        return Err("method/path transcript mismatch".into());
    }
    for key in [
        "authorization",
        "x-api-key",
        "x-autorouter-token",
        "cookie",
        "anthropic-beta",
        "anthropic-version",
    ] {
        let actual = observed
            .headers
            .get(key)
            .map(|value| value.to_str().unwrap());
        if actual != expected["headers"].get(key).and_then(Value::as_str) {
            return Err(format!("semantic header mismatch: {key}"));
        }
    }
    let actual: Value =
        serde_json::from_slice(&observed.body).map_err(|_| "invalid JSON request")?;
    let wanted: Value =
        serde_json::from_str(expected["request_body_utf8"].as_str().unwrap()).unwrap();
    if actual != wanted {
        return Err("complete JSON payload transcript mismatch".into());
    }
    Ok(())
}
fn sink(events: &Events) -> EventSink {
    let events = events.clone();
    Arc::new(move |document| {
        let mut rows = events.lock().unwrap();
        assert!(rows.len() < 128);
        rows.push(document.to_serde_observation_lossy());
    })
}
#[derive(Default)]
struct Stub {
    calls: AtomicUsize,
}
impl GatewayRouter for Stub {
    async fn route(
        &self,
        _: Arc<JsDocument>,
        _: RouteOptions,
        _: &HeaderMap,
        _: &CancellationToken,
        _: &str,
    ) -> Result<Value, EvaluationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"model":"claude-haiku-4-5-20251001","source":"test"}))
    }
    fn complete(&self, _: &str, _: &Value) -> bool {
        false
    }
    fn shutdown(&self) {}
    async fn close(&self) {
        self.shutdown();
    }
}
struct Transport {
    native: Arc<NativeHttpClient>,
    mock: Option<Value>,
    mocked: Mutex<Vec<Observation>>,
    allowed_authorities: Vec<String>,
}
impl HttpTransport for Transport {
    type ResponseBody = Body;
    async fn request(&self, request: Request<Full<Bytes>>) -> Result<Response<Body>, HttpError> {
        if let Some(mock) = &self.mock
            && request.uri() == mock["url"].as_str().unwrap()
        {
            let (parts, body) = request.into_parts();
            let bytes = body.collect().await.unwrap().to_bytes();
            let observation = Observation {
                method: parts.method.to_string(),
                path: parts.uri.to_string(),
                headers: parts.headers,
                body: bytes.to_vec(),
            };
            {
                let mut calls = self.mocked.lock().unwrap();
                if !calls.is_empty() {
                    return Err(HttpError::InvalidRequest);
                }
                calls.push(observation);
            }
            let reply: String = mock["reads"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|read| read.get("utf8").and_then(Value::as_str))
                .collect();
            assert!(!reply.is_empty());
            return Ok(Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(
                    Full::new(Bytes::from(reply))
                        .map_err(|error: Infallible| -> Error { match error {} })
                        .boxed_unsync(),
                )
                .unwrap());
        }
        // Any external request would be a fixture defect, not permission to call it.
        if request.uri().scheme_str() != Some("http")
            || !request.uri().authority().is_some_and(|authority| {
                self.allowed_authorities
                    .iter()
                    .any(|allowed| allowed == authority.as_str())
            })
        {
            return Err(HttpError::InvalidRequest);
        }
        self.native.request(request).await.map(|response| {
            response.map(|body| {
                body.map_err(|error| -> Error { Box::new(error) })
                    .boxed_unsync()
            })
        })
    }
}
struct Fixture {
    client: Arc<NativeHttpClient>,
    transport: Arc<Transport>,
    handle: Option<GatewayHandle>,
    upstream: Option<Peer>,
    jev: Option<Peer>,
    stub: Option<Arc<Stub>>,
    gate: Option<StreamGate>,
    logs: Events,
    statuses: Events,
}
struct Reply {
    status: u16,
    headers: HeaderMap,
    body: Vec<u8>,
    first: Option<Vec<u8>>,
    first_while_held: bool,
}
struct Observations {
    upstream: Vec<Observation>,
    jev: Vec<Observation>,
    mocked: Vec<Observation>,
    logs: Vec<Value>,
    statuses: Vec<Value>,
    stub_calls: usize,
}
impl Fixture {
    async fn new(number: usize, row: &Value) -> Self {
        let upstream_rows = expected_peer(row, "upstream");
        let mut held = None;
        let mut gate = None;
        if number == 1 {
            let response = &upstream_rows[0];
            let writes = response["response_writes"].as_array().unwrap();
            let (plan, witness) = ResponsePlan::held(
                200,
                headers(&response["response_headers"]),
                Bytes::from(writes[0]["utf8"].as_str().unwrap().to_owned()),
                Bytes::from(writes[1]["utf8"].as_str().unwrap().to_owned()),
            );
            held = Some(plan);
            gate = Some(witness);
        }
        let held = Mutex::new(held);
        let upstream = Peer::start(move |observation, index| {
            let expected = upstream_rows.get(index).ok_or("extra upstream request")?;
            compare(observation, expected)?;
            if let Some(plan) = held.lock().unwrap().take() {
                return Ok(plan);
            }
            Ok(ResponsePlan::bytes(
                expected["response_status"].as_u64().unwrap() as u16,
                headers(&expected["response_headers"]),
                response_bytes(expected),
            ))
        })
        .await
        .unwrap();
        let jev = if number == 8 {
            let rows = expected_peer(row, "jev");
            Some(
                Peer::start(move |observation, index| {
                    let expected = rows.get(index).ok_or("extra evaluator request")?;
                    compare(observation, expected)?;
                    Ok(ResponsePlan::bytes(
                        200,
                        headers(&expected["response_headers"]),
                        response_bytes(expected),
                    ))
                })
                .await
                .unwrap(),
            )
        } else {
            None
        };
        let env = &row["config_arguments"].as_array().unwrap().last().unwrap()[0];
        let mut config = read_config(env, false, Path::new("/tmp")).unwrap();
        config.local_token = row["gateway_config"]["localToken"]
            .as_str()
            .map(str::to_owned);
        config.auth_mode = if number >= 9 {
            autorouter_core::config::AuthMode::Subscription
        } else {
            autorouter_core::config::AuthMode::ApiKey
        };
        config.upstream = format!("http://{}", upstream.address);
        if let Some(jev) = &jev {
            config.jev_endpoint = format!("http://{}/v1/systemone", jev.address);
        }
        let client = Arc::new(NativeHttpClient::new().unwrap());
        let transport = Arc::new(Transport {
            native: client.clone(),
            mock: row["mocked_fetch"].as_array().unwrap().first().cloned(),
            mocked: Mutex::new(Vec::new()),
            allowed_authorities: std::iter::once(upstream.address.to_string())
                .chain(jev.as_ref().map(|peer| peer.address.to_string()))
                .collect(),
        });
        let logs = Events::default();
        let statuses = Events::default();
        let sinks = EventSinks {
            log: Some(sink(&logs)),
            status: Some(sink(&statuses)),
            ..Default::default()
        };
        let mut stub = None;
        let handle = if number == 8 {
            Gateway::new(config, transport.clone(), sinks)
                .unwrap()
                .listen(0)
                .await
                .unwrap()
        } else if number == 9 {
            let mut router_config =
                read_config(&row["config_arguments"][0][0], false, Path::new("/tmp")).unwrap();
            assert!(router_config.jev_key.is_none());
            // JS receives the server-owned count callback. Rust's Router owns
            // that counter, so bind only its upstream endpoint to the same peer.
            router_config.upstream = config.upstream.clone();
            let router = Arc::new(Router::new(transport.clone(), router_config));
            Gateway::with_router(config, transport.clone(), router, sinks)
                .unwrap()
                .listen(0)
                .await
                .unwrap()
        } else {
            let router = Arc::new(Stub::default());
            stub = Some(router.clone());
            Gateway::with_router(config, transport.clone(), router, sinks)
                .unwrap()
                .listen(0)
                .await
                .unwrap()
        };
        Self {
            client,
            transport,
            handle: Some(handle),
            upstream: Some(upstream),
            jev,
            stub,
            gate,
            logs,
            statuses,
        }
    }
    async fn send(&self, row: &Value, streaming: bool) -> Result<Reply, String> {
        let path = url::Url::parse(row["url"].as_str().unwrap()).unwrap();
        let suffix = format!(
            "{}{}",
            path.path(),
            path.query()
                .map(|value| format!("?{value}"))
                .unwrap_or_default()
        );
        let mut request = Request::builder()
            .method(row["method"].as_str().unwrap())
            .uri(format!(
                "http://{}{suffix}",
                self.handle.as_ref().unwrap().address
            ));
        *request.headers_mut().unwrap() = headers(&row["headers"]);
        let request = request
            .body(Full::new(Bytes::from(
                row["body"].as_str().unwrap().to_owned(),
            )))
            .unwrap();
        let response = self
            .client
            .request(request)
            .await
            .map_err(|error| error.to_string())?;
        let (parts, mut body) = response.into_parts();
        let (first, first_while_held) = if streaming {
            let gate = self.gate.as_ref().unwrap();
            gate.wait_first_polled().await?;
            let first = body
                .frame()
                .await
                .ok_or("first frame absent")?
                .map_err(|error| error.to_string())?
                .into_data()
                .map_err(|_| "first frame was trailers")?;
            let held = gate.first_was_polled() && !gate.final_was_polled() && !gate.released();
            gate.release();
            (Some(first.to_vec()), held)
        } else {
            (None, false)
        };
        let rest = Limited::new(body, LIMIT)
            .collect()
            .await
            .map_err(|error| error.to_string())?
            .to_bytes();
        Ok(Reply {
            status: parts.status.as_u16(),
            headers: parts.headers,
            body: rest.to_vec(),
            first,
            first_while_held,
        })
    }
    async fn finish(mut self) -> Result<Observations, String> {
        let closed = tokio::time::timeout(BOUND, self.handle.take().unwrap().close()).await;
        let observed = Observations {
            upstream: self.upstream.as_ref().unwrap().observations(),
            jev: self
                .jev
                .as_ref()
                .map(Peer::observations)
                .unwrap_or_default(),
            mocked: self.transport.mocked.lock().unwrap().clone(),
            logs: self.logs.lock().unwrap().clone(),
            statuses: self.statuses.lock().unwrap().clone(),
            stub_calls: self
                .stub
                .as_ref()
                .map(|stub| stub.calls.load(Ordering::SeqCst))
                .unwrap_or(0),
        };
        let peer_result = self.upstream.take().unwrap().close().await;
        let jev_result = if let Some(jev) = self.jev.take() {
            jev.close().await
        } else {
            Ok(())
        };
        closed.map_err(|_| "gateway cleanup timed out")?;
        peer_result?;
        jev_result?;
        Ok(observed)
    }
}
async fn run(number: usize, row: &Value) -> (Vec<Reply>, Observations) {
    let fixture = Fixture::new(number, row).await;
    let result = tokio::time::timeout(BOUND, async {
        let mut responses = Vec::new();
        for request in row["downstream"].as_array().unwrap() {
            responses.push(fixture.send(request, number == 1).await?);
        }
        Ok::<_, String>(responses)
    })
    .await;
    let observations = fixture.finish().await;
    (
        result
            .expect("bounded original schedule")
            .expect("HTTP schedule"),
        observations.expect("owned cleanup and sticky peer checks"),
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
        let capture: Value = serde_json::from_str(CAPTURE).unwrap();
        let definition = capture["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|definition| definition["number"] == self.number)
            .unwrap();
        for (index, site) in definition["assertions"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            assert_eq!(
                self.counts.get(&(index + 1)).copied().unwrap_or(0),
                site["executions"].as_u64().unwrap() as usize,
                "server#{}:assert-{} execution count",
                self.number,
                index + 1
            );
        }
        assert_eq!(
            self.counts.len(),
            definition["assertions"].as_array().unwrap().len()
        );
    }
}
fn header_is(headers: &HeaderMap, key: &str, value: Option<&str>) -> bool {
    headers.get(key).and_then(|value| value.to_str().ok()) == value
}
fn private_absent(value: &Value, needle: &str) -> bool {
    !value.to_string().contains(needle)
}
fn status<'a>(events: &'a [Value], name: &'a str) -> impl Iterator<Item = &'a Value> {
    events.iter().filter(move |event| event["event"] == name)
}

#[tokio::test]
async fn original_streams_before_completion_preserving_payload_headers_query_and_privacy() {
    let row = case(1);
    let (replies, seen) = run(1, &row).await;
    let reply = &replies[0];
    let peer = &seen.upstream[0];
    let mut a = Assertions::new(1);
    assert_eq!(seen.upstream.len(), 1);
    assert!(seen.jev.is_empty() && seen.mocked.is_empty());
    a.check(1, peer.path == "/v1/messages?beta=true");
    a.check(
        2,
        header_is(&peer.headers, "x-api-key", Some("upstream-secret")),
    );
    a.check(3, header_is(&peer.headers, "authorization", None));
    a.check(
        4,
        header_is(&peer.headers, "anthropic-beta", Some("test-beta")),
    );
    a.check(
        5,
        header_is(&peer.headers, "anthropic-version", Some("2023-06-01")),
    );
    a.check(
        6,
        body_json(peer)
            == serde_json::from_str::<Value>(
                expected_peer(&row, "upstream")[0]["request_body_utf8"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap(),
    );
    a.check(7, header_is(&reply.headers, "request-id", Some("test-id")));
    let writes = expected_peer(&row, "upstream")[0]["response_writes"]
        .as_array()
        .unwrap()
        .clone();
    assert!(
        reply.first_while_held,
        "positive downstream first receipt precedes final release"
    );
    a.check(
        8,
        reply.first.as_deref() == Some(writes[0]["utf8"].as_str().unwrap().as_bytes()),
    );
    a.check(
        9,
        reply.body == writes[1]["utf8"].as_str().unwrap().as_bytes(),
    );
    a.check(10, seen.stub_calls == 1);
    assert!(
        !seen.logs.is_empty(),
        "privacy checks observe actual log rows"
    );
    a.check(11, private_absent(&json!(seen.logs), "Fix a typo"));
    a.check(12, private_absent(&json!(seen.logs), "secret"));
    a.finish();
}

#[tokio::test]
async fn original_real_jev_three_requests_two_counts_and_capacity_outcomes() {
    let row = case(8);
    let (replies, seen) = run(8, &row).await;
    let mut a = Assertions::new(8);
    for call in &seen.jev {
        let payload = body_json(call);
        a.check(1, call.path == "/v1/systemone");
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
            payload["state"].to_string().encode_utf16().count() <= 12000,
        );
        a.check(4, payload["state"]["original_task"] == "Fix a typo");
        a.check(
            5,
            payload["questions"]["tier"]["criteria"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                == ["haiku", "sonnet", "opus"],
        );
    }
    let expected = expected_peer(&row, "upstream");
    for (call, expected) in seen.upstream.iter().zip(&expected) {
        let parsed = body_json(call);
        let wanted: Value =
            serde_json::from_str(expected["request_body_utf8"].as_str().unwrap()).unwrap();
        if call.path == "/v1/messages/count_tokens" {
            a.check(
                6,
                header_is(&call.headers, "x-api-key", Some("upstream-secret")),
            );
            a.check(7, header_is(&call.headers, "x-autorouter-token", None));
            a.check(8, parsed["model"] == "claude-haiku-4-5-20251001");
            a.check(9, parsed["tools"] == wanted["tools"]);
            a.check(10, parsed.get("max_tokens").is_none());
        } else {
            a.check(11, parsed == wanted);
        }
    }
    for (reply, model) in replies.iter().zip([
        "claude-haiku-4-5-20251001",
        "claude-haiku-4-5-20251001",
        "claude-sonnet-5",
    ]) {
        a.check(12, reply.status == 200);
        a.check(
            13,
            serde_json::from_slice::<Value>(&reply.body).unwrap()["model"] == model,
        );
    }
    a.check(14, seen.jev.len() == 3);
    a.check(
        15,
        seen.upstream
            .iter()
            .filter(|call| call.path == "/v1/messages/count_tokens")
            .count()
            == 2,
    );
    assert_eq!(seen.upstream.len(), 5);
    assert_eq!(replies.len(), 3);
    let routes: Vec<_> = status(&seen.statuses, "route").collect();
    assert_eq!(routes.len(), 3);
    a.check(16, routes[1]["model"] == "claude-haiku-4-5-20251001");
    a.check(17, routes[1]["context_check"] == "within_budget");
    a.check(
        18,
        routes[1]["counted_input_tokens"].as_f64() == Some(54481.0),
    );
    a.check(19, routes.last().unwrap()["reason"] == "context_capacity");
    a.check(
        20,
        status(&seen.statuses, "upstream_model").last().unwrap()["model"] == "claude-sonnet-5",
    );
    assert!(!seen.logs.is_empty());
    a.check(
        21,
        private_absent(
            &json!({"logs":seen.logs,"statuses":seen.statuses}),
            "Synthetic tool schema context",
        ),
    );
    a.finish();
}

#[tokio::test]
async fn original_subscription_automatic_count_uses_current_oauth_and_complete_system() {
    let row = case(9);
    let (replies, seen) = run(9, &row).await;
    let mut a = Assertions::new(9);
    let requested: Value =
        serde_json::from_str(row["downstream"][0]["body"].as_str().unwrap()).unwrap();
    for call in &seen.upstream {
        a.check(
            1,
            header_is(
                &call.headers,
                "authorization",
                Some("Bearer fake-subscription-token"),
            ),
        );
        a.check(
            2,
            header_is(
                &call.headers,
                "anthropic-beta",
                Some("oauth-2025-04-20,future-capability"),
            ),
        );
        a.check(3, header_is(&call.headers, "x-api-key", None));
        a.check(4, header_is(&call.headers, "x-autorouter-token", None));
        a.check(5, header_is(&call.headers, "cookie", None));
        let payload = body_json(call);
        a.check(6, payload["model"] == requested["model"]);
        a.check(7, payload["system"] == requested["system"]);
    }
    a.check(8, replies[0].status == 200);
    a.check(
        9,
        serde_json::from_slice::<Value>(&replies[0].body).unwrap()["model"] == requested["model"],
    );
    a.check(
        10,
        seen.upstream
            .iter()
            .map(|call| call.path.as_str())
            .collect::<Vec<_>>()
            == [
                "/v1/messages/count_tokens?beta=true",
                "/v1/messages?beta=true",
            ],
    );
    a.check(
        11,
        status(&seen.statuses, "route").next().unwrap()["counted_input_tokens"].as_f64()
            == Some(54000.0),
    );
    assert!(!seen.logs.is_empty() && !seen.statuses.is_empty());
    for secret in [
        "fake-subscription-token",
        "PRIVATE_COOKIE",
        "local-test-token-123456789",
    ] {
        a.check(
            12,
            private_absent(&json!({"logs":seen.logs,"statuses":seen.statuses}), secret),
        );
    }
    assert_eq!(seen.mocked.len(), 1);
    assert!(seen.jev.is_empty());
    let mock = &row["mocked_fetch"][0];
    assert_eq!(seen.mocked[0].path, mock["url"]);
    assert_eq!(seen.mocked[0].method, "POST");
    assert_eq!(seen.mocked[0].headers, headers(&mock["headers"]));
    assert_eq!(
        body_json(&seen.mocked[0]),
        serde_json::from_str::<Value>(mock["body"].as_str().unwrap()).unwrap()
    );
    assert_eq!(
        seen.mocked[0].headers["authorization"], "Bearer undefined",
        "No Jev key added to the original fixture"
    );
    a.finish();
}

#[tokio::test]
async fn original_subscription_refresh_preserves_sse_usage_header_and_credential_isolation() {
    let row = case(10);
    let (replies, seen) = run(10, &row).await;
    let mut a = Assertions::new(10);
    let expected = expected_peer(&row, "upstream");
    for (call, expected) in seen.upstream.iter().zip(&expected) {
        a.check(
            1,
            header_is(
                &call.headers,
                "anthropic-beta",
                Some("oauth-2025-04-20,future-capability"),
            ),
        );
        a.check(
            2,
            header_is(&call.headers, "anthropic-version", Some("2023-06-01")),
        );
        a.check(3, header_is(&call.headers, "x-api-key", None));
        a.check(4, header_is(&call.headers, "x-autorouter-token", None));
        a.check(5, header_is(&call.headers, "cookie", None));
        a.check(
            6,
            body_json(call)
                == serde_json::from_str::<Value>(expected["request_body_utf8"].as_str().unwrap())
                    .unwrap(),
        );
    }
    for (reply, expected) in replies.iter().zip(&expected) {
        a.check(7, reply.status == 200);
        a.check(
            8,
            header_is(
                &reply.headers,
                "anthropic-ratelimit-unified-5h-utilization",
                Some("0.3"),
            ),
        );
        a.check(9, reply.body == response_bytes(expected));
    }
    a.check(
        10,
        seen.upstream
            .iter()
            .map(|call| call.headers["authorization"].to_str().unwrap())
            .collect::<Vec<_>>()
            == [
                "Bearer fake-subscription-token",
                "Bearer fake-refreshed-token",
            ],
    );
    a.check(11, seen.stub_calls == 2);
    assert_eq!(replies.len(), 2);
    assert!(seen.jev.is_empty() && seen.mocked.is_empty());
    assert!(!seen.logs.is_empty());
    for secret in [
        "fake-subscription-token",
        "fake-refreshed-token",
        "local-test-token-123456789",
        "upstream-secret",
    ] {
        a.check(12, private_absent(&json!(seen.logs), secret));
    }
    a.finish();
}

#[test]
fn complete_transcript_comparator_rejects_wrong_body_path_stale_oauth_and_local_leaks() {
    let row = case(10);
    let expected = expected_peer(&row, "upstream");
    let good = Observation {
        method: "POST".into(),
        path: expected[1]["path"].as_str().unwrap().into(),
        headers: headers(&expected[1]["headers"]),
        body: expected[1]["request_body_utf8"]
            .as_str()
            .unwrap()
            .as_bytes()
            .to_vec(),
    };
    assert!(compare(&good, &expected[1]).is_ok());
    let mut bad = good.clone();
    bad.headers.insert(
        "authorization",
        "Bearer fake-subscription-token".parse().unwrap(),
    );
    assert!(
        compare(&bad, &expected[1])
            .unwrap_err()
            .contains("authorization")
    );
    let mut bad = good.clone();
    bad.headers
        .insert("x-autorouter-token", "leaked-local".parse().unwrap());
    assert!(
        compare(&bad, &expected[1])
            .unwrap_err()
            .contains("x-autorouter-token")
    );
    let mut bad = good.clone();
    bad.body = b"{}".to_vec();
    assert!(compare(&bad, &expected[1]).unwrap_err().contains("payload"));
    let mut bad = good;
    bad.path = "/v1/messages".into();
    assert!(
        compare(&bad, &expected[1])
            .unwrap_err()
            .contains("method/path")
    );
}

#[tokio::test]
async fn incorrect_count_cannot_pass_full_upstream_payload_transcript() {
    let mut row = case(8);
    let upstream = row["peers"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|peer| peer["role"] == "upstream")
        .unwrap();
    assert_eq!(upstream["requests"][1]["path"], "/v1/messages/count_tokens");
    // Change the actual finite peer answer, leaving the original expected
    // inference payload and policy assertions untouched.
    upstream["requests"][1]["response_writes"][0]["utf8"] = json!("{\"input_tokens\":227338}");
    let fixture = Fixture::new(8, &row).await;
    let result = tokio::time::timeout(BOUND, async {
        fixture.send(&row["downstream"][0], false).await?;
        fixture.send(&row["downstream"][1], false).await
    })
    .await;
    let cleanup = fixture.finish().await;
    result
        .expect("negative control schedule bounded")
        .expect("negative HTTP exchange completed");
    assert!(
        cleanup
            .err()
            .unwrap()
            .contains("complete JSON payload transcript mismatch"),
        "wrong count must change the actual inference payload and fail its complete comparison"
    );
}

#[tokio::test]
async fn held_buffered_reply_is_rejected_by_original_first_read_and_release_boundary() {
    let original = case(1);
    let mut row = original.clone();
    let writes = &mut row["peers"][0]["requests"][0]["response_writes"];
    let first = writes[0]["utf8"].as_str().unwrap().to_owned();
    let all = first.clone() + writes[1]["utf8"].as_str().unwrap();
    writes[0]["utf8"] = json!("");
    writes[1]["utf8"] = json!(all);
    let fixture = Fixture::new(1, &row).await;
    let gate = fixture.gate.as_ref().unwrap().clone();
    let result = tokio::time::timeout(BOUND, async {
        let sending = fixture.send(&row["downstream"][0], true); tokio::pin!(sending);
        tokio::select! {
            result = &mut sending => return Err(format!("buffered response unexpectedly finished before release: {}", result.is_ok())),
            first = gate.wait_first_polled() => first?,
        }
        assert!(!gate.released() && !gate.final_was_polled());
        gate.release(); sending.await
    }).await;
    let cleanup = fixture.finish().await;
    let reply = result
        .expect("buffered negative control bounded")
        .expect("buffered negative control response");
    cleanup.expect("negative control owned cleanup");
    assert_ne!(reply.first.as_deref(), Some(first.as_bytes()));
    assert!(
        !reply.first_while_held,
        "cannot label post-release bytes as early streaming"
    );
}

#[tokio::test]
async fn unregistered_owned_listener_is_rejected_without_network_io() {
    let row = case(1);
    let fixture = Fixture::new(1, &row).await;
    // The negative endpoint is itself owned by this test; a broken boundary
    // cannot touch an unrelated service. It is deliberately not registered
    // as an upstream/evaluator peer and must never receive a TCP connection.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let result = tokio::time::timeout(
        BOUND,
        fixture.transport.request(
            Request::post(format!(
                "http://{}/v1/systemone",
                listener.local_addr().unwrap()
            ))
            .body(Full::new(Bytes::from_static(b"{}")))
            .unwrap(),
        ),
    )
    .await;
    let unexpected_connection = listener.accept();
    drop(listener);
    let cleaned = fixture.finish().await;
    assert!(matches!(result, Ok(Err(HttpError::InvalidRequest))));
    assert!(
        matches!(unexpected_connection, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    let observed = cleaned.expect("endpoint rejection owned cleanup");
    assert!(observed.upstream.is_empty() && observed.jev.is_empty() && observed.mocked.is_empty());
}
