//! Complete async-router counterparts of frozen turn-state definitions 5, 6,
//! 15 and 16. Transport answers are the original synthetic classifier responses;
//! actual classification, task discovery, selection and completion run natively.
use super::*;
use autorouter_core::config::read_config;
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response};
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

struct Evaluator {
    tier: &'static str,
    calls: AtomicUsize,
}

impl HttpTransport for Evaluator {
    type ResponseBody = Full<Bytes>;

    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, crate::http_client::HttpError> {
        assert_eq!(request.method(), hyper::Method::POST);
        assert_eq!(request.uri().path(), "/v1/systemone");
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Response::new(Full::new(Bytes::from(
            json!({"answers":{"tier":{"choice":self.tier,"confidence":0.99}}}).to_string(),
        ))))
    }
}

fn configuration() -> RouterConfig {
    read_config(
        &json!({"AUTOROUTER_EVALUATOR":"jev"}),
        false,
        Path::new("/synthetic"),
    )
    .unwrap()
}

fn evaluator(tier: &'static str) -> Arc<Evaluator> {
    Arc::new(Evaluator {
        tier,
        calls: AtomicUsize::new(0),
    })
}

async fn route(
    router: &Router<Evaluator>,
    body: &Value,
    scope: &str,
    prompt: &str,
    request: Option<&str>,
) -> Value {
    router
        .route(
            Arc::new(JsDocument::parse(body.to_string().as_bytes()).unwrap()),
            RouteOptions {
                scope: scope.into(),
                prompt_id: prompt.into(),
                request_id: request.map(str::to_owned),
                ..RouteOptions::default()
            },
            &HeaderMap::new(),
            &CancellationToken::new(),
            "",
        )
        .await
        .unwrap()
}

fn done(model: &str, tools: Value) -> Value {
    json!({"model":model,"continuation_model":model,"tool_uses":tools})
}

fn continuation(initial: &Value, id: &str, text: &str) -> Value {
    let mut body = initial.clone();
    let messages = body["messages"].as_array_mut().unwrap();
    messages.push(json!({"role":"assistant","content":[{"type":"tool_use","id":id,"name":"Read","input":{}}]}));
    messages.push(
        json!({"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":text}]}),
    );
    body
}

#[tokio::test]
async fn original_provider_fallback_survives_expiry_and_new_task_can_downgrade() {
    let mut config = configuration();
    config.cache_entries = 1;
    config.turn_ttl_ms = 1;
    let clock = Arc::new(AtomicU64::new(0));
    let now = clock.clone();
    let router = Router::with_clock(
        evaluator("haiku"),
        config.clone(),
        Arc::new(move || now.load(Ordering::SeqCst)),
    );
    let body = json!({"model":config.models.haiku,"messages":[{"role":"user","content":"Read and fix fixture"}],"max_tokens":1024});
    route(&router, &body, "session/agent", "task", Some("initial")).await;
    router.complete(
        "initial",
        &done(
            &config.models.opus,
            json!([{"id":"read","model":config.models.opus}]),
        ),
    );
    clock.store(1_000_000, Ordering::SeqCst);
    let mut body = continuation(&body, "read", "File");
    let next = route(&router, &body, "session/agent", "task", Some("next")).await;
    assert_eq!(next["model"], config.models.opus); // turn#5:assert-1
    assert_eq!(next["continuity_state"], "confirmed"); // turn#5:assert-2
    router.complete("next", &done(&config.models.opus, json!([])));
    let messages = body["messages"].as_array_mut().unwrap();
    messages.push(json!({"role":"assistant","content":"Finished"}));
    messages.push(json!({"role":"user","content":"Return [].length"}));
    let fresh = route(&router, &body, "session/agent", "fresh", Some("fresh")).await;
    assert_eq!(fresh["model"], config.models.haiku); // turn#5:assert-3
    router.complete("fresh", &Value::Null);
    router.shutdown();
}

#[tokio::test]
async fn original_unknown_safeguards_reject_admission_and_retain_active_continuity() {
    let mut config = configuration();
    config.turn_entries = Some(1);
    let transport = evaluator("opus");
    let router = Router::new(transport.clone(), config.clone());
    let initial = json!({"model":config.models.haiku,"messages":[{"role":"user","content":"Investigate a fixture"}]});
    route(&router, &initial, "occupied", "first", Some("first")).await;
    router.complete(
        "first",
        &done(
            &config.models.opus,
            json!([{"id":"pending","model":config.models.opus}]),
        ),
    );
    let mut protected = initial.clone();
    protected["safeguards"] = json!([{"type":"future_contract"}]);
    let next = route(&router, &protected, "other", "other", Some("other")).await;
    assert_eq!(next["model"], protected["model"]); // turn#6:assert-1
    assert_eq!(next["source"], "passthrough"); // turn#6:assert-2
    assert_eq!(next["continuity_state"], "capacity_exhausted"); // turn#6:assert-3
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1); // turn#6:assert-4
    {
        let policy = router.policy.lock().unwrap();
        assert_eq!(policy.turns.record_count(), 1); // turn#6:assert-5
        assert_eq!(policy.turns.attempt_count(), 0); // turn#6:assert-6
    }
    let body = continuation(&initial, "pending", "fixture");
    let retained = route(&router, &body, "occupied", "first", Some("retained")).await;
    assert_eq!(retained["model"], config.models.opus); // turn#6:assert-7
    assert_eq!(retained["continuity_state"], "confirmed"); // turn#6:assert-8
    router.complete("retained", &Value::Null);
    router.shutdown();
}

#[tokio::test]
async fn original_headerless_continuation_recovers_scoped_tool_owner() {
    let config = configuration();
    let router = Router::new(evaluator("haiku"), config.clone());
    let initial = json!({"model":config.models.haiku,"max_tokens":1024,"messages":[{"role":"user","content":"Read the file"}]});
    for (prompt, model) in [("a", &config.models.opus), ("b", &config.models.haiku)] {
        route(&router, &initial, "s", prompt, Some(prompt)).await;
        router.complete(
            prompt,
            &done(
                model,
                json!([{"id":format!("tool-{prompt}"),"model":model}]),
            ),
        );
    }
    let result = continuation(&initial, "tool-a", "File");
    let decision = route(&router, &result, "s", "", Some("continue")).await;
    assert_eq!(decision["model"], config.models.opus); // turn#15:assert-1
    assert_eq!(decision["continuity_state"], "confirmed"); // turn#15:assert-2
    router.complete("continue", &done(&config.models.opus, json!([])));
    assert_eq!(
        route(&router, &result, "different", "", None).await["continuity_state"],
        "unknown"
    ); // turn#15:assert-3
    router.shutdown();
}

#[tokio::test]
async fn original_ambiguous_headerless_continuation_cannot_overwrite_either_task() {
    let config = configuration();
    let router = Router::new(evaluator("haiku"), config.clone());
    let mut body =
        json!({"model":config.models.haiku,"messages":[{"role":"user","content":"Same task"}]});
    for (prompt, model) in [("a", &config.models.opus), ("b", &config.models.haiku)] {
        route(&router, &body, "s", prompt, Some(prompt)).await;
        router.complete(
            prompt,
            &done(
                model,
                json!([{"id":format!("tool-{prompt}"),"model":model}]),
            ),
        );
    }
    let messages = body["messages"].as_array_mut().unwrap();
    messages.push(json!({"role":"assistant","content":"Context is incomplete"}));
    messages.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"unobserved","content":"Result"}]}));
    let decision = route(&router, &body, "s", "", Some("ambiguous")).await;
    assert_eq!(decision["reason"], "unknown_continuation"); // turn#16:assert-1
    assert_eq!(decision["continuity_state"], "unknown"); // turn#16:assert-2
    assert!(!router.complete("ambiguous", &done(&config.models.sonnet, json!([])))); // turn#16:assert-3
    {
        let policy = router.policy.lock().unwrap();
        assert_eq!(
            policy.turns.tool_owner("s", &["tool-a".into()]).unwrap()["pin"]["model"],
            config.models.opus
        ); // turn#16:assert-4
        assert_eq!(
            policy.turns.tool_owner("s", &["tool-b".into()]).unwrap()["pin"]["model"],
            config.models.haiku
        ); // turn#16:assert-5
    }
    router.shutdown();
}
