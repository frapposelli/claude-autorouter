//! Exact original routing compatibility #2–#5: all73 assertions and actual HTTP.
#[allow(dead_code)]
#[path = "support/server_streaming_routing.rs"]
mod peer;
use autorouter_core::{
    config::read_config,
    js_json::{JsDocument, JsNode},
    router::RouteOptions,
};
use autorouter_runtime::{
    evaluator::EvaluationError,
    http_client::{HttpError, HttpTransport, NativeHttpClient},
    response_observer::CompletionEvidence,
    router::Router,
    server::{Gateway, GatewayHandle, GatewayRouter},
    server_events::{EventSink, EventSinks},
    token_counter::TokenCounter,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{HeaderMap, Request, Response};
use peer::{Observation, Peer, ResponsePlan};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const CORPUS: &str = include_str!("../../../parity/cases/routing-compatibility-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../parity/cases/routing-compatibility-contracts.capture.json");
const LIMIT: usize = 2 * 1024 * 1024;
const BOUND: Duration = Duration::from_secs(5);
type Rows = Arc<Mutex<Vec<Value>>>;
fn case(number: usize) -> Value {
    assert_eq!(
        format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
        "a326874632344f59ccd01eb05cf361034b0abc31e594d1d8fa02b3a76fd9e098"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(CAPTURE.as_bytes())),
        "df1848d2cd186af13aea9a49140520db7f21c427fcaf3b4af0d90fc7ffc4e0ae"
    );
    let capture: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(capture["static_assertions"], 22);
    assert_eq!(capture["expanded_assertions"], 73);
    assert_eq!(capture["callbacks_executed"], 4);
    CORPUS
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|row| row["source_test"] == format!("test/routing-compatibility.test.mjs#{number}"))
        .unwrap()
}
fn parse(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
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
fn decode(value: &Value) -> Vec<u8> {
    let text = value.as_str().unwrap();
    assert!(text.len() < LIMIT * 2);
    openssl::base64::decode_block(text).unwrap()
}
fn response_bytes(row: &Value) -> Vec<u8> {
    row["response_writes"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|write| decode(&write["base64"]))
        .collect()
}
fn header(map: &HeaderMap, name: &str) -> Option<String> {
    map.get(name)
        .map(|value| value.to_str().unwrap().to_owned())
}
fn recorded_headers(map: &HeaderMap) -> Value {
    json!(
        map.iter()
            .map(|(key, value)| (key.as_str().to_owned(), value.to_str().unwrap().to_owned()))
            .collect::<BTreeMap<_, _>>()
    )
}
fn source_provider_rows(row: &Value, index: usize) -> Vec<Value> {
    row["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|peer| peer["fixture_index"] == index)
        .unwrap()["requests"]
        .as_array()
        .unwrap()
        .clone()
}
fn compare_provider(actual: &Observation, wanted: &Value) -> Result<(), String> {
    if actual.method != wanted["method"]
        || actual.path != wanted["path"]
        || actual.body != wanted["request_body_utf8"].as_str().unwrap().as_bytes()
    {
        return Err("complete provider method/path/request bytes differ".into());
    }
    for name in [
        "authorization",
        "x-api-key",
        "x-autorouter-token",
        "cookie",
        "content-type",
        "anthropic-beta",
        "anthropic-version",
        "accept-encoding",
        "x-claude-code-session-id",
        "x-claude-code-agent-id",
        "x-claude-code-prompt-id",
        "x-claude-code-request-class",
    ] {
        if header(&actual.headers, name).as_deref() != wanted["headers"][name].as_str() {
            return Err(format!("provider semantic header differs: {name}"));
        }
    }
    Ok(())
}
struct Script {
    expected: Vec<Value>,
    calls: Rows,
}
impl HttpTransport for Script {
    type ResponseBody = Full<Bytes>;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        assert!(bytes.len() < LIMIT);
        let mut calls = self.calls.lock().unwrap();
        let index = calls.len();
        assert!(
            index < self.expected.len(),
            "unexpected evaluator/count call"
        );
        let wanted = &self.expected[index];
        assert_eq!(parts.uri.to_string(), wanted["url"]);
        assert_eq!(parts.method.as_str(), wanted["method"]);
        assert_eq!(parts.headers, headers(&wanted["headers"]));
        assert_eq!(
            bytes.as_ref(),
            wanted["body"].as_str().unwrap().as_bytes(),
            "complete evaluator/count wire bytes"
        );
        calls.push(json!({"url":parts.uri.to_string(),"method":parts.method.as_str(),"headers":recorded_headers(&parts.headers),"body_utf8":std::str::from_utf8(&bytes).unwrap()}));
        let response = wanted["reads"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|read| read["kind"] == "read" && read["done"] == false)
            .flat_map(|read| decode(&read["base64"]))
            .collect::<Vec<_>>();
        let mut result = Response::builder()
            .status(wanted["status"].as_u64().unwrap() as u16)
            .body(Full::new(Bytes::from(response)))
            .unwrap();
        *result.headers_mut() = headers(&wanted["response_headers"]);
        Ok(result)
    }
}
struct RealRouter {
    inner: Router<Script>,
    script: Arc<Script>,
    routes: Rows,
    completions: Rows,
}
impl GatewayRouter for RealRouter {
    async fn route(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> Result<Value, EvaluationError> {
        let before = document.stringify();
        let decision = self
            .inner
            .route(document.clone(), options, headers, cancellation, search)
            .await?;
        assert_eq!(
            document.stringify(),
            before,
            "authoritative request remains immutable"
        );
        let mut rows = self.routes.lock().unwrap();
        assert!(rows.len() < 16);
        rows.push(json!({"document_utf8":before,"decision":decision}));
        Ok(decision)
    }
    fn complete(&self, id: &str, evidence: &Value) -> bool {
        let accepted = self.inner.complete(id, evidence);
        self.completions
            .lock()
            .unwrap()
            .push(json!({"request_id":id,"evidence":evidence,"accepted":accepted}));
        accepted
    }
    fn complete_exact(&self, id: &str, evidence: &CompletionEvidence) -> bool {
        let accepted = self.inner.complete_exact(id, evidence);
        self.completions
            .lock()
            .unwrap()
            .push(json!({"request_id":id,"evidence":evidence,"accepted":accepted}));
        accepted
    }
    fn shutdown(&self) {
        self.inner.shutdown();
    }
    async fn close(&self) {
        self.inner.close().await;
    }
}
struct Reply {
    status: u16,
    headers: HeaderMap,
    bytes: Vec<u8>,
}
struct Fixture {
    client: NativeHttpClient,
    router: Arc<RealRouter>,
    gateway: Option<GatewayHandle>,
    peer: Option<Peer>,
    logs: Rows,
}
impl Fixture {
    async fn new(number: usize, row: &Value, index: usize) -> Self {
        let provider = source_provider_rows(row, index);
        let peer = Peer::start(move |actual, n| {
            let wanted = provider.get(n).ok_or("unexpected provider call")?;
            compare_provider(actual, wanted)?;
            Ok(ResponsePlan::bytes(
                wanted["response_status"].as_u64().unwrap() as u16,
                headers(&wanted["response_headers"]),
                response_bytes(wanted),
            ))
        })
        .await
        .unwrap();
        let mut cfg =
            read_config(&row["config_arguments"][index][0], false, Path::new("/tmp")).unwrap();
        cfg.local_token = row["gateway_configs"][index]["localToken"]
            .as_str()
            .map(str::to_owned);
        cfg.upstream = format!("http://{}", peer.address);
        let expected = if number == 2 {
            vec![row["mocked_fetch"][index].clone()]
        } else {
            row["mocked_fetch"].as_array().unwrap().clone()
        };
        let script = Arc::new(Script {
            expected,
            calls: Rows::default(),
        });
        let router = Arc::new(RealRouter {
            inner: Router::new(script.clone(), cfg.clone()),
            script,
            routes: Rows::default(),
            completions: Rows::default(),
        });
        let logs = Rows::default();
        let observed = logs.clone();
        let sink: EventSink = Arc::new(move |event| {
            let mut rows = observed.lock().unwrap();
            assert!(rows.len() < 128);
            rows.push(event.to_serde_observation_lossy());
        });
        let client = NativeHttpClient::new().unwrap();
        let gateway = Gateway::with_router(
            cfg,
            Arc::new(NativeHttpClient::new().unwrap()),
            router.clone(),
            EventSinks {
                log: Some(sink),
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
            logs,
        }
    }
    async fn send(&self, request: &Value) -> Reply {
        let url = url::Url::parse(request["url"].as_str().unwrap()).unwrap();
        let address = self.gateway.as_ref().unwrap().address;
        let path = format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        let mut outgoing = Request::post(format!("http://{address}{path}"))
            .body(Full::new(Bytes::from(
                request["body"].as_str().unwrap().as_bytes().to_vec(),
            )))
            .unwrap();
        *outgoing.headers_mut() = headers(&request["headers"]);
        let response = tokio::time::timeout(BOUND, self.client.request(outgoing))
            .await
            .unwrap()
            .unwrap();
        let (parts, body) = response.into_parts();
        let bytes = tokio::time::timeout(BOUND, Limited::new(body, LIMIT).collect())
            .await
            .unwrap()
            .unwrap()
            .to_bytes()
            .to_vec();
        Reply {
            status: parts.status.as_u16(),
            headers: parts.headers,
            bytes,
        }
    }
    fn provider(&self) -> Vec<Observation> {
        self.peer.as_ref().unwrap().observations()
    }
    fn calls(&self) -> Vec<Value> {
        self.router.script.calls.lock().unwrap().clone()
    }
    fn evaluations(&self) -> usize {
        self.calls()
            .iter()
            .filter(|call| call["url"] == "https://api.typesafe.ai/v1/systemone")
            .count()
    }
    fn counts(&self) -> usize {
        self.calls()
            .iter()
            .filter(|call| call["url"].as_str().unwrap().ends_with("/count_tokens"))
            .count()
            + self
                .provider()
                .iter()
                .filter(|call| call.path.starts_with("/v1/messages/count_tokens"))
                .count()
    }
    async fn finish(mut self, number: usize, index: usize, replies: &[Reply]) -> Value {
        // Client EOF can race the writer's final flush witness. Read the actual
        // state until retirement; never manufacture a cleanup completion call.
        let before = tokio::time::timeout(BOUND, async {
            loop {
                let state = self.router.inner.continuity_snapshot();
                if state.attempts == 0 {
                    break state;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("attempts retire before close");
        tokio::time::timeout(BOUND, self.gateway.take().unwrap().close())
            .await
            .unwrap();
        let provider = self.peer.as_ref().unwrap().observations();
        self.peer.take().unwrap().close().await.unwrap();
        let report = json!({"number":number,"fixture_index":index,"provider":provider.iter().map(|request|json!({"method":request.method,"path":request.path,"headers":recorded_headers(&request.headers),"body_base64":openssl::base64::encode_block(&request.body)})).collect::<Vec<_>>(),"replies":replies.iter().map(|reply|json!({"status":reply.status,"headers":recorded_headers(&reply.headers),"body_base64":openssl::base64::encode_block(&reply.bytes)})).collect::<Vec<_>>(),"logs":self.logs.lock().unwrap().clone(),"routes":self.router.routes.lock().unwrap().clone(),"completions":self.router.completions.lock().unwrap().clone(),"fetch_calls":self.calls(),"continuity_before_close":{"records":before.records,"attempts":before.attempts,"aliases":before.aliases}});
        save(number, index, &report);
        report
    }
}
fn save(number: usize, index: usize, value: &Value) {
    if let Some(directory) = std::env::var_os("AUTOROUTER_ROUTING_COMPAT_REPORT_DIR") {
        std::fs::write(
            Path::new(&directory).join(format!("native-{number}-{index}.json")),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}
struct Assertions {
    number: usize,
    calls: Vec<usize>,
}
impl Assertions {
    fn new(number: usize) -> Self {
        Self {
            number,
            calls: Vec::new(),
        }
    }
    fn check(&mut self, site: usize, result: bool) {
        self.calls.push(site);
        assert!(
            result,
            "routing-compatibility#{}:assert-{site}",
            self.number
        );
    }
    fn finish(self) {
        let metadata: Value = serde_json::from_str(CAPTURE).unwrap();
        let original = metadata["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["number"] == self.number)
            .unwrap();
        let mut counts = BTreeMap::new();
        for site in &self.calls {
            *counts.entry(*site).or_insert(0usize) += 1;
        }
        for (site, source) in original["assertions"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            assert_eq!(
                counts.get(&(site + 1)).copied().unwrap_or(0),
                source["executions"].as_u64().unwrap() as usize
            );
        }
        assert_eq!(
            counts.len(),
            original["assertions"].as_array().unwrap().len()
        );
        save(
            self.number,
            99,
            &json!({"original_assertion_execution_order":self.calls}),
        );
    }
}
fn source_logs(row: &Value, index: usize) -> Vec<Value> {
    row["log_calls"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["fixture_index"] == index)
        .map(|r| r["value"].clone())
        .collect()
}
fn stable_log(value: &Value, source: bool) -> Value {
    match value {
        Value::Array(values) => {
            Value::Array(values.iter().map(|v| stable_log(v, source)).collect())
        }
        Value::Object(values) => {
            let mut output = serde_json::Map::new();
            for (key, value) in values {
                if source && value == &json!({"$js_type":"undefined"}) {
                    assert_eq!(
                        key, "blocks",
                        "only explicit optional shape-metadata field has native omission"
                    );
                    continue;
                }
                if [
                    "latency_ms",
                    "evaluation_latency_ms",
                    "routing_latency_ms",
                    "decision_latency_ms",
                    "first_response_ms",
                    "total_latency_ms",
                ]
                .contains(&key.as_str())
                {
                    let number = value.as_f64().unwrap();
                    assert!(number.is_finite() && number >= 0.0);
                    output.insert(key.clone(), json!(0));
                } else {
                    output.insert(key.clone(), stable_log(value, source));
                }
            }
            Value::Object(output)
        }
        _ => value.clone(),
    }
}
fn compare_logs(actual: &[Value], row: &Value, index: usize) {
    let source = source_logs(row, index);
    assert_eq!(
        actual.len(),
        source.len(),
        "complete diagnostic event count"
    );
    let actual = stable_log(&json!(actual), false);
    let wanted = stable_log(&json!(source), true);
    assert!(
        diagnostic_equal(&wanted, &actual),
        "all safe diagnostic fields, with explicit optional-field and exact numeric representation adaptations:\nactual={actual:?}\nwanted={wanted:?}"
    );
}
// Exact numeric_equal semantics from xtask/src/parity.rs. This only compares
// parsed diagnostic values; raw JSON spelling and provider bytes stay intact.
fn numeric_equal(a: &serde_json::Number, b: &serde_json::Number) -> bool {
    if a == b {
        return true;
    }
    if !a.is_f64() && !b.is_f64() {
        return false;
    }
    let (Some(left), Some(right)) = (a.as_f64(), b.as_f64()) else {
        return false;
    };
    if left != right {
        return false;
    }
    if a.is_f64() && b.is_f64() {
        return true;
    }
    let integer = if a.is_f64() { b } else { a };
    if let Some(value) = integer.as_u64() {
        (0.0..18_446_744_073_709_551_616.0).contains(&left) && left as u64 == value
    } else if let Some(value) = integer.as_i64() {
        (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&left)
            && left as i64 == value
    } else {
        false
    }
}
fn diagnostic_equal(wanted: &Value, actual: &Value) -> bool {
    match (wanted, actual) {
        (Value::Number(a), Value::Number(b)) => numeric_equal(a, b),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| diagnostic_equal(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter().all(|(key, value)| {
                    b.get(key)
                        .is_some_and(|other| diagnostic_equal(value, other))
                })
        }
        _ => wanted == actual,
    }
}
#[test]
fn supplemental_diagnostic_number_comparison_rejects_lossy_and_saturating_integers() {
    for (a, b, equal) in [
        ("200", "200.0", true),
        ("9007199254740992", "9007199254740993", false),
        ("9007199254740993", "9007199254740992.0", false),
        ("18446744073709551615", "18446744073709551616.0", false),
        ("-9223372036854775808", "-9223372036854775808.0", true),
        ("9223372036854775807", "9223372036854775808.0", false),
    ] {
        assert_eq!(
            diagnostic_equal(&parse(a.as_bytes()), &parse(b.as_bytes())),
            equal
        );
    }
    assert!(!diagnostic_equal(
        &json!({"status":200}),
        &json!({"status":200,"extra":null})
    ));
    assert!(!diagnostic_equal(&json!([200, 201]), &json!([201, 200])));
}

#[tokio::test]
async fn original_02_forced_tool_preserves_three_profiles_bodies_headers_and_zero_counts() {
    let row = case(2);
    let mut a = Assertions::new(2);
    for index in 0..3 {
        let f = Fixture::new(2, &row, index).await;
        let wanted = &row["downstream"][index];
        let reply = f.send(wanted).await;
        let body = parse(wanted["body"].as_str().unwrap().as_bytes());
        let provider = f.provider();
        a.check(1, reply.status == 200);
        a.check(2, parse(&reply.bytes)["model"] == body["model"]);
        a.check(
            3,
            header(&reply.headers, "request-id").as_deref() == Some("synthetic-upstream-id"),
        );
        a.check(4, parse(&provider[0].body) == body);
        a.check(5, provider[0].path == "/v1/messages?beta=true");
        a.check(
            6,
            header(&provider[0].headers, "anthropic-beta").as_deref()
                == Some("synthetic-future-beta"),
        );
        a.check(7, f.evaluations() == 1);
        a.check(8, f.counts() == 0);
        assert_eq!(
            reply.bytes,
            response_bytes(&source_provider_rows(&row, index)[0]),
            "provider bytes forwarded unchanged"
        );
        let seen = f.finish(2, index, &[reply]).await;
        compare_logs(seen["logs"].as_array().unwrap(), &row, index);
    }
    a.finish();
}
#[tokio::test]
async fn original_03_five_malformed_shapes_reject_both_endpoints_before_any_work_or_leak() {
    let row = case(3);
    let f = Fixture::new(3, &row, 0).await;
    let mut a = Assertions::new(3);
    let mut replies = Vec::new();
    for request in row["downstream"].as_array().unwrap() {
        let reply = f.send(request).await;
        let text = std::str::from_utf8(&reply.bytes).unwrap();
        a.check(1, reply.status == 400);
        a.check(2, text.contains("Invalid Messages API request shape"));
        a.check(3, !text.contains("PRIVATE_TEST_VALUE"));
        let source = request["reads"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["kind"] == "text")
            .unwrap()["value"]
            .as_str()
            .unwrap();
        assert_eq!(
            parse(&reply.bytes),
            parse(source.as_bytes()),
            "full local JSON error category/message; raw serializer bytes retained separately"
        );
        replies.push(reply);
    }
    a.check(4, f.evaluations() == 0);
    a.check(5, f.counts() == 0);
    a.check(6, f.provider().is_empty());
    a.check(
        7,
        !serde_json::to_string(&*f.logs.lock().unwrap())
            .unwrap()
            .contains("PRIVATE_TEST_VALUE"),
    );
    assert!(
        f.router.routes.lock().unwrap().is_empty(),
        "no malformed input reaches routing"
    );
    let seen = f.finish(3, 0, &replies).await;
    compare_logs(seen["logs"].as_array().unwrap(), &row, 0);
    a.finish();
}
#[tokio::test]
async fn original_04_opaque_extensions_and_same_model_contracts_reach_provider_unchanged() {
    let row = case(4);
    let f = Fixture::new(4, &row, 0).await;
    let mut a = Assertions::new(4);
    let mut replies = Vec::new();
    for (index, request) in row["downstream"].as_array().unwrap().iter().enumerate() {
        let reply = f.send(request).await;
        a.check(1, reply.status == 200);
        a.check(
            2,
            parse(&f.provider().last().unwrap().body)
                == parse(request["body"].as_str().unwrap().as_bytes()),
        );
        assert_eq!(
            reply.bytes,
            response_bytes(&source_provider_rows(&row, 0)[index]),
            "provider bytes unchanged"
        );
        replies.push(reply);
    }
    assert_eq!(
        f.calls().len(),
        5,
        "original evaluates each full request despite same human text"
    );
    let seen = f.finish(4, 0, &replies).await;
    compare_logs(seen["logs"].as_array().unwrap(), &row, 0);
    a.finish();
}
#[tokio::test]
async fn original_05_same_counter_skips_incompatible_targets_then_counts_same_model() {
    let row = case(5);
    let cfg = read_config(&row["config_arguments"][0][0], false, Path::new("/tmp")).unwrap();
    let script = Arc::new(Script {
        expected: row["mocked_fetch"].as_array().unwrap().clone(),
        calls: Rows::default(),
    });
    let counter = TokenCounter::new(script.clone(), &cfg);
    let line = CORPUS
        .lines()
        .find(|line| {
            serde_json::from_str::<Value>(line).unwrap()["source_test"]
                == "test/routing-compatibility.test.mjs#5"
        })
        .unwrap();
    let exact = JsDocument::parse(line.as_bytes()).unwrap();
    let node = exact.get(exact.root(), "counter_calls").unwrap();
    let JsNode::Array(calls) = exact.node(node).unwrap() else {
        panic!("source calls array")
    };
    let mut a = Assertions::new(5);
    let mut results = Vec::new();
    for (index, call) in calls.iter().enumerate() {
        let args = exact.get(*call, "arguments").unwrap();
        let JsNode::Array(args) = exact.node(args).unwrap() else {
            panic!("source arguments array")
        };
        let raw = exact.stringify_node(args[0]);
        let document = JsDocument::parse(raw.as_bytes()).unwrap();
        let model = exact.string(args[1]).unwrap().to_scalar().unwrap();
        let count = counter
            .count(
                &document,
                &model,
                &HeaderMap::new(),
                &CancellationToken::new(),
                "",
            )
            .await;
        assert_eq!(
            document.stringify(),
            raw,
            "counter input remains exact and immutable"
        );
        match index {
            0 => a.check(1, count.is_none()),
            1 => {
                a.check(2, count.is_none());
                a.check(3, script.calls.lock().unwrap().is_empty());
            }
            2 => {
                a.check(4, count == Some(100));
                a.check(5, script.calls.lock().unwrap().len() == 1);
            }
            _ => unreachable!(),
        }
        // Source undefined is retained in its tagged raw transcript. Native
        // Option::None is recorded explicitly; it is not normalized to a JS value.
        results.push(json!({"document_utf8":raw,"model":model,"native_optional_count":count}));
    }
    save(
        5,
        0,
        &json!({"native_counter_calls":results,"fetch_calls":script.calls.lock().unwrap().clone()}),
    );
    a.finish();
}
