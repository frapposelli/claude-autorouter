//! Exact frozen report/parser assertions and real native routing over mock I/O.
use autorouter_core::config::read_config;
use autorouter_core::evaluation_report::{EvaluationPolicy, create_evaluation_policy};
use autorouter_runtime::http_client::{HttpError, HttpTransport};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;
#[path = "../../crates/autorouter-core/tests/support/evaluation_contract.rs"]
mod support;
use support::*;

#[test]
fn both_parsers_replay_all_original_threshold_and_deadline_arguments() {
    let mut count = 0;
    for row in cases() {
        let op = row["op"].as_str().unwrap();
        if !["parse_evaluation_args", "parse_ollama_evaluation_args"].contains(&op) {
            continue;
        }
        let args: Vec<String> = row["input"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().into())
            .collect();
        let actual = outcome(if op == "parse_evaluation_args" {
            crate::evaluation::parse_args(&args)
        } else {
            crate::ollama_evaluation::parse_args(&args, Path::new("/synthetic"))
        });
        assert!(
            equal(&actual, &row["node_expected"]),
            "{}: {actual} != {}",
            row["id"],
            row["node_expected"]
        );
        count += 1;
    }
    assert_eq!(count, 52);
}

struct Replay {
    expected: Mutex<VecDeque<Value>>,
    calls: AtomicUsize,
    invalid: AtomicUsize,
}
impl Replay {
    fn verified(&self, expected_calls: usize) -> bool {
        self.invalid.load(Ordering::SeqCst) == 0
            && self.calls.load(Ordering::SeqCst) == expected_calls
            && self.expected.lock().unwrap().is_empty()
    }
}
fn request_matches(
    url: &str,
    method: &str,
    headers: &Value,
    body: &Value,
    expected: &Value,
) -> bool {
    url == expected["url"]
        && method == expected["method"]
        && equal(headers, &expected["headers"])
        && equal(
            body,
            &serde_json::from_str::<Value>(expected["body"].as_str().unwrap()).unwrap(),
        )
}
impl HttpTransport for Replay {
    type ResponseBody = Full<Bytes>;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let expected = self.expected.lock().unwrap().pop_front();
        let Some(expected) = expected else {
            self.invalid.fetch_add(1, Ordering::SeqCst);
            return Err(HttpError::Network);
        };
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let headers: serde_json::Map<String, Value> = parts
            .headers
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v.to_str().unwrap())))
            .collect();
        if !request_matches(
            &parts.uri.to_string(),
            parts.method.as_str(),
            &json!(headers),
            &body,
            &expected,
        ) {
            self.invalid.fetch_add(1, Ordering::SeqCst);
            return Err(HttpError::Network);
        }
        if expected.get("error_name").is_some() {
            return Err(HttpError::Network);
        }
        Ok(Response::builder()
            .status(expected["status"].as_u64().unwrap() as u16)
            .body(Full::new(Bytes::from(
                expected["response"].as_str().unwrap().to_owned(),
            )))
            .unwrap())
    }
}
fn strip_time(mut report: Value) -> Value {
    report.as_object_mut().unwrap().remove("routing_p50_ms");
    report.as_object_mut().unwrap().remove("routing_p95_ms");
    for row in report["rows"].as_array_mut().unwrap() {
        row.as_object_mut().unwrap().remove("ms");
    }
    report
}
#[tokio::test]
async fn actual_native_runner_replays_both_profiles_override_outage_and_collapsed_quality() {
    let mut runs = 0;
    let mut requests = 0;
    for row in cases()
        .into_iter()
        .filter(|row| row["op"] == "run_evaluation")
    {
        let input = &row["input"];
        let config = read_config(&input["env"], false, Path::new("/synthetic")).unwrap();
        let policy = match create_evaluation_policy(Some(&input["policy"])) {
            Ok(policy) => policy,
            Err(_) => {
                // Feed the original invalid threshold through the real runner's
                // admission check; do not replace it with a precomputed error.
                assert_eq!(input["policy"], json!({"minAgreement":2}));
                EvaluationPolicy {
                    min_agreement: 2.0,
                    ..EvaluationPolicy::default()
                }
            }
        };
        let expected = row["requests"].as_array().unwrap();
        let mock = Arc::new(Replay {
            expected: Mutex::new(expected.iter().cloned().collect()),
            calls: AtomicUsize::new(0),
            invalid: AtomicUsize::new(0),
        });
        let before = input["cases"].to_string();
        assert_eq!(digest(before.as_bytes()), row["cases_before_sha256"]);
        let result = crate::evaluation::run_evaluation(
            mock.clone(),
            &config,
            &input["cases"],
            policy,
            &CancellationToken::new(),
        )
        .await;
        let actual = outcome(result.map(strip_time));
        assert!(
            mock.verified(expected.len()),
            "{}: incomplete or incorrect requests",
            row["id"]
        );
        assert!(
            equal(&actual, &row["node_expected"]),
            "{}: actual={actual} expected={}",
            row["id"],
            row["node_expected"]
        );
        assert_eq!(
            digest(input["cases"].to_string().as_bytes()),
            row["cases_after_sha256"]
        );
        requests += expected.len();
        runs += 1;
    }
    assert_eq!((runs, requests), (6, 45));
}
#[tokio::test]
async fn runtime_comparison_rejects_missing_auth_or_changed_task_even_on_expected_outage() {
    let row = cases()
        .into_iter()
        .find(|r| r["op"] == "run_evaluation" && !r["requests"].as_array().unwrap().is_empty())
        .unwrap();
    let request = &row["requests"][0];
    let body: Value = serde_json::from_str(request["body"].as_str().unwrap()).unwrap();
    let url = request["url"].as_str().unwrap();
    let method = request["method"].as_str().unwrap();
    assert!(request_matches(
        url,
        method,
        &request["headers"],
        &body,
        request
    ));
    assert!(!request_matches(url, method, &json!({}), &body, request));
    assert!(!request_matches(
        url,
        method,
        &request["headers"],
        &json!({}),
        request
    ));
    assert!(!request_matches(
        "https://invalid.example/",
        method,
        &request["headers"],
        &body,
        request
    ));
    let outage = cases()
        .into_iter()
        .find(|row| {
            row["op"] == "run_evaluation"
                && row["requests"].as_array().is_some_and(|requests| {
                    !requests.is_empty()
                        && requests
                            .iter()
                            .all(|request| request.get("error_name").is_some())
                })
        })
        .unwrap();
    let mut expected = outage["requests"].as_array().unwrap().clone();
    for request in &mut expected {
        request["headers"] = json!({});
    }
    let count = expected.len();
    let mock = Arc::new(Replay {
        expected: Mutex::new(expected.into_iter().collect()),
        calls: AtomicUsize::new(0),
        invalid: AtomicUsize::new(0),
    });
    let input = &outage["input"];
    let config = read_config(&input["env"], false, Path::new("/synthetic")).unwrap();
    let result = crate::evaluation::run_evaluation(
        mock.clone(),
        &config,
        &input["cases"],
        create_evaluation_policy(Some(&input["policy"])).unwrap(),
        &CancellationToken::new(),
    )
    .await;
    // Every corrupted request still becomes the expected fallback report. The
    // independent request proof must reject this otherwise matching output.
    assert!(equal(
        &outcome(result.map(strip_time)),
        &outage["node_expected"]
    ));
    assert_eq!(mock.invalid.load(Ordering::SeqCst), count);
    assert!(!mock.verified(count));
}
