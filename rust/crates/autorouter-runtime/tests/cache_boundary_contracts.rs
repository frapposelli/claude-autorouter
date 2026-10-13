//! Exact default cache boundaries, zero embedding options and active ownership.
use autorouter_core::{
    config::{RouterConfig, read_config},
    js_json::JsDocument,
    router::RouteOptions,
};
use autorouter_runtime::{
    classifier::Classifier,
    http_client::{HttpError, HttpTransport},
    router::Router,
    token_counter::{CacheOptions, TokenCounter},
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Request, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const CORPUS: &str = include_str!("../../../parity/cases/cache-boundary-contracts.jsonl");
const CAPTURE: &str = include_str!("../../../parity/cases/cache-boundary-contracts.capture.json");
const ORIGIN: u64 = 1000000;
fn case(id: &str) -> (Value, JsDocument) {
    assert_eq!(
        format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
        "0055526eeacc73e787a1a2f78abd0b9555c47c57075054cd60678ed61b8d44e0"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(CAPTURE.as_bytes())),
        "56cffa6b45599be71f44cbf75219388f0b06123718191800a4a013a8af3f3278"
    );
    let capture: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(
        capture["baseline_commit"],
        "ea930c247626ce2af5ccdad721b5121417bf4ad8"
    );
    assert_eq!(capture["status"], "passed");
    assert_eq!(capture["fixtures_executed"], 9);
    assert_eq!(capture["assertions_executed"], 24050);
    let line = CORPUS
        .lines()
        .find(|line| serde_json::from_str::<Value>(line).unwrap()["id"] == id)
        .unwrap();
    (
        serde_json::from_str(line).unwrap(),
        JsDocument::parse(line.as_bytes()).unwrap(),
    )
}
fn configuration(plan: &Value) -> RouterConfig {
    let mut config = read_config(&plan["config_env"], false, Path::new("/tmp")).unwrap();
    assert_eq!(config.cache_entries, 1000);
    assert_eq!(config.cache_ttl_ms, 300000);
    if let Some(entries) = plan["cache_options"]["entries"].as_u64() {
        config.cache_entries = usize::try_from(entries).unwrap();
    }
    if let Some(ttl) = plan["cache_options"]["ttl_ms"].as_u64() {
        config.cache_ttl_ms = ttl;
    }
    config
}
fn body(index: &Value) -> JsDocument {
    let text = if let Some(integer) = index.as_u64() {
        integer.to_string()
    } else {
        index.as_str().unwrap().to_owned()
    };
    JsDocument::parse(format!(r#"{{"model":"claude-sonnet-5","max_tokens":4096,"messages":[{{"role":"user","content":"Synthetic cache task {text}"}}]}}"#).as_bytes()).unwrap()
}
fn captured_body(document: &JsDocument, key: &str) -> Arc<JsDocument> {
    let node = document.get(document.root(), key).unwrap();
    Arc::new(JsDocument::parse(document.stringify_node(node).as_bytes()).unwrap())
}
fn recorded_headers(headers: &HeaderMap) -> Value {
    json!(
        headers
            .iter()
            .map(|(key, value)| (key.as_str().to_owned(), value.to_str().unwrap().to_owned()))
            .collect::<BTreeMap<_, _>>()
    )
}
fn count_headers(plan: &Value) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (key, value) in plan["count_headers"].as_object().unwrap() {
        headers.insert(
            hyper::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
            value.as_str().unwrap().parse().unwrap(),
        );
    }
    headers
}
#[derive(Clone, Copy)]
enum Kind {
    Classifier,
    Count,
    Continuity,
}
struct Wire {
    kind: Kind,
    calls: Mutex<Vec<Value>>,
    clock: Arc<AtomicU64>,
}
impl Wire {
    fn new(kind: Kind, clock: Arc<AtomicU64>) -> Self {
        Self {
            kind,
            calls: Mutex::new(Vec::new()),
            clock,
        }
    }
    fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}
impl HttpTransport for Wire {
    type ResponseBody = Full<Bytes>;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        assert!(bytes.len() < 8192);
        assert_eq!(parts.method.as_str(), "POST");
        let mut calls = self.calls.lock().unwrap();
        assert!(calls.len() < 1010);
        let number = calls.len() + 1;
        let response = match self.kind {
            Kind::Count => {
                assert_eq!(
                    parts.uri.to_string(),
                    "https://api.anthropic.com/v1/messages/count_tokens?beta=true"
                );
                format!(r#"{{"input_tokens":{number}}}"#)
            }
            Kind::Classifier | Kind::Continuity => {
                assert_eq!(
                    parts.uri.to_string(),
                    "https://api.typesafe.ai/v1/systemone"
                );
                let tier = if matches!(self.kind, Kind::Continuity) && number <= 2 {
                    "opus"
                } else {
                    "haiku"
                };
                format!(r#"{{"answers":{{"tier":{{"choice":"{tier}","confidence":0.99}}}}}}"#)
            }
        };
        calls.push(json!({"url":parts.uri.to_string(),"method":parts.method.as_str(),"headers":recorded_headers(&parts.headers),"body_utf8":std::str::from_utf8(&bytes).unwrap(),"response_utf8":response,"wall_ms":self.clock.load(Ordering::SeqCst)}));
        Ok(Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(response)))
            .unwrap())
    }
}
fn save(id: &str, steps: &[Value], wire: &Wire, extra: Value) {
    if let Some(directory) = std::env::var_os("AUTOROUTER_CACHE_BOUNDARY_REPORT_DIR") {
        let report =
            json!({"id":id,"calls":wire.calls.lock().unwrap().clone(),"steps":steps,"extra":extra});
        let bytes = serde_json::to_vec_pretty(&report).unwrap();
        assert!(bytes.len() <= 8 * 1024 * 1024);
        std::fs::write(
            Path::new(&directory).join(format!("native-{id}.json")),
            bytes,
        )
        .unwrap();
    }
}
fn program(plan: &Value) -> Vec<Value> {
    let mut expanded = Vec::new();
    for group in plan["program"].as_array().unwrap() {
        if group["op"] != "range" {
            expanded.push(group.clone());
            continue;
        }
        let count = group["count"].as_u64().unwrap();
        assert!(count <= 1000);
        for offset in 0..count {
            let calls = group["calls_constant"]
                .as_u64()
                .unwrap_or_else(|| group["calls_start"].as_u64().unwrap() + offset);
            let mut step = json!({"op":"call","index":group["start"].as_u64().unwrap()+offset,"expect":group["expect"],"expected_calls":calls});
            if let Some(start) = group["count_start"].as_u64() {
                step["expected_count"] = json!(start + offset);
            }
            expanded.push(step);
        }
    }
    assert!(expanded.len() <= 2010);
    expanded
}
async fn advance(milliseconds: u64, clock: &AtomicU64, start: Instant) {
    clock.fetch_add(milliseconds, Ordering::SeqCst);
    tokio::time::advance(Duration::from_millis(milliseconds)).await;
    assert_eq!(
        start.elapsed(),
        Duration::from_millis(clock.load(Ordering::SeqCst) - ORIGIN),
        "paused cache clock never advances implicitly"
    );
}
async fn classifier_plan(id: &str) {
    let (plan, _) = case(id);
    let config = configuration(&plan);
    let clock = Arc::new(AtomicU64::new(ORIGIN));
    let start = Instant::now();
    let wire = Arc::new(Wire::new(Kind::Classifier, clock.clone()));
    let classifier = Classifier::new(wire.clone(), &config);
    let cancellation = CancellationToken::new();
    let mut steps = Vec::new();
    for step in program(&plan) {
        if step["op"] == "advance" {
            let ms = step["milliseconds"].as_u64().unwrap();
            advance(ms, &clock, start).await;
            steps.push(json!({"operation":"advance","milliseconds":ms,"wall_ms":clock.load(Ordering::SeqCst)}));
            continue;
        }
        let document = body(&step["index"]);
        let before = document.stringify();
        let result = classifier
            .classify(&document, &config, &cancellation)
            .await
            .unwrap();
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(
            document.stringify(),
            before,
            "authoritative input immutable"
        );
        assert_eq!(
            wire.count() as u64,
            step["expected_calls"].as_u64().unwrap()
        );
        assert_eq!(
            value["source"],
            if step["expect"] == "hit" {
                "cache"
            } else {
                "jev"
            }
        );
        assert_eq!(value["tier"], "haiku");
        assert_eq!(value["classified_tier"], "haiku");
        assert_eq!(value["confidence"], 0.99);
        assert_eq!(value["reason"], "classified");
        assert_eq!(
            start.elapsed(),
            Duration::from_millis(clock.load(Ordering::SeqCst) - ORIGIN)
        );
        steps.push(json!({"operation":"classifier","body_utf8":before,"index":step["index"],"expected":step,"value":value,"calls":wire.count(),"wall_ms":clock.load(Ordering::SeqCst)}));
    }
    classifier.close().await;
    save(
        id,
        &steps,
        &wire,
        json!({"cache_observation":"Public native cache storage is private; exact resident hits and touched-entry eviction/call counts prove capacity and expiry semantics. Source literal entry counts remain in its raw transcript. Standalone native Classifier has no human-task state owner; source Router.classify leaves its separate TurnState empty."}),
    );
}
async fn count_plan(id: &str) {
    let (plan, _) = case(id);
    let config = configuration(&plan);
    let clock = Arc::new(AtomicU64::new(ORIGIN));
    let start = Instant::now();
    let wire = Arc::new(Wire::new(Kind::Count, clock.clone()));
    let defaults = CacheOptions::default();
    assert_eq!(defaults.entries, 100);
    assert_eq!(defaults.ttl, Duration::from_millis(300000));
    let options = CacheOptions {
        entries: plan["cache_options"]["entries"]
            .as_u64()
            .map(|n| usize::try_from(n).unwrap())
            .unwrap_or(defaults.entries),
        ttl: plan["cache_options"]["ttl_ms"]
            .as_u64()
            .map(Duration::from_millis)
            .unwrap_or(defaults.ttl),
    };
    let counter = if plan["cache_options"]["defaults"] == true {
        TokenCounter::new(wire.clone(), &config)
    } else {
        TokenCounter::with_cache(wire.clone(), &config, options)
    };
    let headers = count_headers(&plan);
    let cancellation = CancellationToken::new();
    let mut steps = Vec::new();
    for step in program(&plan) {
        if step["op"] == "advance" {
            let ms = step["milliseconds"].as_u64().unwrap();
            advance(ms, &clock, start).await;
            steps.push(json!({"operation":"advance","milliseconds":ms,"wall_ms":clock.load(Ordering::SeqCst)}));
            continue;
        }
        let document = body(&step["index"]);
        let before = document.stringify();
        let value = counter
            .count(
                &document,
                "claude-sonnet-5",
                &headers,
                &cancellation,
                plan["count_search"].as_str().unwrap(),
            )
            .await;
        assert_eq!(value, Some(step["expected_count"].as_u64().unwrap()));
        assert_eq!(
            wire.count() as u64,
            step["expected_calls"].as_u64().unwrap()
        );
        assert_eq!(document.stringify(), before);
        assert_eq!(
            start.elapsed(),
            Duration::from_millis(clock.load(Ordering::SeqCst) - ORIGIN)
        );
        steps.push(json!({"operation":"count","body_utf8":before,"index":step["index"],"expected":step,"value":value,"calls":wire.count(),"wall_ms":clock.load(Ordering::SeqCst)}));
    }
    save(
        id,
        &steps,
        &wire,
        json!({"cache_observation":"Both source count closure and native TokenCounter keep storage private; actual retained count values and all wire call counts distinguish hits from fresh evaluations."}),
    );
}
#[tokio::test(start_paused = true)]
async fn classifier_default_1000_1001_and_touched_lru_eviction() {
    classifier_plan("classifier-default-capacity").await;
}
#[tokio::test(start_paused = true)]
async fn classifier_default_300000_expiry_and_zero_embedding_options() {
    for id in [
        "classifier-default-ttl",
        "classifier-zero-ttl",
        "classifier-zero-capacity",
    ] {
        classifier_plan(id).await;
    }
}
#[tokio::test(start_paused = true)]
async fn count_default_100_101_and_touched_lru_eviction() {
    count_plan("count-default-capacity").await;
}
#[tokio::test(start_paused = true)]
async fn count_default_300000_expiry_and_zero_embedding_options() {
    for id in ["count-default-ttl", "count-zero-ttl", "count-zero-capacity"] {
        count_plan(id).await;
    }
}

fn options(value: &Value) -> RouteOptions {
    RouteOptions {
        scope: value["scope"].as_str().unwrap().into(),
        prompt_id: value["promptId"].as_str().unwrap().into(),
        request_id: Some(value["requestId"].as_str().unwrap().into()),
        ..Default::default()
    }
}
fn evidence(model: &str, tools: Vec<Value>) -> Value {
    json!({"model":model,"continuation_model":model,"stop_reason":if tools.is_empty(){"end_turn"}else{"tool_use"},"tool_uses":tools})
}
struct ContinuityRunner {
    router: Router<Wire>,
    wire: Arc<Wire>,
    clock: Arc<AtomicU64>,
    steps: Vec<Value>,
    start: Instant,
}
impl ContinuityRunner {
    async fn call(
        &mut self,
        body: Arc<JsDocument>,
        options: Value,
        model: &str,
        reason: &str,
        completion: Value,
        acceptance: bool,
    ) -> Value {
        let before = body.stringify();
        let decision = self
            .router
            .route(
                body.clone(),
                crate::options(&options),
                &HeaderMap::new(),
                &CancellationToken::new(),
                "",
            )
            .await
            .unwrap();
        assert_eq!(body.stringify(), before);
        assert_eq!(decision["model"], model);
        assert_eq!(decision["reason"], reason);
        let accepted = self
            .router
            .complete(options["requestId"].as_str().unwrap(), &completion);
        assert_eq!(accepted, acceptance);
        let state = self.router.continuity_snapshot();
        assert_eq!(state.attempts, 0);
        assert!(state.records <= 1000);
        assert_eq!(
            self.start.elapsed(),
            Duration::from_millis(self.clock.load(Ordering::SeqCst) - ORIGIN)
        );
        self.steps.push(json!({"operation":"route_complete","body_utf8":before,"options":options,"decision":decision,"complete_evidence":completion,"accepted":accepted,"calls":self.wire.count(),"wall_ms":self.clock.load(Ordering::SeqCst),"continuity":{"records":state.records,"attempts":state.attempts,"aliases":state.aliases}}));
        decision
    }
}
#[tokio::test(start_paused = true)]
async fn cache_eviction_and_expiry_preserve_confirmed_active_tasks_and_tool_ownership() {
    let (plan, exact) = case("active-ownership-survives-cache-pressure");
    let mut config = configuration(&plan);
    config.turn_ttl_ms = plan["turn_ttl_ms"].as_u64().unwrap();
    let opus = config.models.opus.clone();
    let haiku = config.models.haiku.clone();
    let clock = Arc::new(AtomicU64::new(ORIGIN));
    let now = clock.clone();
    let wire = Arc::new(Wire::new(Kind::Continuity, clock.clone()));
    let router = Router::with_clock(
        wire.clone(),
        config,
        Arc::new(move || now.load(Ordering::SeqCst)),
    );
    let mut run = ContinuityRunner {
        router,
        wire,
        clock,
        steps: Vec::new(),
        start: Instant::now(),
    };
    run.call(
        captured_body(&exact, "initial_tool_body"),
        plan["initial_tool_options"].clone(),
        &opus,
        "classified",
        evidence(&opus, vec![json!({"id":"owned-tool","model":opus})]),
        true,
    )
    .await;
    run.call(
        captured_body(&exact, "initial_human_body"),
        plan["initial_human_options"].clone(),
        &opus,
        "classified",
        evidence(&opus, vec![]),
        true,
    )
    .await;
    for index in 0..plan["fill_count"].as_u64().unwrap() {
        run.call(Arc::new(body(&json!(index))),json!({"scope":"filler-scope","promptId":format!("filler-{index}"),"requestId":format!("filler-request-{index}")}),&haiku,"classified",evidence(&haiku,vec![]),true).await;
    }
    assert_eq!(run.wire.count(), 1003);
    let ms = plan["advance_ms"].as_u64().unwrap();
    advance(ms, &run.clock, run.start).await;
    run.steps.push(
        json!({"operation":"advance","milliseconds":ms,"wall_ms":run.clock.load(Ordering::SeqCst)}),
    );
    let mut tool_options = plan["initial_tool_options"].clone();
    tool_options["requestId"] = json!("tool-continue");
    let tool = run
        .call(
            captured_body(&exact, "tool_continuation_body"),
            tool_options,
            &opus,
            "tool_turn_pinned",
            Value::Null,
            false,
        )
        .await;
    assert_eq!(tool["classified_tier"], "haiku");
    let mut human_options = plan["initial_human_options"].clone();
    human_options["requestId"] = json!("human-repeat");
    let human = run
        .call(
            captured_body(&exact, "initial_human_body"),
            human_options,
            &opus,
            "prompt_turn_pinned",
            Value::Null,
            false,
        )
        .await;
    assert_eq!(human["classified_tier"], "haiku");
    run.call(Arc::new(body(&json!("new-human"))),json!({"scope":"active-human-scope","promptId":"new-human","requestId":"new-human-request"}),&haiku,"classified",evidence(&haiku,vec![]),true).await;
    assert_eq!(run.wire.count(), 1006);
    let mut owned_options = plan["initial_tool_options"].clone();
    owned_options["requestId"] = json!("tool-after-new-human");
    let owned = run
        .call(
            captured_body(&exact, "tool_continuation_body"),
            owned_options,
            &opus,
            "tool_turn_pinned",
            Value::Null,
            false,
        )
        .await;
    assert_eq!(owned["classified_tier"], "haiku");
    assert_eq!(owned["source"], "cache");
    assert_eq!(run.wire.count(), 1006);
    let state = run.router.continuity_snapshot();
    assert_eq!(state.attempts, 0);
    run.router.close().await;
    save(
        plan["id"].as_str().unwrap(),
        &run.steps,
        &run.wire,
        json!({"final_continuity":{"records":state.records,"attempts":state.attempts,"aliases":state.aliases},"clock":"Classifier/count expiry use paused Tokio Instant; Router human-task clock is separately supplied as an exact u64 wall millisecond clock. Tests use only safe nonnegative integers."}),
    );
}
