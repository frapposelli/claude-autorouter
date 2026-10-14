//! All four expanded original server #18/#19 schedules, against real native HTTP.
#[allow(dead_code)] // Shared helpers retain separate policies used by sibling targets.
#[path = "support/server_contract.rs"]
mod contract;
use contract::{
    chunks, decode, events, header_is, identity_headers as headers, parsed, response_bytes,
    single_sink as sink, upstream_rows,
};
// This shared fixture also exposes held-stream gates to other test targets.
#[allow(dead_code)]
#[path = "support/server_streaming_routing.rs"]
mod peer;
use autorouter_core::{config::read_config, js_json::JsDocument, router::RouteOptions};
use autorouter_runtime::{
    evaluator::EvaluationError,
    http_client::{HttpError, HttpTransport, NativeHttpClient},
    response_observer::CompletionEvidence,
    router::Router,
    server::{Gateway, GatewayHandle, GatewayRouter},
    server_events::EventSinks,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited, combinators::UnsyncBoxBody};
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

const PROMPT_CORPUS: &str =
    include_str!("../../../parity/cases/server-auto-goal-prompt-ids-contracts.jsonl");
const HEADERLESS_CORPUS: &str =
    include_str!("../../../parity/cases/server-auto-goal-headerless-contracts.jsonl");
const CAPTURE: &str = include_str!("../../../parity/cases/server-auto-goal-contracts.capture.json");
const BOUND: Duration = Duration::from_secs(5);
const LIMIT: usize = 2 * 1024 * 1024;
type Error = Box<dyn std::error::Error + Send + Sync>;
type Body = UnsyncBoxBody<Bytes, Error>;
type Events = Arc<Mutex<Vec<Value>>>;

fn case(number: usize, use_prompt_ids: bool) -> Value {
    let prompt = PROMPT_CORPUS.lines().collect::<Vec<_>>();
    let headerless = HEADERLESS_CORPUS.lines().collect::<Vec<_>>();
    assert_eq!(prompt.len(), 2);
    assert_eq!(headerless.len(), 2);
    let combined = format!(
        "{}\n{}\n{}\n{}\n",
        prompt[0], headerless[0], prompt[1], headerless[1]
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(combined.as_bytes())),
        "6ccb01e63126ebf19c9201ed0b68d60be9d7530d491adba9e3ff0de1bd92003c"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(CAPTURE.as_bytes())),
        "b2e8033bb261c1297e6a707e4d4360034343e2f065bb1aec2545aaa08d7126fa"
    );
    let metadata: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(metadata["static_assertions"], 38);
    assert_eq!(metadata["expanded_assertions"], 290);
    assert_eq!(metadata["callbacks_executed"], 4);
    (if use_prompt_ids {
        PROMPT_CORPUS
    } else {
        HEADERLESS_CORPUS
    })
    .lines()
    .map(|line| serde_json::from_str::<Value>(line).unwrap())
    .find(|row| {
        row["source_test"] == format!("test/server.test.mjs#{number}")
            && row["use_prompt_ids"] == use_prompt_ids
    })
    .unwrap()
}
fn route_tuple(row: &Value) -> Value {
    let mut result = serde_json::Map::new();
    for key in ["model", "source", "reason", "classified_tier"] {
        if let Some(value) = row.get(key).filter(|v| v["$js_type"] != "undefined") {
            result.insert(key.into(), value.clone());
        }
    }
    Value::Object(result)
}
fn compare_upstream(observed: &Observation, expected: &Value) -> Result<(), String> {
    if observed.method != expected["method"].as_str().unwrap()
        || observed.path != expected["path"].as_str().unwrap()
    {
        return Err("unexpected upstream method/path (including count endpoint)".into());
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
    // Original assertions compare complete parsed request objects. Native context,
    // signed blocks, clear-at instructions, cache controls and goal schema remain.
    if parsed(&observed.body) != parsed(expected["request_body_utf8"].as_str().unwrap().as_bytes())
    {
        return Err("complete forwarded request differs".into());
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
                || parsed(&observation.body) != parsed(row["body"].as_str().unwrap().as_bytes())
            {
                return Err(self.fault("complete evaluator request differs"));
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
            return Err(self.fault("unregistered endpoint"));
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
    real: Router<Transport>,
    calls: Mutex<Vec<Value>>,
    completions: Mutex<Vec<Value>>,
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
        let before = document.stringify();
        let observed = document.clone();
        let input = json!({"document":parsed(before.as_bytes()),"scope":options.scope,"prompt_id":options.prompt_id,"request_class":options.request_class,"signal_live":!cancellation.is_cancelled()});
        let result = self
            .real
            .route(document, options, headers, cancellation, search)
            .await;
        let mut calls = self.calls.lock().unwrap();
        assert!(calls.len() < 8);
        calls.push(json!({"input":input,"unchanged":observed.stringify()==before}));
        result
    }
    fn complete(&self, id: &str, evidence: &Value) -> bool {
        let accepted = self.real.complete(id, evidence);
        let mut rows = self.completions.lock().unwrap();
        assert!(rows.len() < 32);
        rows.push(json!({"id":id,"evidence":evidence,"accepted":accepted}));
        accepted
    }
    fn complete_exact(&self, id: &str, evidence: &CompletionEvidence) -> bool {
        let accepted = self.real.complete_exact(id, evidence);
        let mut rows = self.completions.lock().unwrap();
        assert!(rows.len() < 32);
        rows.push(json!({"id":id,"evidence":evidence,"accepted":accepted}));
        accepted
    }
    fn shutdown(&self) {
        self.real.shutdown();
    }
    async fn close(&self) {
        self.real.close().await;
    }
}
struct Fixture {
    client: Arc<NativeHttpClient>,
    transport: Arc<Transport>,
    router: Arc<AuditRouter>,
    gateway: Option<GatewayHandle>,
    peer: Option<Peer>,
    logs: Events,
    statuses: Events,
}
struct Reply {
    status: u16,
    headers: HeaderMap,
    bytes: Vec<u8>,
}
struct Seen {
    requests: Vec<Observation>,
    mocked: Vec<Observation>,
    routes: Vec<Value>,
    completions: Vec<Value>,
    logs: Vec<Value>,
    statuses: Vec<Value>,
}
impl Fixture {
    async fn new(row: &Value) -> Self {
        let script = upstream_rows(row);
        let peer = Peer::start(move |request, index| {
            let row = script.get(index).ok_or("extra upstream request")?;
            compare_upstream(request, row)?;
            Ok(ResponsePlan::chunks(
                row["response_status"].as_u64().unwrap() as u16,
                headers(&row["response_headers"]),
                chunks(row),
            ))
        })
        .await
        .unwrap();
        let client = Arc::new(NativeHttpClient::new().unwrap());
        let transport = Arc::new(Transport {
            native: client.clone(),
            authority: peer.address.to_string(),
            script: row["mocked_fetch"].as_array().unwrap().clone(),
            mocked: Mutex::new(Vec::new()),
            faults: Mutex::new(Vec::new()),
        });
        let original =
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
        config.models = original.models.clone();
        config.client_profile = original.client_profile;
        config.upstream = format!("http://{}", peer.address);
        let router = Arc::new(AuditRouter {
            real: Router::new(transport.clone(), original),
            calls: Mutex::new(Vec::new()),
            completions: Mutex::new(Vec::new()),
        });
        let logs = Events::default();
        let statuses = Events::default();
        let gateway = Gateway::with_router(
            config,
            transport.clone(),
            router.clone(),
            EventSinks {
                log: Some(sink(&logs)),
                status: Some(sink(&statuses)),
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
            logs,
            statuses,
        }
    }
    async fn send(&self, row: &Value) -> Result<Reply, String> {
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
        let request = request
            .body(Full::new(Bytes::from(
                row["body"].as_str().unwrap().to_owned(),
            )))
            .unwrap();
        let response = self
            .client
            .request(request)
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
    async fn finish(mut self) -> Result<Seen, String> {
        let closed = tokio::time::timeout(BOUND, self.gateway.take().unwrap().close()).await;
        let requests = self.peer.as_ref().unwrap().observations();
        let peer_closed = self.peer.take().unwrap().close().await;
        closed.map_err(|_| "gateway cleanup timed out")?;
        peer_closed?;
        let faults = self.transport.faults.lock().unwrap().clone();
        if !faults.is_empty() {
            return Err(format!("sticky transport faults: {faults:?}"));
        }
        for rows in [&self.logs, &self.statuses] {
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
            mocked: self.transport.mocked.lock().unwrap().clone(),
            routes: self.router.calls.lock().unwrap().clone(),
            completions: self.router.completions.lock().unwrap().clone(),
            logs: self.logs.lock().unwrap().clone(),
            statuses: self.statuses.lock().unwrap().clone(),
        })
    }
}
async fn run(row: &Value) -> (Vec<Reply>, Seen) {
    let f = Fixture::new(row).await;
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
        let capture: Value = serde_json::from_str(CAPTURE).unwrap();
        let row = capture["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["number"] == self.number)
            .unwrap();
        for (index, site) in row["assertions"].as_array().unwrap().iter().enumerate() {
            let executions = site["executions"].as_u64().unwrap() as usize;
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
        }
        assert_eq!(
            self.counts.len(),
            row["assertions"].as_array().unwrap().len()
        );
    }
}
fn supplemental_exact_observations(row: &Value, seen: &Seen) {
    let calls = row["route_calls"].as_array().unwrap();
    assert_eq!(seen.routes.len(), calls.len());
    for (got, wanted) in seen.routes.iter().zip(calls) {
        assert_eq!(got["input"]["document"], wanted["document"]);
        assert_eq!(
            got["unchanged"], true,
            "route must retain the caller's full immutable input"
        );
        assert_eq!(got["input"]["signal_live"], true);
        for (native, source) in [
            ("scope", "scope"),
            ("prompt_id", "promptId"),
            ("request_class", "requestClass"),
        ] {
            if wanted["options"][source]["$js_type"] == "undefined" {
                // RouteOptions has String fields: absence is the empty string,
                // while the actual forwarded HTTP header must remain absent.
                assert_eq!(got["input"][native], "");
            } else {
                assert_eq!(got["input"][native], wanted["options"][source]);
            }
        }
    }
    let all_source = row["completions"].as_array().unwrap();
    // The native request lease consumes successful evidence once. JavaScript
    // subsequently calls complete(id, undefined), a false-returning no-op.
    // Compare every actual evidence-bearing commit; do not claim identical
    // callback cardinality or silently normalize the source's extra calls.
    let source = all_source
        .iter()
        .filter(|row| row["evidence"]["$js_type"] != "undefined")
        .collect::<Vec<_>>();
    assert_eq!(all_source.len(), source.len() * 2);
    for cleanup in all_source
        .iter()
        .filter(|row| row["evidence"]["$js_type"] == "undefined")
    {
        assert_eq!(cleanup["result"], false);
    }
    assert_eq!(seen.completions.len(), source.len());
    let starts = events(&seen.statuses, "request_start");
    for (got, wanted) in seen.completions.iter().zip(source) {
        let index = wanted["request_id"]
            .as_str()
            .unwrap()
            .strip_prefix("request-")
            .unwrap()
            .parse::<usize>()
            .unwrap()
            - 1;
        assert_eq!(
            got["id"], starts[index]["request_id"],
            "complete calls retain request ownership"
        );
        assert_eq!(got["accepted"], wanted["result"]);
        assert_eq!(
            got["evidence"], wanted["evidence"],
            "exact clean completion and emitted tool ownership"
        );
    }
    assert_eq!(
        events(&seen.statuses, "request_complete").len(),
        calls.len()
    );
    assert!(events(&seen.statuses, "request_error").is_empty());
}
fn retain_native_observations(row: &Value, replies: &[Reply], seen: &Seen) {
    let Some(directory) = std::env::var_os("AUTOROUTER_NATIVE_PARITY_CAPTURE_DIR") else {
        return;
    };
    let path = Path::new(&directory).join(format!("{}.native.json", row["id"].as_str().unwrap()));
    let request = |observed: &Observation| {
        json!({
            "method":observed.method,"path":observed.path,
            "headers":observed.headers.iter().map(|(k,v)|(k.as_str().to_owned(),v.to_str().unwrap().to_owned())).collect::<BTreeMap<_,_>>(),
            "body_utf8":std::str::from_utf8(&observed.body).unwrap()
        })
    };
    let observed = json!({
        "schema_version":1,"status":"passed","source_test":row["source_test"],"use_prompt_ids":row["use_prompt_ids"],
        "upstream_requests":seen.requests.iter().map(request).collect::<Vec<_>>(),
        "mocked_evaluator_requests":seen.mocked.iter().map(request).collect::<Vec<_>>(),
        "route_calls":seen.routes,"complete_calls":seen.completions,"logs":seen.logs,"statuses":seen.statuses,
        "downstream":replies.iter().map(|reply|json!({"status":reply.status,
            "headers":reply.headers.iter().map(|(k,v)|(k.as_str().to_owned(),v.to_str().unwrap().to_owned())).collect::<BTreeMap<_,_>>(),
            "body_base64":openssl::base64::encode_block(&reply.bytes)})).collect::<Vec<_>>(),
        "api_adaptations":["Absent typed prompt_id is an empty String; the actual HTTP header remains absent.",
            "Native successful completion consumes the request lease once; the source additionally invokes a false-returning undefined cleanup callback."],
        "projection":"None: these native observations retain their actual generated IDs, latencies, HTTP header maps and complete bytes."
    });
    let bytes = serde_json::to_vec_pretty(&observed).unwrap();
    assert!(bytes.len() <= 8 * 1024 * 1024);
    contract::write_capture_file(&path, &bytes);
}
async fn original_18(use_prompt_ids: bool) {
    let row = case(18, use_prompt_ids);
    let original = row.clone();
    let (replies, seen) = run(&row).await;
    let mut a = Assertions::new(18);
    for (index, call) in seen.mocked.iter().enumerate() {
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
        a.check(3, index < 4);
    }
    let expected = upstream_rows(&row);
    for (index, request) in seen.requests.iter().enumerate() {
        a.check(4, request.path == "/v1/messages?beta=true");
        a.check(
            5,
            header_is(
                &request.headers,
                "authorization",
                Some("Bearer fake-subscription-token"),
            ),
        );
        a.check(
            6,
            header_is(
                &request.headers,
                "anthropic-beta",
                expected[index]["headers"]["anthropic-beta"].as_str(),
            ),
        );
        a.check(
            7,
            header_is(
                &request.headers,
                "x-claude-code-session-id",
                Some("auto-switch-session"),
            ),
        );
        a.check(
            8,
            header_is(
                &request.headers,
                "x-claude-code-prompt-id",
                if use_prompt_ids {
                    Some(
                        [
                            "auto-first",
                            "auto-demanding",
                            "auto-demanding",
                            "auto-next",
                        ][index],
                    )
                } else {
                    None
                },
            ),
        );
        a.check(9, header_is(&request.headers, "x-autorouter-token", None));
        a.check(10, replies[index].status == 200);
        a.check(11, replies[index].bytes == response_bytes(&expected[index]));
        let mut input = parsed(
            row["downstream"][index]["body"]
                .as_str()
                .unwrap()
                .as_bytes(),
        );
        input["model"] = json!(
            [
                "claude-sonnet-5-5",
                "claude-opus-5-5",
                "claude-opus-5-5",
                "claude-sonnet-5-5"
            ][index]
        );
        a.check(12, parsed(&request.body) == input);
    }
    a.check(13, seen.mocked.len() == 4);
    a.check(14, seen.requests.len() == 4);
    a.check(
        15,
        row == original && seen.routes.iter().all(|row| row["unchanged"] == true),
    );
    a.check(16,events(&seen.statuses,"route").iter().map(route_tuple).collect::<Vec<_>>()==vec![json!({"model":"claude-sonnet-5-5","source":"jev","reason":"auto_mode_floor","classified_tier":"haiku"}),json!({"model":"claude-opus-5-5","source":"jev","reason":"classified","classified_tier":"opus"}),json!({"model":"claude-opus-5-5","source":"jev","reason":"tool_turn_pinned","classified_tier":"haiku"}),json!({"model":"claude-sonnet-5-5","source":"jev","reason":"auto_mode_floor","classified_tier":"haiku"})]);
    assert!(!seen.logs.is_empty() && !seen.statuses.is_empty());
    for secret in [
        "Synthetic Sonnet reasoning.",
        "signed-sonnet-history+/==",
        "Synthetic Opus reasoning.",
        "signed-opus-history+/==",
        "Keep the native tool review context.",
        "classifier-secret",
        "fake-subscription-token",
    ] {
        a.check(
            17,
            !json!({"logs":seen.logs,"statuses":seen.statuses})
                .to_string()
                .contains(secret),
        );
    }
    a.finish();
    supplemental_exact_observations(&row, &seen);
    retain_native_observations(&row, &replies, &seen);
}
async fn original_19(use_prompt_ids: bool) {
    let row = case(19, use_prompt_ids);
    let original = row.clone();
    let (replies, seen) = run(&row).await;
    let mut a = Assertions::new(19);
    for (index, call) in seen.mocked.iter().enumerate() {
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
        a.check(3, index < 4);
    }
    let expected = upstream_rows(&row);
    for (index, request) in seen.requests.iter().enumerate() {
        a.check(4, request.path == "/v1/messages?beta=true");
        a.check(
            5,
            header_is(
                &request.headers,
                "authorization",
                Some("Bearer fake-subscription-token"),
            ),
        );
        a.check(
            6,
            header_is(
                &request.headers,
                "anthropic-beta",
                Some("oauth-2025-04-20,future-capability"),
            ),
        );
        a.check(
            7,
            header_is(&request.headers, "anthropic-version", Some("2023-06-01")),
        );
        a.check(
            8,
            header_is(
                &request.headers,
                "x-claude-code-session-id",
                Some("synthetic-goal-session"),
            ),
        );
        a.check(
            9,
            header_is(
                &request.headers,
                "x-claude-code-agent-id",
                Some("synthetic-main-agent"),
            ),
        );
        a.check(
            10,
            header_is(
                &request.headers,
                "x-claude-code-prompt-id",
                if use_prompt_ids {
                    Some(if index == 4 {
                        "new-human-prompt"
                    } else {
                        "goal-prompt"
                    })
                } else {
                    None
                },
            ),
        );
        a.check(
            11,
            header_is(
                &request.headers,
                "x-claude-code-request-class",
                Some(if index == 1 { "auxiliary" } else { "main" }),
            ),
        );
        a.check(12, header_is(&request.headers, "x-autorouter-token", None));
        a.check(13, replies[index].status == 200);
        a.check(
            14,
            header_is(
                &replies[index].headers,
                "request-id",
                Some(&format!("goal-provider-{index}")),
            ),
        );
        a.check(15, replies[index].bytes == response_bytes(&expected[index]));
        let mut input = parsed(
            row["downstream"][index]["body"]
                .as_str()
                .unwrap()
                .as_bytes(),
        );
        let model = [
            "claude-sonnet-5-5",
            "claude-haiku-4-5-20251001",
            "claude-sonnet-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-4-5-20251001",
        ][index];
        input["model"] = json!(model);
        input["thinking"] =
            json!({"type":if model=="claude-sonnet-5-5" {"between_tools"} else {"disabled"}});
        a.check(16, parsed(&request.body) == input);
    }
    a.check(17, seen.mocked.len() == 4);
    a.check(
        18,
        row == original && seen.routes.iter().all(|row| row["unchanged"] == true),
    );
    a.check(19,events(&seen.statuses,"route").iter().map(route_tuple).collect::<Vec<_>>()==vec![json!({"model":"claude-sonnet-5-5","source":"jev","reason":"classified","classified_tier":"sonnet"}),json!({"model":"claude-haiku-4-5-20251001","source":"passthrough","reason":"internal_request"}),json!({"model":"claude-sonnet-5-5","source":"jev","reason":if use_prompt_ids {"prompt_turn_pinned"} else {"goal_turn_pinned"},"classified_tier":"haiku"}),json!({"model":"claude-sonnet-5-5","source":"jev","reason":"tool_turn_pinned","classified_tier":"haiku"}),json!({"model":"claude-haiku-4-5-20251001","source":"jev","reason":"classified","classified_tier":"haiku"})]);
    a.check(
        20,
        events(&seen.logs, "upstream_model")
            .iter()
            .map(|row| row["model"].clone())
            .collect::<Vec<_>>()
            == vec![
                json!("claude-sonnet-5-5"),
                json!("claude-haiku-4-5-20251001"),
                json!("claude-sonnet-5-5"),
                json!("claude-sonnet-5-5"),
                json!("claude-haiku-4-5-20251001"),
            ],
    );
    assert!(!seen.logs.is_empty() && !seen.statuses.is_empty());
    for secret in [
        "Create the fixture file once the synthetic resource is available.",
        "The synthetic resource is unavailable. Please enable it to continue.",
        "{\"ok\":false,\"reason\":\"The synthetic resource is not available yet.\"}",
        "classifier-secret",
        "fake-subscription-token",
    ] {
        a.check(
            21,
            !json!({"logs":seen.logs,"statuses":seen.statuses})
                .to_string()
                .contains(secret),
        );
    }
    a.finish();
    supplemental_exact_observations(&row, &seen);
    retain_native_observations(&row, &replies, &seen);
}
#[tokio::test]
async fn original_18_auto_human_switches_with_prompt_ids() {
    original_18(true).await;
}
#[tokio::test]
async fn original_18_auto_human_switches_without_prompt_ids() {
    original_18(false).await;
}
#[tokio::test]
async fn original_19_goal_auxiliary_tool_and_human_with_prompt_ids() {
    original_19(true).await;
}
#[tokio::test]
async fn original_19_goal_expanded_command_without_prompt_ids() {
    original_19(false).await;
}
