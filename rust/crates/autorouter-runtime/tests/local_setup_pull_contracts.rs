//! Original local setup admission/pull/warm callbacks over real native APIs.
#[path = "support/local_setup_pull.rs"]
mod support;
use autorouter_runtime::http_client::NativeHttpClient;
use autorouter_runtime::ollama_setup::{SetupOptions, setup_ollama};
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::convert::Infallible;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use support::{
    BOUND, PULL_LIMIT, Replay, Step, cases, execute, matches, request_projection, settings,
};
use tokio_util::sync::CancellationToken;

fn definition(number: usize) -> Vec<Value> {
    let id = format!("test/ollama-setup.test.mjs#{number}");
    cases()
        .into_iter()
        .filter(|row| row["source_test"] == id)
        .collect()
}
async fn original(row: &Value) -> Replay {
    let replay = Replay::captured(row);
    let result = execute(row, &replay).await;
    assert!(matches(row, &replay, &result), "{} {:?}", row["id"], result);
    let expected: Vec<_> = row["requests"]
        .as_array()
        .into_iter()
        .flatten()
        .map(request_projection)
        .collect();
    assert_eq!(*replay.observed.lock().unwrap(), expected, "{}", row["id"]);
    replay
}

#[tokio::test]
async fn original_local_endpoint_and_model_admission_rejects_before_any_request() {
    let rows = definition(4);
    assert_eq!(rows.len(), 18);
    let mut endpoints = 0;
    let mut inspections = 0;
    let mut models = 0;
    for row in &rows {
        let replay = original(row).await;
        // Original #4 forbidden-fetch assertion is never entered.
        assert!(replay.observed.lock().unwrap().is_empty());
        match row["kind"].as_str().unwrap() {
            "endpoint" => endpoints += 1,
            "inspect" => {
                inspections += 1;
                assert_eq!(row["node_expected"]["ok"], false);
            }
            "model" => {
                models += 1;
                assert_eq!(row["node_expected"]["ok"], false);
            }
            _ => unreachable!(),
        }
    }
    assert_eq!((endpoints, inspections, models), (8, 5, 5));
    assert_eq!(
        rows[..3]
            .iter()
            .map(|r| r["node_expected"]["result"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "http://127.0.0.1:11434",
            "https://127.0.0.1:8443",
            "http://[::1]:11434"
        ]
    );
}

#[tokio::test]
async fn original_explicit_pull_preserves_split_chunks_progress_and_full_warm_payload() {
    let rows = definition(6);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    let replay = original(row).await;
    assert_eq!(
        replay.paths(),
        [
            "/api/version",
            "/api/tags",
            "/api/pull",
            "/api/version",
            "/api/tags",
            "/api/show",
            "/api/show",
            "/v1/systemone"
        ]
    );
    let requests = replay.observed.lock().unwrap();
    assert_eq!(
        requests[2]["body"],
        json!({"model":"nimble:9b-q4_K_M","stream":true})
    );
    assert_eq!(requests[7]["body"]["model"], "nimble:9b-q4_K_M");
    assert_eq!(requests[7]["body"]["keep_alive"], "5m");
    let mut keys: Vec<_> = requests[7]["body"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["keep_alive", "model", "questions", "state"]);
    assert_eq!(
        requests[7]["body"]["questions"],
        autorouter_runtime::evaluator::ollama_questions()
    );
    assert_eq!(
        requests[7]["body"]["state"],
        json!({"system":"","original_task":"Return the literal word ready.",
        "current_task":"Return the literal word ready.","recent_messages":[],"message_count":1,"tool_count":0,"context_is_excerpt":true})
    );
    assert!(
        requests
            .iter()
            .all(|r| r["headers"].get("authorization").is_none())
    );
    assert_eq!(replay.lifetimes[2].chunks.load(Ordering::SeqCst), 4);
    let original_chunks = support::source_chunks(&row["requests"][2]["response"]);
    assert_eq!(original_chunks.len(), 4);
    assert_eq!(
        String::from_utf8_lossy(&original_chunks[2])
            .matches("PRIVATE_PROVIDER_DATA")
            .count(),
        3
    );
    assert!(!original_chunks[3].ends_with(b"\n"));
    assert_eq!(
        row["progress"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s.as_str().unwrap().contains("(50%)"))
            .count(),
        1
    );
    assert!(
        !row["progress"]
            .to_string()
            .contains("PRIVATE_PROVIDER_DATA")
    );
    assert_eq!(
        row["node_expected"]["result"],
        json!({"model":"nimble:9b-q4_K_M","pulled":true,"warmed":true})
    );
}

#[tokio::test]
async fn original_installed_model_skips_pull_and_real_delayed_warm_uses_independent_budget() {
    let rows = definition(7);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    let config = settings(row);
    assert_eq!(config.ollama_timeout_ms, 1);
    assert_eq!(config.ollama_keep_alive, "10m");
    assert_eq!(config.jev_key.as_deref(), Some("PRIVATE_JEV_KEY"));
    assert_eq!(
        config.anthropic_key.as_deref(),
        Some("PRIVATE_ANTHROPIC_KEY")
    );
    let replay = original(row).await;
    assert_eq!(
        replay.paths(),
        [
            "/api/version",
            "/api/tags",
            "/api/show",
            "/api/show",
            "/v1/systemone"
        ]
    );
    assert_eq!(replay.delays_started.load(Ordering::SeqCst), 1);
    assert_eq!(replay.delays_finished.load(Ordering::SeqCst), 1);
    let requests = replay.observed.lock().unwrap();
    assert_eq!(requests[4]["body"]["keep_alive"], "10m");
    assert!(
        !serde_json::to_string(&*requests)
            .unwrap()
            .contains("PRIVATE_")
    );
    assert_eq!(
        row["node_expected"]["result"],
        json!({"model":"nimble:9b-q4_K_M","pulled":false,"warmed":true})
    );
}

#[tokio::test]
async fn original_four_failed_pull_streams_release_bodies_without_reinspection_or_warm() {
    let rows = definition(13);
    assert_eq!(rows.len(), 4);
    for row in &rows {
        let replay = original(row).await;
        assert_eq!(replay.paths(), ["/api/version", "/api/tags", "/api/pull"]);
        assert_eq!(row["node_expected"]["ok"], false);
        assert!(
            replay
                .lifetimes
                .iter()
                .all(|l| l.dropped.load(Ordering::SeqCst) == 1)
        );
        assert!(replay.lifetimes[2].polled.load(Ordering::SeqCst) > 0);
        assert!(!row["progress"].to_string().contains("Preloading"));
    }
    let oversized = support::source_chunks(&rows[2]["requests"][2]["response"]);
    assert_eq!(oversized.len(), 1);
    assert_eq!(oversized[0].iter().filter(|b| **b == b'x').count(), 70000);
    assert!(String::from_utf8_lossy(&oversized[0]).contains("PRIVATE_"));
}

#[tokio::test]
async fn transcript_verification_rejects_wrong_requests_even_when_results_match() {
    let row = definition(6).remove(0);
    for change in 0..8 {
        let mut steps: Vec<_> = row["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| Step::captured(r, false))
            .collect();
        match change {
            0 => steps[0].request["url"] = json!("http://127.0.0.1:11434/wrong"),
            1 => steps[7].request["body"]["model"] = json!("forged-model"),
            2 => steps[7].request["body"]["keep_alive"] = json!("99m"),
            3 => steps[7].request["body"]["state"]["current_task"] = json!("forged-task"),
            4 => steps[7].request["body"]["questions"] = json!({}),
            5 => steps[2].request["headers"]["authorization"] = json!("synthetic-forbidden"),
            6 => steps[3].request["method"] = json!("POST"),
            7 => {
                let a = steps[3].request.clone();
                steps[3].request = steps[4].request.clone();
                steps[4].request = a;
            }
            _ => unreachable!(),
        }
        let replay = Replay::new(steps);
        let result = execute(&row, &replay).await;
        assert_eq!(
            result.0,
            support::expected(&row),
            "scripted success still matches"
        );
        assert!(replay.invalid.load(Ordering::SeqCst) > 0);
        assert!(!matches(&row, &replay, &result));
    }
    let mut steps: Vec<_> = row["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| Step::captured(r, false))
        .collect();
    steps.push(Step::captured(&row["requests"][0], false));
    let replay = Replay::new(steps);
    let result = execute(&row, &replay).await;
    assert_eq!(result.0, support::expected(&row));
    assert!(
        !matches(&row, &replay, &result),
        "unused expected response is rejected"
    );
}

#[tokio::test]
async fn output_progress_and_stream_suffix_controls_cannot_forge_acceptance() {
    let row = definition(6).remove(0);
    let replay = Replay::captured(&row);
    let result = execute(&row, &replay).await;
    for change in 0..4 {
        let mut forged = row.clone();
        match change {
            0 => forged["node_expected"]["result"]["model"] = json!("forged"),
            1 => forged["node_expected"]["result"]["pulled"] = json!(false),
            2 => {
                let progress = forged["progress"].as_array_mut().unwrap();
                progress.push(progress[2].clone());
            }
            3 => {
                forged["progress"].as_array_mut().unwrap().remove(1);
            }
            _ => unreachable!(),
        }
        assert!(!matches(&forged, &replay, &result));
    }
    let mut steps: Vec<_> = row["requests"]
        .as_array()
        .unwrap()
        .iter()
        .take(3)
        .map(|r| Step::captured(r, false))
        .collect();
    steps[2].chunks = vec![Bytes::from_static(b"{\"status\":\"pulling manifest\"}\n")];
    let replay = Replay::new(steps);
    let result = execute(&row, &replay).await;
    assert_eq!(result.0["error"]["code"], "OLLAMA_PULL");
    assert!(!matches(&row, &replay, &result));
    assert!(replay.verified());
    assert_eq!(replay.paths(), ["/api/version", "/api/tags", "/api/pull"]);
    let mut failed = definition(13).remove(1);
    let replay = Replay::captured(&failed);
    let result = execute(&failed, &replay).await;
    failed["node_expected"]["error"]["code"] = json!("OLLAMA_RESPONSE");
    assert!(!matches(&failed, &replay, &result));
}

async fn stream_boundary(
    chunks: Vec<Bytes>,
    success: bool,
    message: Option<&str>,
) -> Arc<support::Lifetime> {
    let row = definition(6).remove(0);
    let mut steps: Vec<_> = row["requests"]
        .as_array()
        .unwrap()
        .iter()
        .take(if success { 8 } else { 3 })
        .map(|r| Step::captured(r, false))
        .collect();
    steps[2].chunks = chunks;
    let lifetime = steps[2].lifetime.clone();
    let replay = Replay::new(steps);
    let result = execute(&row, &replay).await;
    assert_eq!(result.0["ok"], success);
    assert!(replay.verified());
    assert_eq!(lifetime.dropped.load(Ordering::SeqCst), 1);
    if let Some(message) = message {
        assert_eq!(result.0["error"]["message"], message);
        assert_eq!(replay.paths(), ["/api/version", "/api/tags", "/api/pull"]);
    }
    lifetime
}

#[tokio::test]
async fn actual_stream_line_and_aggregate_boundaries_release_owned_bodies() {
    for oversized in [false, true] {
        let prefix = b"{\"status\":\"success\",\"pad\":\"";
        let suffix = b"\"}";
        let size = 65536 + usize::from(oversized);
        let mut line = prefix.to_vec();
        line.resize(size - suffix.len(), b'x');
        line.extend_from_slice(suffix);
        assert_eq!(line.len(), size);
        let live = stream_boundary(
            vec![Bytes::from(line)],
            !oversized,
            oversized.then_some("Ollama returned an oversized download update."),
        )
        .await;
        assert_eq!(live.bytes.load(Ordering::SeqCst), size);
    }
    for oversized in [false, true] {
        let success = Bytes::from_static(b"{\"status\":\"success\"}\n");
        let size = PULL_LIMIT + usize::from(oversized);
        let filler = size - success.len();
        // Repeated complete blank lines are ignored by both original and native
        // parsers; actual cumulative byte accounting still includes every byte.
        let mut blank_line = vec![b' '; 8192];
        blank_line[8191] = b'\n';
        let block = Bytes::from(blank_line);
        let mut chunks = vec![success];
        chunks.extend(std::iter::repeat_n(block, filler / 8192));
        if !filler.is_multiple_of(8192) {
            chunks.push(Bytes::from(vec![b' '; filler % 8192]));
        }
        let live = stream_boundary(
            chunks,
            !oversized,
            oversized.then_some("Ollama returned too many download updates."),
        )
        .await;
        assert_eq!(live.bytes.load(Ordering::SeqCst), size);
    }
}

#[tokio::test]
async fn actual_native_302_response_never_follows_location_or_reaches_model_endpoints() {
    let row = definition(6).remove(0);
    let mut config = settings(&row);
    let client = NativeHttpClient::new().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let live = Arc::new(AtomicUsize::new(0));
    let stop = CancellationToken::new();
    let owner_stop = stop.clone();
    let owner_calls = recorded.clone();
    let owner_live = live.clone();
    let server = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                biased;
                _ = owner_stop.cancelled() => break,
                incoming = listener.accept() => {
                    let (stream, _) = incoming.unwrap(); let calls = owner_calls.clone();
                    let closed = owner_stop.clone(); let live = owner_live.clone();
                    tasks.spawn(async move {
                        struct Owned(Arc<AtomicUsize>);
                        impl Drop for Owned { fn drop(&mut self) { self.0.fetch_sub(1, Ordering::SeqCst); } }
                        live.fetch_add(1, Ordering::SeqCst); let _owned = Owned(live);
                        let service = service_fn(move |request: Request<Incoming>| {
                            calls.lock().unwrap().push(request.uri().path().to_owned());
                            async move { Ok::<_, Infallible>(Response::builder().status(302)
                                .header("location", format!("http://{address}/must-not-follow"))
                                .body(Full::new(Bytes::from_static(b"PRIVATE_REDIRECT_BODY"))).unwrap()) }
                        });
                        let connection = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service);
                        tokio::select! { biased; _ = closed.cancelled() => {}, _ = connection => {} }
                    });
                },
                Some(result) = tasks.join_next(), if !tasks.is_empty() => { result.unwrap(); },
            }
        }
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
    });
    config.ollama_endpoint = format!("http://{address}");
    let result = tokio::time::timeout(
        BOUND,
        setup_ollama(
            &client,
            &config,
            &CancellationToken::new(),
            &SetupOptions {
                pull: true,
                ..Default::default()
            },
            &mut |_| {},
        ),
    )
    .await;
    drop(client);
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
    let error = result.expect("local redirect check bounded").unwrap_err();
    assert_eq!(error.code, "OLLAMA_HTTP");
    assert!(!error.message.contains("PRIVATE_"));
    assert_eq!(*recorded.lock().unwrap(), ["/api/version"]);
    assert_eq!(live.load(Ordering::SeqCst), 0);
}
