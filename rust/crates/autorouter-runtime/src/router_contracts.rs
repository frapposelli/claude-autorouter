//! Real native classifier/count/cache execution against captured synthetic I/O.
//! Captured decisions are expected output here, never supplied classifier input.
use super::*;
use crate::http_client::HttpError;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

#[path = "../../autorouter-core/tests/support/router_contract.rs"]
mod support;
use support::*;

#[derive(Default)]
struct Responses {
    evaluations: VecDeque<Value>,
    counts: VecDeque<Value>,
    observed: Vec<&'static str>,
}
struct Replay {
    dictionary: Arc<Value>,
    current: Mutex<Responses>,
    active: AtomicUsize,
    started: AtomicUsize,
    entered: Notify,
    invalid: AtomicUsize,
}
struct ActiveRequest<'a>(&'a AtomicUsize);
fn request_matches(
    url: &str,
    method: &str,
    headers: &Value,
    payload: &Value,
    expected: &Value,
    expected_body: &Value,
) -> bool {
    url == expected["url"]
        && method == expected["method"]
        && *headers == expected["headers"]
        && payload == expected_body
}
impl Drop for ActiveRequest<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Replay {
    fn set(&self, step: &Value) {
        assert_eq!(self.active.load(Ordering::SeqCst), 0);
        let mut current = self.current.lock().unwrap();
        assert!(current.evaluations.is_empty() && current.counts.is_empty());
        *current = Responses {
            evaluations: step["evaluator_calls"]
                .as_array()
                .unwrap()
                .iter()
                .cloned()
                .collect(),
            counts: step["count_calls"]
                .as_array()
                .unwrap()
                .iter()
                .cloned()
                .collect(),
            observed: Vec::new(),
        };
    }
    fn checked(&self, step: &Value) {
        // A request mismatch must survive even if an evaluator catches the
        // synthetic network error and produces an otherwise expected fallback.
        assert_eq!(
            self.invalid.load(Ordering::SeqCst),
            0,
            "Native request contract mismatch"
        );
        assert_eq!(self.active.load(Ordering::SeqCst), 0);
        let current = self.current.lock().unwrap();
        assert!(
            current.evaluations.is_empty(),
            "Missing native evaluator request"
        );
        assert!(current.counts.is_empty(), "Missing native count request");
        assert_eq!(
            current
                .observed
                .iter()
                .filter(|&&kind| kind == "evaluate")
                .count(),
            step["evaluator_calls"].as_array().unwrap().len()
        );
        assert_eq!(
            current
                .observed
                .iter()
                .filter(|&&kind| kind == "count")
                .count(),
            step["count_calls"].as_array().unwrap().len()
        );
    }
}
impl HttpTransport for Replay {
    type ResponseBody = Full<Bytes>;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        self.active.fetch_add(1, Ordering::SeqCst);
        let _active = ActiveRequest(&self.active);
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        let payload: Value = serde_json::from_slice(&bytes).unwrap();
        let is_count = parts.uri.path().ends_with("count_tokens");
        let captured = {
            let mut current = self.current.lock().unwrap();
            current
                .observed
                .push(if is_count { "count" } else { "evaluate" });
            if is_count {
                current
                    .counts
                    .pop_front()
                    .expect("Unexpected native counting request")
            } else {
                current
                    .evaluations
                    .pop_front()
                    .expect("Unexpected native evaluator request")
            }
        };
        let expected = if is_count {
            &captured["prepared_request"]
        } else {
            &captured
        };
        let headers: serde_json::Map<String, Value> = parts
            .headers
            .iter()
            .map(|(key, value)| (key.to_string(), json!(value.to_str().unwrap())))
            .collect();
        let expected_body = if is_count {
            document(expected, &self.dictionary).to_serde_observation_lossy()
        } else {
            serde_json::from_str(expected["body"].as_str().unwrap()).unwrap()
        };
        if !request_matches(
            &parts.uri.to_string(),
            parts.method.as_str(),
            &json!(headers),
            &payload,
            expected,
            &expected_body,
        ) {
            self.invalid.fetch_add(1, Ordering::SeqCst);
            return Err(HttpError::Network);
        }
        self.started.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        if is_count {
            let outcome = &captured["outcome"];
            // The API-only nonfinite/undefined cases remain explicit partial
            // boundaries; here invalid/missing HTTP counts exercise Option::None.
            let response = if outcome["kind"] == "json" {
                json!({"input_tokens":outcome["value"]})
            } else {
                json!({})
            };
            Ok(Response::new(Full::new(Bytes::from(response.to_string()))))
        } else if captured["error_name"] == "TimeoutError" {
            std::future::pending().await
        } else if captured.get("error_name").is_some() {
            Err(HttpError::Network)
        } else {
            Ok(Response::builder()
                .status(captured["status"].as_u64().unwrap() as u16)
                .body(Full::new(Bytes::from(
                    captured["response"].as_str().unwrap().to_owned(),
                )))
                .unwrap())
        }
    }
}

async fn replay(definitions: &[usize], timed: bool) -> (usize, usize) {
    assert_eq!(digest(CASES.as_bytes()), CASES_SHA256);
    let mut lines = CASES.lines();
    let dictionary = Arc::new(serde_json::from_str::<Value>(lines.next().unwrap()).unwrap());
    let mut instances = 0;
    let mut routes = 0;
    for line in lines {
        let row: Value = serde_json::from_str(line).unwrap();
        if row["kind"] != "router" {
            continue;
        }
        let input = &row["input"];
        let number: usize = input["source_test"]
            .as_str()
            .unwrap()
            .rsplit('#')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        if !definitions.contains(&number) {
            continue;
        }
        instances += 1;
        let transport = Arc::new(Replay {
            dictionary: dictionary.clone(),
            current: Mutex::new(Responses::default()),
            active: AtomicUsize::new(0),
            started: AtomicUsize::new(0),
            entered: Notify::new(),
            invalid: AtomicUsize::new(0),
        });
        let clock = Arc::new(AtomicU64::new(0));
        let now = clock.clone();
        let router = Arc::new(Router::with_clock(
            transport.clone(),
            configuration(&input["config"]),
            Arc::new(move || now.load(Ordering::SeqCst)),
        ));
        for step in input["steps"].as_array().unwrap() {
            routes += 1;
            transport.set(step);
            clock.store(step["now"].as_u64().unwrap(), Ordering::SeqCst);
            let body = Arc::new(document(step, &dictionary));
            let options = &step["options"];
            let options = RouteOptions {
                scope: text(options, "scope").into(),
                prompt_id: text(options, "promptId").into(),
                request_class: text(options, "requestClass").into(),
                request_id: options["requestId"].as_str().map(str::to_owned),
                count_tokens: step["count_present"] == true,
            };
            let task_router = router.clone();
            let task_body = body.clone();
            let started = transport.started.load(Ordering::SeqCst);
            let timeout = step["evaluator_calls"]
                .as_array()
                .unwrap()
                .iter()
                .any(|call| call["error_name"] == "TimeoutError");
            let task = tokio::spawn(async move {
                task_router
                    .route(
                        task_body,
                        options,
                        &HeaderMap::new(),
                        &CancellationToken::new(),
                        "",
                    )
                    .await
            });
            if timeout {
                assert!(timed, "Timeout replay requires a paused clock");
                while transport.started.load(Ordering::SeqCst) == started {
                    transport.entered.notified().await;
                }
                assert!(!task.is_finished());
                tokio::time::advance(Duration::from_millis(19)).await;
                tokio::task::yield_now().await;
                assert!(!task.is_finished(), "20ms deadline fired early");
                tokio::time::advance(Duration::from_millis(1)).await;
            }
            let mut actual = task.await.unwrap().unwrap();
            actual.as_object_mut().unwrap().remove("latency_ms");
            actual
                .as_object_mut()
                .unwrap()
                .remove("evaluation_latency_ms");
            assert_eq!(actual, step["node_expected"], "{} route{routes}", row["id"]);
            assert_eq!(
                digest(body.stringify().as_bytes()),
                step["body_after_sha256"]
            );
            transport.checked(step);
        }
        router.shutdown();
        assert_eq!(transport.active.load(Ordering::SeqCst), 0);
    }
    assert!(instances > 0 && routes > 0);
    (instances, routes)
}

#[tokio::test]
async fn all_jev_tiers_guards_and_cache_use_exact_request_contracts() {
    assert_eq!(
        replay(&[1, 2, 3, 8, 10, 14, 35, 47, 48], false).await,
        (23, 38)
    );
}
#[tokio::test]
async fn bypass_review_and_large_context_preserve_exact_payloads_and_call_totals() {
    assert_eq!(
        replay(&[4, 5, 6, 9, 24, 33, 36, 46], false).await,
        (36, 108)
    );
}
#[tokio::test]
async fn full_continuity_schedules_and_actual_outages_retain_safe_model_ownership() {
    assert_eq!(
        replay(
            &[
                7, 12, 13, 15, 19, 20, 21, 22, 23, 25, 26, 27, 28, 29, 30, 31, 32, 34, 43, 44, 45
            ],
            false
        )
        .await,
        (41, 94)
    );
}
#[tokio::test(start_paused = true)]
async fn twenty_ms_timeout_cleans_transport_and_same_body_retries_without_cached_failure() {
    assert_eq!(replay(&[11], true).await, (1, 2));
}
#[tokio::test]
async fn exact_count_requests_cover_both_source_models_and_invalid_count_adaptations() {
    assert_eq!(replay(&[37, 38, 39, 41, 42, 49, 50], false).await, (18, 18));
}
#[tokio::test]
async fn small_and_large_hard_locked_requests_never_start_count_transport() {
    // Full captured decisions/count totals are checked. The source's callback
    // invocation order is not claimed from asynchronous transport arrival.
    assert_eq!(replay(&[51], false).await, (2, 3));
}

#[test]
fn native_request_comparison_rejects_lost_payload_auth_and_count_projection_fields() {
    let expected = json!({"url":"https://api.typesafe.ai/v1/systemone","method":"POST","headers":{"authorization":"Bearer synthetic"}});
    let payload = json!({"model":"jev-latest","state":{"original_task":"Synthetic"},"questions":{"tier":{"type":"choice"}}});
    let matches = |headers: &Value, body: &Value| {
        request_matches(
            expected["url"].as_str().unwrap(),
            "POST",
            headers,
            body,
            &expected,
            &payload,
        )
    };
    assert!(matches(&expected["headers"], &payload));
    assert!(!matches(&json!({}), &payload));
    for field in ["state", "questions"] {
        let mut missing = payload.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(!matches(&expected["headers"], &missing));
    }
    let count = json!({"model":"haiku","messages":[{"role":"user","content":"Synthetic"}]});
    let mut extra = count.clone();
    extra["max_tokens"] = json!(4096);
    assert!(!request_matches(
        expected["url"].as_str().unwrap(),
        "POST",
        &expected["headers"],
        &extra,
        &expected,
        &count
    ));
}
