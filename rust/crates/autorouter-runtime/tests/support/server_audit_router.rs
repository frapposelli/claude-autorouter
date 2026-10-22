//! Shared audit recorder; each parent retains its document-witness policy.
use super::peer::{Observation, Peer};
use super::{BOUND, Events, Reply, contract, parsed, real};
use autorouter_core::{js_json::JsDocument, router::RouteOptions};
use autorouter_runtime::{
    evaluator::EvaluationError,
    http_client::NativeHttpClient,
    observer_tasks::ObserverTasks,
    response_observer::CompletionEvidence,
    server::{GatewayHandle, GatewayRouter},
};
use hyper::HeaderMap;
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio_util::sync::CancellationToken;

pub enum RouterImpl {
    Mock(Vec<Value>),
    Real(Box<real::OriginalRouter>),
}
pub trait DocumentWitness: Send + Sync + 'static {
    fn capture(&self, document: &Arc<JsDocument>);
    fn unchanged(&self) -> Vec<bool> {
        Vec::new()
    }
}
impl DocumentWitness for () {
    fn capture(&self, _: &Arc<JsDocument>) {}
}
impl DocumentWitness for Mutex<Vec<(Arc<JsDocument>, String)>> {
    fn capture(&self, document: &Arc<JsDocument>) {
        let mut witnesses = self.lock().unwrap();
        assert!(witnesses.len() < 8);
        witnesses.push((document.clone(), document.stringify()));
    }
    fn unchanged(&self) -> Vec<bool> {
        self.lock()
            .unwrap()
            .iter()
            .map(|(d, s)| d.stringify() == *s)
            .collect()
    }
}
pub struct AuditRouter<W: DocumentWitness> {
    pub inner: RouterImpl,
    pub calls: Mutex<Vec<Value>>,
    pub completions: Mutex<Vec<Value>>,
    pub closed: AtomicBool,
    pub witnesses: W,
}
impl<W: DocumentWitness> GatewayRouter for AuditRouter<W> {
    async fn route(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> Result<Value, EvaluationError> {
        self.witnesses.capture(&document);
        let index = {
            let mut calls = self.calls.lock().unwrap();
            let index = calls.len();
            assert!(index < 8);
            calls.push(json!({"document":parsed(document.stringify().as_bytes()),"scope":options.scope,"prompt_id":options.prompt_id,"request_class":options.request_class,"request_id":options.request_id,"count_tokens":options.count_tokens,"signal_live":!cancellation.is_cancelled()}));
            index
        };
        match &self.inner {
            RouterImpl::Mock(decisions) => Ok(decisions[index].clone()),
            RouterImpl::Real(real) => {
                real.route(document, options, headers, cancellation, search)
                    .await
            }
        }
    }
    fn complete(&self, id: &str, evidence: &Value) -> bool {
        let accepted = match &self.inner {
            RouterImpl::Mock(_) => false,
            RouterImpl::Real(real) => real.complete(id, evidence),
        };
        let mut rows = self.completions.lock().unwrap();
        assert!(rows.len() < 32);
        rows.push(json!({"id":id,"evidence":evidence,"accepted":accepted}));
        accepted
    }
    fn complete_exact(&self, id: &str, evidence: &CompletionEvidence) -> bool {
        let accepted = match &self.inner {
            RouterImpl::Mock(_) => false,
            RouterImpl::Real(real) => real.complete_exact(id, evidence),
        };
        let mut rows = self.completions.lock().unwrap();
        assert!(rows.len() < 32);
        rows.push(json!({"id":id,"evidence":evidence,"accepted":accepted}));
        accepted
    }
    fn shutdown(&self) {
        if let RouterImpl::Real(real) = &self.inner {
            real.shutdown();
        }
    }
    async fn close(&self) {
        if let RouterImpl::Real(real) = &self.inner {
            real.close().await;
        }
        self.closed.store(true, Ordering::SeqCst);
    }
}

pub struct Fixture<W: DocumentWitness> {
    pub client: Arc<NativeHttpClient>,
    pub router: Arc<AuditRouter<W>>,
    pub gateway: Option<GatewayHandle>,
    pub peer: Option<Peer>,
    pub statuses: Events,
    pub history: Events,
    pub logs: Events,
    pub log_history: Events,
    pub decisions: Events,
    pub records: Events,
    pub callback_outcomes: Events,
    pub real: bool,
    pub observer_owner: Option<ObserverTasks>,
    pub never_polled: Arc<std::sync::atomic::AtomicUsize>,
    pub never_dropped: Arc<std::sync::atomic::AtomicUsize>,
}
#[allow(dead_code)] // Each target records only its original observation fields.
pub struct Seen {
    pub requests: Vec<Observation>,
    pub routes: Vec<Value>,
    pub completions: Vec<Value>,
    pub statuses: Vec<Value>,
    pub history: Vec<Value>,
    pub logs: Vec<Value>,
    pub decisions: Vec<Value>,
    pub records: Vec<Value>,
    pub callback_outcomes: Vec<Value>,
    pub evaluator_calls: Vec<Value>,
    pub real: bool,
    pub provider_closed_before_owner_close: Option<bool>,
    pub manufactured_provider_disconnect: Option<bool>,
    pub continuity_before_close: Option<Value>,
    pub input_documents_unchanged: Vec<bool>,
    pub observer_before_close: Option<Value>,
    pub observer_after_close: Option<Value>,
    pub never_polled_before_close: usize,
    pub never_dropped_after_close: usize,
}
impl<W: DocumentWitness> Fixture<W> {
    pub async fn send(&self, row: &Value) -> Result<Reply, String> {
        contract::send_buffered(&self.client, self.gateway.as_ref(), row).await
    }
    pub async fn wait_for_status(&self, event: &str) -> Result<(), String> {
        contract::wait_for_status(&self.statuses, event).await
    }
    pub async fn finish(mut self) -> Result<Seen, String> {
        let continuity_before_close = match &self.router.inner {
            RouterImpl::Real(real) => Some(real.continuity()),
            RouterImpl::Mock(_) => None,
        };
        let observer_before_close = self
            .observer_owner
            .as_ref()
            .map(|owner| real::snapshot(owner.snapshot()));
        let never_polled_before_close = self.never_polled.load(Ordering::SeqCst);
        let requests = contract::close_gateway(&mut self.gateway, &mut self.peer, BOUND).await?;
        if let Some(owner) = &self.observer_owner {
            tokio::time::timeout(BOUND, owner.close())
                .await
                .map_err(|_| "observer owner close timed out")?;
        }
        let observer_after_close = self
            .observer_owner
            .as_ref()
            .map(|owner| real::snapshot(owner.snapshot()));
        let never_dropped_after_close = self.never_dropped.load(Ordering::SeqCst);
        if !self.router.closed.load(Ordering::SeqCst) {
            return Err("mock router closure not observed".into());
        }
        contract::check_recorders([&self.statuses, &self.history, &self.logs, &self.log_history])?;
        Ok(Seen {
            requests,
            routes: self.router.calls.lock().unwrap().clone(),
            completions: self.router.completions.lock().unwrap().clone(),
            statuses: self.statuses.lock().unwrap().clone(),
            history: self.history.lock().unwrap().clone(),
            logs: self.logs.lock().unwrap().clone(),
            decisions: self.decisions.lock().unwrap().clone(),
            records: self.records.lock().unwrap().clone(),
            callback_outcomes: self.callback_outcomes.lock().unwrap().clone(),
            evaluator_calls: match &self.router.inner {
                RouterImpl::Real(r) => r.calls(),
                RouterImpl::Mock(_) => Vec::new(),
            },
            real: self.real,
            provider_closed_before_owner_close: None,
            manufactured_provider_disconnect: None,
            continuity_before_close,
            input_documents_unchanged: self.router.witnesses.unchanged(),
            observer_before_close,
            observer_after_close,
            never_polled_before_close,
            never_dropped_after_close,
        })
    }
}
