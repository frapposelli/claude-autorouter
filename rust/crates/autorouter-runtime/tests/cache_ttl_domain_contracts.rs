//! Extreme direct-embedding TTLs retain successful bounded cache entries.
//! Each test owns one cache and only the classifier's existing tracked worker.
//! Default/equality/ownership schedules also run in cache_boundary_contracts.
use autorouter_core::{
    config::{RouterConfig, read_config},
    js_json::JsDocument,
};
use autorouter_runtime::{
    classifier::Classifier,
    http_client::{HttpError, HttpTransport},
    token_counter::{CacheOptions, TokenCounter},
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    HeaderMap, Request, Response,
    body::{Body, Frame},
};
use serde_json::{Value, json};
use std::{
    any::Any,
    collections::{BTreeMap, VecDeque},
    future::{Future, poll_fn},
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const SONNET: &str = "claude-sonnet-5";
const BOUND: Duration = Duration::from_secs(5);
struct ObservedBody {
    chunks: VecDeque<Bytes>,
    dropped: Arc<AtomicUsize>,
}
impl Body for ObservedBody {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        Poll::Ready(self.chunks.pop_front().map(|chunk| Ok(Frame::data(chunk))))
    }
}
impl Drop for ObservedBody {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}
struct Wire {
    classifier: bool,
    calls: Mutex<Vec<Value>>,
    dropped: Arc<AtomicUsize>,
}
impl Wire {
    fn new(classifier: bool) -> Self {
        Self {
            classifier,
            calls: Mutex::new(Vec::new()),
            dropped: Arc::new(AtomicUsize::new(0)),
        }
    }
    fn snapshot(&self) -> Value {
        json!({"calls":self.calls.lock().unwrap().clone(),"response_bodies_dropped":self.dropped.load(Ordering::SeqCst)})
    }
}
impl HttpTransport for Wire {
    type ResponseBody = ObservedBody;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        assert!(bytes.len() < 8192);
        let mut calls = self.calls.lock().unwrap();
        assert!(calls.len() < 4);
        let response = if self.classifier {
            r#"{"answers":{"tier":{"choice":"haiku","confidence":0.99}}}"#.to_owned()
        } else {
            format!(r#"{{"input_tokens":{}}}"#, calls.len() + 1)
        };
        calls.push(json!({"url":parts.uri.to_string(),"method":parts.method.to_string(),"headers":parts.headers.iter().map(|(key,value)|(key.as_str().to_owned(),value.to_str().unwrap().to_owned())).collect::<BTreeMap<_,_>>(),"body_utf8":std::str::from_utf8(&bytes).unwrap(),"response_utf8":response}));
        Ok(Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(ObservedBody {
                chunks: response
                    .as_bytes()
                    .chunks(13)
                    .map(Bytes::copy_from_slice)
                    .collect(),
                dropped: self.dropped.clone(),
            })
            .unwrap())
    }
}
fn configuration() -> RouterConfig {
    let config = read_config(
        &json!({"AUTOROUTER_EVALUATOR":"jev","TYPESAFE_API_KEY":"synthetic-cache-domain-key"}),
        false,
        Path::new("/synthetic"),
    )
    .unwrap();
    assert_eq!(config.cache_ttl_ms, 300000);
    assert!(config.jev_timeout_ms > 0);
    assert!(config.token_count_timeout_ms > 0);
    config
}
fn document() -> JsDocument {
    JsDocument::parse(br#"{"model":"claude-sonnet-5","max_tokens":4096,"messages":[{"role":"user","content":"Synthetic extreme TTL audit"}]}"#).unwrap()
}
fn panic_value(payload: Box<dyn Any + Send>) -> Value {
    let message = payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned());
    json!({"kind":"panic","message":message,"non_string_payload":message.is_none()})
}
// Catch actual poll panic and a separate actual future-destructor panic. A
// timeout drops only this owned future and never spawns a facsimile async task.
async fn capture<F: Future>(future: F, project: impl FnOnce(F::Output) -> Value) -> Value {
    let mut future = Box::pin(future);
    let outcome = tokio::time::timeout(
        BOUND,
        poll_fn(
            |cx| match catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(cx))) {
                Ok(Poll::Pending) => Poll::Pending,
                Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
                Err(payload) => Poll::Ready(Err(panic_value(payload))),
            },
        ),
    )
    .await;
    let poll_outcome = match outcome {
        Ok(Ok(value)) => json!({"kind":"returned","value":project(value)}),
        Ok(Err(panic)) => panic,
        Err(_) => json!({"kind":"control_timeout","milliseconds":5000}),
    };
    let destructor = match catch_unwind(AssertUnwindSafe(|| drop(future))) {
        Ok(()) => json!({"kind":"dropped_cleanly"}),
        Err(payload) => panic_value(payload),
    };
    json!({"poll":poll_outcome,"future_destructor":destructor})
}
fn arithmetic(duration: Duration) -> Value {
    let base = Instant::now();
    let checked = base.checked_add(duration);
    let unchecked = catch_unwind(AssertUnwindSafe(|| base + duration));
    let result = match unchecked {
        Ok(expiry) => json!({"kind":"returned","matches_checked_add":checked==Some(expiry)}),
        Err(payload) => panic_value(payload),
    };
    json!({"duration_seconds":duration.as_secs().to_string(),"duration_subsec_nanoseconds":duration.subsec_nanos(),"checked_add_representable":checked.is_some(),"unchecked_add":result,"same_base_instant":true})
}
fn save(id: &str, value: Value) {
    let Some(directory) = std::env::var_os("AUTOROUTER_CACHE_TTL_DOMAIN_REPORT_DIR") else {
        return;
    };
    let path = Path::new(&directory).join(format!("native-{id}.json"));
    let raw = serde_json::to_vec_pretty(&value).unwrap();
    assert!(raw.len() <= 65536);
    // Refuse to silently replace pre-fix/failing evidence.
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    file.write_all(&raw).unwrap();
}
async fn token_control(id: &str, duration: Duration, default: bool) {
    let arithmetic = arithmetic(duration);
    let config = configuration();
    let wire = Arc::new(Wire::new(false));
    let counter = if default {
        TokenCounter::new(wire.clone(), &config)
    } else {
        TokenCounter::with_cache(
            wire.clone(),
            &config,
            CacheOptions {
                entries: 100,
                ttl: duration,
            },
        )
    };
    let document = document();
    let before = document.stringify();
    let headers = HeaderMap::new();
    let cancellation = CancellationToken::new();
    let mut steps = Vec::new();
    for _ in 0..2 {
        let outcome = capture(
            counter.count(&document, SONNET, &headers, &cancellation, ""),
            |value| json!(value),
        )
        .await;
        steps.push(json!({"outcome":outcome,"wire":wire.snapshot()}));
        assert_eq!(document.stringify(), before);
    }
    drop(counter);
    let final_wire = wire.snapshot();
    save(
        id,
        json!({"id":id,"status":"raw_native_characterization","arithmetic":arithmetic,"steps":steps,"after_owner_drop":final_wire,"cancellation_enabled":!cancellation.is_cancelled(),"provider_timer_milliseconds":config.token_count_timeout_ms,"input_unchanged":true}),
    );
    {
        // Post-guard every successful value is retained, including None expiry.
        let zero = duration.is_zero();
        for (index, step) in steps.iter().enumerate() {
            assert_eq!(step["outcome"]["poll"]["kind"], "returned");
            assert_eq!(
                step["outcome"]["poll"]["value"],
                if zero { index + 1 } else { 1 }
            );
            assert_eq!(
                step["outcome"]["future_destructor"]["kind"],
                "dropped_cleanly"
            );
        }
        assert_eq!(
            final_wire["response_bodies_dropped"],
            if zero { 2 } else { 1 }
        );
    }
}
async fn classifier_control(id: &str, ttl: u64) {
    let duration = Duration::from_millis(ttl);
    let arithmetic = arithmetic(duration);
    let mut config = configuration();
    config.cache_ttl_ms = ttl;
    let wire = Arc::new(Wire::new(true));
    let classifier = Classifier::new(wire.clone(), &config);
    let document = document();
    let before = document.stringify();
    let cancellation = CancellationToken::new();
    let mut steps = Vec::new();
    for _ in 0..2 {
        let outcome = capture(
            classifier.classify(&document, &config, &cancellation),
            |value| match value {
                Ok(value) => json!({"kind":"decision","value":value}),
                Err(error) => json!({"kind":"evaluation_error","debug":format!("{error:?}")}),
            },
        )
        .await;
        steps.push(json!({"outcome":outcome,"wire":wire.snapshot()}));
        assert_eq!(document.stringify(), before);
    }
    let close = capture(classifier.close(), |()| json!({"closed":true})).await;
    drop(classifier);
    let final_wire = wire.snapshot();
    save(
        id,
        json!({"id":id,"status":"raw_native_characterization","arithmetic":arithmetic,"steps":steps,"close":close,"after_owner_drop":final_wire,"cancellation_enabled":!cancellation.is_cancelled(),"provider_timer_milliseconds":config.jev_timeout_ms,"input_unchanged":true,"private_cache_and_task_counts":"Unavailable to external consumers; no invented observations."}),
    );
    {
        // Post-guard every successful value is retained, including None expiry.
        for (index, step) in steps.iter().enumerate() {
            assert_eq!(step["outcome"]["poll"]["kind"], "returned");
            assert_eq!(step["outcome"]["poll"]["value"]["kind"], "decision");
            assert_eq!(step["outcome"]["poll"]["value"]["value"]["tier"], "haiku");
            assert_eq!(
                step["outcome"]["poll"]["value"]["value"]["source"],
                if ttl == 0 || index == 0 {
                    "jev"
                } else {
                    "cache"
                }
            );
            assert_eq!(
                step["outcome"]["future_destructor"]["kind"],
                "dropped_cleanly"
            );
        }
        assert_eq!(close["poll"]["kind"], "returned");
        assert_eq!(close["poll"]["value"]["closed"], true);
        assert_eq!(
            final_wire["response_bodies_dropped"],
            if ttl == 0 { 2 } else { 1 }
        );
    }
}
#[tokio::test]
async fn token_default() {
    token_control("token_default", Duration::from_millis(300000), true).await;
}
#[tokio::test]
async fn token_zero_ttl() {
    token_control("token_zero_ttl", Duration::ZERO, false).await;
}
#[tokio::test]
async fn token_duration_max() {
    token_control("token_duration_max", Duration::MAX, false).await;
}
#[tokio::test]
async fn token_u64_max_millis() {
    token_control(
        "token_u64_max_millis",
        Duration::from_millis(u64::MAX),
        false,
    )
    .await;
}
#[tokio::test]
async fn classifier_default() {
    classifier_control("classifier_default", 300000).await;
}
#[tokio::test]
async fn classifier_zero_ttl() {
    classifier_control("classifier_zero_ttl", 0).await;
}
#[tokio::test]
async fn classifier_u64_max_millis() {
    classifier_control("classifier_u64_max_millis", u64::MAX).await;
}

fn indexed_document(index: usize) -> JsDocument {
    JsDocument::parse(format!(r#"{{"model":"claude-sonnet-5","max_tokens":4096,"messages":[{{"role":"user","content":"Synthetic extreme TTL task {index}"}}]}}"#).as_bytes()).unwrap()
}
async fn huge_token_capacity(id: &str, entries: usize) {
    let duration = Duration::MAX;
    let arithmetic = arithmetic(duration);
    let config = configuration();
    let wire = Arc::new(Wire::new(false));
    let counter = TokenCounter::with_cache(
        wire.clone(),
        &config,
        CacheOptions {
            entries,
            ttl: duration,
        },
    );
    let headers = HeaderMap::new();
    let cancellation = CancellationToken::new();
    let indexes: &[usize] = if entries == 0 {
        &[0, 0]
    } else {
        &[0, 1, 0, 2, 0, 1]
    };
    let expected_calls: &[usize] = if entries == 0 {
        &[1, 2]
    } else {
        &[1, 2, 2, 3, 3, 4]
    };
    let expected_counts: &[usize] = if entries == 0 {
        &[1, 2]
    } else {
        &[1, 2, 1, 3, 1, 4]
    };
    let mut steps = Vec::new();
    for (position, index) in indexes.iter().enumerate() {
        let body = indexed_document(*index);
        let before = body.stringify();
        let outcome = capture(
            counter.count(&body, SONNET, &headers, &cancellation, ""),
            |value| json!(value),
        )
        .await;
        let snapshot = wire.snapshot();
        steps.push(
            json!({"operation":"count","body_utf8":before,"outcome":outcome,"wire":snapshot}),
        );
        assert_eq!(outcome["poll"]["kind"], "returned");
        assert_eq!(outcome["poll"]["value"], expected_counts[position]);
        assert_eq!(outcome["future_destructor"]["kind"], "dropped_cleanly");
        assert_eq!(
            snapshot["calls"].as_array().unwrap().len(),
            expected_calls[position]
        );
        assert_eq!(
            snapshot["response_bodies_dropped"],
            expected_calls[position]
        );
        assert_eq!(body.stringify(), before);
    }
    if entries > 0 {
        let body = indexed_document(0);
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let value = counter.count(&body, SONNET, &headers, &cancelled, "").await;
        assert_eq!(value, None);
        assert_eq!(wire.snapshot()["calls"].as_array().unwrap().len(), 4);
        steps.push(json!({"operation":"already_cancelled_count","native_result":value,"wire":wire.snapshot()}));
        let value = counter
            .count(&body, SONNET, &headers, &cancellation, "")
            .await;
        assert_eq!(value, Some(1));
        assert_eq!(wire.snapshot()["calls"].as_array().unwrap().len(), 4);
        steps.push(json!({"operation":"uncancelled_retained_count","native_result":value,"wire":wire.snapshot()}));
    }
    drop(counter);
    save(
        id,
        json!({"id":id,"status":"postguard_actual_successful_retention_control","arithmetic":arithmetic,"capacity":entries,"steps":steps,"after_owner_drop":wire.snapshot(),"private_cache_size":"Unavailable: actual hits, victim and wire count prove capacity/LRU; no invented observation."}),
    );
}
async fn huge_classifier_capacity(id: &str, entries: usize) {
    let arithmetic = arithmetic(Duration::from_millis(u64::MAX));
    let mut config = configuration();
    config.cache_entries = entries;
    config.cache_ttl_ms = u64::MAX;
    let wire = Arc::new(Wire::new(true));
    let classifier = Classifier::new(wire.clone(), &config);
    let cancellation = CancellationToken::new();
    let indexes: &[usize] = if entries == 0 {
        &[0, 0]
    } else {
        &[0, 1, 0, 2, 0, 1]
    };
    let expected_calls: &[usize] = if entries == 0 {
        &[1, 2]
    } else {
        &[1, 2, 2, 3, 3, 4]
    };
    let expected_sources: &[&str] = if entries == 0 {
        &["jev", "jev"]
    } else {
        &["jev", "jev", "cache", "jev", "cache", "jev"]
    };
    let mut steps = Vec::new();
    for (position, index) in indexes.iter().enumerate() {
        let body = indexed_document(*index);
        let before = body.stringify();
        let outcome = capture(
            classifier.classify(&body, &config, &cancellation),
            |value| match value {
                Ok(value) => json!({"kind":"decision","value":value}),
                Err(error) => json!({"kind":"evaluation_error","debug":format!("{error:?}")}),
            },
        )
        .await;
        let snapshot = wire.snapshot();
        steps.push(
            json!({"operation":"classify","body_utf8":before,"outcome":outcome,"wire":snapshot}),
        );
        assert_eq!(outcome["poll"]["kind"], "returned");
        assert_eq!(outcome["poll"]["value"]["kind"], "decision");
        assert_eq!(outcome["poll"]["value"]["value"]["tier"], "haiku");
        assert_eq!(
            outcome["poll"]["value"]["value"]["source"],
            expected_sources[position]
        );
        assert_eq!(outcome["future_destructor"]["kind"], "dropped_cleanly");
        assert_eq!(
            snapshot["calls"].as_array().unwrap().len(),
            expected_calls[position]
        );
        assert_eq!(
            snapshot["response_bodies_dropped"],
            expected_calls[position]
        );
        assert_eq!(body.stringify(), before);
    }
    if entries > 0 {
        let body = indexed_document(0);
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let value = classifier.classify(&body, &config, &cancelled).await;
        assert!(matches!(
            value,
            Err(autorouter_runtime::evaluator::EvaluationError::Cancelled)
        ));
        assert_eq!(wire.snapshot()["calls"].as_array().unwrap().len(), 4);
        steps.push(json!({"operation":"already_cancelled_classification","native_result":"EvaluationError::Cancelled","wire":wire.snapshot()}));
        let value = classifier
            .classify(&body, &config, &cancellation)
            .await
            .unwrap();
        assert_eq!(
            value.source,
            autorouter_core::contracts::DecisionSource::Cache
        );
        assert_eq!(value.tier, autorouter_core::contracts::Tier::Haiku);
        assert_eq!(wire.snapshot()["calls"].as_array().unwrap().len(), 4);
        steps.push(json!({"operation":"uncancelled_retained_classification","native_result":value,"wire":wire.snapshot()}));
    }
    let close = capture(classifier.close(), |()| json!({"closed":true})).await;
    assert_eq!(close["poll"]["kind"], "returned");
    assert_eq!(close["future_destructor"]["kind"], "dropped_cleanly");
    drop(classifier);
    save(
        id,
        json!({"id":id,"status":"postguard_actual_successful_retention_control","arithmetic":arithmetic,"capacity":entries,"steps":steps,"close":close,"after_owner_drop":wire.snapshot(),"private_cache_size":"Unavailable: actual hits, victim and wire count prove capacity/LRU; no invented observation."}),
    );
}
#[tokio::test]
async fn token_duration_max_capacity2_lru_and_cancellation() {
    huge_token_capacity("token_duration_max_capacity2_lru_and_cancellation", 2).await;
}
#[tokio::test]
async fn token_duration_max_zero_capacity() {
    huge_token_capacity("token_duration_max_zero_capacity", 0).await;
}
#[tokio::test]
async fn classifier_u64_max_capacity2_lru_and_cancellation() {
    huge_classifier_capacity("classifier_u64_max_capacity2_lru_and_cancellation", 2).await;
}
#[tokio::test]
async fn classifier_u64_max_zero_capacity() {
    huge_classifier_capacity("classifier_u64_max_zero_capacity", 0).await;
}
