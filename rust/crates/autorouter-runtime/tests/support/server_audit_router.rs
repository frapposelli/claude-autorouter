//! Shared audit recorder; each parent retains its document-witness policy.
use super::{parsed, real};
use autorouter_core::{js_json::JsDocument, router::RouteOptions};
use autorouter_runtime::{
    evaluator::EvaluationError, response_observer::CompletionEvidence, server::GatewayRouter,
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
