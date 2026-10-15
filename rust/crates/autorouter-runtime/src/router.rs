//! Async routing boundary: locks protect synchronous turn transitions only.
//! Classification and token counting remain independently cancellable outside
//! that lock, preserving the source router's three observable await phases.

use std::future::{Future, poll_fn};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use autorouter_core::config::RouterConfig;
use autorouter_core::contracts::TypedRouteDecision;
use autorouter_core::js_json::JsDocument;
use autorouter_core::router::{RouteDecision, RouteOptions, Router as PolicyRouter};
use hyper::HeaderMap;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use tokio::{sync::oneshot, time::Instant};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::classifier::{Classifier, Limits};
use crate::evaluator::EvaluationError;
use crate::http_client::HttpTransport;
use crate::token_counter::TokenCounter;

pub struct Router<T> {
    config: RouterConfig,
    policy: Mutex<PolicyRouter>,
    classifier: Classifier<T>,
    counter: Arc<TokenCounter<T>>,
    now: Arc<dyn Fn() -> u64 + Send + Sync>,
    advisory_closing: Mutex<bool>,
    advisory_tasks: TaskTracker,
    advisory_stopping: CancellationToken,
    advisory_limit: usize,
    #[cfg(feature = "development-probes")]
    development_count_observer: Option<Arc<dyn Fn(Option<u64>) + Send + Sync>>,
    #[cfg(test)]
    advisory_gate: Mutex<Option<Arc<crate::classifier::shutdown_gate::WorkerPollGate>>>,
}
/// Read-only accounting; never exposes task keys, models or retained payloads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContinuitySnapshot {
    pub records: usize,
    pub attempts: usize,
    pub aliases: usize,
}

impl<T> Drop for Router<T> {
    fn drop(&mut self) {
        self.advisory_stopping.cancel();
        self.advisory_tasks.close();
    }
}

struct EarlyCount {
    task: Option<tokio::task::JoinHandle<Option<u64>>>,
    preserve_background: bool,
}
impl Drop for EarlyCount {
    fn drop(&mut self) {
        if !self.preserve_background
            && let Some(task) = &self.task
        {
            task.abort();
        }
    }
}
struct AttemptGuard<'a, T: HttpTransport + 'static> {
    router: &'a Router<T>,
    request_id: Option<String>,
    transferred: bool,
}
impl<T: HttpTransport + 'static> Drop for AttemptGuard<'_, T> {
    fn drop(&mut self) {
        if !self.transferred
            && let Some(request_id) = &self.request_id
        {
            self.router.complete(request_id, &Value::Null);
        }
    }
}
impl<T: HttpTransport + 'static> Router<T> {
    pub fn continuity_snapshot(&self) -> ContinuitySnapshot {
        let policy = self.policy.lock().unwrap();
        ContinuitySnapshot {
            records: policy.turns.record_count(),
            attempts: policy.turns.attempt_count(),
            aliases: policy.turns.alias_count(),
        }
    }
    pub fn new(transport: Arc<T>, config: RouterConfig) -> Self {
        Self::with_clock(
            transport,
            config,
            Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64
            }),
        )
    }
    pub fn with_clock(
        transport: Arc<T>,
        config: RouterConfig,
        now: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Self {
        Self::with_clock_and_limits(transport, config, now, Limits::default())
    }
    fn with_clock_and_limits(
        transport: Arc<T>,
        config: RouterConfig,
        now: Arc<dyn Fn() -> u64 + Send + Sync>,
        limits: Limits,
    ) -> Self {
        Self {
            policy: Mutex::new(PolicyRouter::new(config.clone())),
            classifier: Classifier::with_limits(transport.clone(), &config, limits)
                .expect("trusted router classifier limits are valid"),
            counter: Arc::new(TokenCounter::new(transport, &config)),
            config,
            now,
            advisory_closing: Mutex::new(false),
            advisory_tasks: TaskTracker::new(),
            advisory_stopping: CancellationToken::new(),
            // At most one speculative count per eligible classifier caller.
            // Retiring tasks retain this slot until their future is destroyed.
            advisory_limit: limits.subscribers,
            #[cfg(feature = "development-probes")]
            development_count_observer: None,
            #[cfg(test)]
            advisory_gate: Mutex::new(None),
        }
    }
    /// Development measurement observation after the real counter future
    /// resolves, including its parser/cache/finally work. This optional hook is
    /// absent from ordinary shipping builds and cannot change the count value.
    /// The probe owns its bounded collector and synchronous callback workload.
    #[cfg(feature = "development-probes")]
    pub fn with_development_count_observer(
        mut self,
        observer: Arc<dyn Fn(Option<u64>) + Send + Sync>,
    ) -> Self {
        self.development_count_observer = Some(observer);
        self
    }
    pub fn complete(&self, request_id: &str, evidence: &Value) -> bool {
        self.policy
            .lock()
            .unwrap()
            .complete(request_id, evidence, (self.now)())
    }
    pub fn complete_exact(
        &self,
        request_id: &str,
        evidence: &crate::response_observer::CompletionEvidence,
    ) -> bool {
        let evidence = evidence.continuation_model.as_ref().map(|model| {
            autorouter_core::turn_state::ContinuationEvidence {
                model: model.clone(),
                tools: evidence
                    .tool_uses
                    .iter()
                    .map(|tool| autorouter_core::turn_state::ToolIdentity {
                        id: tool.id.as_str().into(),
                        model: tool.model.clone(),
                    })
                    .collect(),
            }
        });
        self.policy
            .lock()
            .unwrap()
            .complete_exact(request_id, evidence.as_ref(), (self.now)())
    }
    pub fn shutdown(&self) {
        self.classifier.shutdown();
    }

    /// Permanently stops admission and drains internally spawned classifier
    /// and advisory-count work. Concurrent or cancelled waiters retain the same
    /// underlying ownership. Caller-owned route futures remain caller-owned.
    pub async fn close(&self) {
        {
            let mut closing = self.advisory_closing.lock().unwrap();
            *closing = true;
        }
        self.classifier.begin_close();
        self.advisory_stopping.cancel();
        self.advisory_tasks.close();
        tokio::join!(self.classifier.close(), self.advisory_tasks.wait());
    }

    fn start_early_count(
        &self,
        document: Arc<JsDocument>,
        model: &str,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
        started: Option<oneshot::Sender<()>>,
    ) -> Result<Option<tokio::task::JoinHandle<Option<u64>>>, EvaluationError> {
        let counter = self.counter.clone();
        let model = model.to_owned();
        let headers = headers.clone();
        let cancellation = cancellation.clone();
        let stopping = self.advisory_stopping.clone();
        let search = search.to_owned();
        #[cfg(feature = "development-probes")]
        let observer = self.development_count_observer.clone();
        let worker = {
            let closing = self.advisory_closing.lock().unwrap();
            if *closing {
                return Err(EvaluationError::Cancelled);
            }
            if self.advisory_tasks.len() >= self.advisory_limit {
                return Ok(None);
            }
            let worker = async move {
                let count = counter.count(&document, &model, &headers, &cancellation, &search);
                tokio::pin!(count);
                let mut started = started;
                let count = poll_fn(|context| {
                    let result = count.as_mut().poll(context);
                    // Spawning alone does not establish source callback-start
                    // order. Acknowledge only after the real count future has
                    // been polled; never wait for its response to classify.
                    if let Some(started) = started.take() {
                        let _ = started.send(());
                    }
                    result
                });
                let count = tokio::select! {
                    biased;
                    _ = stopping.cancelled() => None,
                    count = count => count,
                };
                #[cfg(feature = "development-probes")]
                if let Some(observer) = observer {
                    let _ = crate::callback_privacy::contain_callback(|| observer(count));
                }
                count
            };
            #[cfg(test)]
            let worker = crate::classifier::shutdown_gate::schedule(
                worker,
                self.advisory_gate.lock().unwrap().clone(),
            );
            self.advisory_tasks.track_future(worker)
        };
        Ok(Some(tokio::spawn(worker)))
    }

    async fn required_count(
        &self,
        document: &JsDocument,
        model: &str,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> Option<u64> {
        let count = tokio::select! {
            biased;
            _ = self.advisory_stopping.cancelled() => None,
            count = self.counter.count(document, model, headers, cancellation, search) => count,
        };
        #[cfg(feature = "development-probes")]
        if let Some(observer) = &self.development_count_observer {
            let _ = crate::callback_privacy::contain_callback(|| observer(count));
        }
        count
    }

    pub async fn route(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> Result<Value, EvaluationError> {
        self.route_exact(document, options, headers, cancellation, search)
            .await
            .map(|result| result.decision)
    }
    /// Legacy JSON observation facade. Production policy and gateway use the
    /// typed operation below, without erasing classifier metadata into Value.
    pub async fn route_exact(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> Result<RouteDecision, EvaluationError> {
        self.route_typed_exact(document, options, headers, cancellation, search)
            .await
            .map(|result| RouteDecision {
                decision: result.metadata.observation(),
                model: result.model,
            })
    }
    pub async fn route_typed_exact(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> Result<TypedRouteDecision, EvaluationError> {
        if *self.advisory_closing.lock().unwrap() {
            return Err(EvaluationError::Cancelled);
        }
        let started = Instant::now();
        let count_tokens = options.count_tokens;
        let mut attempt = AttemptGuard {
            router: self,
            request_id: options.request_id.clone(),
            transferred: false,
        };
        let start = self
            .policy
            .lock()
            .unwrap()
            .begin_route(&document, options, (self.now)());
        if let Some(mut passthrough) = start.passthrough_contract {
            passthrough.evaluation_latency_ms = 0.0;
            passthrough.latency_ms = rounded_ms(started);
            attempt.transferred = true;
            let model = document
                .get(document.root(), "model")
                .and_then(|node| document.string(node))
                .cloned()
                .unwrap_or_else(|| "".into());
            return Ok(TypedRouteDecision {
                metadata: passthrough,
                model,
            });
        }
        let early_model = start.early_count_model.clone();
        let mut early = EarlyCount {
            preserve_background: false,
            task: None,
        };
        if let Some(model) = early_model.as_ref() {
            let (started, ready) = oneshot::channel();
            early.task = self.start_early_count(
                document.clone(),
                model,
                headers,
                cancellation,
                search,
                Some(started),
            )?;
            if early.task.is_some() {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return Err(EvaluationError::Cancelled),
                    _ = self.advisory_stopping.cancelled() => return Err(EvaluationError::Cancelled),
                    // Worker failure still represents an unavailable count;
                    // its join result is handled by the existing count path.
                    _ = ready => {},
                }
            }
        }
        let evaluation_started = Instant::now();
        let decision = self
            .classifier
            .classify(&document, &self.config, cancellation)
            .await?;
        let evaluation_latency = rounded_ms(evaluation_started);
        let pending =
            self.policy
                .lock()
                .unwrap()
                .classified_typed(&document, start, decision, (self.now)());
        let count = if !count_tokens {
            None
        } else if let Some(model) = pending.count_model.as_ref() {
            if early_model.as_ref() == Some(model) {
                if let Some(early) = early.task.as_mut() {
                    early.await.ok().flatten()
                } else {
                    // Capacity can omit speculation, never a required count.
                    self.required_count(&document, model, headers, cancellation, search)
                        .await
                }
            } else {
                self.required_count(&document, model, headers, cancellation, search)
                    .await
            }
        } else {
            None
        };
        // A cancelled request must not install a new continuity selection even
        // if an advisory count independently resolves to unknown on abort.
        if cancellation.is_cancelled() || self.advisory_stopping.is_cancelled() {
            return Err(EvaluationError::Cancelled);
        }
        let mut decision = self.policy.lock().unwrap().finish_route_typed_exact(
            &document,
            pending,
            count,
            (self.now)(),
        );
        decision.metadata.evaluation_latency_ms = evaluation_latency;
        decision.metadata.latency_ms = rounded_ms(started);
        // An unused advisory count has the source promise's bounded lifetime:
        // it may complete/cache until its deadline or request cancellation.
        // Dropping/aborting route before success instead aborts that task.
        early.preserve_background = true;
        attempt.transferred = true;
        Ok(decision)
    }
}
fn rounded_ms(start: Instant) -> f64 {
    (start.elapsed().as_secs_f64() * 100_000.0).round() / 100.0
}

#[cfg(test)]
#[path = "router_contracts.rs"]
mod contracts;

#[cfg(test)]
#[path = "router_turn_contracts.rs"]
mod turn_contracts;

#[cfg(test)]
#[path = "router_concurrency_contracts.rs"]
mod concurrency_contracts;

#[cfg(test)]
#[path = "ollama_router_contracts.rs"]
mod ollama_contracts;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_client::HttpError;
    use autorouter_core::config::read_config;
    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::{Request, Response};
    use std::{
        path::Path,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use tokio::sync::Semaphore;

    struct Mock {
        counts: AtomicUsize,
        evaluations: AtomicUsize,
        started: Mutex<Vec<&'static str>>,
        count_value: AtomicUsize,
        active: Arc<AtomicUsize>,
        count_permits: Semaphore,
        evaluation_permits: Semaphore,
        block: AtomicBool,
        opus: AtomicBool,
    }
    impl Mock {
        fn new(block: bool) -> Self {
            Self {
                counts: AtomicUsize::new(0),
                evaluations: AtomicUsize::new(0),
                started: Mutex::new(Vec::new()),
                count_value: AtomicUsize::new(1000),
                active: Arc::new(AtomicUsize::new(0)),
                count_permits: Semaphore::new(0),
                evaluation_permits: Semaphore::new(0),
                block: AtomicBool::new(block),
                opus: AtomicBool::new(false),
            }
        }
    }
    struct Active(Arc<AtomicUsize>);
    impl Drop for Active {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    impl HttpTransport for Mock {
        type ResponseBody = Full<Bytes>;
        async fn request(
            &self,
            request: Request<Full<Bytes>>,
        ) -> Result<Response<Full<Bytes>>, HttpError> {
            self.active.fetch_add(1, Ordering::SeqCst);
            let _active = Active(self.active.clone());
            let count = request.uri().path().ends_with("count_tokens");
            self.started
                .lock()
                .unwrap()
                .push(if count { "count" } else { "evaluate" });
            if count {
                self.counts.fetch_add(1, Ordering::SeqCst);
            } else {
                self.evaluations.fetch_add(1, Ordering::SeqCst);
            }
            if self.block.load(Ordering::SeqCst) {
                if count {
                    self.count_permits.acquire().await.unwrap().forget();
                } else {
                    self.evaluation_permits.acquire().await.unwrap().forget();
                }
            }
            let bytes = if count {
                json!({"input_tokens":self.count_value.load(Ordering::SeqCst)})
            } else {
                json!({"answers":{"tier":{"choice":if self.opus.load(Ordering::SeqCst){"opus"}else{"haiku"},"confidence":0.9}}})
            };
            Ok(Response::new(Full::new(Bytes::from(
                serde_json::to_vec(&bytes).unwrap(),
            ))))
        }
    }
    fn config() -> RouterConfig {
        read_config(&json!({"AUTOROUTER_EVALUATOR":"jev","TYPESAFE_API_KEY":"synthetic-jev","ANTHROPIC_API_KEY":"synthetic-claude"}),false,Path::new("/synthetic")).unwrap()
    }
    fn document(large: bool) -> Arc<JsDocument> {
        Arc::new(JsDocument::parse(&serde_json::to_vec(&json!({"model":"claude-haiku-4-5-20251001","messages":[{"role":"user","content":if large{"a".repeat(160_000)}else{"Synthetic task".into()}}],"max_tokens":4096,"thinking":{"type":"disabled"}})).unwrap()).unwrap())
    }
    fn options() -> RouteOptions {
        RouteOptions {
            scope: "synthetic-scope".into(),
            prompt_id: "synthetic-prompt".into(),
            request_id: Some("synthetic-request".into()),
            count_tokens: true,
            ..RouteOptions::default()
        }
    }
    #[tokio::test]
    async fn typed_passthrough_keeps_exact_identities_without_evaluator_or_count_calls() {
        let transport = Arc::new(Mock::new(false));
        let router = Router::new(transport.clone(), config());
        let cancellation = CancellationToken::new();
        let mut decisions = Vec::new();
        for bytes in [
            br#"{"model":"synthetic-\ud800","messages":[{"role":"user","content":"Synthetic"}]}"#
                .as_slice(),
            br#"{"model":"synthetic-\ud801","messages":[{"role":"user","content":"Synthetic"}]}"#
                .as_slice(),
        ] {
            let document = Arc::new(JsDocument::parse(bytes).unwrap());
            let result = router
                .route_typed_exact(
                    document,
                    RouteOptions {
                        request_class: "auxiliary".into(),
                        ..RouteOptions::default()
                    },
                    &HeaderMap::new(),
                    &cancellation,
                    "",
                )
                .await
                .unwrap();
            assert_eq!(
                result.metadata.source,
                autorouter_core::contracts::DecisionSource::Passthrough
            );
            assert_eq!(result.metadata.reason, "internal_request");
            assert!(result.metadata.tier.is_none() && result.metadata.evaluator.is_none());
            assert!(result.metadata.latency_ms.is_finite() && result.metadata.latency_ms >= 0.0);
            assert_eq!(result.metadata.evaluation_latency_ms, 0.0);
            decisions.push(result);
        }
        assert_eq!(decisions[0].metadata.model, decisions[1].metadata.model);
        assert_ne!(decisions[0].model, decisions[1].model);
        assert_eq!(transport.evaluations.load(Ordering::SeqCst), 0);
        assert_eq!(transport.counts.load(Ordering::SeqCst), 0);
        router.close().await;
    }
    async fn wait_requests(mock: &Mock) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while mock.counts.load(Ordering::SeqCst) != 1
                || mock.evaluations.load(Ordering::SeqCst) != 1
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    async fn wait_idle(mock: &Mock) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while mock.active.load(Ordering::SeqCst) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[cfg(feature = "development-probes")]
    #[tokio::test]
    async fn development_count_observer_reports_actual_finished_value_not_admission() {
        let mock = Arc::new(Mock::new(true));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let sink = observed.clone();
        let router = Router::new(mock.clone(), config()).with_development_count_observer(Arc::new(
            move |count| sink.lock().unwrap().push(count),
        ));
        let task = router
            .start_early_count(
                document(true),
                "claude-haiku-4-5-20251001",
                &HeaderMap::new(),
                &CancellationToken::new(),
                "",
                None,
            )
            .unwrap()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while mock.counts.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(observed.lock().unwrap().is_empty());
        mock.count_permits.add_permits(1);
        assert_eq!(task.await.unwrap(), Some(1000));
        assert_eq!(*observed.lock().unwrap(), [Some(1000)]);
        let unsupported = JsDocument::parse(br#"{"model":"claude-haiku-4-5-20251001","messages":[{"role":"user","content":"Synthetic"}],"synthetic_unknown":true}"#).unwrap();
        assert_eq!(
            router
                .required_count(
                    &unsupported,
                    "claude-haiku-4-5-20251001",
                    &HeaderMap::new(),
                    &CancellationToken::new(),
                    ""
                )
                .await,
            None
        );
        assert_eq!(*observed.lock().unwrap(), [Some(1000), None]);
        assert_eq!(mock.counts.load(Ordering::SeqCst), 1);
        router.close().await;
    }

    #[tokio::test]
    async fn early_count_and_classification_run_concurrently_and_count_is_reused() {
        let mock = Arc::new(Mock::new(true));
        let config = config();
        let haiku = config.models.haiku.clone();
        let router = Arc::new(Router::new(mock.clone(), config));
        let task_router = router.clone();
        let task = tokio::spawn(async move {
            task_router
                .route(
                    document(true),
                    options(),
                    &HeaderMap::new(),
                    &CancellationToken::new(),
                    "",
                )
                .await
        });
        wait_requests(&mock).await;
        mock.evaluation_permits.add_permits(1);
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        mock.count_permits.add_permits(1);
        let decision = task.await.unwrap().unwrap();
        assert_eq!(decision["model"], haiku);
        assert_eq!(decision["context_check"], "within_budget");
        assert_eq!(decision["counted_input_tokens"], 1000);
        assert_eq!(mock.counts.load(Ordering::SeqCst), 1);
        assert!(decision["evaluation_latency_ms"].as_f64().is_some());
        assert!(!router.complete("synthetic-request", &Value::Null));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn count_starts_before_evaluator_and_keeps_both_responses_concurrent() {
        let mock = Arc::new(Mock::new(true));
        mock.count_value.store(40000, Ordering::SeqCst);
        let settings = config();
        let haiku = settings.models.haiku.clone();
        let document = Arc::new(
            JsDocument::parse(
                &serde_json::to_vec(&json!({
                    "model":haiku,"max_tokens":4096,
                    "messages":[{"role":"user","content":"Simple task"}],
                    "system":"x".repeat(160000)
                }))
                .unwrap(),
            )
            .unwrap(),
        );
        let before = document.stringify();
        let router = Arc::new(Router::new(mock.clone(), settings));
        let gate = Arc::new(crate::classifier::shutdown_gate::WorkerPollGate::default());
        let release = gate.hold();
        *router.advisory_gate.lock().unwrap() = Some(gate.clone());
        let task_router = router.clone();
        let task_document = document.clone();
        let task = tokio::spawn(async move {
            task_router
                .route(
                    task_document,
                    options(),
                    &HeaderMap::new(),
                    &CancellationToken::new(),
                    "",
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), gate.wait_blocked())
            .await
            .unwrap();
        // Positively hold the admitted count before its first poll. Merely
        // spawning it must not let the evaluator overtake its actual start.
        assert_eq!(router.advisory_tasks.len(), 1);
        assert!(mock.started.lock().unwrap().is_empty());
        assert_eq!(mock.counts.load(Ordering::SeqCst), 0);
        assert_eq!(mock.evaluations.load(Ordering::SeqCst), 0);
        assert!(!task.is_finished());
        drop(release);
        wait_requests(&mock).await;
        // Record outside the transport: a caught callback panic/fallback
        // cannot masquerade as having executed the expected start sequence.
        assert_eq!(*mock.started.lock().unwrap(), ["count", "evaluate"]);
        assert_eq!(mock.active.load(Ordering::SeqCst), 2);
        mock.evaluation_permits.add_permits(1);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while mock.active.load(Ordering::SeqCst) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            !task.is_finished(),
            "count response must still be independently pending"
        );
        mock.count_permits.add_permits(1);
        let decision = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(decision["model"], haiku);
        assert_eq!(decision["source"], "jev");
        assert_eq!(decision["context_check"], "within_budget");
        assert_eq!(decision["counted_input_tokens"], 40000);
        assert_eq!(mock.counts.load(Ordering::SeqCst), 1);
        assert_eq!(mock.evaluations.load(Ordering::SeqCst), 1);
        assert_eq!(document.stringify(), before);
        assert!(!router.complete("synthetic-request", &Value::Null));
        tokio::time::timeout(std::time::Duration::from_secs(2), router.close())
            .await
            .unwrap();
        assert_eq!(mock.active.load(Ordering::SeqCst), 0);
        assert!(router.advisory_tasks.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_before_count_first_poll_never_starts_evaluator_and_releases_slot() {
        let mock = Arc::new(Mock::new(true));
        let router = Router::new(mock.clone(), config());
        let gate = Arc::new(crate::classifier::shutdown_gate::WorkerPollGate::default());
        let release = gate.hold();
        *router.advisory_gate.lock().unwrap() = Some(gate.clone());
        let caller = CancellationToken::new();
        let headers = HeaderMap::new();
        let mut route = Box::pin(router.route(document(true), options(), &headers, &caller, ""));
        assert!(
            poll_fn(|context| std::task::Poll::Ready(route.as_mut().poll(context)))
                .await
                .is_pending()
        );
        tokio::time::timeout(std::time::Duration::from_secs(2), gate.wait_blocked())
            .await
            .unwrap();
        assert_eq!(router.advisory_tasks.len(), 1);
        assert!(mock.started.lock().unwrap().is_empty());
        caller.cancel();
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), &mut route)
                .await
                .unwrap(),
            Err(EvaluationError::Cancelled)
        ));
        drop(route);
        tokio::time::timeout(std::time::Duration::from_secs(2), gate.wait_finished())
            .await
            .unwrap();
        assert!(router.advisory_tasks.is_empty());
        assert!(mock.started.lock().unwrap().is_empty());
        assert_eq!(mock.counts.load(Ordering::SeqCst), 0);
        assert_eq!(mock.evaluations.load(Ordering::SeqCst), 0);
        assert_eq!(mock.active.load(Ordering::SeqCst), 0);
        assert_eq!(router.classifier.pending_counts(), (0, 0));
        drop(release);
        *router.advisory_gate.lock().unwrap() = None;
        mock.block.store(false, Ordering::SeqCst);
        let decision = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            router.route(
                document(true),
                options(),
                &headers,
                &CancellationToken::new(),
                "",
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(decision["source"], "jev");
        assert_eq!(mock.counts.load(Ordering::SeqCst), 1);
        assert_eq!(mock.evaluations.load(Ordering::SeqCst), 1);
        assert!(!router.complete("synthetic-request", &Value::Null));
        tokio::time::timeout(std::time::Duration::from_secs(2), router.close())
            .await
            .unwrap();
        assert!(router.advisory_tasks.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stopped_advisory_does_not_acknowledge_an_unpolled_counter() {
        let mock = Arc::new(Mock::new(true));
        let router = Router::new(mock.clone(), config());
        let gate = Arc::new(crate::classifier::shutdown_gate::WorkerPollGate::default());
        let release = gate.hold();
        *router.advisory_gate.lock().unwrap() = Some(gate.clone());
        let (started, ready) = oneshot::channel();
        let task = router
            .start_early_count(
                document(true),
                &router.config.models.haiku,
                &HeaderMap::new(),
                &CancellationToken::new(),
                "",
                Some(started),
            )
            .unwrap()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), gate.wait_blocked())
            .await
            .unwrap();
        assert_eq!(router.advisory_tasks.len(), 1);
        router.advisory_stopping.cancel();
        drop(release);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap(),
            None
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), ready)
                .await
                .unwrap()
                .is_err()
        );
        assert!(mock.started.lock().unwrap().is_empty());
        assert_eq!(mock.active.load(Ordering::SeqCst), 0);
        assert!(router.advisory_tasks.is_empty());
        tokio::time::timeout(std::time::Duration::from_secs(2), router.close())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn advisory_count_disabled_never_contacts_count_endpoint_for_large_context() {
        let mock = Arc::new(Mock::new(false));
        let router = Router::new(mock.clone(), config());
        let mut options = options();
        options.count_tokens = false;
        let decision = router
            .route(
                document(true),
                options,
                &HeaderMap::new(),
                &CancellationToken::new(),
                "",
            )
            .await
            .unwrap();
        assert_eq!(decision["context_check"], "count_unavailable");
        assert_eq!(mock.evaluations.load(Ordering::SeqCst), 1);
        assert_eq!(mock.counts.load(Ordering::SeqCst), 0);
        router.complete("synthetic-request", &Value::Null);
    }

    #[tokio::test]
    async fn exact_auxiliary_model_reaches_request_preparation_without_unicode_repair() {
        let mock = Arc::new(Mock::new(false));
        let router = Router::new(mock.clone(), config());
        let document = Arc::new(
            JsDocument::parse(
                br#"{"model":"vendor-\ud800","messages":[{"role":"user","content":"Synthetic"}]}"#,
            )
            .unwrap(),
        );
        let mut options = options();
        options.request_class = "auxiliary".into();
        let decision = router
            .route_exact(
                document.clone(),
                options,
                &HeaderMap::new(),
                &CancellationToken::new(),
                "",
            )
            .await
            .unwrap();
        assert_eq!(decision.model.units().last(), Some(&0xd800));
        let (prepared, changes) = autorouter_core::model_request::prepare_request_document_exact(
            &document,
            &decision.model,
        );
        assert!(changes.is_empty());
        assert_eq!(prepared.stringify(), document.stringify());
        assert_eq!(mock.evaluations.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn unused_early_count_retains_bounded_source_lifetime_and_request_cancellation() {
        let mock = Arc::new(Mock::new(true));
        mock.opus.store(true, Ordering::SeqCst);
        let config = config();
        let opus = config.models.opus.clone();
        let router = Arc::new(Router::new(mock.clone(), config));
        let task_router = router.clone();
        let cancel = CancellationToken::new();
        let caller = cancel.clone();
        let task = tokio::spawn(async move {
            task_router
                .route(document(true), options(), &HeaderMap::new(), &caller, "")
                .await
        });
        wait_requests(&mock).await;
        mock.evaluation_permits.add_permits(1);
        let decision = task.await.unwrap().unwrap();
        assert_eq!(decision["model"], opus);
        assert_eq!(mock.active.load(Ordering::SeqCst), 1);
        cancel.cancel();
        wait_idle(&mock).await;
        assert_eq!(mock.counts.load(Ordering::SeqCst), 1);
        router.complete("synthetic-request", &Value::Null);
    }

    #[tokio::test]
    async fn dropping_route_aborts_advisory_count_and_last_classifier_waiter() {
        let mock = Arc::new(Mock::new(true));
        let router = Arc::new(Router::new(mock.clone(), config()));
        let task_router = router.clone();
        let task = tokio::spawn(async move {
            task_router
                .route(
                    document(true),
                    options(),
                    &HeaderMap::new(),
                    &CancellationToken::new(),
                    "",
                )
                .await
        });
        wait_requests(&mock).await;
        task.abort();
        let _ = task.await;
        wait_idle(&mock).await;
        assert!(!router.complete(
            "synthetic-request",
            &json!({"continuation_model":"claude-haiku-4-5-20251001","tool_uses":[]})
        ));
    }

    #[tokio::test]
    async fn auxiliary_passthrough_avoids_classifier_and_clean_completion_is_explicit() {
        let mock = Arc::new(Mock::new(false));
        let router = Router::new(mock.clone(), config());
        let mut auxiliary = options();
        auxiliary.request_class = "auxiliary".into();
        let pass = router
            .route(
                document(false),
                auxiliary,
                &HeaderMap::new(),
                &CancellationToken::new(),
                "",
            )
            .await
            .unwrap();
        assert_eq!(pass["source"], "passthrough");
        assert_eq!(mock.evaluations.load(Ordering::SeqCst), 0);
        let routed = router
            .route(
                document(false),
                options(),
                &HeaderMap::new(),
                &CancellationToken::new(),
                "",
            )
            .await
            .unwrap();
        assert!(router.complete(
            "synthetic-request",
            &json!({"continuation_model":routed["model"],"tool_uses":[]})
        ));
        assert!(!router.complete("synthetic-request", &Value::Null));
    }
}

#[cfg(test)]
#[path = "router_shutdown_contracts.rs"]
mod shutdown_contracts;
