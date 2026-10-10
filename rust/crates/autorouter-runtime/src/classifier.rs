//! Bounded classifier coalescing with independently cancellable subscribers.
//! Active turns live elsewhere. Successful cache entries contain only SHA-256
//! digests and decisions; pending tasks retain just evaluator settings/excerpts.

use std::collections::{HashMap, VecDeque};
use std::future::poll_fn;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use autorouter_core::config::{Evaluator, RouterConfig};
use autorouter_core::js_json::JsDocument;
use autorouter_core::prompt_state::{build_ollama_state_document, build_state_document};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, watch};
use tokio::time::Instant;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::evaluator::{
    ClassifierDecision, EvaluationError, evaluate_serialized_state, unavailable,
};
use crate::http_client::HttpTransport;

type Key = [u8; 32];
type ResultValue = Result<ClassifierDecision, EvaluationError>;
#[derive(Clone, Copy)]
pub struct Limits {
    pub pending: usize,
    pub subscribers: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            pending: 256,
            subscribers: 1024,
        }
    }
}

#[derive(Default)]
struct WorkerCompletion {
    finished: AtomicBool,
    changed: Notify,
}
impl WorkerCompletion {
    async fn wait(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.finished.load(Ordering::Acquire) {
                return;
            }
            changed.await;
        }
    }
}
// Constructed before spawning, so an unpolled/dropped task also terminates its
// completion witness. In the worker this outlives the evaluator future scope.
struct FinishWorker(Arc<WorkerCompletion>);
impl Drop for FinishWorker {
    fn drop(&mut self) {
        self.0.finished.store(true, Ordering::Release);
        // Persistent state is published first; arbitrary wake functions run
        // outside the registry and cannot erase the completion observation.
        let _ = catch_unwind(AssertUnwindSafe(|| self.0.changed.notify_waiters()));
    }
}
struct Entry {
    controller: CancellationToken,
    result: watch::Sender<Option<ResultValue>>,
    completed: Arc<WorkerCompletion>,
}
struct Pending {
    entry: Arc<Entry>,
    subscribers: HashMap<u64, CancellationToken>,
}
struct Cached {
    value: ClassifierDecision,
    expires: Instant,
}
#[derive(Default)]
struct State {
    closing: bool,
    pending: HashMap<Key, Pending>,
    subscribers: usize,
    next_id: u64,
    cache: HashMap<Key, Cached>,
    order: VecDeque<Key>,
}
impl State {
    fn next_id(&mut self) -> u64 {
        self.next_id = self.next_id.wrapping_add(1);
        self.next_id
    }
    fn cache_get(&mut self, key: &Key) -> Option<ClassifierDecision> {
        let cached = self.cache.remove(key)?;
        self.order.retain(|candidate| candidate != key);
        if cached.expires <= Instant::now() {
            return None;
        }
        let mut value = cached.value.clone();
        value.source = "cache";
        self.cache.insert(*key, cached);
        self.order.push_back(*key);
        Some(value)
    }
    fn cache_set(&mut self, key: Key, value: ClassifierDecision, limit: usize, ttl: Duration) {
        self.order.retain(|candidate| *candidate != key);
        self.order.push_back(key);
        self.cache.insert(
            key,
            Cached {
                value,
                expires: Instant::now() + ttl,
            },
        );
        while self.cache.len() > limit {
            if let Some(key) = self.order.pop_front() {
                self.cache.remove(&key);
            }
        }
    }
    fn prune_cancelled(&mut self) -> Vec<CancellationToken> {
        let mut retired = Vec::new();
        self.pending.retain(|_, pending| {
            let before = pending.subscribers.len();
            pending.subscribers.retain(|_, token| !token.is_cancelled());
            self.subscribers -= before - pending.subscribers.len();
            if pending.subscribers.is_empty() {
                retired.push(pending.entry.controller.clone());
                false
            } else {
                true
            }
        });
        retired
    }
}
struct Shared<T> {
    transport: Arc<T>,
    state: Mutex<State>,
    limits: Limits,
    cache_entries: usize,
    cache_ttl: Duration,
    workers: TaskTracker,
    stopping: CancellationToken,
    #[cfg(test)]
    test_worker_gate: Mutex<Option<Arc<shutdown_gate::WorkerPollGate>>>,
    #[cfg(test)]
    test_spawn_gate: Mutex<Option<Arc<shutdown_gate::WorkerSpawnGate>>>,
}
impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        // Drop initiates cleanup without blocking; explicit close awaits it.
        self.stopping.cancel();
        self.workers.close();
    }
}

pub struct Classifier<T> {
    shared: Arc<Shared<T>>,
}
impl<T> Clone for Classifier<T> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
        }
    }
}
#[derive(Eq, PartialEq)]
enum Detached {
    Shared,
    Last,
    Retired,
}
struct Subscription<T> {
    shared: Weak<Shared<T>>,
    key: Key,
    entry: Arc<Entry>,
    subscriber: u64,
    attached: bool,
}
impl<T> Subscription<T> {
    fn detach(&mut self) -> Detached {
        if !std::mem::replace(&mut self.attached, false) {
            return Detached::Retired;
        }
        let Some(shared) = self.shared.upgrade() else {
            return Detached::Retired;
        };
        let mut state = shared.state.lock().unwrap();
        let Some(pending) = state
            .pending
            .get_mut(&self.key)
            .filter(|pending| Arc::ptr_eq(&pending.entry, &self.entry))
        else {
            return Detached::Retired;
        };
        let before = pending.subscribers.len();
        pending.subscribers.remove(&self.subscriber);
        // An unpolled subscriber whose token is cancelled is not live work.
        pending.subscribers.retain(|_, token| !token.is_cancelled());
        let remaining = pending.subscribers.len();
        state.subscribers -= before - remaining;
        if remaining == 0 {
            state.pending.remove(&self.key);
            Detached::Last
        } else {
            Detached::Shared
        }
    }
    async fn cancel(mut self) -> ResultValue {
        if self.detach() != Detached::Shared {
            // The exact retained generation may already have been pruned or
            // replaced. Never cancel or await a replacement for the same key.
            self.entry.controller.cancel();
            self.entry.completed.wait().await;
        }
        Err(EvaluationError::Cancelled)
    }
    fn retire_finished(&mut self) {
        debug_assert!(self.entry.completed.finished.load(Ordering::Acquire));
        self.attached = false;
        if let Some(shared) = self.shared.upgrade() {
            let mut state = shared.state.lock().unwrap();
            if state
                .pending
                .get(&self.key)
                .is_some_and(|pending| Arc::ptr_eq(&pending.entry, &self.entry))
            {
                let pending = state
                    .pending
                    .remove(&self.key)
                    .expect("matching finished entry");
                state.subscribers -= pending.subscribers.len();
            }
        }
    }
}
impl<T> Drop for Subscription<T> {
    fn drop(&mut self) {
        if self.detach() == Detached::Last {
            self.entry.controller.cancel();
        }
    }
}

fn settings_key(document: &JsDocument, config: &RouterConfig) -> Key {
    let (settings, rubric) = match config.evaluator {
        Evaluator::Ollama => (
            json!({"evaluator":"ollama","ollamaEndpoint":config.ollama_endpoint,"ollamaModel":config.ollama_model,"ollamaTimeoutMs":config.ollama_timeout_ms,"ollamaStateChars":config.ollama_state_chars,"ollamaKeepAlive":config.ollama_keep_alive}),
            "be151cedb4de4b7ef3f7162d751f70ce7d9dd14efc66fae1835f73ffd04027be",
        ),
        Evaluator::Jev => (
            json!({"evaluator":"jev","jevEndpoint":config.jev_endpoint,"jevModel":config.jev_model,"jevKey":config.jev_key,"jevTimeoutMs":config.jev_timeout_ms,"minConfidence":config.min_confidence,"stateChars":config.state_chars}),
            "0188f45daca9a670c97197fc1726ce6fd2e8cbb646ca5773b15c68b73ace184d",
        ),
    };
    let mut settings =
        JsDocument::parse(&serde_json::to_vec(&settings).expect("finite settings JSON"))
            .expect("serialized settings JSON");
    if config.evaluator == Evaluator::Jev
        && let Some(model) = &config.exact_jev_model
    {
        settings
            .set_root_field_json("jevModel", model.stringify().as_bytes())
            .expect("exact classifier model JSON");
    }
    let settings = settings.stringify();
    let mut hash = Sha256::new();
    hash.update(b"[");
    hash.update(document.stringify().as_bytes());
    hash.update(b",");
    hash.update(settings.as_bytes());
    hash.update(b",\"");
    hash.update(rubric.as_bytes());
    hash.update(b"\"]");
    hash.finalize().into()
}

impl<T: HttpTransport + 'static> Classifier<T> {
    #[cfg(test)]
    pub(crate) fn pending_counts(&self) -> (usize, usize) {
        let state = self.shared.state.lock().unwrap();
        (state.pending.len(), state.subscribers)
    }

    #[cfg(test)]
    pub(crate) fn test_worker_poll_gate(&self, gate: Arc<shutdown_gate::WorkerPollGate>) {
        *self.shared.test_worker_gate.lock().unwrap() = Some(gate);
    }

    pub fn new(transport: Arc<T>, config: &RouterConfig) -> Self {
        Self::with_limits(transport, config, Limits::default())
    }
    pub fn with_limits(transport: Arc<T>, config: &RouterConfig, limits: Limits) -> Self {
        Self {
            shared: Arc::new(Shared {
                transport,
                state: Mutex::new(State::default()),
                limits,
                cache_entries: config.cache_entries,
                cache_ttl: Duration::from_millis(config.cache_ttl_ms),
                workers: TaskTracker::new(),
                stopping: CancellationToken::new(),
                #[cfg(test)]
                test_worker_gate: Mutex::new(None),
                #[cfg(test)]
                test_spawn_gate: Mutex::new(None),
            }),
        }
    }
    /// Configuration is snapshotted for each admission. Relevant evaluator
    /// endpoint/model/account/deadline/rubric changes cannot reuse old decisions.
    pub async fn classify(
        &self,
        document: &JsDocument,
        config: &RouterConfig,
        cancellation: &CancellationToken,
    ) -> ResultValue {
        if cancellation.is_cancelled() {
            return Err(EvaluationError::Cancelled);
        }
        let key = settings_key(document, config);
        let requested = document
            .get(document.root(), "model")
            .and_then(|node| document.string(node))
            .and_then(|model| model.to_scalar())
            .unwrap_or_default();
        let mut work = None;
        let retired;
        let subscription;
        let mut receiver;
        {
            let mut state = self.shared.state.lock().unwrap();
            if state.closing {
                return Err(EvaluationError::Cancelled);
            }
            retired = state.prune_cancelled();
            // Cancellations are propagated outside the mutex, even on an early
            // cache/capacity return, through this small scope-owned guard.
            struct CancelOnDrop(Vec<CancellationToken>);
            impl Drop for CancelOnDrop {
                fn drop(&mut self) {
                    for token in &self.0 {
                        token.cancel();
                    }
                }
            }
            // Do not run arbitrary wake functions while holding the registry.
            let cancelled = CancelOnDrop(retired);
            let result = if let Some(value) = state.cache_get(&key) {
                Some(Ok(value))
            } else if state.subscribers >= self.shared.limits.subscribers
                || (!state.pending.contains_key(&key)
                    && (state.pending.len() >= self.shared.limits.pending
                        || self.shared.workers.len() >= self.shared.limits.pending))
            {
                Some(Ok(unavailable(
                    &requested,
                    config.evaluator,
                    "capacity_exhausted",
                )))
            } else {
                None
            };
            if let Some(result) = result {
                drop(state);
                drop(cancelled);
                return result;
            }
            if !state.pending.contains_key(&key) {
                let excerpt = catch_unwind(AssertUnwindSafe(|| match config.evaluator {
                    Evaluator::Ollama => {
                        build_ollama_state_document(document, config.ollama_state_chars).stringify()
                    }
                    Evaluator::Jev => {
                        build_state_document(document, config.state_chars).stringify()
                    }
                }));
                let Ok(excerpt) = excerpt else {
                    drop(state);
                    drop(cancelled);
                    return Ok(unavailable(
                        &requested,
                        config.evaluator,
                        "invalid_response",
                    ));
                };
                let (result, _) = watch::channel(None);
                let entry = Arc::new(Entry {
                    controller: self.shared.stopping.child_token(),
                    result,
                    completed: Arc::new(WorkerCompletion::default()),
                });
                let completion = FinishWorker(entry.completed.clone());
                state.pending.insert(
                    key,
                    Pending {
                        entry: entry.clone(),
                        subscribers: HashMap::new(),
                    },
                );
                let mut settings = config.clone();
                settings.anthropic_key = None;
                settings.local_token = None;
                settings.session_log_dir = None;
                // Register the whole unpolled future before admission unlocks.
                // A concurrent close must also own work not yet spawned.
                work = Some(self.shared.workers.track_future(self.worker(
                    key,
                    entry,
                    excerpt,
                    settings,
                    requested.clone(),
                    completion,
                )));
            }
            let subscriber = state.next_id();
            let pending = state.pending.get_mut(&key).expect("admitted entry");
            pending.subscribers.insert(subscriber, cancellation.clone());
            receiver = pending.entry.result.subscribe();
            subscription = Subscription {
                shared: Arc::downgrade(&self.shared),
                key,
                entry: pending.entry.clone(),
                subscriber,
                attached: true,
            };
            state.subscribers += 1;
            drop(state);
            drop(cancelled);
        }
        let mut subscription = subscription;
        if let Some(worker) = work {
            #[cfg(test)]
            {
                let gate = self.shared.test_spawn_gate.lock().unwrap().clone();
                if let Some(gate) = gate {
                    gate.before_spawn().await;
                }
            }
            tokio::spawn(worker);
        }

        loop {
            if cancellation.is_cancelled() || subscription.entry.controller.is_cancelled() {
                return subscription.cancel().await;
            }
            // Acquire completion before reading the result: publication may
            // race this loop, and a finished worker has already sent its value.
            let finished = subscription
                .entry
                .completed
                .finished
                .load(Ordering::Acquire);
            // Release the watch borrow before any cancellation cleanup awaits.
            let result = receiver.borrow().clone();
            if let Some(result) = result {
                if cancellation.is_cancelled()
                    || subscription.entry.controller.is_cancelled()
                    || matches!(result, Err(EvaluationError::Cancelled))
                {
                    return subscription.cancel().await;
                }
                drop(subscription);
                return result;
            }
            if finished {
                // Retaining Entry also retains its Sender. Worker abort/panic
                // without a result must not wait forever for a closed channel.
                subscription.retire_finished();
                return Ok(unavailable(
                    &requested_model(document),
                    config.evaluator,
                    "network_error",
                ));
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return subscription.cancel().await,
                _ = subscription.entry.completed.wait() => {},
                changed = receiver.changed() => {
                    if changed.is_err() {
                        return Ok(unavailable(&requested_model(document), config.evaluator, "network_error"));
                    }
                }
            }
        }
    }

    fn worker(
        &self,
        key: Key,
        entry: Arc<Entry>,
        excerpt: String,
        settings: RouterConfig,
        requested: String,
        completion: FinishWorker,
    ) -> impl std::future::Future<Output = ()> + Send + 'static {
        let weak = Arc::downgrade(&self.shared);
        let transport = self.shared.transport.clone();
        let worker = async move {
            let _completion = completion;
            let result = {
                let future = evaluate_serialized_state(
                    transport.as_ref(),
                    &settings,
                    &excerpt,
                    &requested,
                    &entry.controller,
                );
                tokio::pin!(future);
                poll_fn(
                    |cx| match catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(cx))) {
                        Ok(result) => result,
                        Err(_) => std::task::Poll::Ready(Ok(unavailable(
                            &requested,
                            settings.evaluator,
                            "network_error",
                        ))),
                    },
                )
                .await
            }; // Drop evaluator-owned request/body before result publication.
            if let Some(shared) = weak.upgrade() {
                let mut state = shared.state.lock().unwrap();
                if state
                    .pending
                    .get(&key)
                    .is_some_and(|pending| Arc::ptr_eq(&pending.entry, &entry))
                {
                    let pending = state.pending.remove(&key).expect("matching pending entry");
                    state.subscribers -= pending.subscribers.len();
                    let active = pending
                        .subscribers
                        .values()
                        .any(|token| !token.is_cancelled());
                    if active
                        && !entry.controller.is_cancelled()
                        && let Ok(value) = &result
                        && value.source != "fallback"
                    {
                        state.cache_set(key, value.clone(), shared.cache_entries, shared.cache_ttl);
                    }
                }
            }
            let _ = entry.result.send(Some(result));
        };
        #[cfg(test)]
        let worker =
            shutdown_gate::schedule(worker, self.shared.test_worker_gate.lock().unwrap().clone());
        worker
    }

    pub(crate) fn begin_close(&self) {
        let pending = {
            let mut state = self.shared.state.lock().unwrap();
            state.closing = true;
            state.subscribers = 0;
            std::mem::take(&mut state.pending)
        };
        // Cancellation, watch notification and tracker wakes are outside locks.
        self.shared.stopping.cancel();
        for pending in pending.into_values() {
            pending.entry.controller.cancel();
            let _ = pending
                .entry
                .result
                .send(Some(Err(EvaluationError::Cancelled)));
        }
        self.shared.workers.close();
    }

    /// Permanently stops admission and waits for all owned worker destructors.
    /// Cancelling this waiter does not cancel or consume the shared drain.
    pub async fn close(&self) {
        self.begin_close();
        self.shared.workers.wait().await;
    }

    /// Shutdown abandons pending shared work; it does not mutate turn state.
    pub fn shutdown(&self) {
        let pending = {
            let mut state = self.shared.state.lock().unwrap();
            state.subscribers = 0;
            std::mem::take(&mut state.pending)
        };
        for pending in pending.into_values() {
            pending.entry.controller.cancel();
            let _ = pending
                .entry
                .result
                .send(Some(Err(EvaluationError::Cancelled)));
        }
    }
}
fn requested_model(document: &JsDocument) -> String {
    document
        .get(document.root(), "model")
        .and_then(|node| document.string(node))
        .and_then(|model| model.to_scalar())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluator_cache_identity_distinguishes_unpaired_configured_model_units() {
        let mut first = autorouter_core::config::read_config(
            &json!({"AUTOROUTER_EVALUATOR":"jev","TYPESAFE_API_KEY":"synthetic"}),
            false,
            std::path::Path::new("/synthetic"),
        )
        .unwrap();
        first.jev_model = "synthetic-\u{fffd}".into();
        first.exact_jev_model = Some(autorouter_core::js_json::JsString::from_utf16(vec![0xd800]));
        let mut second = first.clone();
        second.exact_jev_model = Some(autorouter_core::js_json::JsString::from_utf16(vec![0xd801]));
        let document =
            JsDocument::parse(br#"{"model":"claude-sonnet-5-5","messages":[]}"#).unwrap();
        assert_ne!(
            settings_key(&document, &first),
            settings_key(&document, &second)
        );
    }
    use crate::http_client::HttpError;
    use autorouter_core::config::read_config;
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::{Request, Response};
    use std::{
        path::Path,
        sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering},
    };
    use tokio::sync::Semaphore;

    struct Mock {
        calls: AtomicUsize,
        waiting: Arc<AtomicUsize>,
        active: Arc<AtomicUsize>,
        blocked: AtomicBool,
        permits: Semaphore,
        status: AtomicU16,
        payloads: Mutex<Vec<Vec<u8>>>,
    }
    impl Mock {
        fn new(blocked: bool) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                waiting: Arc::new(AtomicUsize::new(0)),
                active: Arc::new(AtomicUsize::new(0)),
                blocked: AtomicBool::new(blocked),
                permits: Semaphore::new(0),
                status: AtomicU16::new(200),
                payloads: Mutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
        fn release(&self, n: usize) {
            self.permits.add_permits(n);
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
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.active.fetch_add(1, Ordering::SeqCst);
            let _active = Active(self.active.clone());
            let payload = request
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec();
            self.payloads.lock().unwrap().push(payload);
            if self.blocked.load(Ordering::SeqCst) {
                self.waiting.fetch_add(1, Ordering::SeqCst);
                let _waiting = Active(self.waiting.clone());
                self.permits.acquire().await.unwrap().forget();
            }
            Ok(Response::builder()
                .status(self.status.load(Ordering::SeqCst))
                .body(Full::new(Bytes::from_static(
                    br#"{"answers":{"tier":{"choice":"haiku","confidence":0.9}}}"#,
                )))
                .unwrap())
        }
    }
    fn config() -> RouterConfig {
        read_config(&json!({"AUTOROUTER_EVALUATOR":"jev","TYPESAFE_API_KEY":"synthetic-jev-key","ANTHROPIC_API_KEY":"synthetic-claude-key"}),false,Path::new("/synthetic")).unwrap()
    }
    fn document(task: &str) -> Arc<JsDocument> {
        Arc::new(JsDocument::parse(&serde_json::to_vec(&json!({"model":"claude-sonnet-5","messages":[{"role":"user","content":task}],"max_tokens":4096})).unwrap()).unwrap())
    }
    fn start(
        classifier: &Classifier<Mock>,
        body: Arc<JsDocument>,
        config: &RouterConfig,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<ResultValue> {
        let classifier = classifier.clone();
        let config = config.clone();
        tokio::spawn(async move { classifier.classify(&body, &config, &cancel).await })
    }
    async fn wait_subscribers(classifier: &Classifier<Mock>, subscribers: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if classifier.shared.state.lock().unwrap().subscribers == subscribers {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    async fn wait_calls(mock: &Mock, count: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while mock.calls() < count {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn identical_work_is_shared_but_a_waiters_cancel_does_not_cancel_others() {
        let mock = Arc::new(Mock::new(true));
        let config = config();
        let classifier = Classifier::new(mock.clone(), &config);
        let body = document("Synthetic identical task");
        let first = CancellationToken::new();
        let second = CancellationToken::new();
        let a = start(&classifier, body.clone(), &config, first.clone());
        let b = start(&classifier, body.clone(), &config, second);
        wait_subscribers(&classifier, 2).await;
        wait_calls(&mock, 1).await;
        first.cancel();
        assert!(matches!(a.await.unwrap(), Err(EvaluationError::Cancelled)));
        assert_eq!(mock.active.load(Ordering::SeqCst), 1);
        mock.release(1);
        assert_eq!(b.await.unwrap().unwrap().source, "jev");
        let cached = classifier
            .classify(&body, &config, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(cached.source, "cache");
        assert_eq!(mock.calls(), 1);
        assert_eq!(classifier.shared.state.lock().unwrap().subscribers, 0);
    }

    #[tokio::test]
    async fn all_cancelled_work_cannot_cache_or_remove_a_replacement_entry() {
        let mock = Arc::new(Mock::new(true));
        let config = config();
        let classifier = Classifier::new(mock.clone(), &config);
        let body = document("Synthetic task");
        let cancel = CancellationToken::new();
        let a = start(&classifier, body.clone(), &config, cancel.clone());
        let b = start(&classifier, body.clone(), &config, cancel.clone());
        wait_subscribers(&classifier, 2).await;
        wait_calls(&mock, 1).await;
        cancel.cancel();
        let replacement = start(&classifier, body.clone(), &config, CancellationToken::new());
        wait_calls(&mock, 2).await;
        assert!(matches!(a.await.unwrap(), Err(EvaluationError::Cancelled)));
        assert!(matches!(b.await.unwrap(), Err(EvaluationError::Cancelled)));
        mock.release(1);
        assert_eq!(replacement.await.unwrap().unwrap().source, "jev");
        assert_eq!(
            classifier
                .classify(&body, &config, &CancellationToken::new())
                .await
                .unwrap()
                .source,
            "cache"
        );
        assert_eq!(mock.calls(), 2);
    }

    #[tokio::test]
    async fn cancellation_at_completion_and_future_drop_release_registry_without_caching() {
        let mock = Arc::new(Mock::new(true));
        let config = config();
        let classifier = Classifier::new(mock.clone(), &config);
        let body = document("Synthetic task");
        let cancel = CancellationToken::new();
        let first = start(&classifier, body.clone(), &config, cancel.clone());
        wait_calls(&mock, 1).await;
        cancel.cancel();
        mock.release(1);
        assert!(matches!(
            first.await.unwrap(),
            Err(EvaluationError::Cancelled)
        ));
        assert!(classifier.shared.state.lock().unwrap().cache.is_empty());
        mock.permits
            .forget_permits(mock.permits.available_permits());
        let dropped = start(&classifier, body.clone(), &config, CancellationToken::new());
        wait_calls(&mock, 2).await;
        dropped.abort();
        let _ = dropped.await;
        wait_subscribers(&classifier, 0).await;
        tokio::task::yield_now().await;
        assert!(classifier.shared.state.lock().unwrap().pending.is_empty());
        assert!(classifier.shared.state.lock().unwrap().cache.is_empty());
        let final_call = start(&classifier, body, &config, CancellationToken::new());
        wait_calls(&mock, 3).await;
        mock.release(1);
        assert_eq!(final_call.await.unwrap().unwrap().source, "jev");
    }

    #[tokio::test]
    async fn already_cancelled_calls_skip_network_and_cached_decisions() {
        let mock = Arc::new(Mock::new(false));
        let config = config();
        let classifier = Classifier::new(mock.clone(), &config);
        let body = document("Synthetic task");
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            classifier.classify(&body, &config, &cancel).await,
            Err(EvaluationError::Cancelled)
        ));
        assert_eq!(mock.calls(), 0);
        classifier
            .classify(&body, &config, &CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(
            classifier.classify(&body, &config, &cancel).await,
            Err(EvaluationError::Cancelled)
        ));
        assert_eq!(mock.calls(), 1);
    }

    #[tokio::test]
    async fn subscriber_and_work_capacity_have_distinct_bounded_admission() {
        assert_eq!(
            (Limits::default().pending, Limits::default().subscribers),
            (256, 1024)
        );
        let mock = Arc::new(Mock::new(true));
        let config = config();
        let classifier = Classifier::with_limits(
            mock.clone(),
            &config,
            Limits {
                pending: 2,
                subscribers: 3,
            },
        );
        let body = document("Synthetic first");
        let cancel = CancellationToken::new();
        let a = start(&classifier, body.clone(), &config, cancel.clone());
        let b = start(&classifier, body.clone(), &config, CancellationToken::new());
        let c = start(
            &classifier,
            document("Synthetic second"),
            &config,
            CancellationToken::new(),
        );
        wait_subscribers(&classifier, 3).await;
        wait_calls(&mock, 2).await;
        for doc in [body.clone(), document("Synthetic third")] {
            let result = classifier
                .classify(&doc, &config, &CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(result.classifier_error, Some("capacity_exhausted"));
            assert_eq!(result.source, "fallback");
        }
        cancel.cancel();
        assert!(matches!(a.await.unwrap(), Err(EvaluationError::Cancelled)));
        let blocked = classifier
            .classify(
                &document("Synthetic third"),
                &config,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(blocked.classifier_error, Some("capacity_exhausted"));
        let d = start(&classifier, body, &config, CancellationToken::new());
        wait_subscribers(&classifier, 3).await;
        mock.release(2);
        for task in [b, c, d] {
            assert_eq!(task.await.unwrap().unwrap().source, "jev");
        }
        assert_eq!(mock.calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn successful_cache_is_bounded_lru_with_expiration_but_failures_are_not_cached() {
        let mock = Arc::new(Mock::new(false));
        let mut config = config();
        config.cache_entries = 2;
        config.cache_ttl_ms = 10;
        let classifier = Classifier::new(mock.clone(), &config);
        let cancel = CancellationToken::new();
        for (task, source) in [
            ("A", "jev"),
            ("B", "jev"),
            ("A", "cache"),
            ("C", "jev"),
            ("B", "jev"),
        ] {
            assert_eq!(
                classifier
                    .classify(&document(task), &config, &cancel)
                    .await
                    .unwrap()
                    .source,
                source
            );
        }
        assert_eq!(mock.calls(), 4);
        tokio::time::advance(Duration::from_millis(11)).await;
        assert_eq!(
            classifier
                .classify(&document("B"), &config, &cancel)
                .await
                .unwrap()
                .source,
            "jev"
        );
        assert_eq!(mock.calls(), 5);
        mock.status.store(503, Ordering::SeqCst);
        for _ in 0..2 {
            assert_eq!(
                classifier
                    .classify(&document("failure"), &config, &cancel)
                    .await
                    .unwrap()
                    .source,
                "fallback"
            );
        }
        assert_eq!(mock.calls(), 7);
        assert!(classifier.shared.state.lock().unwrap().cache.len() <= 2);
    }

    #[tokio::test]
    async fn full_request_and_evaluator_settings_account_and_floor_are_cache_identity() {
        let mock = Arc::new(Mock::new(false));
        let config = config();
        let classifier = Classifier::new(mock.clone(), &config);
        let body = document("Synthetic task");
        let cancel = CancellationToken::new();
        classifier.classify(&body, &config, &cancel).await.unwrap();
        for index in 0..7 {
            let mut changed = config.clone();
            match index {
                0 => changed.jev_key = Some("synthetic-other-account".into()),
                1 => changed.jev_endpoint.push_str("?endpoint=2"),
                2 => changed.jev_model.push_str("-other"),
                3 => changed.jev_timeout_ms += 1,
                4 => changed.min_confidence = 0.91,
                5 => changed.state_chars += 1,
                _ => changed.jev_key = None,
            };
            assert_eq!(
                classifier
                    .classify(&body, &changed, &cancel)
                    .await
                    .unwrap()
                    .source,
                "jev"
            );
        }
        let mut changed = (*body).clone();
        changed.set_root_field_json("max_tokens", b"8192").unwrap();
        assert_eq!(
            classifier
                .classify(&changed, &config, &cancel)
                .await
                .unwrap()
                .source,
            "jev"
        );
        changed
            .set_root_field_json("model", b"\"claude-opus-5\"")
            .unwrap();
        assert_eq!(
            classifier
                .classify(&changed, &config, &cancel)
                .await
                .unwrap()
                .source,
            "jev"
        );
        let mut unrelated = config.clone();
        unrelated.anthropic_key = Some("synthetic-refreshed-claude-key".into());
        assert_eq!(
            classifier
                .classify(&body, &unrelated, &cancel)
                .await
                .unwrap()
                .source,
            "cache"
        );
        assert_eq!(mock.calls(), 10);
    }

    #[tokio::test]
    async fn bounded_excerpt_uses_exact_utf16_wire_and_retains_no_full_body_in_registry() {
        let mock = Arc::new(Mock::new(false));
        let config = config();
        let classifier = Classifier::new(mock.clone(), &config);
        let body=JsDocument::parse(br#"{"model":"claude-sonnet-5","messages":[{"role":"user","content":"Synthetic \ud800 task"}]}"#).unwrap();
        classifier
            .classify(&body, &config, &CancellationToken::new())
            .await
            .unwrap();
        let payloads = mock.payloads.lock().unwrap();
        let payload = String::from_utf8_lossy(&payloads[0]);
        assert!(payload.contains("\\ud800"));
        assert!(!payload.contains('\u{fffd}'));
        assert!(!payload.contains("synthetic-claude-key"));
        assert!(classifier.shared.state.lock().unwrap().pending.is_empty());
    }

    #[tokio::test]
    async fn shutdown_cancels_all_subscribers_and_pending_transport() {
        let mock = Arc::new(Mock::new(true));
        let config = config();
        let classifier = Classifier::new(mock.clone(), &config);
        let first = start(
            &classifier,
            document("A"),
            &config,
            CancellationToken::new(),
        );
        let second = start(
            &classifier,
            document("B"),
            &config,
            CancellationToken::new(),
        );
        wait_calls(&mock, 2).await;
        classifier.shutdown();
        for task in [first, second] {
            assert!(matches!(
                task.await.unwrap(),
                Err(EvaluationError::Cancelled)
            ));
        }
        assert!(classifier.shared.state.lock().unwrap().pending.is_empty());
        assert_eq!(classifier.shared.state.lock().unwrap().subscribers, 0);
    }
    include!("classifier_cancellation_contracts.rs");
    include!("classifier_shutdown_contracts.rs");
}

#[cfg(test)]
#[path = "classifier_shutdown_gate.rs"]
pub(crate) mod shutdown_gate;
