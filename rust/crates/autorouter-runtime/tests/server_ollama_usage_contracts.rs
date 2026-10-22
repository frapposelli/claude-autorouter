//! Complete frozen server #29–#32 Ollama cancellation and usage/pricing schedules.
#[allow(dead_code)] // Shared helpers retain separate policies used by sibling targets.
#[path = "support/server_contract.rs"]
mod contract;
use contract::{
    Reply, compare_metadata, compare_request, decode, event_names, events, headers,
    javascript_number_equality, parsed, response_bytes, sink, upstream_rows,
};
#[path = "support/server_ollama_usage.rs"]
mod ollama;
#[allow(dead_code)] // Shared peer also supplies streaming interfaces to other targets.
#[path = "support/server_streaming_routing.rs"]
mod peer;
use autorouter_core::config::read_config;
use autorouter_runtime::{
    http_client::NativeHttpClient,
    server::{Gateway, GatewayHandle},
    server_events::EventSinks,
};
use bytes::Bytes;
use peer::{Observation, Peer, ResponsePlan};
use serde_json::{Value, json};
use std::{
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
    contract::captured_server_case(
        number,
        CORPUS,
        CAPTURE,
        [
            "4fb38321dcbf5b071841529ca25bdb3af0b077d888b4359718697885c4b02b9a",
            "360be4af55d128b570698731c9653f01e9e158548c966694942e154056d9905e",
        ],
        (26, 36, Some(4)),
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
        let peer = Peer::start(contract::indexed_responder(row, ResponsePlan::chunks))
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
        })
    }
}
use contract::SharedAssertions as Assertions;
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
        let raw = json!({"schema_version":1,"status":"passed","source_test":row["source_test"],"upstream_requests":seen.requests.iter().map(contract::capture_request).collect::<Vec<_>>(),"route_calls":seen.routes,"complete_calls":seen.completions,"statuses":seen.statuses,"all_emitted_statuses":seen.history,"logs":seen.logs,"downstream":replies.iter().map(contract::capture_reply).collect::<Vec<_>>(),"projection":"None. Actual generated IDs, measured times, deterministic routing times, headers and complete bytes retained."});
        contract::write_row_capture(&directory, &row["id"], &raw, LIMIT, true);
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
        contract::write_row_capture(&directory, &row["id"], raw, LIMIT, false);
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
