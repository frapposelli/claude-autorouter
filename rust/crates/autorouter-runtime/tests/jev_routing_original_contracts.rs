//! Original Jev privacy and forced-tool profile contracts, with exact captured
//! evaluator bytes and separate real gateway forwarding evidence.
use autorouter_core::{config::read_config, js_json::JsDocument, router::RouteOptions};
use autorouter_runtime::{
    http_client::{HttpError, HttpTransport, NativeHttpClient},
    router::Router,
    server::Gateway,
    server_events::EventSinks,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    HeaderMap, Request, Response,
    body::{Body, Frame, Incoming},
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    convert::Infallible,
    path::Path,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const CASES: &str = include_str!("../../../parity/cases/jev-routing-original-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../parity/cases/jev-routing-original-contracts.capture.json");
fn cases() -> Vec<Value> {
    assert_eq!(
        format!("{:x}", Sha256::digest(CASES)),
        "78693d36774927695cbad9487b82e4d3d2742ab895a6cee8fde218ac36b710ab"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(CAPTURE)),
        "315466e92395060cf01f7f8b3ca7e6362308ce02b5275150c0d6bf08358ed488"
    );
    let capture: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(capture["static_assertions"], 8);
    assert_eq!(capture["expanded_assertions"], 63);
    let rows: Vec<Value> = CASES
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 13);
    rows
}
macro_rules! source_assert {
    ($id:literal, $condition:expr) => {{
        assert!($condition, "original assertion {}", $id);
        eprintln!("ORIGINAL_ASSERT\t{}", $id);
    }};
}
macro_rules! source_equal {
    ($id:literal, $left:expr, $right:expr) => {{
        assert_eq!($left, $right, "original assertion {}", $id);
        eprintln!("ORIGINAL_ASSERT\t{}", $id);
    }};
}
struct TrackedBody {
    bytes: Option<Bytes>,
    live: Arc<AtomicUsize>,
}
impl Body for TrackedBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        Poll::Ready(
            self.get_mut()
                .bytes
                .take()
                .map(|bytes| Ok(Frame::data(bytes))),
        )
    }
}
impl Drop for TrackedBody {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::SeqCst);
    }
}
#[derive(Clone)]
struct Observation {
    method: String,
    uri: String,
    headers: Value,
    body: String,
}
fn compare_request(actual: &Observation, expected: &Value) -> Result<(), String> {
    if actual.method != expected["method"] {
        return Err("method differs".into());
    }
    if actual.uri != expected["url"] {
        return Err("URL differs".into());
    }
    if actual.headers != expected["headers"] {
        return Err("headers differ".into());
    }
    if actual.body != expected["body"] {
        return Err("complete evaluator payload bytes differ".into());
    }
    Ok(())
}
struct Script {
    remaining: Mutex<VecDeque<Value>>,
    calls: Mutex<Vec<Observation>>,
    failures: Mutex<Vec<String>>,
    live: Arc<AtomicUsize>,
}
impl Script {
    fn new(row: &Value) -> Self {
        Self {
            remaining: Mutex::new(
                row["requests"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect(),
            ),
            calls: Mutex::new(Vec::new()),
            failures: Mutex::new(Vec::new()),
            live: Arc::new(AtomicUsize::new(0)),
        }
    }
    fn check(&self) -> Result<(), String> {
        if !self.failures.lock().unwrap().is_empty() {
            return Err(self.failures.lock().unwrap().join("; "));
        }
        if !self.remaining.lock().unwrap().is_empty() {
            return Err("expected requests missing".into());
        }
        if self.live.load(Ordering::SeqCst) != 0 {
            return Err("response body still owned".into());
        }
        Ok(())
    }
}
impl HttpTransport for Script {
    type ResponseBody = TrackedBody;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        assert!(bytes.len() < 65536);
        let actual = Observation {
            method: parts.method.to_string(),
            uri: parts.uri.to_string(),
            headers: serde_json::to_value(
                parts
                    .headers
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.to_str().unwrap()))
                    .collect::<std::collections::BTreeMap<_, _>>(),
            )
            .unwrap(),
            body: String::from_utf8(bytes.to_vec()).unwrap(),
        };
        let expected = self.remaining.lock().unwrap().pop_front();
        let (status, response) = if let Some(expected) = expected {
            if let Err(error) = compare_request(&actual, &expected) {
                self.failures.lock().unwrap().push(error);
            }
            (
                expected["status"].as_u64().unwrap() as u16,
                expected["response_arguments"][0].clone(),
            )
        } else {
            self.failures
                .lock()
                .unwrap()
                .push("unexpected request".into());
            (
                200,
                json!({"answers":{"tier":{"choice":"sonnet","confidence":0.9}}}),
            )
        };
        let mut calls = self.calls.lock().unwrap();
        assert!(calls.len() < 32);
        calls.push(actual);
        drop(calls);
        self.live.fetch_add(1, Ordering::SeqCst);
        Ok(Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(TrackedBody {
                bytes: Some(Bytes::from(response.to_string())),
                live: self.live.clone(),
            })
            .unwrap())
    }
}
struct Replay {
    decision: Value,
    before: Value,
    after: Value,
    payload: String,
}
async fn replay(row: &Value) -> Result<Replay, String> {
    let config = read_config(&row["config_arguments"][0], false, Path::new("/tmp")).unwrap();
    let transport = Arc::new(Script::new(row));
    let router = Router::new(transport.clone(), config);
    let raw = row["body_json"].as_str().unwrap();
    let document = Arc::new(JsDocument::parse(raw.as_bytes()).unwrap());
    let before = serde_json::from_str::<Value>(&document.stringify()).unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        router.route_exact(
            document.clone(),
            RouteOptions::default(),
            &HeaderMap::new(),
            &CancellationToken::new(),
            "",
        ),
    )
    .await;
    let closed = tokio::time::timeout(Duration::from_secs(5), router.close()).await;
    transport.check()?;
    closed.map_err(|_| "router cleanup deadline")?;
    let mut decision = result
        .map_err(|_| "route deadline")?
        .map_err(|_| "route rejected")?
        .decision;
    for key in ["latency_ms", "evaluation_latency_ms"] {
        if !decision[key]
            .as_f64()
            .is_some_and(|x| x.is_finite() && x >= 0.0)
        {
            return Err("invalid latency".into());
        }
        decision.as_object_mut().unwrap().remove(key);
    }
    if decision != row["decision"] {
        return Err(format!("decision differs: {decision}"));
    }
    let after_json = document.stringify();
    if after_json != row["body_after_json"] || after_json != raw {
        return Err("request mutated".into());
    }
    let calls = transport.calls.lock().unwrap();
    if calls.len() != 1 {
        return Err("wrong request count".into());
    }
    Ok(Replay {
        decision,
        before,
        after: serde_json::from_str(&after_json).unwrap(),
        payload: calls[0].body.clone(),
    })
}
#[tokio::test]
async fn original_jev_redaction_body_and_input_preservation() {
    let row = cases().remove(0);
    let actual = replay(&row).await.unwrap();
    let secret = ["gh", "p_", &"B".repeat(36)].concat();
    source_assert!(
        "test/redaction.test.mjs#8:assert-1",
        !actual.payload.is_empty() && !actual.payload.contains(&secret)
    );
    source_assert!(
        "test/redaction.test.mjs#8:assert-2",
        actual.payload.contains("Use [REDACTED:secret] to push")
    );
    source_equal!(
        "test/redaction.test.mjs#8:assert-3",
        actual.after,
        actual.before
    );
}
#[tokio::test]
async fn original_forced_tool_upgrade_matrix_preserves_every_profile() {
    let rows = cases();
    let mut dimensions = std::collections::BTreeSet::new();
    for row in rows.iter().skip(1) {
        let actual = replay(row).await.unwrap();
        let profile = row["config_arguments"][0]["AUTOROUTER_CLIENT_PROFILE"]
            .as_str()
            .unwrap();
        let tier = row["requests"][0]["response_arguments"][0]["answers"]["tier"]["choice"]
            .as_str()
            .unwrap();
        let choice = actual.before["tool_choice"]["type"].as_str().unwrap();
        assert!(dimensions.insert((profile.to_owned(), tier.to_owned(), choice.to_owned())));
        source_equal!(
            "test/routing-compatibility.test.mjs#1:assert-1",
            actual.decision["model"],
            actual.before["model"]
        );
        source_equal!(
            "test/routing-compatibility.test.mjs#1:assert-2",
            actual.decision["classified_tier"],
            tier
        );
        source_equal!(
            "test/routing-compatibility.test.mjs#1:assert-3",
            actual.decision["reason"],
            if profile == "auto" {
                "auto_mode_incompatible"
            } else {
                "model_incompatible"
            }
        );
        source_equal!(
            "test/routing-compatibility.test.mjs#1:assert-4",
            actual.decision["compatibility_reason"],
            "forced_tool_choice"
        );
        source_equal!(
            "test/routing-compatibility.test.mjs#1:assert-5",
            actual.after,
            actual.before
        );
    }
    let expected: std::collections::BTreeSet<_> = ["compatible", "native", "auto"]
        .into_iter()
        .flat_map(|p| {
            ["sonnet", "opus"].into_iter().flat_map(move |t| {
                ["any", "tool"]
                    .into_iter()
                    .map(move |c| (p.to_owned(), t.to_owned(), c.to_owned()))
            })
        })
        .collect();
    assert_eq!(dimensions, expected);
}
#[tokio::test]
async fn forged_transcripts_cannot_hide_behind_successful_decisions() {
    let original = cases().remove(0);
    for field in ["state", "questions", "model"] {
        let mut row = original.clone();
        let mut body: Value =
            serde_json::from_str(row["requests"][0]["body"].as_str().unwrap()).unwrap();
        body[field] = json!("forged");
        row["requests"][0]["body"] = json!(body.to_string());
        assert!(
            replay(&row)
                .await
                .err()
                .unwrap()
                .contains("payload bytes differ")
        );
    }
    let mut row = original.clone();
    row["requests"][0]["headers"]["authorization"] = json!("Bearer forged");
    assert!(replay(&row).await.err().unwrap().contains("headers differ"));
    let mut row = original.clone();
    row["requests"][0]["url"] = json!("https://wrong.invalid/v1/systemone");
    assert!(replay(&row).await.err().unwrap().contains("URL differs"));
    let mut row = original.clone();
    row["requests"] = json!([]);
    assert!(
        replay(&row)
            .await
            .err()
            .unwrap()
            .contains("unexpected request")
    );
    let mut row = original.clone();
    let extra = row["requests"][0].clone();
    row["requests"].as_array_mut().unwrap().push(extra);
    assert!(
        replay(&row)
            .await
            .err()
            .unwrap()
            .contains("requests missing")
    );
    let mut row = original.clone();
    row["decision"]["model"] = json!("forged");
    assert!(
        replay(&row)
            .await
            .err()
            .unwrap()
            .contains("decision differs")
    );
    let mut row = original;
    row["body_after_json"] = json!("{}");
    assert!(
        replay(&row)
            .await
            .err()
            .unwrap()
            .contains("request mutated")
    );
}

#[tokio::test]
async fn actual_gateway_sends_redacted_jev_payload_and_preserves_upstream_request() {
    let row = cases().remove(0);
    let received = Arc::new(Mutex::new(Vec::<(String, HeaderMap, Vec<u8>)>::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stopping = CancellationToken::new();
    let stop = stopping.clone();
    let observed = received.clone();
    let response_body = Bytes::from_static(b"{\"type\":\"message\",\"model\":\"claude-sonnet-5\",\"content\":[{\"type\":\"text\",\"text\":\"Synthetic reply.\"}],\"stop_reason\":\"end_turn\"}");
    let expected_reply = response_body.clone();
    let mut mock = tokio::spawn(async move {
        let mut children = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                socket = listener.accept() => {
                    let (socket, _) = socket.unwrap(); assert!(children.len() < 4);
                    let observed = observed.clone(); let reply = response_body.clone();
                    children.spawn(async move {
                        let service = service_fn(move |request: Request<Incoming>| {
                            let observed = observed.clone(); let reply = reply.clone();
                            async move {
                                let (parts,body) = request.into_parts();
                                let bytes = body.collect().await.unwrap().to_bytes(); assert!(bytes.len() < 65536);
                                let is_evaluator = parts.uri.path() == "/v1/systemone";
                                { let mut calls = observed.lock().unwrap(); assert!(calls.len() < 4); calls.push((parts.uri.to_string(),parts.headers,bytes.to_vec())); }
                                let bytes = if is_evaluator { Bytes::from_static(b"{\"answers\":{\"tier\":{\"choice\":\"sonnet\",\"confidence\":0.9}}}") } else { reply };
                                Ok::<_,Infallible>(Response::builder().header("content-type","application/json").body(Full::new(bytes)).unwrap())
                            }
                        });
                        let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(socket),service).await;
                    });
                },
                result = children.join_next(), if !children.is_empty() => { result.unwrap().unwrap(); }
            }
        }
        drop(listener);
        children.abort_all();
        while children.join_next().await.is_some() {}
    });
    let mut config = read_config(&row["config_arguments"][0], false, Path::new("/tmp")).unwrap();
    config.local_token = Some("synthetic-privacy-gateway-token".into());
    config.upstream = format!("http://{address}");
    config.jev_endpoint = format!("http://{address}/v1/systemone");
    let records = Arc::new(Mutex::new(Vec::new()));
    let record_sink = records.clone();
    let client = Arc::new(NativeHttpClient::new().unwrap());
    let handle = Gateway::new(
        config,
        client.clone(),
        EventSinks {
            record: Some(Arc::new(move |record| {
                let mut rows = record_sink.lock().unwrap();
                assert!(rows.len() < 32);
                rows.push(record)
            })),
            ..Default::default()
        },
    )
    .unwrap()
    .listen(0)
    .await
    .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(5), async {
        let response = client
            .request(
                Request::post(format!("http://{}/v1/messages", handle.address))
                    .header("x-api-key", "synthetic-privacy-gateway-token")
                    .header("content-type", "application/json")
                    .body(Full::new(Bytes::from(
                        row["body_json"].as_str().unwrap().to_owned(),
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, bytes)
    })
    .await;
    let closed = tokio::time::timeout(Duration::from_secs(5), handle.close()).await;
    stopping.cancel();
    let joined = tokio::time::timeout(Duration::from_secs(5), &mut mock).await;
    // Cancel and join the mock owner before reporting any cleanup failure.
    // Its JoinSet aborts remaining peers if this forced cleanup path is needed.
    if joined.is_err() {
        mock.abort();
        let _ = tokio::time::timeout(Duration::from_secs(1), mock).await;
    }
    closed.expect("gateway cleanup bounded");
    joined.expect("mock cleanup bounded").unwrap();
    let (status, bytes) = response.unwrap();
    assert_eq!(status, 200);
    assert_eq!(bytes, expected_reply);
    let calls = received.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, "/v1/systemone");
    assert_eq!(calls[0].1["authorization"], "Bearer test-jev");
    assert_eq!(
        calls[0].2,
        row["requests"][0]["body"].as_str().unwrap().as_bytes()
    );
    assert_eq!(calls[1].0, "/v1/messages");
    assert_eq!(calls[1].1["x-api-key"], "test-anthropic");
    assert_eq!(
        serde_json::from_slice::<Value>(&calls[1].2).unwrap(),
        serde_json::from_str::<Value>(row["body_json"].as_str().unwrap()).unwrap()
    );
    let secret = ["gh", "p_", &"B".repeat(36)].concat();
    assert!(!String::from_utf8_lossy(&calls[0].2).contains(&secret));
    assert!(String::from_utf8_lossy(&calls[1].2).contains(&secret));
    let records = records.lock().unwrap();
    for event in ["decision", "outcome"] {
        assert_eq!(
            records.iter().filter(|row| row["event"] == event).count(),
            1,
            "privacy checks require actual decision and outcome records"
        );
    }
    assert!(!serde_json::to_string(&*records).unwrap().contains(&secret));
}
