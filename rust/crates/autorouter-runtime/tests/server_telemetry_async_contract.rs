//! Additive complete original server #26 replay through actual returned futures.
//! The earlier synchronous-only partial replay/evidence stays unchanged.
#[allow(dead_code)]
#[path = "support/server_streaming_routing.rs"]
mod peer;
use autorouter_core::{config::read_config, js_json::JsDocument, router::RouteOptions};
use autorouter_runtime::{
    evaluator::EvaluationError,
    http_client::{HttpTransport, NativeHttpClient},
    observer_tasks::{
        ObserverFuture, ObserverLimits, ObserverRejected, ObserverSnapshot, ObserverTasks,
    },
    response_observer::CompletionEvidence,
    server::{Gateway, GatewayRouter},
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
const CORPUS: &str = include_str!("../../../parity/cases/server-stream-failure-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../parity/cases/server-stream-failure-contracts.capture.json");
const LIMIT: usize = 2 * 1024 * 1024;
const BOUND: Duration = Duration::from_secs(5);
type Events = Arc<Mutex<Vec<Value>>>;
fn case() -> Value {
    assert_eq!(
        format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
        "93427d9e2bcb68304b4bd28af30974083a0238b97d305377725029bcef87a21f"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(CAPTURE.as_bytes())),
        "0eb917770f8b49a114ce914d9a31b6c24a987d2c980221b82305d9a3f3427332"
    );
    let capture: Value = serde_json::from_str(CAPTURE).unwrap();
    let definition = capture["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["number"] == 26)
        .unwrap();
    assert_eq!(definition["assertions"].as_array().unwrap().len(), 3);
    assert!(
        definition["assertions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["executions"] == 1)
    );
    CORPUS
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|r| r["source_test"] == "test/server.test.mjs#26")
        .unwrap()
}
fn parsed(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}
fn headers(value: &Value) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (k, v) in value.as_object().unwrap() {
        map.insert(
            hyper::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            v.as_str().unwrap().parse().unwrap(),
        );
    }
    map
}
fn decode(value: &Value) -> Vec<u8> {
    openssl::base64::decode_block(value.as_str().unwrap()).unwrap()
}
fn upstream(row: &Value) -> Vec<Value> {
    row["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["role"] == "upstream")
        .unwrap()["requests"]
        .as_array()
        .unwrap()
        .clone()
}
fn check_request(actual: &Observation, wanted: &Value) -> Result<(), String> {
    if actual.method != wanted["method"] || actual.path != wanted["path"] {
        return Err("provider path/method mismatch".into());
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
        if actual.headers.get(name).and_then(|v| v.to_str().ok())
            != wanted["headers"][name].as_str()
        {
            return Err(format!("semantic provider header mismatch: {name}"));
        }
    }
    if parsed(&actual.body) != parsed(wanted["request_body_utf8"].as_str().unwrap().as_bytes()) {
        return Err("full forwarded request mismatch".into());
    }
    Ok(())
}
struct MockRouter {
    decision: Value,
    calls: Events,
    completions: Events,
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
        let mut calls = self.calls.lock().unwrap();
        assert!(calls.is_empty());
        calls.push(json!({"document":parsed(document.stringify().as_bytes()),"scope":options.scope,"prompt_id":options.prompt_id,"request_class":options.request_class,"request_id":options.request_id,"count_tokens":options.count_tokens,"signal_live":!cancellation.is_cancelled()}));
        Ok(self.decision.clone())
    }
    fn complete(&self, id: &str, evidence: &Value) -> bool {
        let mut rows = self.completions.lock().unwrap();
        assert!(rows.len() < 8);
        rows.push(json!({"id":id,"evidence":evidence,"accepted":false}));
        false
    }
    fn complete_exact(&self, id: &str, evidence: &CompletionEvidence) -> bool {
        let mut rows = self.completions.lock().unwrap();
        assert!(rows.len() < 8);
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
fn sink(rows: Events) -> EventSink {
    Arc::new(move |document| {
        let mut rows = rows.lock().unwrap();
        assert!(rows.len() < 32);
        rows.push(document.to_serde_observation_lossy());
    })
}
fn snapshot(s: ObserverSnapshot) -> Value {
    json!({"closed":s.closed,"worker_finished":s.worker_finished,"pending":s.pending,"factories":s.factories,"charged_bytes":s.charged_bytes,"completed":s.completed,"rejected":s.rejected,"factory_panicked":s.factory_panicked,"poll_panicked":s.poll_panicked,"destructor_panicked":s.destructor_panicked,"timed_out":s.timed_out,"cancelled":s.cancelled,"declined_closed":s.declined_closed,"declined_cancelled":s.declined_cancelled,"declined_payload":s.declined_payload,"declined_capacity":s.declined_capacity,"declined_deadline":s.declined_deadline})
}
include!("../../autorouter-core/src/json_compare_macros.rs");
contract_json_equal!(number_equality, a, b => a.as_f64() == b.as_f64());
fn metadata(wanted: &Value, actual: &Value) {
    let ignored = ["request_id", "first_response_ms", "total_latency_ms"];
    let expected_keys = wanted
        .as_object()
        .unwrap()
        .iter()
        .filter(|(k, v)| !ignored.contains(&k.as_str()) && v["$js_type"] != "undefined")
        .map(|(k, _)| k)
        .collect::<BTreeSet<_>>();
    let keys = actual
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| !ignored.contains(&k.as_str()))
        .collect::<BTreeSet<_>>();
    assert_eq!(keys, expected_keys);
    for key in expected_keys {
        assert!(
            number_equality(&actual[key], &wanted[key]),
            "full safe field {key}"
        );
    }
    for key in ["first_response_ms", "total_latency_ms"] {
        if wanted
            .get(key)
            .is_some_and(|v| v["$js_type"] != "undefined")
        {
            assert!(
                actual[key]
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && n >= 0.)
            );
        } else {
            assert!(actual.get(key).is_none());
        }
    }
}
#[tokio::test]
async fn original_26_actual_status_factory_unwinds_and_returned_rejections_preserve_inference() {
    let row = case();
    let plans = upstream(&row);
    let script = plans.clone();
    let peer = Peer::start(move |request, index| {
        let wanted = script.get(index).ok_or("unexpected provider request")?;
        check_request(request, wanted)?;
        let chunks = wanted["response_writes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| Bytes::from(decode(&r["base64"])))
            .collect();
        Ok(ResponsePlan::chunks(
            wanted["response_status"].as_u64().unwrap() as u16,
            headers(&wanted["response_headers"]),
            chunks,
        ))
    })
    .await
    .unwrap();
    let client = Arc::new(NativeHttpClient::new().unwrap());
    let mut config = read_config(&row["config_arguments"][0][0], false, Path::new("/tmp")).unwrap();
    config.local_token = row["gateway_config"]["localToken"]
        .as_str()
        .map(str::to_owned);
    config.upstream = format!("http://{}", peer.address);
    let router = Arc::new(MockRouter {
        decision: row["route_calls"][0]["decision"].clone(),
        calls: Events::default(),
        completions: Events::default(),
        closed: AtomicBool::new(false),
    });
    let statuses = Events::default();
    let logs = Events::default();
    let callback_outcomes = Events::default();
    let owner = ObserverTasks::new(ObserverLimits::default()).unwrap();
    let handle = owner.handle();
    let callbacks = callback_outcomes.clone();
    let status_rows = statuses.clone();
    let status=handle.event_sink(move |document| -> ObserverFuture {
        let entry=document.to_serde_observation_lossy();let event=entry["event"].clone();
        let invocation={let mut rows=status_rows.lock().unwrap();assert!(rows.len()<32);rows.push(entry);rows.len()};
        {let mut rows=callbacks.lock().unwrap();assert!(rows.len()<32);rows.push(json!({"invocation":invocation,"event":event,"outcome":if invocation%2==1{"synchronous_unwind"}else{"returned_future_rejection"}}));}
        if invocation%2==1{panic!("Status storage unavailable");}
        Box::pin(async{Err(ObserverRejected)})
    });
    let gateway = Gateway::with_router(
        config,
        client.clone(),
        router.clone(),
        EventSinks {
            status: Some(status),
            log: Some(sink(logs.clone())),
            ..Default::default()
        },
    )
    .unwrap()
    .listen(0)
    .await
    .unwrap();
    let source_request = &row["downstream"][0];
    let mut request = Request::builder()
        .method(source_request["method"].as_str().unwrap())
        .uri(format!("http://{}/v1/messages", gateway.address));
    *request.headers_mut().unwrap() = headers(&source_request["headers"]);
    let outcome = tokio::time::timeout(BOUND, async {
        let response = client
            .request(
                request
                    .body(Full::new(Bytes::from(
                        source_request["body"].as_str().unwrap().to_owned(),
                    )))
                    .unwrap(),
            )
            .await
            .map_err(|_| "native client request failed")?;
        let (parts, body) = response.into_parts();
        let bytes = Limited::new(body, LIMIT)
            .collect()
            .await
            .map_err(|_| "native client body failed")?
            .to_bytes();
        for _ in 0..100 {
            if statuses
                .lock()
                .unwrap()
                .iter()
                .any(|r| r["event"] == "request_complete")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Ok::<_, String>((parts.status.as_u16(), parts.headers, bytes))
    })
    .await;
    // As in the source, inference is already delivered before observer work is
    // inspected. Both actual returned rejections must be handled before close.
    for _ in 0..100 {
        if owner.snapshot().rejected == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let before = owner.snapshot();
    let raw_statuses = statuses.lock().unwrap().clone();
    let raw_logs = logs.lock().unwrap().clone();
    let raw_callbacks = callback_outcomes.lock().unwrap().clone();
    let routes = router.calls.lock().unwrap().clone();
    let completions = router.completions.lock().unwrap().clone();
    let requests = peer.observations();
    let closed = tokio::time::timeout(BOUND, gateway.close()).await;
    let owner_closed = tokio::time::timeout(BOUND, owner.close()).await;
    let peer_closed = peer.close().await;
    closed.unwrap();
    owner_closed.unwrap();
    peer_closed.unwrap();
    let (status, response_headers, bytes) = outcome.unwrap().unwrap();
    let mut executed = BTreeMap::new();
    let mut check = |site: usize, predicate: bool| {
        assert!(predicate, "server#26:assert-{site}");
        *executed.entry(site).or_insert(0) += 1;
    };
    check(1, status == 200);
    check(2, parsed(&bytes)["model"] == "claude-haiku-4-5-20251001");
    check(3, raw_callbacks.len() == 5);
    assert_eq!(executed, BTreeMap::from([(1, 1), (2, 1), (3, 1)]));
    assert_eq!(before.factory_panicked, 3);
    assert_eq!(before.rejected, 2);
    assert_eq!(before.pending, 0);
    assert_eq!(before.charged_bytes, 0);
    assert_eq!(before.cancelled, 0);
    assert_eq!(before.timed_out, 0);
    let after = owner.snapshot();
    assert!(after.closed && after.worker_finished);
    assert_eq!(after.pending, 0);
    assert_eq!(after.charged_bytes, 0);
    assert!(router.closed.load(Ordering::SeqCst));
    assert_eq!(
        raw_callbacks
            .iter()
            .map(|r| r["outcome"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "synchronous_unwind",
            "returned_future_rejection",
            "synchronous_unwind",
            "returned_future_rejection",
            "synchronous_unwind"
        ]
    );
    assert_eq!(
        raw_statuses.len(),
        row["statuses"].as_array().unwrap().len()
    );
    for (actual, wanted) in raw_statuses.iter().zip(row["statuses"].as_array().unwrap()) {
        assert_eq!(actual["request_id"], raw_statuses[0]["request_id"]);
        metadata(wanted, actual);
    }
    assert_eq!(requests.len(), 1);
    check_request(&requests[0], &plans[0]).unwrap();
    let delivered = row["downstream"][0]["reads"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == "read" && r["done"] == false)
        .flat_map(|r| decode(&r["base64"]))
        .collect::<Vec<_>>();
    assert_eq!(bytes.as_ref(), delivered);
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0]["document"], row["route_calls"][0]["document"]);
    assert_eq!(routes[0]["request_id"], raw_statuses[0]["request_id"]);
    assert_eq!(
        routes[0]["scope"],
        row["route_calls"][0]["options"]["scope"]
    );
    assert_eq!(routes[0]["prompt_id"], "");
    assert_eq!(routes[0]["request_class"], "");
    assert_eq!(routes[0]["signal_live"], true);
    assert_eq!(routes[0]["count_tokens"], true);
    assert_eq!(completions.len(), 1);
    assert_eq!(completions[0]["id"], raw_statuses[0]["request_id"]);
    assert!(completions[0]["evidence"].is_null());
    assert_eq!(completions[0]["accepted"], false);
    for secret in [
        "Fix a typo",
        "local-test-token-123456789",
        "upstream-secret",
        "classifier-secret",
        "Status storage unavailable",
        "Asynchronous status failure",
    ] {
        assert!(
            !json!({"statuses":raw_statuses,"logs":raw_logs})
                .to_string()
                .contains(secret)
        );
    }
    if let Some(directory) = std::env::var_os("AUTOROUTER_NATIVE_PARITY_CAPTURE_DIR") {
        let observation = |r: &Observation| json!({"method":r.method,"path":r.path,"headers":r.headers.iter().map(|(k,v)|(k.as_str().to_owned(),v.to_str().unwrap().to_owned())).collect::<BTreeMap<_,_>>(),"body_utf8":std::str::from_utf8(&r.body).unwrap()});
        let raw = json!({"schema_version":1,"status":"passed","source_test":"test/server.test.mjs#26","source_assertion_executions":executed,"upstream_requests":requests.iter().map(observation).collect::<Vec<_>>(),"route_calls":routes,"complete_calls":completions,"statuses":raw_statuses,"logs":raw_logs,"callback_outcomes":raw_callbacks,"observer_before_close":snapshot(before),"observer_after_close":snapshot(after),"downstream":{"status":status,"headers":response_headers.iter().map(|(k,v)|(k.as_str().to_owned(),v.to_str().unwrap().to_owned())).collect::<BTreeMap<_,_>>(),"body_base64":openssl::base64::encode_block(&bytes)},"projection":"None. Actual status documents, request IDs, timing values, return-future rejection evidence, raw HTTP headers and every delivered byte retained."});
        let data = serde_json::to_vec_pretty(&raw).unwrap();
        assert!(data.len() < LIMIT);
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(Path::new(&directory).join("server-telemetry-async-26.native.json"))
            .unwrap();
        file.write_all(&data).unwrap();
        file.write_all(b"\n").unwrap();
    }
}
