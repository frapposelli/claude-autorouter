//! Frozen assertion observations with native policy and cancellation schedules.
use super::*;
use crate::evaluator::{ClassifierDecision, EvaluationError};
use crate::http_client::HttpError;
use autorouter_core::config::read_config;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame};
use hyper::{Request, Response};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::oneshot;

const CASES: &str = include_str!("../../../parity/cases/router-concurrency-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../parity/cases/router-concurrency-contracts.capture.json");

struct Observations {
    remaining: BTreeMap<usize, VecDeque<Value>>,
}
impl Observations {
    fn new(number: usize) -> Self {
        let capture: Value = serde_json::from_str(CAPTURE).unwrap();
        assert_eq!(
            capture["cases_sha256"],
            format!("{:x}", Sha256::digest(CASES))
        );
        let case: Value = CASES
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|row| row["number"] == number)
            .unwrap();
        let mut remaining = BTreeMap::<usize, VecDeque<Value>>::new();
        for observed in case["observations"].as_array().unwrap() {
            let index = observed["assertion_id"]
                .as_str()
                .unwrap()
                .rsplit('-')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            remaining
                .entry(index)
                .or_default()
                .push_back(observed.clone());
        }
        Self { remaining }
    }
    fn value(&mut self, index: usize, actual: Value) {
        let expected = self
            .remaining
            .get_mut(&index)
            .unwrap()
            .pop_front()
            .expect("extra native assertion");
        assert!(matches!(
            expected["method"].as_str(),
            Some("equal" | "deepEqual" | "ok")
        ));
        assert_eq!(expected["arguments"][0]["kind"], "json");
        assert_eq!(
            actual, expected["arguments"][0]["value"],
            "assertion {index}"
        );
    }
    fn cancelled(&mut self, index: usize, result: Result<ClassifierDecision, EvaluationError>) {
        assert!(matches!(result, Err(EvaluationError::Cancelled)));
        let expected = self.remaining.get_mut(&index).unwrap().pop_front().unwrap();
        assert_eq!(expected["method"], "rejects");
        assert_eq!(expected["arguments"][0]["kind"], "promise");
        // Native cancellation is an enum: JS reason/predicate identity remains
        // an explicit partial-mapping boundary, never silently normalized.
        assert!(matches!(
            expected["arguments"][1]["kind"].as_str(),
            Some("regexp" | "predicate")
        ));
    }
    fn finish(self) {
        assert!(
            self.remaining.values().all(VecDeque::is_empty),
            "missing native assertions"
        );
    }
}

#[test]
fn assertion_comparison_rejects_wrong_missing_extra_and_unadapted_observations() {
    assert!(
        std::panic::catch_unwind(|| {
            Observations::new(1).value(1, json!(2));
        })
        .is_err()
    );
    assert!(
        std::panic::catch_unwind(|| {
            Observations::new(1).finish();
        })
        .is_err()
    );
    assert!(
        std::panic::catch_unwind(|| {
            let mut observed = Observations::new(1);
            observed.value(1, json!(1));
            observed.value(1, json!(1));
        })
        .is_err()
    );
    assert!(
        std::panic::catch_unwind(|| {
            // A source rejection predicate may only enter the explicit native
            // cancellation adaptation, never masquerade as an ordinary boolean.
            Observations::new(3).value(1, json!(true));
        })
        .is_err()
    );
}

#[derive(Clone)]
struct Reply {
    status: u16,
    bytes: Bytes,
    stalled: bool,
}
impl Reply {
    fn answer(tier: &str, confidence: f64) -> Self {
        Self::json(json!({"answers":{"tier":{"choice":tier,"confidence":confidence}}}))
    }
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            bytes: Bytes::from(serde_json::to_vec(&value).unwrap()),
            stalled: false,
        }
    }
}
struct ResponseBody {
    bytes: Option<Bytes>,
    stalled: bool,
    finished: bool,
    aborted: Arc<AtomicBool>,
}
impl Body for ResponseBody {
    type Data = Bytes;
    type Error = HttpError;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, HttpError>>> {
        if let Some(bytes) = self.bytes.take() {
            return Poll::Ready(Some(Ok(Frame::data(bytes))));
        }
        if self.stalled {
            return Poll::Pending;
        }
        self.finished = true;
        Poll::Ready(None)
    }
}
impl Drop for ResponseBody {
    fn drop(&mut self) {
        if !self.finished {
            self.aborted.store(true, Ordering::SeqCst);
        }
    }
}
struct Call {
    path: String,
    send: Option<oneshot::Sender<Reply>>,
    aborted: Arc<AtomicBool>,
}
#[derive(Default)]
struct Mock {
    calls: Mutex<Vec<Call>>,
    automatic: Mutex<Option<Reply>>,
    active: AtomicUsize,
}
struct Active<'a> {
    mock: &'a Mock,
    aborted: Arc<AtomicBool>,
    completed: bool,
}
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.mock.active.fetch_sub(1, Ordering::SeqCst);
        if !self.completed {
            self.aborted.store(true, Ordering::SeqCst);
        }
    }
}
impl Mock {
    fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
    fn release(&self, index: usize, reply: Reply) -> bool {
        self.calls.lock().unwrap()[index]
            .send
            .take()
            .unwrap()
            .send(reply)
            .is_ok()
    }
    fn aborted(&self, index: usize) -> bool {
        self.calls.lock().unwrap()[index]
            .aborted
            .load(Ordering::SeqCst)
    }
}
impl HttpTransport for Mock {
    type ResponseBody = ResponseBody;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<ResponseBody>, HttpError> {
        self.active.fetch_add(1, Ordering::SeqCst);
        let aborted = Arc::new(AtomicBool::new(false));
        let mut active = Active {
            mock: self,
            aborted: aborted.clone(),
            completed: false,
        };
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        assert!(serde_json::from_slice::<Value>(&bytes).unwrap().is_object());
        let path = parts.uri.path().to_owned();
        let (send, receive) = oneshot::channel();
        self.calls.lock().unwrap().push(Call {
            path: path.clone(),
            send: Some(send),
            aborted: aborted.clone(),
        });
        let automatic = self.automatic.lock().unwrap().clone();
        let reply = if path == "/api/show" {
            Reply::json(json!({"details":{"parameter_size":"9B"}}))
        } else if let Some(reply) = automatic {
            reply
        } else {
            receive.await.map_err(|_| HttpError::Network)?
        };
        active.completed = true;
        Ok(Response::builder()
            .status(reply.status)
            .body(ResponseBody {
                bytes: (!reply.bytes.is_empty()).then_some(reply.bytes),
                stalled: reply.stalled,
                finished: false,
                aborted,
            })
            .unwrap())
    }
}
fn config() -> RouterConfig {
    read_config(&json!({"AUTOROUTER_EVALUATOR":"jev","TYPESAFE_API_KEY":"synthetic-key","AUTOROUTER_AUTH_MODE":"subscription"}), false, Path::new("/synthetic")).unwrap()
}
fn body(text: &str) -> Arc<JsDocument> {
    Arc::new(JsDocument::parse(&serde_json::to_vec(&json!({"model":"claude-haiku-4-5-20251001","max_tokens":16,"messages":[{"role":"user","content":text}]})).unwrap()).unwrap())
}
fn start(
    classifier: &Classifier<Mock>,
    document: Arc<JsDocument>,
    config: &RouterConfig,
    cancellation: CancellationToken,
) -> tokio::task::JoinHandle<Result<ClassifierDecision, EvaluationError>> {
    let classifier = classifier.clone();
    let config = config.clone();
    tokio::spawn(async move { classifier.classify(&document, &config, &cancellation).await })
}
async fn bounded(schedule: impl std::future::Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(5), schedule)
        .await
        .expect("native concurrency schedule exceeded its independent deadline");
}

async fn ready(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn eight_shared_evaluations_keep_independent_decisions_and_turn_records() {
    bounded(async {
        let mut observed = Observations::new(1);
        let mock = Arc::new(Mock::default());
        let router = Arc::new(Router::new(mock.clone(), config()));
        let mut tasks = Vec::new();
        for agent in 0..8 {
            let router = router.clone();
            tasks.push(tokio::spawn(async move {
                router
                    .route(
                        body("Synthetic task"),
                        RouteOptions {
                            scope: format!("agent-{agent}"),
                            prompt_id: format!("prompt-{agent}"),
                            ..RouteOptions::default()
                        },
                        &HeaderMap::new(),
                        &CancellationToken::new(),
                        "",
                    )
                    .await
                    .unwrap()
            }));
        }
        ready(|| router.classifier.pending_counts() == (1, 8) && mock.count() == 1).await;
        observed.value(1, json!(mock.count()));
        assert!(mock.release(0, Reply::answer("haiku", 1.0)));
        let mut decisions = Vec::new();
        for task in tasks {
            decisions.push(task.await.unwrap());
        }
        observed.value(
            2,
            json!(
                decisions
                    .iter()
                    .all(|row| row["source"] == "jev" && row["classified_tier"] == "haiku")
            ),
        );
        decisions[0]["tier"] = json!("opus");
        observed.value(3, decisions[1]["tier"].clone());
        let cached = router
            .classifier
            .classify(
                &body("Synthetic task"),
                &router.config,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        observed.value(4, json!(cached.tier));
        let (pending, subscribers) = router.classifier.pending_counts();
        observed.value(5, json!(pending));
        observed.value(6, json!(subscribers));
        observed.value(7, json!(router.policy.lock().unwrap().turns.record_count()));
        assert_eq!(mock.active.load(Ordering::SeqCst), 0);
        observed.finish();
    })
    .await;
}

#[tokio::test]
async fn cancelled_subscriber_leaves_other_waiter_and_cached_answer_intact() {
    bounded(async {
        let mut observed = Observations::new(3);
        let mock = Arc::new(Mock::default());
        let config = config();
        let classifier = Classifier::new(mock.clone(), &config);
        let cancel = CancellationToken::new();
        let first = start(&classifier, body("Synthetic task"), &config, cancel.clone());
        let second = start(
            &classifier,
            body("Synthetic task"),
            &config,
            CancellationToken::new(),
        );
        ready(|| classifier.pending_counts() == (1, 2) && mock.count() == 1).await;
        cancel.cancel();
        observed.cancelled(1, first.await.unwrap());
        observed.value(2, json!(mock.aborted(0)));
        observed.value(3, json!(classifier.pending_counts().1));
        assert!(mock.release(0, Reply::answer("opus", 1.0)));
        observed.value(4, json!(second.await.unwrap().unwrap().tier));
        observed.value(
            5,
            json!(
                classifier
                    .classify(&body("Synthetic task"), &config, &CancellationToken::new())
                    .await
                    .unwrap()
                    .source
            ),
        );
        observed.value(6, json!(mock.count()));
        assert_eq!(classifier.pending_counts(), (0, 0));
        assert_eq!(mock.active.load(Ordering::SeqCst), 0);
        observed.finish();
    })
    .await;
}

#[tokio::test]
async fn shared_classification_keeps_each_agents_confirmed_tool_model() {
    bounded(async {
    let mut observed = Observations::new(2);
    let mock = Arc::new(Mock::default());
    *mock.automatic.lock().unwrap() = Some(Reply::answer("haiku", 1.0));
    let router = Arc::new(Router::new(mock.clone(), config()));
    for (scope, model) in [("a", "claude-opus-5-5"), ("b", "claude-sonnet-5")] {
        router
            .route(
                body("Synthetic task"),
                RouteOptions {
                    scope: scope.into(),
                    prompt_id: "same".into(),
                    request_id: Some(scope.into()),
                    ..RouteOptions::default()
                },
                &HeaderMap::new(),
                &CancellationToken::new(),
                "",
            )
            .await
            .unwrap();
        assert!(router.complete(
            scope,
            &json!({"continuation_model":model,"tool_uses":[{"id":"read","model":model}]})
        ));
    }
    let before = mock.count();
    *mock.automatic.lock().unwrap() = None;
    let continuation = Arc::new(JsDocument::parse(br#"{"model":"claude-haiku-4-5-20251001","max_tokens":16,"messages":[{"role":"user","content":"Synthetic task"},{"role":"assistant","content":[{"type":"tool_use","id":"read","name":"Read","input":{}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"read","content":"Synthetic result"}]}]}"#).unwrap());
    let mut tasks = Vec::new();
    for scope in ["a", "b"] {
        let router = router.clone();
        let continuation = continuation.clone();
        tasks.push(tokio::spawn(async move {
            router
                .route(
                    continuation,
                    RouteOptions {
                        scope: scope.into(),
                        prompt_id: "same".into(),
                        ..RouteOptions::default()
                    },
                    &HeaderMap::new(),
                    &CancellationToken::new(),
                    "",
                )
                .await
                .unwrap()
        }));
    }
    ready(|| router.classifier.pending_counts() == (1, 2) && mock.count() == before + 1).await;
    assert!(mock.release(before, Reply::answer("haiku", 1.0)));
    let mut decisions = Vec::new();
    for task in tasks {
        decisions.push(task.await.unwrap());
    }
    observed.value(1, json!(mock.count() - before));
    observed.value(
        2,
        json!(
            decisions
                .iter()
                .map(|value| value["model"].clone())
                .collect::<Vec<_>>()
        ),
    );
    observed.value(
        3,
        json!(
            decisions
                .iter()
                .all(|value| value["reason"] == "tool_turn_pinned")
        ),
    );
    assert_eq!(router.classifier.pending_counts(), (0, 0));
    assert_eq!(mock.active.load(Ordering::SeqCst), 0);
    observed.finish();
    }).await;
}

#[tokio::test]
async fn zero_deadline_ollama_shares_metadata_without_merging_waiter_cancellation() {
    bounded(async {
    let mut observed = Observations::new(10);
    let config = read_config(&json!({"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_OLLAMA_TIMEOUT_MS":"0"}), false, Path::new("/synthetic")).unwrap();
    assert_eq!(config.ollama_timeout_ms, 0);
    let mock = Arc::new(Mock::default());
    let classifier = Classifier::new(mock.clone(), &config);
    let cancel = CancellationToken::new();
    let first = start(&classifier, body("Synthetic task"), &config, cancel.clone());
    let second = start(
        &classifier,
        body("Synthetic task"),
        &config,
        CancellationToken::new(),
    );
    ready(|| classifier.pending_counts() == (1, 2) && mock.count() == 2).await;
    cancel.cancel();
    observed.cancelled(1, first.await.unwrap());
    observed.value(2, json!(mock.aborted(1)));
    assert!(mock.release(1, Reply::json(json!({"model":config.ollama_model,"answers":{"tier":{"type":"choice","choice":"haiku","confidence":1,"probabilities":{"haiku":1,"sonnet":0,"opus":0}}},"usage":{"input_tokens":100,"output_tokens":1}}))));
    observed.value(3, json!(second.await.unwrap().unwrap().source));
    observed.value(
        4,
        json!(
            mock.calls
                .lock()
                .unwrap()
                .iter()
                .map(|call| call.path.clone())
                .collect::<Vec<_>>()
        ),
    );
    assert_eq!(classifier.pending_counts(), (0, 0));
    assert_eq!(mock.active.load(Ordering::SeqCst), 0);
    observed.finish();
    }).await;
}

#[tokio::test]
async fn last_waiter_cancellation_cannot_cache_or_remove_replacement_work() {
    bounded(async {
        let mut observed = Observations::new(4);
        let mock = Arc::new(Mock::default());
        let config = config();
        let classifier = Classifier::new(mock.clone(), &config);
        let cancellations = [CancellationToken::new(), CancellationToken::new()];
        let tasks: Vec<_> = cancellations
            .iter()
            .map(|cancel| start(&classifier, body("Synthetic task"), &config, cancel.clone()))
            .collect();
        ready(|| classifier.pending_counts() == (1, 2) && mock.count() == 1).await;
        for cancel in &cancellations {
            cancel.cancel();
        }
        for task in tasks {
            observed.cancelled(1, task.await.unwrap());
        }
        ready(|| mock.active.load(Ordering::SeqCst) == 0).await;
        observed.value(2, json!(mock.aborted(0)));
        observed.value(3, json!(classifier.pending_counts().0));
        let retry = start(
            &classifier,
            body("Synthetic task"),
            &config,
            CancellationToken::new(),
        );
        ready(|| mock.count() == 2).await;
        observed.value(4, json!(mock.count()));
        // Native cancellation drops the old transport future. The old server-side
        // answer cannot enter the replacement even when its producer still exists.
        assert!(!mock.release(0, Reply::answer("opus", 1.0)));
        tokio::task::yield_now().await;
        observed.value(5, json!(classifier.pending_counts().0));
        assert!(mock.release(1, Reply::answer("haiku", 1.0)));
        observed.value(6, json!(retry.await.unwrap().unwrap().tier));
        observed.value(
            7,
            json!(
                classifier
                    .classify(&body("Synthetic task"), &config, &CancellationToken::new())
                    .await
                    .unwrap()
                    .tier
            ),
        );
        assert_eq!(classifier.pending_counts(), (0, 0));
        assert_eq!(mock.active.load(Ordering::SeqCst), 0);
        observed.finish();
    })
    .await;
}

#[tokio::test]
async fn already_cancelled_cache_hits_and_unpolled_work_do_not_send() {
    bounded(async {
        let mut observed = Observations::new(5);
        let mock = Arc::new(Mock::default());
        let config = config();
        *mock.automatic.lock().unwrap() = Some(Reply::answer("sonnet", 1.0));
        let classifier = Classifier::new(mock.clone(), &config);
        let document = body("Synthetic task");
        classifier
            .classify(&document, &config, &CancellationToken::new())
            .await
            .unwrap();
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        observed.cancelled(1, classifier.classify(&document, &config, &cancelled).await);
        let immediate = CancellationToken::new();
        let document = body("new");
        let pending = classifier.classify(&document, &config, &immediate);
        immediate.cancel();
        observed.cancelled(2, pending.await);
        observed.value(3, json!(mock.count()));
        observed.value(4, json!(classifier.pending_counts().0));
        assert_eq!(mock.active.load(Ordering::SeqCst), 0);
        observed.finish();
    })
    .await;
}

#[tokio::test]
async fn shared_failures_leave_other_keys_and_later_retry_usable() {
    bounded(async {
        let mut observed = Observations::new(6);
        let mock = Arc::new(Mock::default());
        let config = config();
        let classifier = Classifier::new(mock.clone(), &config);
        let first = start(
            &classifier,
            body("Synthetic task"),
            &config,
            CancellationToken::new(),
        );
        let second = start(
            &classifier,
            body("Synthetic task"),
            &config,
            CancellationToken::new(),
        );
        ready(|| classifier.pending_counts() == (1, 2) && mock.count() == 1).await;
        let other = start(
            &classifier,
            body("different"),
            &config,
            CancellationToken::new(),
        );
        ready(|| classifier.pending_counts() == (2, 3) && mock.count() == 2).await;
        observed.value(1, json!(mock.count()));
        assert!(mock.release(
            0,
            Reply {
                status: 503,
                bytes: Bytes::from_static(b"PRIVATE_FAILURE"),
                stalled: false
            }
        ));
        assert!(mock.release(1, Reply::answer("haiku", 1.0)));
        let values = [
            first.await.unwrap().unwrap(),
            second.await.unwrap().unwrap(),
            other.await.unwrap().unwrap(),
        ];
        observed.value(
            2,
            json!(values.iter().map(|value| value.source).collect::<Vec<_>>()),
        );
        observed.value(3, json!(values[0].classifier_status));
        let retry = start(
            &classifier,
            body("Synthetic task"),
            &config,
            CancellationToken::new(),
        );
        ready(|| mock.count() == 3).await;
        assert!(mock.release(2, Reply::answer("opus", 1.0)));
        observed.value(4, json!(retry.await.unwrap().unwrap().tier));
        observed.value(5, json!(classifier.pending_counts().1));
        assert_eq!(mock.active.load(Ordering::SeqCst), 0);
        observed.finish();
    })
    .await;
}

#[tokio::test]
async fn original_body_floor_and_mutated_evaluator_config_sequence_is_not_cached() {
    bounded(async {
        let mut observed = Observations::new(7);
        let mock = Arc::new(Mock::default());
        let mut config = config();
        *mock.automatic.lock().unwrap() = Some(Reply::answer("haiku", 0.5));
        let classifier = Classifier::new(mock.clone(), &config);
        let cancel = CancellationToken::new();
        let document = body("Synthetic task");
        observed.value(
            1,
            json!(
                classifier
                    .classify(&document, &config, &cancel)
                    .await
                    .unwrap()
                    .tier
            ),
        );
        let mut opus = (*document).clone();
        opus.set_root_field_json("model", br#""claude-opus-5-5""#)
            .unwrap();
        observed.value(
            2,
            json!(
                classifier
                    .classify(&opus, &config, &cancel)
                    .await
                    .unwrap()
                    .tier
            ),
        );
        let mut opaque = (*document).clone();
        opaque
            .set_root_field_json("opaque_provider_field", br#""new context""#)
            .unwrap();
        classifier
            .classify(&opaque, &config, &cancel)
            .await
            .unwrap();
        config.min_confidence = 0.0;
        observed.value(
            3,
            json!(
                classifier
                    .classify(&document, &config, &cancel)
                    .await
                    .unwrap()
                    .tier
            ),
        );
        for step in 0..5 {
            match step {
                0 => config.jev_model = "other-synthetic-model".into(),
                1 => config.jev_key = Some("other-synthetic-key".into()),
                2 => config.jev_endpoint = "https://example.invalid/v1/systemone".into(),
                3 => config.jev_timeout_ms = 900,
                _ => config.state_chars = 1000,
            }
            classifier
                .classify(&document, &config, &cancel)
                .await
                .unwrap();
        }
        observed.value(4, json!(mock.count()));
        observed.value(
            5,
            json!(
                classifier
                    .classify(&document, &config, &cancel)
                    .await
                    .unwrap()
                    .source
            ),
        );
        assert_eq!(classifier.pending_counts(), (0, 0));
        assert_eq!(mock.active.load(Ordering::SeqCst), 0);
        observed.finish();
    })
    .await;
}

#[tokio::test]
async fn full_default_work_and_subscriber_limits_recover_after_all_cancellations() {
    bounded(async {
        let mut observed = Observations::new(8);
        let limits = crate::classifier::Limits::default();
        assert_eq!((limits.pending, limits.subscribers), (256, 1024));
        for distinct in [true, false] {
            let mock = Arc::new(Mock::default());
            let config = config();
            let classifier = Classifier::new(mock.clone(), &config);
            let limit = if distinct {
                limits.pending
            } else {
                limits.subscribers
            };
            let pending_count = if distinct { limit } else { 1 };
            let mut cancellations = Vec::new();
            let mut tasks = Vec::new();
            for index in 0..limit {
                let cancel = CancellationToken::new();
                tasks.push(start(
                    &classifier,
                    body(&if distinct {
                        index.to_string()
                    } else {
                        "same".into()
                    }),
                    &config,
                    cancel.clone(),
                ));
                cancellations.push(cancel);
            }
            ready(|| {
                classifier.pending_counts() == (pending_count, limit)
                    && mock.count() == pending_count
            })
            .await;
            let extra = classifier
                .classify(&body("overflow"), &config, &CancellationToken::new())
                .await
                .unwrap();
            observed.value(1, json!(extra.source));
            observed.value(2, json!(extra.classifier_error));
            observed.value(3, json!(classifier.pending_counts().0));
            observed.value(4, json!(classifier.pending_counts().1));
            for cancel in cancellations {
                cancel.cancel();
            }
            for task in tasks {
                assert!(matches!(
                    task.await.unwrap(),
                    Err(EvaluationError::Cancelled)
                ));
            }
            ready(|| mock.active.load(Ordering::SeqCst) == 0).await;
            observed.value(5, json!(classifier.pending_counts().0));
            observed.value(6, json!(classifier.pending_counts().1));
            observed.value(7, json!((0..mock.count()).all(|index| mock.aborted(index))));
            let retry = start(
                &classifier,
                body("overflow"),
                &config,
                CancellationToken::new(),
            );
            ready(|| mock.count() == pending_count + 1).await;
            assert!(mock.release(pending_count, Reply::answer("sonnet", 1.0)));
            observed.value(8, json!(retry.await.unwrap().unwrap().source));
            for index in 0..pending_count {
                assert!(!mock.release(index, Reply::answer("sonnet", 1.0)));
            }
            tokio::task::yield_now().await;
            assert_eq!(classifier.pending_counts(), (0, 0));
            assert_eq!(mock.active.load(Ordering::SeqCst), 0);
        }
        observed.finish();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn each_jev_body_failure_retries_privately_with_cleanup_and_deadline_after_headers() {
    bounded(async {
        let mut observed = Observations::new(9);
        for mode in ["oversize", "malformed", "stalled", "http_error"] {
            let mut config = config();
            config.jev_timeout_ms = 20;
            let mock = Arc::new(Mock::default());
            *mock.automatic.lock().unwrap() = Some(match mode {
                "oversize" => Reply {
                    status: 200,
                    bytes: Bytes::from(vec![0; 65537]),
                    stalled: true,
                },
                "malformed" => Reply {
                    status: 200,
                    bytes: Bytes::from_static(b"PRIVATE malformed JSON"),
                    stalled: false,
                },
                "stalled" => Reply {
                    status: 200,
                    bytes: Bytes::new(),
                    stalled: true,
                },
                _ => Reply {
                    status: 503,
                    bytes: Bytes::new(),
                    stalled: true,
                },
            });
            let classifier = Classifier::new(mock.clone(), &config);
            for _ in 0..2 {
                let result = classifier
                    .classify(&body("Synthetic task"), &config, &CancellationToken::new())
                    .await
                    .unwrap();
                observed.value(1, json!(result.source));
                observed.value(2, json!(result.classifier_error));
                observed.value(
                    3,
                    json!(!serde_json::to_string(&result).unwrap().contains("PRIVATE")),
                );
            }
            observed.value(4, json!(mock.count()));
            observed.value(5, json!(classifier.pending_counts().0));
            if mode != "malformed" {
                observed.value(6, json!((0..mock.count()).all(|index| mock.aborted(index))));
            }
            assert_eq!(classifier.pending_counts(), (0, 0));
            assert_eq!(mock.active.load(Ordering::SeqCst), 0);
        }
        observed.finish();
    })
    .await;
}
