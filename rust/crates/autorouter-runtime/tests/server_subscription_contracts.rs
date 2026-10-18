//! Exact frozen server inputs through actual downstream and upstream HTTP.
#[path = "support/server_simple.rs"]
mod simple;
const LOCAL_AUTH_HEADER: &str = "x-autorouter-token";
use autorouter_core::config::{AuthMode, read_config};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::{HeaderMap, Request, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::convert::Infallible;
use std::sync::{Arc, Mutex, atomic::Ordering};
use std::time::Duration;

const CASES: &str = include_str!("../../../parity/cases/server-subscription-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../parity/cases/server-subscription-contracts.capture.json");
const CASES_SHA: &str = "9ac909139dcbc8ff1cac25393e684f52ff4bf95d2dc6b2b0f77752c8693104da";
const LIMIT: Duration = Duration::from_secs(5);

fn input(number: usize) -> Value {
    assert_eq!(format!("{:x}", Sha256::digest(CASES.as_bytes())), CASES_SHA);
    let report: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(report["cases_sha256"], CASES_SHA);
    assert_eq!(report["static_assertions"], 24);
    CASES
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|row| row["id"] == format!("baseline-server-subscription-{number}"))
        .unwrap()
}
async fn bounded(future: impl std::future::Future<Output = ()>) {
    tokio::time::timeout(LIMIT, future)
        .await
        .expect("Complete synthetic server schedule exceeded deadline");
}
use simple::{Fixture, Seen};
impl Fixture {
    async fn new(row: &Value) -> Self {
        Self::start(
            || {
                let plans = row.get("responses").cloned().unwrap_or_else(|| {
                    json!([
                        {"status":200,"headers":{"content-type":"application/json"},"body_text":"{}"},
                        {"status":200,"headers":{"content-type":"application/json"},"body_text":"{}"}
                    ])
                });
                let replies: Vec<_> = plans
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|plan| {
                        let status = plan["status"].as_u64().unwrap() as u16;
                        let text = plan["body_text"].as_str().unwrap();
                        assert!(text.len() <= 4096);
                        let mut headers = HeaderMap::new();
                        for (key, value) in plan["headers"].as_object().unwrap() {
                            headers.insert(
                                hyper::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
                                value.as_str().unwrap().parse().unwrap(),
                            );
                        }
                        (status, headers, Bytes::copy_from_slice(text.as_bytes()))
                    })
                    .collect();
                assert!(replies.len() <= 2);
                let replies = Arc::new(replies);
                move |request: Request<Incoming>, seen: Arc<Mutex<Vec<Seen>>>| {
                    let replies = replies.clone();
                    async move {
                        let (parts, body) = request.into_parts();
                        let body = Limited::new(body, 4096).collect().await.unwrap().to_bytes();
                        let index = {
                            let mut calls = seen.lock().unwrap();
                            assert!(calls.len() < replies.len(), "Unexpected extra upstream request/retry");
                            let index = calls.len();
                            calls.push(Seen { method: parts.method.to_string(), path: parts.uri.to_string(), headers: parts.headers, body });
                            index
                        };
                        let (status, headers, bytes) = &replies[index];
                        let mut response = Response::builder().status(*status).body(Full::new(bytes.clone())).unwrap();
                        *response.headers_mut() = headers.clone();
                        Ok::<_, Infallible>(response)
                    }
                }
            },
            |address| {
                let token = row["token"].as_str().unwrap().to_owned();
                let mut config = read_config(&json!({"AUTOROUTER_EVALUATOR":"jev","ANTHROPIC_API_KEY":"upstream-secret","TYPESAFE_API_KEY":"classifier-secret","AUTOROUTER_TOKEN":token}), false, std::path::Path::new("/tmp")).unwrap();
                config.upstream = format!("http://{address}");
                config.auth_mode = AuthMode::Subscription;
                (config, token)
            },
        )
        .await
    }
    fn private_events(&self) {
        let text = json!({"logs":*self.logs.lock().unwrap(), "statuses":*self.statuses.lock().unwrap(), "decisions":*self.decisions.lock().unwrap()}).to_string();
        for secret in [
            &self.token,
            "fake-subscription-token",
            "upstream-secret",
            "classifier-secret",
            "billable-api-key",
        ] {
            assert!(
                !text.contains(secret),
                "Synthetic credentials entered an event"
            );
        }
    }
    async fn close(mut self) {
        self.close_connections().await;
        self.private_events();
    }
}

fn headers(row: &Value) -> Vec<(&str, &str)> {
    row.as_object()
        .unwrap()
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str().unwrap()))
        .collect()
}

#[tokio::test]
async fn subscription_admission_rejects_all_six_original_headers_before_routing() {
    bounded(async {
        let row = input(11);
        let fixture = Fixture::new(&row).await;
        for extra in row["rejection_headers"].as_array().unwrap() {
            let (status, _, bytes) = fixture
                .send(
                    "/v1/messages",
                    "POST",
                    row["request"].to_string(),
                    &headers(extra),
                )
                .await;
            assert_eq!(status, 401);
            assert!(
                !std::str::from_utf8(&bytes)
                    .unwrap()
                    .contains("billable-api-key")
            );
        }
        for (path, method) in [("/health", "GET"), ("/api/hello", "HEAD")] {
            assert_eq!(fixture.send(path, method, String::new(), &[]).await.0, 200);
        }
        assert_eq!(fixture.router.calls.load(Ordering::SeqCst), 0);
        assert!(fixture.seen.lock().unwrap().is_empty());
        fixture.private_events();
        fixture.close().await;
    })
    .await;
}

#[tokio::test]
async fn subscription_401_and_429_forward_exact_provider_bytes_and_never_retry_with_api_key() {
    bounded(async {
        let row = input(12);
        let fixture = Fixture::new(&row).await;
        for reply in row["responses"].as_array().unwrap() {
            let (status, fields, bytes) = fixture
                .send(
                    "/v1/messages",
                    "POST",
                    row["request"].to_string(),
                    &headers(&row["oauth_headers"]),
                )
                .await;
            assert_eq!(u64::from(status), reply["status"].as_u64().unwrap());
            assert_eq!(fields["retry-after"], "12");
            assert_eq!(fields["anthropic-ratelimit-unified-status"], "rejected");
            assert_eq!(
                bytes.as_ref(),
                reply["body_text"].as_str().unwrap().as_bytes()
            );
        }
        {
            let seen = fixture.seen.lock().unwrap();
            assert_eq!(seen.len(), 2);
            for call in seen.iter() {
                assert_eq!(
                    call.headers["authorization"],
                    row["oauth_headers"]["authorization"].as_str().unwrap()
                );
                assert!(!call.headers.contains_key("x-api-key"));
                assert!(!call.headers.contains_key("x-autorouter-token"));
                assert_eq!(call.method, "POST");
                assert_eq!(call.path, "/v1/messages");
                let mut expected = row["request"].clone();
                expected["model"] = json!("claude-haiku-4-5-20251001");
                assert_eq!(
                    serde_json::from_slice::<Value>(&call.body).unwrap(),
                    expected
                );
            }
        }
        assert_eq!(fixture.router.calls.load(Ordering::SeqCst), 2);
        fixture.private_events();
        fixture.close().await;
    })
    .await;
}

#[tokio::test]
async fn subscription_model_discovery_and_token_count_share_oauth_without_classification() {
    bounded(async {
        let row = input(13);
        let fixture = Fixture::new(&row).await;
        for (path, method, body) in [
            ("/v1/models", "GET", String::new()),
            (
                "/v1/messages/count_tokens",
                "POST",
                row["request"].to_string(),
            ),
        ] {
            let (status, _, response) = fixture
                .send(path, method, body, &headers(&row["oauth_headers"]))
                .await;
            assert_eq!(status, 200);
            assert_eq!(response.as_ref(), b"{}");
        }
        {
            let seen = fixture.seen.lock().unwrap();
            assert_eq!(seen.len(), 2);
            for (call, path) in seen.iter().zip(["/v1/models", "/v1/messages/count_tokens"]) {
                assert_eq!(call.path, path);
                for key in ["authorization", "anthropic-beta"] {
                    assert_eq!(
                        call.headers[key],
                        row["oauth_headers"][key].as_str().unwrap()
                    );
                }
                assert!(!call.headers.contains_key("x-autorouter-token"));
                assert!(!call.headers.contains_key("x-api-key"));
            }
            assert_eq!(seen[0].method, "GET");
            assert!(seen[0].body.is_empty());
            assert_eq!(seen[1].method, "POST");
            assert_eq!(
                serde_json::from_slice::<Value>(&seen[1].body).unwrap(),
                row["request"]
            );
        }
        assert_eq!(fixture.router.calls.load(Ordering::SeqCst), 0);
        fixture.private_events();
        fixture.close().await;
    })
    .await;
}

#[tokio::test]
async fn subscription_turn_scoped_system_messages_and_beta_are_preserved() {
    bounded(async {
        let row = input(14);
        let fixture = Fixture::new(&row).await;
        let (status, _, response) = fixture
            .send(
                "/v1/messages",
                "POST",
                row["request"].to_string(),
                &headers(&row["oauth_headers"]),
            )
            .await;
        assert_eq!(status, 200);
        assert_eq!(response.as_ref(), b"{}");
        {
            let seen = fixture.seen.lock().unwrap();
            assert_eq!(seen.len(), 1);
            assert_eq!(
                seen[0].headers["anthropic-beta"],
                row["oauth_headers"]["anthropic-beta"].as_str().unwrap()
            );
            let mut expected = row["request"].clone();
            expected["model"] = json!("claude-haiku-4-5-20251001");
            assert_eq!(
                serde_json::from_slice::<Value>(&seen[0].body).unwrap(),
                expected
            );
        }
        assert_eq!(fixture.router.calls.load(Ordering::SeqCst), 1);
        fixture.private_events();
        fixture.close().await;
    })
    .await;
}
