//! Exact frozen server inputs through actual downstream and upstream HTTP.
#[path = "support/server_simple.rs"]
mod simple;
const LOCAL_AUTH_HEADER: &str = "x-api-key";
use autorouter_core::config::read_config;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::{HeaderMap, Request, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::convert::Infallible;
use std::sync::{Arc, Mutex, atomic::Ordering};
use std::time::Duration;

const CASES: &str = include_str!("../../../parity/cases/server-response-contracts.jsonl");
const CAPTURE: &str = include_str!("../../../parity/cases/server-response-contracts.capture.json");
const CASES_SHA: &str = "7c158c333972674c503b3a8767fa5077ec775369998894a72bde8609bcd3e217";
const LIMIT: Duration = Duration::from_secs(5);

fn input(number: usize) -> Value {
    assert_eq!(format!("{:x}", Sha256::digest(CASES.as_bytes())), CASES_SHA);
    let report: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(report["cases_sha256"], CASES_SHA);
    assert_eq!(report["static_assertions"], 22);
    CASES
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|row| row["id"] == format!("baseline-server-response-{number}"))
        .unwrap()
}
async fn bounded(future: impl std::future::Future<Output = ()>) {
    tokio::time::timeout(LIMIT, future)
        .await
        .expect("Complete synthetic server schedule exceeded deadline");
}
use simple::{Fixture, Seen};
impl Fixture {
    async fn new(row: &Value, response_bytes: Vec<u8>, encoding: Option<&str>) -> Self {
        assert!(response_bytes.len() <= 4096);
        Self::start(
            || {
                let status = row["response_status"].as_u64().unwrap_or(500) as u16;
                let mut headers = HeaderMap::new();
                if let Some(fields) = row["response_headers"].as_object() {
                    for (key, value) in fields {
                        headers.insert(
                            hyper::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
                            value.as_str().unwrap().parse().unwrap(),
                        );
                    }
                }
                headers.remove("content-encoding");
                if let Some(encoding) = encoding {
                    headers.insert("content-encoding", encoding.parse().unwrap());
                }
                let bytes = Bytes::from(response_bytes);
                move |request: Request<Incoming>, seen: Arc<Mutex<Vec<Seen>>>| {
                    let headers = headers.clone();
                    let bytes = bytes.clone();
                    async move {
                        let (parts, body) = request.into_parts();
                        let body = Limited::new(body, 4096).collect().await.unwrap().to_bytes();
                        {
                            let mut calls = seen.lock().unwrap();
                            assert!(calls.len() < 16);
                            calls.push(Seen { method: parts.method.to_string(), path: parts.uri.to_string(), headers: parts.headers, body });
                        }
                        let mut response = Response::builder().status(status).body(Full::new(bytes)).unwrap();
                        *response.headers_mut() = headers;
                        Ok::<_, Infallible>(response)
                    }
                }
            },
            |address| {
                let token = row["token"].as_str().unwrap().to_owned();
                let mut config = read_config(&json!({"AUTOROUTER_EVALUATOR":"jev","ANTHROPIC_API_KEY":"upstream-secret","TYPESAFE_API_KEY":"classifier-secret","AUTOROUTER_TOKEN":token}), false, std::path::Path::new("/tmp")).unwrap();
                config.upstream = format!("http://{address}");
                if let Some(maximum) = row["maximum_body_bytes"].as_u64() {
                    config.max_body_bytes = maximum as usize;
                }
                (config, token)
            },
        )
        .await
    }
    async fn completed(&self) {
        while !self
            .statuses
            .lock()
            .unwrap()
            .iter()
            .any(|row| row["event"] == "request_complete")
        {
            tokio::task::yield_now().await;
        }
    }
    async fn close(mut self) {
        self.close_connections().await;
    }
}

#[tokio::test]
async fn frozen_token_count_error_preserves_body_status_headers_and_bypasses_evaluation() {
    bounded(async {
        let row = input(2);
        let expected = row["response_text"].as_str().unwrap().as_bytes();
        let fixture = Fixture::new(&row, expected.to_vec(), None).await;
        let (status, headers, bytes) = fixture
            .send(
                "/v1/messages/count_tokens",
                "POST",
                row["request"].to_string(),
                &[],
            )
            .await;
        assert_eq!(status, 429);
        assert_eq!(headers["retry-after"], "7");
        assert_eq!(bytes.as_ref(), expected);
        assert_eq!(fixture.router.calls.load(Ordering::SeqCst), 0);
        assert!(fixture.statuses.lock().unwrap().is_empty());
        assert!(fixture.decisions.lock().unwrap().is_empty());
        {
            let seen = fixture.seen.lock().unwrap();
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].method, "POST");
            assert_eq!(seen[0].path, "/v1/messages/count_tokens");
            assert_eq!(
                serde_json::from_slice::<Value>(&seen[0].body).unwrap(),
                row["request"]
            );
        }
        fixture.close().await;
    })
    .await;
}

#[tokio::test]
async fn frozen_local_rejections_never_contact_upstream_or_emit_decisions() {
    bounded(async {
        let row = input(3);
        let fixture = Fixture::new(&row, Vec::new(), None).await;
        let mut malformed = row["request"].clone();
        malformed["messages"] = json!([null]);
        for (path, method, body, extra, expected) in [
            (
                "/v1/messages",
                "POST",
                String::new(),
                vec![("x-api-key", "wrong")],
                401,
            ),
            (
                "/health",
                "GET",
                String::new(),
                vec![("origin", "https://example.com")],
                403,
            ),
            ("/v1/messages", "POST", "{bad".into(), vec![], 400),
            ("/v1/messages", "POST", malformed.to_string(), vec![], 400),
            ("/unexpected", "GET", String::new(), vec![], 404),
        ] {
            assert_eq!(
                fixture.send(path, method, body, &extra).await.0,
                expected,
                "{path}"
            );
        }
        let (status, headers, _) = fixture
            .send("/v1/messages", "POST", "x".repeat(1001), &[])
            .await;
        assert_eq!(status, 413);
        assert_eq!(headers["connection"], "close");
        assert_eq!(
            fixture.send("/health", "GET", String::new(), &[]).await.0,
            200
        );
        assert_eq!(fixture.router.calls.load(Ordering::SeqCst), 0);
        assert!(fixture.decisions.lock().unwrap().is_empty());
        assert!(fixture.seen.lock().unwrap().is_empty());
        fixture.close().await;
    })
    .await;
}

#[tokio::test]
async fn frozen_gzip_bytes_are_opaque_and_encoding_controls_observation() {
    bounded(async {
        let row = input(20);
        let gzip: Vec<u8> = row["response_bytes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u8)
            .collect();
        let plain = row["decoded_synthetic_text"]
            .as_str()
            .unwrap()
            .as_bytes()
            .to_vec();
        for (bytes, encoding, observed) in [
            (gzip, Some("gzip"), false),
            // Sensitivity controls: parseable SSE labelled gzip must stay opaque;
            // the same unencoded bytes must produce model observation.
            (plain.clone(), Some("gzip"), false),
            (plain, None, true),
        ] {
            let fixture = Fixture::new(&row, bytes.clone(), encoding).await;
            let (status, headers, received) = fixture
                .send(
                    "/v1/messages",
                    "POST",
                    row["request"].to_string(),
                    &[("content-type", "application/json")],
                )
                .await;
            assert_eq!(status, 200);
            assert_eq!(
                headers.get("content-encoding").map(|v| v.to_str().unwrap()),
                encoding
            );
            assert_eq!(received.as_ref(), bytes);
            fixture.completed().await;
            assert_eq!(
                fixture
                    .logs
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|r| r["event"] == "upstream_model"),
                observed
            );
            {
                let seen = fixture.seen.lock().unwrap();
                assert_eq!(seen.len(), 1);
                assert_eq!(seen[0].headers["accept-encoding"], "identity");
            }
            fixture.close().await;
        }
    })
    .await;
}
