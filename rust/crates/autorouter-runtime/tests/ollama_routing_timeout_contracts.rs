//! Original four-route contract with an actual paused native 5 ms deadline.
#[path = "support/ollama_routing_timeout.rs"]
mod support;
use autorouter_core::js_json::JsDocument;
use autorouter_core::router::RouteOptions;
use autorouter_runtime::router::Router;
use hyper::HeaderMap;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use support::{CORPUS, Replay, captured, settings, validate};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

const BOUND: Duration = Duration::from_secs(2);
struct OwnedRouter(Arc<Router<Replay>>);
impl Drop for OwnedRouter {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}
fn options(step: &Value) -> RouteOptions {
    assert_eq!(step["options"], json!({"kind":"absent","argument_count":1}));
    RouteOptions::default()
}
fn projected(mut result: Value) -> Value {
    for field in ["latency_ms", "evaluation_latency_ms"] {
        let elapsed = result
            .as_object_mut()
            .unwrap()
            .remove(field)
            .unwrap()
            .as_f64()
            .unwrap();
        assert!(elapsed.is_finite() && elapsed >= 0.0);
    }
    result
}
fn matches(actual: &Value, step: &Value) -> bool {
    step["result"]["tags"] == json!([]) && actual == &step["result"]["value"]
}
async fn execute(row: &Value) -> (Vec<Value>, Arc<Replay>) {
    let transport = Arc::new(Replay::default());
    let router = OwnedRouter(Arc::new(Router::new(transport.clone(), settings(row))));
    let mut observed = Vec::new();
    for (index, step) in row["steps"].as_array().unwrap().iter().enumerate() {
        assert_eq!(step["operation"], "route");
        assert_eq!(step["body_tags"], json!([]));
        let bytes = step["body_json"].as_str().unwrap().as_bytes();
        assert!(bytes.len() <= 65536);
        let document = Arc::new(JsDocument::parse(bytes).unwrap());
        transport.set(step);
        let before = transport.count();
        let token = CancellationToken::new();
        let _guard = token.clone().drop_guard();
        let (worker, body, cancel, settings) = (
            router.0.clone(),
            document.clone(),
            token.clone(),
            options(step),
        );
        // JoinSet owns cancellation even if an assertion or timeout panics.
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            worker
                .route(body, settings, &HeaderMap::new(), &cancel, "")
                .await
        });
        if index == 1 {
            tokio::time::timeout(BOUND, transport.wait_pending())
                .await
                .expect("inference entry barrier");
            let start = transport.clock_barrier();
            assert_eq!(transport.pending_counts(), (1, 0, 1));
            assert!(!token.is_cancelled());
            assert!(tasks.try_join_next().is_none());
            tokio::time::advance(Duration::from_millis(4)).await;
            tokio::task::yield_now().await;
            assert_eq!(start.elapsed(), Duration::from_millis(4));
            assert!(
                tasks.try_join_next().is_none(),
                "5 ms deadline fired before 5 ms"
            );
            assert_eq!(transport.pending_counts(), (1, 0, 1));
            tokio::time::advance(Duration::from_millis(1)).await;
            let actual = tokio::time::timeout(BOUND, tasks.join_next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(start.elapsed(), Duration::from_millis(5));
            assert_eq!(
                transport.pending_counts(),
                (1, 1, 0),
                "deadline dropped the owned pending request"
            );
            observed.push(projected(actual));
        } else {
            observed.push(projected(
                tokio::time::timeout(BOUND, tasks.join_next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap()
                    .unwrap(),
            ));
        }
        assert!(
            !token.is_cancelled(),
            "caller cancellation is not the timeout source"
        );
        assert!(tasks.is_empty());
        assert_eq!(
            document.stringify().as_bytes(),
            bytes,
            "original request unchanged"
        );
        assert_eq!(transport.count() - before, 2);
        assert!(
            transport.idle(),
            "all request/response owners dropped before next route"
        );
    }
    assert_eq!(transport.count(), 8);
    assert_eq!(transport.pending_counts(), (1, 1, 0));
    router.0.shutdown();
    assert!(transport.idle());
    (observed, transport)
}
#[tokio::test(start_paused = true)]
async fn same_router_real_five_ms_timeout_recovers_and_preserves_adaptive_guard() {
    let (row, _) = captured();
    let steps = row["steps"].as_array().unwrap();
    assert_eq!(steps[1]["body_json"], steps[2]["body_json"]);
    let mut guarded: Value = serde_json::from_str(steps[2]["body_json"].as_str().unwrap()).unwrap();
    guarded["thinking"] = json!({"type":"adaptive"});
    assert_eq!(
        guarded,
        serde_json::from_str::<Value>(steps[3]["body_json"].as_str().unwrap()).unwrap()
    );
    let (actual, transport) = execute(&row).await;
    assert!(transport.verified(), "independent full I/O transcript");
    for (actual, expected) in actual.iter().zip(steps) {
        assert!(
            matches(actual, expected),
            "complete result mismatch: {actual}"
        );
    }
}
#[tokio::test(start_paused = true)]
async fn matching_timeout_cannot_hide_wrong_inference_request() {
    for mode in ["body", "url"] {
        let (mut row, _) = captured();
        let request = &mut row["steps"][1]["requests"][1];
        if mode == "body" {
            request["parsed_body"]["state"]["current_task"] = json!("wrong captured task");
            request["body"] = json!(request["parsed_body"].to_string());
        } else {
            request["url"] = json!("http://127.0.0.1:11434/wrong-inference");
        }
        let (actual, transport) = execute(&row).await;
        for (actual, expected) in actual.iter().zip(row["steps"].as_array().unwrap()) {
            assert!(matches(actual, expected));
        }
        assert!(!transport.verified(), "matching timeout hid wrong {mode}");
    }
}
#[tokio::test(start_paused = true)]
async fn complete_results_reject_wrong_timeout_cache_and_guard_categories() {
    let (row, _) = captured();
    let (actual, transport) = execute(&row).await;
    assert!(transport.verified());
    for (index, field, value) in [
        (1, "classifier_error", json!("network_error")),
        (1, "classified_tier", json!("sonnet")),
        (2, "source", json!("cache")),
        (3, "model", json!("claude-haiku-4-5-20251001")),
        (3, "reason", json!("classified")),
        (3, "classifier_error", json!("timeout")),
    ] {
        let mut wrong = row["steps"][index].clone();
        wrong["result"]["value"][field] = value;
        assert!(!matches(&actual[index], &wrong), "wrong {field} accepted");
    }
}
#[test]
fn immutable_schedule_rejects_config_option_and_corpus_mutation() {
    let (row, capture) = captured();
    for mode in ["timeout", "options", "missing_step", "error_slot"] {
        let mut changed = row.clone();
        match mode {
            "timeout" => changed["config"]["ollamaTimeoutMs"] = json!(20),
            "options" => {
                changed["steps"][1]["options"] = json!({"kind":"present","argument_count":2})
            }
            "missing_step" => {
                changed["steps"].as_array_mut().unwrap().pop();
            }
            "error_slot" => {
                changed["steps"][1]["requests"][1]["outcome"]["name"] = json!("NetworkError")
            }
            _ => unreachable!(),
        }
        let changed = changed.to_string() + "\n";
        let mut metadata = capture.clone();
        metadata["cases_sha256"] = json!(format!("{:x}", Sha256::digest(changed.as_bytes())));
        assert!(validate(&changed, &metadata).is_err());
    }
    let mut wrong = capture.clone();
    wrong["direct_executed_assertions"] = json!(11);
    assert_eq!(validate(CORPUS, &wrong).unwrap_err(), "capture inventory");
    assert_eq!(
        validate(&(CORPUS.to_owned() + " "), &capture).unwrap_err(),
        "immutable corpus identity"
    );
    assert_eq!(
        validate(&" ".repeat(1024 * 1024 + 1), &capture).unwrap_err(),
        "corpus byte bound"
    );
}
