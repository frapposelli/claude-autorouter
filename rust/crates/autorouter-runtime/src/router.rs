//! Async routing boundary: locks protect synchronous turn transitions only.
//! Classification and token counting remain independently cancellable outside
//! that lock, preserving the source router's three observable await phases.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use autorouter_core::config::RouterConfig;
use autorouter_core::js_json::JsDocument;
use autorouter_core::router::{RouteDecision, RouteOptions, Router as PolicyRouter};
use hyper::HeaderMap;
use serde_json::{Value, json};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::classifier::Classifier;
use crate::evaluator::EvaluationError;
use crate::http_client::HttpTransport;
use crate::token_counter::TokenCounter;

pub struct Router<T> {
    config: RouterConfig,
    policy: Mutex<PolicyRouter>,
    classifier: Classifier<T>,
    counter: Arc<TokenCounter<T>>,
    now: Arc<dyn Fn() -> u64 + Send + Sync>,
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
        Self {
            policy: Mutex::new(PolicyRouter::new(config.clone())),
            classifier: Classifier::new(transport.clone(), &config),
            counter: Arc::new(TokenCounter::new(transport, &config)),
            config,
            now,
        }
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
    pub async fn route_exact(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> Result<RouteDecision, EvaluationError> {
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
        if let Some(mut passthrough) = start.passthrough {
            set_latency(&mut passthrough, 0.0, started);
            attempt.transferred = true;
            let model = document
                .get(document.root(), "model")
                .and_then(|node| document.string(node))
                .cloned()
                .unwrap_or_else(|| "".into());
            return Ok(RouteDecision {
                decision: passthrough,
                model,
            });
        }
        let early_model = start.early_count_model.clone();
        let mut early = EarlyCount {
            preserve_background: false,
            task: early_model.as_ref().map(|model| {
                let counter = self.counter.clone();
                let document = document.clone();
                let model = model.clone();
                let headers = headers.clone();
                let cancellation = cancellation.clone();
                let search = search.to_owned();
                tokio::spawn(async move {
                    counter
                        .count(&document, &model, &headers, &cancellation, &search)
                        .await
                })
            }),
        };
        let evaluation_started = Instant::now();
        let decision = self
            .classifier
            .classify(&document, &self.config, cancellation)
            .await?;
        let evaluation_latency = rounded_ms(evaluation_started);
        let decision =
            serde_json::to_value(decision).map_err(|_| EvaluationError::InvalidResponse)?;
        let pending =
            self.policy
                .lock()
                .unwrap()
                .classified(&document, start, decision, (self.now)());
        let count = if !count_tokens {
            None
        } else if let Some(model) = pending.count_model.as_ref() {
            if early_model.as_ref() == Some(model) {
                if let Some(early) = early.task.as_mut() {
                    early.await.ok().flatten()
                } else {
                    None
                }
            } else {
                self.counter
                    .count(&document, model, headers, cancellation, search)
                    .await
            }
        } else {
            None
        };
        // A cancelled request must not install a new continuity selection even
        // if an advisory count independently resolves to unknown on abort.
        if cancellation.is_cancelled() {
            return Err(EvaluationError::Cancelled);
        }
        let mut decision =
            self.policy
                .lock()
                .unwrap()
                .finish_route_exact(&document, pending, count, (self.now)());
        set_latency(&mut decision.decision, evaluation_latency, started);
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
fn set_latency(value: &mut Value, evaluation: f64, start: Instant) {
    value["evaluation_latency_ms"] = json!(evaluation);
    value["latency_ms"] = json!(rounded_ms(start));
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
                json!({"input_tokens":1000})
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
