//! Original finite Claude-shaped routing contracts; no services or inference.
#[path = "support/ollama_routing.rs"]
mod support;
use autorouter_core::js_json::JsDocument;
use autorouter_core::router::RouteOptions;
use autorouter_runtime::router::Router;
use hyper::HeaderMap;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use support::{Replay, cases, corpus, settings, validate_corpus};
use tokio_util::sync::CancellationToken;

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
fn options(step: &Value) -> RouteOptions {
    assert_eq!(step["option_tags"], json!([]));
    let source = step["options"].as_object().unwrap();
    assert!(
        source
            .keys()
            .all(|k| ["scope", "promptId", "requestClass"].contains(&k.as_str()))
    );
    let options = RouteOptions {
        scope: source.get("scope").unwrap().as_str().unwrap().into(),
        prompt_id: source
            .get("promptId")
            .map(|x| x.as_str().unwrap())
            .unwrap_or("")
            .into(),
        request_class: source
            .get("requestClass")
            .map(|x| x.as_str().unwrap())
            .unwrap_or("")
            .into(),
        ..Default::default()
    };
    let mut roundtrip = json!({"scope":options.scope});
    if source.contains_key("promptId") {
        roundtrip["promptId"] = json!(options.prompt_id);
    }
    if source.contains_key("requestClass") {
        roundtrip["requestClass"] = json!(options.request_class);
    }
    assert_eq!(roundtrip, step["options"]);
    options
}
async fn step(router: &Router<Replay>, step: &Value) -> Value {
    assert_eq!(step["operation"], "route");
    assert_eq!(step["body_tags"], json!([]));
    let bytes = step["body_json"].as_str().unwrap().as_bytes();
    assert!(bytes.len() <= 64 * 1024);
    let document = Arc::new(JsDocument::parse(bytes).unwrap());
    let token = CancellationToken::new();
    let _guard = token.clone().drop_guard();
    let mut result = tokio::time::timeout(
        Duration::from_secs(2),
        router.route(
            document.clone(),
            options(step),
            &HeaderMap::new(),
            &token,
            "",
        ),
    )
    .await
    .expect("Finite routing step deadline")
    .unwrap();
    assert_eq!(
        document.stringify().as_bytes(),
        bytes,
        "Original request unchanged"
    );
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
fn matches(step: &Value, actual: &Value) -> bool {
    assert_eq!(step["result"]["tags"], json!([]));
    actual == &step["result"]["value"]
}
async fn replay(number: usize) -> (usize, usize, usize) {
    let mut totals = (0, 0, 0);
    for row in cases()
        .into_iter()
        .filter(|r| r["source_test"] == format!("test/ollama-routing.test.mjs#{number}"))
    {
        let (router, transport) = instance(&row);
        let steps = row["steps"].as_array().unwrap();
        assert!(!steps.is_empty() && steps.len() <= 3);
        for expected in steps {
            let requests = expected["requests"].as_array().unwrap().clone();
            transport.set(requests.clone());
            let before = transport.count();
            let actual = step(&router.0, expected).await;
            assert!(
                matches(expected, &actual),
                "Complete result {}: {actual}",
                row["id"]
            );
            assert!(
                transport.verified(),
                "Complete request transcript {}",
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
async fn all_six_claude_shapes_route_across_four_original_model_tags() {
    assert_eq!(replay(1).await, (24, 24, 48));
}
#[tokio::test]
async fn same_router_switches_human_turns_from_sonnet_to_opus_to_haiku() {
    assert_eq!(replay(2).await, (1, 3, 6));
}
#[tokio::test]
async fn signed_thinking_retains_sonnet_while_exposing_fresh_haiku_classification() {
    assert_eq!(replay(4).await, (1, 2, 4));
}
#[tokio::test]
async fn matching_results_cannot_hide_wrong_requests_or_missing_calls() {
    for mode in ["body", "url", "missing", "extra"] {
        let row = cases().remove(0);
        let (router, transport) = instance(&row);
        let expected = &row["steps"][0];
        let mut requests = expected["requests"].as_array().unwrap().clone();
        match mode {
            "body" => {
                requests[1]["body"]["state"]["current_task"] = json!("wrong captured task");
                requests[1]["body_text"] = json!(requests[1]["body"].to_string());
            }
            "url" => requests[0]["url"] = json!("http://127.0.0.1:11434/wrong-metadata"),
            "missing" => {
                requests.pop();
            }
            "extra" => {}
            _ => unreachable!(),
        }
        if mode == "extra" {
            // A real repeated call must hit this router's cache; a forged expected
            // extra HTTP transcript cannot be silently accepted as a cache hit.
            transport.set(requests.clone());
            let actual = step(&router.0, expected).await;
            assert!(matches(expected, &actual));
            assert!(transport.verified());
        }
        transport.set(requests);
        let before = transport.count();
        let actual = step(&router.0, expected).await;
        if ["body", "url"].contains(&mode) {
            assert!(matches(expected, &actual), "Mode {mode}: {actual}");
        }
        if mode == "extra" {
            let mut cached = expected.clone();
            cached["result"]["value"]["source"] = json!("cache");
            cached["result"]["value"]["continuity_state"] = json!("selected");
            assert!(matches(&cached, &actual));
            assert_eq!(transport.count(), before, "Cached route makes no fetch");
        }
        assert!(!transport.verified(), "Wrong transcript accepted: {mode}");
        router.0.shutdown();
        assert!(transport.idle());
    }
}
#[tokio::test]
async fn complete_results_reject_stale_turns_and_lost_thinking_preservation() {
    for number in [2, 4] {
        let row = cases()
            .into_iter()
            .find(|r| r["source_test"] == format!("test/ollama-routing.test.mjs#{number}"))
            .unwrap();
        let (router, transport) = instance(&row);
        for (index, expected) in row["steps"].as_array().unwrap().iter().enumerate() {
            transport.set(expected["requests"].as_array().unwrap().clone());
            let actual = step(&router.0, expected).await;
            assert!(matches(expected, &actual));
            assert!(transport.verified());
            if index > 0 {
                let mut wrong = expected.clone();
                wrong["result"]["value"]["model"] = if number == 2 {
                    row["steps"][index - 1]["result"]["value"]["model"].clone()
                } else {
                    json!("claude-haiku-4-5-20251001")
                };
                if number == 4 {
                    wrong["result"]["value"]["reason"] = json!("classified");
                }
                assert!(!matches(&wrong, &actual));
            }
        }
        router.0.shutdown();
        assert!(transport.idle());
    }
}
#[test]
fn corpus_pin_and_inventory_reject_self_consistent_mutation_and_missing_cases() {
    let (text, report) = corpus();
    let mut rows = cases();
    rows[0]["steps"][0]["options"]["scope"] = json!("changed-session");
    let forged = rows
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let mut changed = report.clone();
    changed["cases_sha256"] = json!(format!("{:x}", Sha256::digest(forged.as_bytes())));
    assert!(validate_corpus(&forged, &changed).is_err());
    // The actual same-session schedule includes each original human prompt ID;
    // losing it must fail immutable input admission, independently of outcomes.
    let original = cases();
    for remove in [false, true] {
        let mut rows = original.clone();
        let step = &mut rows[24]["steps"][1];
        if remove {
            step["options"].as_object_mut().unwrap().remove("promptId");
        } else {
            step["options"]["promptId"] = json!("changed-prompt");
        }
        assert_ne!(
            options(step).prompt_id,
            options(&original[24]["steps"][1]).prompt_id
        );
        let changed_text = rows
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let mut changed_report = report.clone();
        changed_report["cases_sha256"] =
            json!(format!("{:x}", Sha256::digest(changed_text.as_bytes())));
        assert!(validate_corpus(&changed_text, &changed_report).is_err());
    }
    assert!(
        validate_corpus(
            &text.lines().skip(1).collect::<Vec<_>>().join("\n"),
            &report
        )
        .is_err()
    );
    changed = report.clone();
    changed["tests"][0]["case_ids"][0] = json!("duplicate");
    assert!(validate_corpus(text, &changed).is_err());
    assert_eq!(
        validate_corpus(&"x".repeat(8 * 1024 * 1024 + 1), &report).unwrap_err(),
        "Corpus byte bound"
    );
}
