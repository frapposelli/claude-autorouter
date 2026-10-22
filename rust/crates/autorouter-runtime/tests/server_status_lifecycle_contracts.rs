//! Complete frozen server #21–#24 callbacks, including actual concurrent HTTP.
#[allow(dead_code)] // Shared helpers retain separate policies used by sibling targets.
#[path = "support/server_contract.rs"]
mod contract;
use contract::{
    Reply, chunks, compare_metadata, compare_request, event_names, events, headers, response_bytes,
    same_request, sink, upstream_rows,
};
#[allow(dead_code)] // Shared peer also supplies streaming interfaces to other targets.
#[path = "support/server_streaming_routing.rs"]
mod peer;
use autorouter_runtime::{
    http_client::NativeHttpClient,
    server::{Gateway, GatewayHandle},
    server_events::EventSinks,
};
use bytes::Bytes;
use peer::{Observation, Peer, ResponsePlan, StreamGate};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const CORPUS: &str = include_str!("../../../parity/cases/server-status-lifecycle-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../parity/cases/server-status-lifecycle-contracts.capture.json");
const LIMIT: usize = 2 * 1024 * 1024;
const BOUND: Duration = Duration::from_secs(5);
type Events = Arc<Mutex<Vec<Value>>>;
fn case(number: usize) -> Value {
    contract::captured_server_case(
        number,
        CORPUS,
        CAPTURE,
        [
            "0f74c4f4763f78f03c66eae953de2bb12c65a36053b9e43ffca96ea964f639df",
            "c2fe30663633294e63949f49a4cf7f1bec98e43928eba48adda02dcdf263877f",
        ],
        (34, 64, Some(4)),
    )
}
use contract::MockRouter;
struct Fixture {
    client: Arc<NativeHttpClient>,
    router: Arc<MockRouter>,
    gateway: Option<GatewayHandle>,
    peer: Option<Peer>,
    statuses: Events,
    history: Events,
    logs: Events,
    log_history: Events,
    overlap: Arc<AtomicBool>,
}
struct Seen {
    requests: Vec<Observation>,
    routes: Vec<Value>,
    completions: Vec<Value>,
    statuses: Vec<Value>,
    history: Vec<Value>,
    logs: Vec<Value>,
    overlap: bool,
}
impl Fixture {
    async fn new(number: usize, row: &Value) -> Self {
        let script = upstream_rows(row);
        let gates = Mutex::new(Vec::<StreamGate>::new());
        let overlap = Arc::new(AtomicBool::new(false));
        let witness = overlap.clone();
        let peer = Peer::start(move |request, _index| {
            let wanted = script
                .iter()
                .find(|wanted| same_request(request, wanted))
                .ok_or("unregistered peer request")?;
            compare_request(request, wanted)?;
            if number == 22 && request.path == "/v1/messages" {
                // Keep the first provider response incomplete until the other
                // real request arrives. This proves overlap without assuming
                // which session wins the native connection scheduler.
                let (plan, gate) = ResponsePlan::held(
                    wanted["response_status"].as_u64().unwrap() as u16,
                    headers(&wanted["response_headers"]),
                    Bytes::new(),
                    Bytes::from(response_bytes(wanted)),
                );
                let mut gates = gates.lock().unwrap();
                assert!(gates.len() < 2);
                gates.push(gate);
                if gates.len() == 2 {
                    witness.store(true, Ordering::SeqCst);
                    for gate in gates.iter() {
                        gate.release();
                    }
                }
                Ok(plan)
            } else {
                Ok(ResponsePlan::chunks(
                    wanted["response_status"].as_u64().unwrap() as u16,
                    headers(&wanted["response_headers"]),
                    chunks(wanted),
                ))
            }
        })
        .await
        .unwrap();
        let client = Arc::new(NativeHttpClient::new().unwrap());
        let config = contract::gateway_config(row, &row["config_arguments"][0][0], peer.address);
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
            overlap,
        }
    }
    async fn send(&self, row: &Value) -> Result<Reply, String> {
        contract::send_buffered(&self.client, self.gateway.as_ref(), row).await
    }
    async fn wait_for_status(&self, event: &str) -> Result<(), String> {
        contract::wait_for_status(&self.statuses, event).await
    }
    async fn finish(mut self) -> Result<Seen, String> {
        let requests = contract::close_gateway(&mut self.gateway, &mut self.peer, BOUND).await?;
        if !self.router.closed.load(Ordering::SeqCst) {
            return Err("mock router closure not observed".into());
        }
        contract::check_recorders([&self.statuses, &self.history, &self.logs, &self.log_history])?;
        Ok(Seen {
            requests,
            routes: self.router.calls.lock().unwrap().clone(),
            completions: self.router.completions.lock().unwrap().clone(),
            statuses: self.statuses.lock().unwrap().clone(),
            history: self.history.lock().unwrap().clone(),
            logs: self.logs.lock().unwrap().clone(),
            overlap: self.overlap.load(Ordering::SeqCst),
        })
    }
}
type Assertions = contract::Assertions<1, false>;
fn supplemental_transcripts(row: &Value, replies: &[Reply], seen: &Seen) {
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
    for route in &seen.routes {
        assert_eq!(route["signal_live"], true);
        assert_eq!(
            route["count_tokens"], true,
            "typed token-count capability remains enabled"
        );
        let wanted = row["route_calls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|wanted| {
                wanted["document"] == route["document"]
                    && wanted["options"]["scope"] == route["scope"]
            })
            .unwrap();
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
            ),
            "route options retain actual gateway request ownership"
        );
    }
    let source = row["statuses"].as_array().unwrap();
    let starts = events(&seen.history, "request_start");
    let mut consumed = BTreeSet::new();
    for wanted_start in events(source, "request_start") {
        let wanted = source
            .iter()
            .filter(|event| event["request_id"] == wanted_start["request_id"])
            .cloned()
            .collect::<Vec<_>>();
        let candidates = starts
            .iter()
            .filter(|start| {
                ["session_id", "agent_id", "prompt_id", "request_class"]
                    .iter()
                    .all(|key| {
                        let expected = &wanted_start[key];
                        if expected["$js_type"] == "undefined" || expected.is_null() {
                            !start.as_object().unwrap().contains_key(*key)
                        } else {
                            start[key] == *expected
                        }
                    })
                    && event_names(
                        &seen
                            .history
                            .iter()
                            .filter(|event| event["request_id"] == start["request_id"])
                            .cloned()
                            .collect::<Vec<_>>(),
                    ) == event_names(&wanted)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            candidates.len(),
            1,
            "unique original request group by scope and event schedule"
        );
        let id = candidates[0]["request_id"].as_str().unwrap();
        assert!(consumed.insert(id.to_owned()));
        let actual = seen
            .history
            .iter()
            .filter(|event| event["request_id"] == id)
            .collect::<Vec<_>>();
        assert_eq!(actual.len(), wanted.len());
        for (got, wanted) in actual.iter().zip(&wanted) {
            compare_metadata(wanted, got);
        }
    }
    assert_eq!(
        consumed.len(),
        starts.len(),
        "no extra native request event groups"
    );
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
    // None of these original responses has clean successful execution evidence:
    // #21 lacks a terminal reason, #22 is {}, #23 is SSE error, #24 is HTTP429.
    assert!(
        seen.completions
            .iter()
            .all(|row| row["evidence"].is_null() && row["accepted"] == false)
    );
    if let Some(directory) = std::env::var_os("AUTOROUTER_NATIVE_PARITY_CAPTURE_DIR") {
        let raw = json!({"schema_version":1,"status":"passed","source_test":row["source_test"],"upstream_requests":seen.requests.iter().map(contract::capture_request).collect::<Vec<_>>(),"route_calls":seen.routes,"complete_calls":seen.completions,"statuses":seen.statuses,"all_emitted_statuses":seen.history,"logs":seen.logs,"native_provider_overlap":seen.overlap,"downstream":replies.iter().map(contract::capture_reply).collect::<Vec<_>>(),"projection":"None. Actual generated IDs, measured times, deterministic routing times, headers and complete bytes retained."});
        contract::write_row_capture(&directory, &row["id"], &raw, LIMIT, true);
    }
}
#[tokio::test]
async fn original_21_correlated_status_identity_and_model_roles() {
    let row = case(21);
    let f = Fixture::new(21, &row).await;
    let result = tokio::time::timeout(BOUND, f.send(&row["downstream"][0])).await;
    f.wait_for_status("request_complete").await.unwrap();
    let seen = f.finish().await.unwrap();
    let reply = result.unwrap().unwrap();
    let mut a = Assertions::new(21);
    a.check(
        1,
        event_names(&seen.statuses)
            == [
                "request_start",
                "route",
                "upstream_response",
                "upstream_model",
                "request_complete",
            ],
    );
    let request_id = seen.statuses[0]["request_id"].as_str().unwrap();
    a.check(
        2,
        regex::Regex::new("^[a-f0-9-]{36}$")
            .unwrap()
            .is_match(request_id),
    );
    for entry in &seen.statuses {
        a.check(3, entry["request_id"] == request_id);
        a.check(4, entry["session_id"] == "session-1");
        a.check(5, entry["agent_id"] == "agent-1");
        a.check(6, entry["prompt_id"] == "prompt-1");
        a.check(7, entry["request_class"] == "main");
        a.check(8, !entry.as_object().unwrap().contains_key("tier"));
    }
    let route = &events(&seen.statuses, "route")[0];
    a.check(9, route["requested_model"] == "claude-haiku-4-5-20251001");
    a.check(10, route["model"] == "claude-sonnet-5");
    a.check(11, route["reason"] == "tool_turn_pinned");
    // JavaScript Number equality does not distinguish the serde observation's
    // integer/f64 representations. Compare the value without rewriting events.
    a.check(12, route["latency_ms"].as_f64() == Some(42.0));
    a.check(
        13,
        events(&seen.statuses, "upstream_model")[0]["model"] == "claude-sonnet-5-provider-revision",
    );
    for secret in [
        "PRIVATE_DETAIL",
        "Fix a typo",
        "local-test-token-123456789",
        "upstream-secret",
        "classifier-secret",
    ] {
        a.check(14, !json!(seen.statuses).to_string().contains(secret));
    }
    a.finish();
    supplemental_transcripts(&row, &[reply], &seen);
}
#[tokio::test]
async fn original_22_overlap_isolation_and_silent_non_inference_traffic() {
    let row = case(22);
    let f = Fixture::new(22, &row).await;
    let mut replies = Vec::new();
    let mut a = Assertions::new(22);
    let result = tokio::time::timeout(BOUND, async {
        for request in row["downstream"].as_array().unwrap().iter().take(4) {
            replies.push(f.send(request).await?);
        }
        a.check(1, f.statuses.lock().unwrap().is_empty());
        let (first, second) =
            tokio::join!(f.send(&row["downstream"][4]), f.send(&row["downstream"][5]));
        replies.push(first?);
        replies.push(second?);
        Ok::<_, String>(())
    })
    .await;
    let seen = f.finish().await.unwrap();
    result.unwrap().unwrap();
    let starts = events(&seen.statuses, "request_start");
    a.check(2, starts.len() == 2);
    a.check(3, starts[0]["request_id"] != starts[1]["request_id"]);
    for start in &starts {
        a.check(
            4,
            seen.statuses
                .iter()
                .filter(|row| row["request_id"] == start["request_id"])
                .all(|row| row["session_id"] == start["session_id"]),
        );
        a.check(5, !start.as_object().unwrap().contains_key("agent_id"));
    }
    a.finish();
    assert!(
        seen.overlap,
        "both original concurrent HTTP requests must be live before either provider body completes"
    );
    supplemental_transcripts(&row, &replies, &seen);
}
#[tokio::test]
async fn original_23_sse_error_preserves_bytes_and_safe_error_metadata() {
    let row = case(23);
    let f = Fixture::new(23, &row).await;
    let result = tokio::time::timeout(BOUND, f.send(&row["downstream"][0])).await;
    f.wait_for_status("request_complete").await.unwrap();
    let seen = f.finish().await.unwrap();
    let reply = result.unwrap().unwrap();
    let mut a = Assertions::new(23);
    a.check(1, reply.status == 200);
    a.check(2, reply.bytes == response_bytes(&upstream_rows(&row)[0]));
    a.check(
        3,
        event_names(&seen.statuses)
            == [
                "request_start",
                "route",
                "upstream_response",
                "upstream_model",
                "upstream_error",
                "request_complete",
            ],
    );
    a.check(
        4,
        events(&seen.statuses, "upstream_error")[0]["error_type"] == "overloaded_error",
    );
    a.check(
        5,
        !json!(seen.statuses).to_string().contains("PRIVATE_REQUEST"),
    );
    a.check(6, !json!(seen.logs).to_string().contains("PRIVATE_REQUEST"));
    a.finish();
    supplemental_transcripts(&row, &[reply], &seen);
}
#[tokio::test]
async fn original_24_local_request_error_and_provider_fallback_metadata() {
    let row = case(24);
    let f = Fixture::new(24, &row).await;
    let mut a = Assertions::new(24);
    let result = tokio::time::timeout(BOUND, async {
        let bad = f.send(&row["downstream"][0]).await?;
        a.check(1, bad.status == 400);
        let before = f.statuses.lock().unwrap().clone();
        a.check(
            2,
            event_names(&before) == ["request_start", "request_error"],
        );
        a.check(3, before[1]["status"].as_f64() == Some(400.0));
        f.statuses.lock().unwrap().clear();
        let response = f.send(&row["downstream"][1]).await?;
        a.check(4, response.status == 429);
        f.wait_for_status("request_complete").await?;
        Ok::<_, String>(vec![bad, response])
    })
    .await;
    let seen = f.finish().await.unwrap();
    let replies = result.unwrap().unwrap();
    let route = &events(&seen.statuses, "route")[0];
    a.check(5, route["source"] == "fallback");
    a.check(6, route["classifier_error"] == "http_error");
    a.check(7, route["classifier_status"].as_f64() == Some(503.0));
    a.check(
        8,
        events(&seen.statuses, "upstream_response")[0]["status"].as_f64() == Some(429.0),
    );
    a.check(
        9,
        events(&seen.statuses, "upstream_error")[0]["error_type"] == "rate_limit_error",
    );
    a.finish();
    assert_eq!(
        seen.history.len(),
        seen.statuses.len() + 2,
        "retain original pre-reset error emissions"
    );
    assert_eq!(
        route["latency_ms"].as_f64(),
        Some(33.0),
        "deterministic fallback timing must not be projected away"
    );
    supplemental_transcripts(&row, &replies, &seen);
}
