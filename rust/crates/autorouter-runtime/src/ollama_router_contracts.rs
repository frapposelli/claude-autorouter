//! Complete frozen Ollama classifier/router schedules; no substituted decisions.
use super::*;
use crate::evaluator::evaluate_serialized_answer;
use std::time::Duration;
#[path = "ollama_router_contracts_support.rs"]
mod support;
use support::{Replay, cases, settings};

struct OwnedRouter(Router<Replay>);
impl Drop for OwnedRouter {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}
fn instance(row: &Value) -> (OwnedRouter, Arc<Replay>) {
    let replay = Arc::new(Replay::default());
    (
        OwnedRouter(Router::new(replay.clone(), settings(row))),
        replay,
    )
}
async fn step(router: &Router<Replay>, transport: &Replay, step: &Value) -> Value {
    let token = CancellationToken::new();
    struct Cancel(CancellationToken);
    impl Drop for Cancel {
        fn drop(&mut self) {
            self.0.cancel();
        }
    }
    let _cancel = Cancel(token.clone());
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut result = match step["operation"].as_str().unwrap() {
            "evaluate" => {
                assert_eq!(step["state_tags"], json!([]));
                let state = step["state_json"].as_str().unwrap();
                assert!(state.len() <= 3000);
                serde_json::to_value(
                    evaluate_serialized_answer(transport, &router.config, state, &token)
                        .await
                        .unwrap(),
                )
                .unwrap()
            }
            operation => {
                assert_eq!(step["body_tags"], json!([]));
                let bytes = step["body_json"].as_str().unwrap().as_bytes();
                assert!(bytes.len() <= 64 * 1024);
                let document = Arc::new(JsDocument::parse(bytes).unwrap());
                let result = if operation == "classify" {
                    serde_json::to_value(
                        router
                            .classifier
                            .classify(&document, &router.config, &token)
                            .await
                            .unwrap(),
                    )
                    .unwrap()
                } else {
                    assert_eq!(operation, "route");
                    let actual = router
                        .route(
                            document.clone(),
                            RouteOptions::default(),
                            &HeaderMap::new(),
                            &token,
                            "",
                        )
                        .await
                        .unwrap();
                    assert_eq!(step["classification"]["tags"], json!([]));
                    for (key, value) in step["classification"]["value"].as_object().unwrap() {
                        assert_eq!(
                            &actual[key], value,
                            "Nested real classification field {key}"
                        );
                    }
                    actual
                };
                assert_eq!(document.stringify().as_bytes(), bytes);
                result
            }
        };
        if step["operation"] == "route" {
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
        }
        assert_eq!(router.classifier.pending_counts(), (0, 0));
        result
    })
    .await
    .expect("Finite Ollama Router step deadline")
}

async fn replay(selected: &[usize]) -> (usize, usize, usize) {
    let mut totals = (0, 0, 0);
    for row in cases() {
        let number = row["source_test"]
            .as_str()
            .unwrap()
            .rsplit('#')
            .next()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        if !selected.contains(&number) {
            continue;
        }
        let (router, transport) = instance(&row);
        let steps = row["steps"].as_array().unwrap();
        assert!(!steps.is_empty() && steps.len() <= 2);
        for expected in steps {
            let requests = expected["requests"].as_array().unwrap().clone();
            transport.set(requests.clone());
            let before = transport.count();
            let actual = step(&router.0, &transport, expected).await;
            assert_eq!(expected["result"]["tags"], json!([]));
            assert_eq!(actual, expected["result"]["value"], "{}", row["id"]);
            assert!(
                transport.verified(),
                "Complete I/O verification {}",
                row["id"]
            );
            assert_eq!(transport.count() - before, requests.len());
            totals.1 += 1;
            totals.2 += requests.len();
        }
        router.0.shutdown();
        assert!(transport.idle());
        totals.0 += 1;
    }
    totals
}
#[tokio::test]
async fn original_three_tiers_use_frozen_policy_and_identical_second_calls_hit_cache() {
    assert_eq!(replay(&[4]).await, (4, 7, 8));
}
#[tokio::test]
async fn original_malformed_and_disabled_deadline_failures_retry_same_opus_body_uncached() {
    assert_eq!(replay(&[5, 14]).await, (16, 32, 64));
}
#[tokio::test]
async fn original_metadata_guards_route_without_prompt_and_preserve_separate_megabyte_bound() {
    assert_eq!(replay(&[12, 13]).await, (6, 6, 7));
}

#[tokio::test]
async fn matching_results_cannot_hide_wrong_requests_or_cache_retry_call_counts() {
    for (id, index, mode) in [
        ("baseline-ollama-router-12-1", 0, "metadata"),
        ("baseline-ollama-router-4-1", 0, "state"),
        ("baseline-ollama-router-4-1", 1, "cache"),
        ("baseline-ollama-router-5-1", 1, "retry"),
    ] {
        let row = cases().into_iter().find(|row| row["id"] == id).unwrap();
        let (router, transport) = instance(&row);
        for (n, expected) in row["steps"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .take(index + 1)
        {
            let mut requests = expected["requests"].as_array().unwrap().clone();
            if n == index {
                match mode {
                    "metadata" => {
                        requests[0]["url"] = json!("http://127.0.0.1:11434/wrong-metadata")
                    }
                    "state" | "retry" => {
                        requests[1]["body"]["state"]["current_task"] =
                            json!("wrong synthetic task");
                        requests[1]["body_text"] = json!(requests[1]["body"].to_string());
                    }
                    "cache" => requests.push(row["steps"][0]["requests"][0].clone()),
                    _ => unreachable!(),
                }
            }
            transport.set(requests);
            let actual = step(&router.0, &transport, expected).await;
            assert_eq!(
                actual, expected["result"]["value"],
                "Scripted outcome stays identical"
            );
            assert_eq!(
                transport.verified(),
                n != index,
                "Independent request/count verification"
            );
        }
    }
}
#[tokio::test]
async fn oversized_metadata_control_detects_wrong_acceptance_and_inference_attempt() {
    let row = cases()
        .into_iter()
        .find(|row| row["id"] == "baseline-ollama-router-13-2")
        .unwrap();
    let (router, transport) = instance(&row);
    let expected = &row["steps"][0];
    let original = expected["requests"][0]["response"]["body"]["text"]
        .as_str()
        .unwrap();
    assert_eq!(original.len(), 1_048_625);
    let mut requests = expected["requests"].as_array().unwrap().clone();
    requests[0]["response"]["body"]["text"] =
        json!(json!({"details":{"parameter_size":"4B"},"tensors":"x".repeat(80_000)}).to_string());
    transport.set(requests);
    let actual = step(&router.0, &transport, expected).await;
    assert_ne!(actual, expected["result"]["value"]);
    assert_eq!(
        transport.count(),
        2,
        "Valid smaller metadata starts inference"
    );
    assert!(!transport.verified());
    assert!(transport.idle());
}
