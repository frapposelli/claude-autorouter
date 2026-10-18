//! Bounded classifier coalescing with independently cancellable subscribers.
//! Active turns live elsewhere. Successful cache entries contain only SHA-256
//! digests and decisions; pending tasks retain just evaluator settings/excerpts.

use crate::callback_privacy::contain_callback;
use std::collections::{HashMap, VecDeque};
use std::future::poll_fn;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use autorouter_core::config::{Credential, Evaluator, RouterConfig};
use autorouter_core::contracts::{ClassifierError, DecisionSource};
use autorouter_core::js_json::JsDocument;
use autorouter_core::prompt_state::{build_ollama_state_document, build_state_document};
use serde_json::json;
use tokio::sync::{Notify, watch};
use tokio::time::Instant;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::evaluator::{
    ClassifierDecision, EvaluationError, evaluate_serialized_state, unavailable_error,
};
use crate::http_client::HttpTransport;

type Key = [u8; 32];
type ResultValue = Result<ClassifierDecision, EvaluationError>;
#[derive(Clone, Copy)]
pub struct Limits {
    pub pending: usize,
    pub subscribers: usize,
}
impl Limits {
    const MAX_PENDING: usize = 256;
    const MAX_SUBSCRIBERS: usize = 1024;

    fn validate(self) -> Result<Self, String> {
        if self.pending > Self::MAX_PENDING || self.subscribers > Self::MAX_SUBSCRIBERS {
            return Err("Invalid classifier limits.".into());
        }
        Ok(self)
    }
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            pending: Self::MAX_PENDING,
            subscribers: Self::MAX_SUBSCRIBERS,
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
        let _ = contain_callback(|| self.0.changed.notify_waiters());
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
    // None denotes an expiry beyond the representable monotonic clock domain.
    // It preserves successful-value caching without clamping the requested TTL.
    expires: Option<Instant>,
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
        if cached
            .expires
            .is_some_and(|expires| expires <= Instant::now())
        {
            return None;
        }
        let mut value = cached.value.clone();
        value.source = DecisionSource::Cache;
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
                expires: Instant::now().checked_add(ttl),
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
    #[cfg(test)]
    test_publication_gate: Mutex<Option<Arc<PublicationGate>>>,
}
// A one-worker test seam at the real evaluator-to-registry handoff. The gate
// owns neither the registry mutex nor a production code path.
#[cfg(test)]
struct PublicationGate {
    observed:
        Mutex<Option<Result<(autorouter_core::contracts::Tier, DecisionSource), EvaluationError>>>,
    arrived: Notify,
    release: tokio::sync::Semaphore,
}
#[cfg(test)]
impl Default for PublicationGate {
    fn default() -> Self {
        Self {
            observed: Mutex::new(None),
            arrived: Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        }
    }
}
#[cfg(test)]
impl PublicationGate {
    async fn pause(&self, result: &ResultValue) {
        *self.observed.lock().unwrap() = Some(
            result
                .as_ref()
                .map(|value| (value.tier, value.source))
                .map_err(|error| *error),
        );
        self.arrived.notify_waiters();
        self.release
            .acquire()
            .await
            .expect("publication gate open")
            .forget();
    }
    async fn observed(
        &self,
    ) -> Result<(autorouter_core::contracts::Tier, DecisionSource), EvaluationError> {
        loop {
            let arrived = self.arrived.notified();
            tokio::pin!(arrived);
            arrived.as_mut().enable();
            if let Some(result) = *self.observed.lock().unwrap() {
                return result;
            }
            arrived.await;
        }
    }
    fn release(&self) {
        self.release.add_permits(1);
    }
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

fn evaluation_settings(config: &RouterConfig) -> RouterConfig {
    let mut settings = config.clone();
    settings.replace_credential(Credential::Anthropic, None);
    settings.replace_credential(Credential::LocalToken, None);
    settings.replace_session_log_dir(None);
    settings
}

fn settings_key(document: &JsDocument, config: &RouterConfig) -> Key {
    let jev_key = (config.evaluator == Evaluator::Jev)
        .then(|| config.credential_json(Credential::Jev))
        .flatten();
    let (settings, rubric) = match config.evaluator {
        Evaluator::Ollama => (
            json!({"evaluator":"ollama","ollamaEndpoint":config.ollama_endpoint,"ollamaModel":config.ollama_model,"ollamaTimeoutMs":config.ollama_timeout_ms,"ollamaStateChars":config.ollama_state_chars,"ollamaKeepAlive":null}),
            "be151cedb4de4b7ef3f7162d751f70ce7d9dd14efc66fae1835f73ffd04027be",
        ),
        Evaluator::Jev => {
            let mut settings = json!({"evaluator":"jev","jevEndpoint":config.jev_endpoint,"jevModel":config.jev_model});
            // Source JSON.stringify omits undefined but retains null. Reserve
            // the original property position before replacing exact JSON.
            if jev_key.is_some() {
                settings["jevKey"] = serde_json::Value::Null;
            }
            settings["jevTimeoutMs"] = json!(config.jev_timeout_ms);
            settings["minConfidence"] = json!(config.min_confidence);
            settings["stateChars"] = json!(config.state_chars);
            (
                settings,
                "0188f45daca9a670c97197fc1726ce6fd2e8cbb646ca5773b15c68b73ace184d",
            )
        }
    };
    let mut settings =
        JsDocument::parse(&serde_json::to_vec(&settings).expect("finite settings JSON"))
            .expect("serialized settings JSON");
    if config.evaluator == Evaluator::Ollama {
        settings
            .set_root_field_json(
                "ollamaKeepAlive",
                config.ollama_keep_alive_json().as_bytes(),
            )
            .expect("exact classifier keep-alive JSON");
    }
    if let Some(key) = jev_key {
        settings
            .set_root_field_json("jevKey", key.as_bytes())
            .expect("exact classifier credential JSON");
    }
    if config.evaluator == Evaluator::Jev
        && let Some(model) = &config.exact_jev_model
    {
        settings
            .set_root_field_json("jevModel", model.stringify().as_bytes())
            .expect("exact classifier model JSON");
    }
    let settings = settings.stringify();
    document.sha256_with_fragments(&["["], &[",", &settings, ",\"", rubric, "\"]"])
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
            .expect("default classifier limits are valid")
    }
    pub fn with_limits(
        transport: Arc<T>,
        config: &RouterConfig,
        limits: Limits,
    ) -> Result<Self, String> {
        let limits = limits.validate()?;
        Ok(Self {
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
                #[cfg(test)]
                test_publication_gate: Mutex::new(None),
            }),
        })
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
                Some(Ok(unavailable_error(
                    &requested,
                    config.evaluator,
                    ClassifierError::CapacityExhausted,
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
                let excerpt = contain_callback(|| match config.evaluator {
                    Evaluator::Ollama => {
                        build_ollama_state_document(document, config.ollama_state_chars).stringify()
                    }
                    Evaluator::Jev => {
                        build_state_document(document, config.state_chars).stringify()
                    }
                });
                let Ok(excerpt) = excerpt else {
                    drop(state);
                    drop(cancelled);
                    return Ok(unavailable_error(
                        &requested,
                        config.evaluator,
                        ClassifierError::InvalidResponse,
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
                let settings = evaluation_settings(config);
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
                return Ok(unavailable_error(
                    &requested_model(document),
                    config.evaluator,
                    ClassifierError::NetworkError,
                ));
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return subscription.cancel().await,
                _ = subscription.entry.completed.wait() => {},
                changed = receiver.changed() => {
                    if changed.is_err() {
                        return Ok(unavailable_error(&requested_model(document), config.evaluator, ClassifierError::NetworkError));
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
                poll_fn(|cx| match contain_callback(|| future.as_mut().poll(cx)) {
                    Ok(result) => result,
                    Err(_) => std::task::Poll::Ready(Ok(unavailable_error(
                        &requested,
                        settings.evaluator,
                        ClassifierError::NetworkError,
                    ))),
                })
                .await
            }; // Drop evaluator-owned request/body before result publication.
            #[cfg(test)]
            if let Some(gate) = weak
                .upgrade()
                .and_then(|shared| shared.test_publication_gate.lock().unwrap().take())
            {
                gate.pause(&result).await;
            }
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
                        && value.source != DecisionSource::Fallback
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
    fn exact_keep_alive_cache_keys_match_four_actual_frozen_source_controls() {
        let document = JsDocument::parse(br#"{"model":"claude-sonnet-5","messages":[{"role":"user","content":"Synthetic exact keep-alive task"}]}"#).unwrap();
        // Recorded by the actual frozen Router.classify with injected transport;
        // these are not generated from the native settings implementation.
        let cases = [
            (
                format!("{}\"5m\"{}", "[".repeat(64), "]".repeat(64)),
                "badcc8cd8259dbe3d3e081f6294b5814973e5826696c2296c8d8ed85c24f0222",
            ),
            (
                format!("{}\"5m\"{}", "[".repeat(80), "]".repeat(80)),
                "a9b30187ee8637382ae392a5a31dfae444f688171337cf9dea93fae86d298748",
            ),
            (
                "\"5m\"".into(),
                "e0306a2a8ae5ab8dbaa42254147af892d1baa6a6d2be9b37304b54dc41279f01",
            ),
            (
                "0".into(),
                "9d75f52d05c2d04a94526b21b5f6f34354c218beb79592e9f8ffc286db3ae498",
            ),
        ];
        for (raw, expected) in cases {
            let config = crate::test_config_document(
                &JsDocument::parse(
                    format!(r#"{{"AUTOROUTER_OLLAMA_KEEP_ALIVE":{raw}}}"#).as_bytes(),
                )
                .unwrap(),
            );
            let key = settings_key(&document, &config);
            let hex: String = key.iter().map(|byte| format!("{byte:02x}")).collect();
            assert_eq!(hex, expected);
            let snapshot = evaluation_settings(&config);
            assert_eq!(snapshot.ollama_keep_alive_json(), raw);
            assert_eq!(settings_key(&document, &snapshot), key);
        }
    }

    #[test]
    fn exact_worker_settings_erase_captured_paths_and_replace_cache_identity() {
        let raw = format!(
            r#"{{"AUTOROUTER_SESSION_LOG_DIR":"synthetic\ud800","AUTOROUTER_OLLAMA_KEEP_ALIVE":{}"5m"{}}}"#,
            "[".repeat(80),
            "]".repeat(80)
        );
        let mut config = crate::test_config_document(&JsDocument::parse(raw.as_bytes()).unwrap());
        let document = JsDocument::parse(br#"{"model":"claude-sonnet-5","messages":[]}"#).unwrap();
        let original = settings_key(&document, &config);
        let snapshot = evaluation_settings(&config);
        assert!(snapshot.session_log_dir.is_none());
        assert!(snapshot.session_log_dir_exact().is_none());
        assert!(config.session_log_dir_exact().is_some());
        assert_eq!(
            snapshot.ollama_keep_alive_json(),
            config.ollama_keep_alive_json()
        );
        config.replace_ollama_keep_alive(json!("5m"));
        assert_ne!(settings_key(&document, &config), original);
        assert_eq!(settings_key(&document, &snapshot), original);
    }

    #[test]
    fn evaluator_cache_identity_distinguishes_unpaired_configured_model_units() {
        let mut first = crate::test_config(
            &json!({"AUTOROUTER_EVALUATOR":"jev","TYPESAFE_API_KEY":"synthetic"}),
        );
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
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::{Request, Response};
    use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
    use tokio::sync::Semaphore;

    struct Mock {
        calls: AtomicUsize,
        waiting: Arc<AtomicUsize>,
        active: Arc<AtomicUsize>,
        blocked: AtomicBool,
        permits: Semaphore,
        status: AtomicU16,
        payloads: Mutex<Vec<Vec<u8>>>,
        replies: Mutex<VecDeque<Bytes>>,
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
                replies: Mutex::new(VecDeque::new()),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
        fn release(&self, n: usize) {
            self.permits.add_permits(n);
        }
        fn queue_reply(&self, payload: &'static [u8]) {
            self.replies
                .lock()
                .unwrap()
                .push_back(Bytes::from_static(payload));
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
            let reply = self.replies.lock().unwrap().pop_front().unwrap_or_else(|| {
                Bytes::from_static(br#"{"answers":{"tier":{"choice":"haiku","confidence":0.9}}}"#)
            });
            Ok(Response::builder()
                .status(self.status.load(Ordering::SeqCst))
                .body(Full::new(reply))
                .unwrap())
        }
    }
    fn config() -> RouterConfig {
        crate::test_config(
            &json!({"AUTOROUTER_EVALUATOR":"jev","TYPESAFE_API_KEY":"synthetic-jev-key","ANTHROPIC_API_KEY":"synthetic-claude-key"}),
        )
    }
    fn fixture(block: bool) -> (Arc<Mock>, RouterConfig, Classifier<Mock>) {
        let mock = Arc::new(Mock::new(block));
        let config = config();
        let classifier = Classifier::new(mock.clone(), &config);
        (mock, config, classifier)
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
        crate::bounded(async {
            loop {
                if classifier.shared.state.lock().unwrap().subscribers == subscribers {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
    }
    async fn wait_pending(classifier: &Classifier<Mock>, counts: (usize, usize)) {
        crate::bounded(async {
            while classifier.pending_counts() != counts {
                tokio::task::yield_now().await;
            }
        })
        .await;
    }
    async fn wait_calls(mock: &Mock, count: usize) {
        crate::bounded(async {
            while mock.calls() < count {
                tokio::task::yield_now().await;
            }
        })
        .await;
    }

    #[tokio::test]
    async fn identical_work_is_shared_but_a_waiters_cancel_does_not_cancel_others() {
        let (mock, config, classifier) = fixture(true);
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
        let (mock, config, classifier) = fixture(true);
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

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_successful_old_generation_cannot_publish_over_same_key_replacement() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mock = Arc::new(Mock::new(true));
            mock.queue_reply(br#"{"answers":{"tier":{"choice":"opus","confidence":0.9}}}"#);
            mock.queue_reply(br#"{"answers":{"tier":{"choice":"haiku","confidence":0.9}}}"#);
            let config = config();
            let classifier = Classifier::new(mock.clone(), &config);
            let body = document("Synthetic same-key task");
            let key = settings_key(&body, &config);
            let gate = Arc::new(PublicationGate::default());
            *classifier.shared.test_publication_gate.lock().unwrap() = Some(gate.clone());

            let first_cancel = CancellationToken::new();
            let second_cancel = CancellationToken::new();
            let first = start(&classifier, body.clone(), &config, first_cancel.clone());
            let second = start(&classifier, body.clone(), &config, second_cancel.clone());
            wait_pending(&classifier, (1, 2)).await;
            wait_calls(&mock, 1).await;
            let old_entry = classifier.shared.state.lock().unwrap().pending[&key]
                .entry
                .clone();
            mock.release(1);
            // The old evaluator has returned an actual successful Opus decision,
            // but the real worker has not touched pending state or cache yet.
            assert_eq!(
                gate.observed().await,
                Ok((autorouter_core::contracts::Tier::Opus, DecisionSource::Jev))
            );
            assert_eq!(mock.active.load(Ordering::SeqCst), 0);
            assert!(!old_entry.completed.finished.load(Ordering::Acquire));
            assert!(classifier.shared.state.lock().unwrap().cache.is_empty());

            first_cancel.cancel();
            second_cancel.cancel();
            wait_pending(&classifier, (0, 0)).await;
            assert!(old_entry.controller.is_cancelled());
            let retry = start(&classifier, body.clone(), &config, CancellationToken::new());
            wait_pending(&classifier, (1, 1)).await;
            wait_calls(&mock, 2).await;
            let replacement_entry = classifier.shared.state.lock().unwrap().pending[&key]
                .entry
                .clone();
            assert!(!Arc::ptr_eq(&old_entry, &replacement_entry));

            // Releasing the successful but cancelled old result must neither
            // remove this new generation nor fill its same-key cache slot.
            gate.release();
            for task in [first, second] {
                assert!(matches!(
                    task.await.unwrap(),
                    Err(EvaluationError::Cancelled)
                ));
            }
            assert!(old_entry.completed.finished.load(Ordering::Acquire));
            assert_eq!(classifier.pending_counts(), (1, 1));
            assert!(Arc::ptr_eq(
                &classifier.shared.state.lock().unwrap().pending[&key].entry,
                &replacement_entry,
            ));
            assert!(classifier.shared.state.lock().unwrap().cache.is_empty());
            assert_eq!(mock.active.load(Ordering::SeqCst), 1);

            mock.release(1);
            let answer = retry.await.unwrap().unwrap();
            assert_eq!(
                (answer.tier, answer.source),
                (autorouter_core::contracts::Tier::Haiku, DecisionSource::Jev)
            );
            let cached = classifier
                .classify(&body, &config, &CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(
                (cached.tier, cached.source),
                (
                    autorouter_core::contracts::Tier::Haiku,
                    DecisionSource::Cache
                )
            );
            assert_eq!(mock.calls(), 2);
            assert_eq!(classifier.pending_counts(), (0, 0));
            assert_eq!(classifier.shared.state.lock().unwrap().cache.len(), 1);
            assert_eq!(mock.active.load(Ordering::SeqCst), 0);
            assert_eq!(mock.waiting.load(Ordering::SeqCst), 0);
            classifier.close().await;
            assert_eq!(classifier.shared.workers.len(), 0);
        })
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn live_success_at_publication_gate_still_populates_cache() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mock = Arc::new(Mock::new(false));
            mock.queue_reply(br#"{"answers":{"tier":{"choice":"opus","confidence":0.9}}}"#);
            let config = config();
            let classifier = Classifier::new(mock.clone(), &config);
            let body = document("Synthetic live task");
            let gate = Arc::new(PublicationGate::default());
            *classifier.shared.test_publication_gate.lock().unwrap() = Some(gate.clone());
            let live = start(&classifier, body.clone(), &config, CancellationToken::new());
            assert_eq!(
                gate.observed().await,
                Ok((autorouter_core::contracts::Tier::Opus, DecisionSource::Jev))
            );
            assert_eq!(classifier.pending_counts(), (1, 1));
            assert!(classifier.shared.state.lock().unwrap().cache.is_empty());
            gate.release();
            let answer = live.await.unwrap().unwrap();
            assert_eq!(
                (answer.tier, answer.source),
                (autorouter_core::contracts::Tier::Opus, DecisionSource::Jev)
            );
            let cached = classifier
                .classify(&body, &config, &CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(
                (cached.tier, cached.source),
                (
                    autorouter_core::contracts::Tier::Opus,
                    DecisionSource::Cache
                )
            );
            assert_eq!(mock.calls(), 1);
            assert_eq!(classifier.pending_counts(), (0, 0));
            classifier.close().await;
            assert_eq!(classifier.shared.workers.len(), 0);
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn cancellation_at_completion_and_future_drop_release_registry_without_caching() {
        let (mock, config, classifier) = fixture(true);
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
        let (mock, config, classifier) = fixture(false);
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

    #[test]
    fn classifier_limits_reject_each_oversized_field_before_owning_transport() {
        // This ordinary test has no Tokio runtime: invalid admission cannot
        // create a worker, call HTTP, or retain the caller's transport.
        let mock = Arc::new(Mock::new(false));
        let config = config();
        for limits in [
            Limits {
                pending: 257,
                subscribers: 0,
            },
            Limits {
                pending: 0,
                subscribers: 1025,
            },
            Limits {
                pending: usize::MAX,
                subscribers: 0,
            },
            Limits {
                pending: 0,
                subscribers: usize::MAX,
            },
        ] {
            let error = match Classifier::with_limits(mock.clone(), &config, limits) {
                Ok(_) => panic!("oversized classifier limits accepted"),
                Err(error) => error,
            };
            assert_eq!(error, "Invalid classifier limits.");
            assert_eq!(Arc::strong_count(&mock), 1);
            assert_eq!(mock.calls(), 0);
        }
        for limits in [
            Limits {
                pending: 0,
                subscribers: 0,
            },
            Limits {
                pending: 2,
                subscribers: 1,
            },
            Limits {
                pending: 256,
                subscribers: 1024,
            },
        ] {
            let classifier = Classifier::with_limits(mock.clone(), &config, limits)
                .expect("valid lowered or exact maximum limits");
            assert_eq!(classifier.shared.limits.pending, limits.pending);
            assert_eq!(classifier.shared.limits.subscribers, limits.subscribers);
            drop(classifier);
            assert_eq!(Arc::strong_count(&mock), 1);
        }
        assert_eq!(mock.calls(), 0);
    }

    #[tokio::test]
    async fn zero_classifier_capacity_is_closed_without_evaluator_requests() {
        let config = config();
        for limits in [
            Limits {
                pending: 0,
                subscribers: 0,
            },
            Limits {
                pending: 0,
                subscribers: 1,
            },
            Limits {
                pending: 1,
                subscribers: 0,
            },
        ] {
            let mock = Arc::new(Mock::new(false));
            let classifier = Classifier::with_limits(mock.clone(), &config, limits)
                .expect("zero capacity remains valid");
            let answer = classifier
                .classify(
                    &document("Synthetic closed capacity"),
                    &config,
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            assert_eq!(answer.source, DecisionSource::Fallback);
            assert_eq!(
                answer.classifier_error,
                Some(ClassifierError::CapacityExhausted)
            );
            assert_eq!(mock.calls(), 0);
            assert_eq!(classifier.pending_counts(), (0, 0));
            assert!(classifier.shared.state.lock().unwrap().cache.is_empty());
            assert_eq!(classifier.shared.workers.len(), 0);
            classifier.close().await;
        }
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
        )
        .expect("bounded capacity test limits");
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
            assert_eq!(
                result.classifier_error.map(ClassifierError::as_str),
                Some("capacity_exhausted")
            );
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
        assert_eq!(
            blocked.classifier_error.map(ClassifierError::as_str),
            Some("capacity_exhausted")
        );
        let d = start(&classifier, body, &config, CancellationToken::new());
        wait_subscribers(&classifier, 3).await;
        mock.release(2);
        for task in [b, c, d] {
            assert_eq!(task.await.unwrap().unwrap().source, "jev");
        }
        assert_eq!(mock.calls(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cached_decision_bypasses_saturated_work_and_subscriber_limits() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mock = Arc::new(Mock::new(false));
            let config = config();
            let classifier = Classifier::with_limits(
                mock.clone(),
                &config,
                Limits {
                    pending: 2,
                    subscribers: 3,
                },
            )
            .expect("bounded cache/capacity test limits");
            let cached_body = document("Synthetic cached task");
            let seeded = classifier
                .classify(&cached_body, &config, &CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(seeded.source, DecisionSource::Jev);
            assert_eq!(mock.calls(), 1);
            assert_eq!(classifier.pending_counts(), (0, 0));

            mock.blocked.store(true, Ordering::SeqCst);
            let shared_body = document("Synthetic shared held task");
            let tokens: Vec<_> = (0..3).map(|_| CancellationToken::new()).collect();
            let tasks = [
                start(&classifier, shared_body.clone(), &config, tokens[0].clone()),
                start(&classifier, shared_body.clone(), &config, tokens[1].clone()),
                start(
                    &classifier,
                    document("Synthetic other held task"),
                    &config,
                    tokens[2].clone(),
                ),
            ];
            wait_subscribers(&classifier, 3).await;
            wait_calls(&mock, 3).await;
            assert_eq!(classifier.pending_counts(), (2, 3));
            assert_eq!(classifier.shared.workers.len(), 2);
            assert_eq!(mock.active.load(Ordering::SeqCst), 2);
            assert_eq!(mock.waiting.load(Ordering::SeqCst), 2);

            // Prove actual saturation before exercising the cache exemption.
            for body in [shared_body, document("Synthetic overflow task")] {
                let refused = classifier
                    .classify(&body, &config, &CancellationToken::new())
                    .await
                    .unwrap();
                assert_eq!(refused.source, DecisionSource::Fallback);
                assert_eq!(
                    refused.classifier_error,
                    Some(ClassifierError::CapacityExhausted)
                );
            }
            let cached = classifier
                .classify(&cached_body, &config, &CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(cached.source, DecisionSource::Cache);
            assert_eq!(cached.tier, seeded.tier);
            assert_eq!(cached.classifier_error, None);
            assert_eq!(mock.calls(), 3);
            assert_eq!(classifier.pending_counts(), (2, 3));
            assert_eq!(classifier.shared.workers.len(), 2);
            assert_eq!(mock.active.load(Ordering::SeqCst), 2);
            assert_eq!(mock.waiting.load(Ordering::SeqCst), 2);

            for token in &tokens {
                token.cancel();
            }
            for task in tasks {
                assert!(matches!(
                    task.await.unwrap(),
                    Err(EvaluationError::Cancelled)
                ));
            }
            classifier.close().await;
            assert_eq!(classifier.pending_counts(), (0, 0));
            assert_eq!(classifier.shared.workers.len(), 0);
            assert_eq!(mock.active.load(Ordering::SeqCst), 0);
            assert_eq!(mock.waiting.load(Ordering::SeqCst), 0);
            assert_eq!(mock.calls(), 3);
        })
        .await
        .expect("cache/capacity schedule exceeded its independent deadline");
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
        let (mock, config, classifier) = fixture(false);
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
        let (mock, config, classifier) = fixture(false);
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
        let (mock, config, classifier) = fixture(true);
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

    #[test]
    fn settings_identity_distinguishes_effective_credential_presence_and_source_values() {
        use sha2::{Digest, Sha256};
        let body = document("synthetic credential identity");
        let checked = |config: &RouterConfig, credential: Option<&str>| {
            let field = credential
                .map(|json| format!(",\"jevKey\":{json}"))
                .unwrap_or_default();
            let legacy = format!(
                r#"[{},{{"evaluator":"jev","jevEndpoint":"https://api.typesafe.ai/v1/systemone","jevModel":"jev-latest"{field},"jevTimeoutMs":1500,"minConfidence":0.75,"stateChars":12000}},"0188f45daca9a670c97197fc1726ce6fd2e8cbb646ca5773b15c68b73ace184d"]"#,
                body.stringify()
            );
            let expected: Key = Sha256::digest(legacy.as_bytes()).into();
            let actual = settings_key(&body, config);
            assert_eq!(actual, expected);
            actual
        };
        let mut missing = config();
        missing.replace_credential(Credential::Jev, None);
        let missing_key = checked(&missing, None);
        let values = [
            serde_json::Value::Null,
            json!(false),
            json!(0),
            json!(["synthetic-key"]),
            json!({"synthetic":"key"}),
            json!(""),
            json!("synthetic-key"),
        ];
        let mut keys = Vec::new();
        for value in values {
            let mut settings = missing.clone();
            settings.replace_credential_value(Credential::Jev, Some(&value));
            let key = checked(&settings, Some(&value.to_string()));
            assert_ne!(key, missing_key);
            assert!(!keys.contains(&key));
            keys.push(key);
        }
        let source = JsDocument::parse(br#"{"TYPESAFE_API_KEY":1e400}"#).unwrap();
        let mut overflow = missing.clone();
        overflow
            .replace_credential_document(
                Credential::Jev,
                &source,
                source.get(source.root(), "TYPESAFE_API_KEY").unwrap(),
            )
            .unwrap();
        // Source JSON.stringify hashes both overflow and null as null.
        assert_eq!(checked(&overflow, Some("null")), keys[0]);
        let mut exact = missing.clone();
        let mut replacement = missing.clone();
        for (settings, bytes) in [
            (
                &mut exact,
                br#"{"TYPESAFE_API_KEY":"synthetic-\ud800"}"#.as_slice(),
            ),
            (
                &mut replacement,
                br#"{"TYPESAFE_API_KEY":"synthetic-\ufffd"}"#.as_slice(),
            ),
        ] {
            let source = JsDocument::parse(bytes).unwrap();
            settings
                .replace_credential_document(
                    Credential::Jev,
                    &source,
                    source.get(source.root(), "TYPESAFE_API_KEY").unwrap(),
                )
                .unwrap();
        }
        assert_eq!(exact.jev_key, replacement.jev_key);
        assert_ne!(
            checked(&exact, Some(r#""synthetic-\ud800""#)),
            checked(&replacement, Some(r#""synthetic-�""#))
        );
        exact.replace_credential(Credential::Jev, Some("synthetic-new".into()));
        assert_ne!(checked(&exact, Some(r#""synthetic-new""#)), missing_key);
        exact.replace_credential(Credential::Jev, None);
        assert_eq!(checked(&exact, None), missing_key);
        let mut local = missing;
        local.evaluator = Evaluator::Ollama;
        local.ollama_model = "tev1:0.8b".into();
        local.ollama_timeout_ms = 1500;
        local.ollama_keep_alive = json!("5m");
        let legacy = format!(
            r#"[{},{{"evaluator":"ollama","ollamaEndpoint":"http://127.0.0.1:11434","ollamaModel":"tev1:0.8b","ollamaTimeoutMs":1500,"ollamaStateChars":3000,"ollamaKeepAlive":"5m"}},"be151cedb4de4b7ef3f7162d751f70ce7d9dd14efc66fae1835f73ffd04027be"]"#,
            body.stringify()
        );
        let expected: Key = Sha256::digest(legacy.as_bytes()).into();
        assert_eq!(settings_key(&body, &local), expected);
    }

    #[test]
    fn evaluator_snapshot_erases_unrelated_source_credentials_and_keeps_jev() {
        let source = JsDocument::parse(br#"{"AUTOROUTER_EVALUATOR":"jev","ANTHROPIC_API_KEY":{"synthetic":"private-provider"},"TYPESAFE_API_KEY":"synthetic-\ud800","AUTOROUTER_TOKEN":["synthetic-private-local"]}"#).unwrap();
        let config = crate::test_config_document(&source);
        let settings = evaluation_settings(&config);
        assert!(settings.anthropic_key.is_none() && settings.local_token.is_none());
        assert!(settings.credential_json(Credential::Anthropic).is_none());
        assert!(settings.credential_json(Credential::LocalToken).is_none());
        assert_eq!(
            settings.credential_json(Credential::Jev),
            config.credential_json(Credential::Jev)
        );
        let output = serde_json::to_string(&settings).unwrap();
        assert!(!output.contains("private-provider") && !output.contains("private-local"));
        assert!(output.contains(r#""jevKey":"synthetic-\ud800""#));
        assert!(config.credential_json(Credential::Anthropic).is_some());
        assert!(config.credential_json(Credential::LocalToken).is_some());
    }

    include!("classifier_cancellation_contracts.rs");
    include!("classifier_shutdown_contracts.rs");
}

#[cfg(test)]
#[path = "classifier_shutdown_gate.rs"]
pub(crate) mod shutdown_gate;
