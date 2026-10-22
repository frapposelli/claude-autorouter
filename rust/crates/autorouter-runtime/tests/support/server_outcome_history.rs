//! Original injected Jev transport; provider forwarding still uses real HTTP.
use super::{Events, LIMIT, decode, headers, parsed};
use autorouter_core::{config::read_config, js_json::JsDocument, router::RouteOptions};
use autorouter_runtime::{
    evaluator::EvaluationError,
    http_client::{HttpError, HttpTransport},
    response_observer::CompletionEvidence,
    router::Router,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Request, Response};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;
struct Evaluator {
    script: Vec<Value>,
    calls: Events,
}
impl HttpTransport for Evaluator {
    type ResponseBody = Full<Bytes>;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Full<Bytes>>, HttpError> {
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        assert!(bytes.len() < LIMIT);
        let mut calls = self.calls.lock().unwrap();
        let index = calls.len();
        assert!(
            index < self.script.len(),
            "no unexpected evaluator/count requests"
        );
        let wanted = &self.script[index];
        assert_eq!(parts.uri.to_string(), wanted["url"]);
        assert_eq!(parts.method.as_str(), wanted["method"]);
        assert_eq!(parts.headers, headers(&wanted["headers"]));
        assert_eq!(
            parsed(&bytes),
            parsed(wanted["body"].as_str().unwrap().as_bytes())
        );
        calls.push(json!({"url":parts.uri.to_string(),"method":parts.method.as_str(),"headers":parts.headers.iter().map(|(k,v)|(k.as_str().to_owned(),v.to_str().unwrap().to_owned())).collect::<std::collections::BTreeMap<_,_>>(),"body_utf8":std::str::from_utf8(&bytes).unwrap()}));
        let response = wanted["reads"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["kind"] == "read" && r["done"] == false)
            .flat_map(|r| decode(&r["base64"]))
            .collect::<Vec<_>>();
        assert!(response.len() < LIMIT);
        let mut result = Response::builder()
            .status(wanted["status"].as_u64().unwrap() as u16)
            .body(Full::new(Bytes::from(response)))
            .unwrap();
        *result.headers_mut() = headers(&wanted["response_headers"]);
        Ok(result)
    }
}
pub struct OriginalRouter {
    inner: Router<Evaluator>,
    evaluator: Arc<Evaluator>,
}
impl OriginalRouter {
    pub fn new(row: &Value) -> Self {
        let evaluator = Arc::new(Evaluator {
            script: row["mocked_fetch"].as_array().unwrap().clone(),
            calls: Events::default(),
        });
        let config = read_config(&row["config_arguments"][0][0], false, Path::new("/tmp")).unwrap();
        Self {
            inner: Router::new(evaluator.clone(), config),
            evaluator,
        }
    }
    pub async fn route(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> Result<Value, EvaluationError> {
        self.inner
            .route(document, options, headers, cancellation, search)
            .await
    }
    pub fn complete(&self, id: &str, evidence: &Value) -> bool {
        self.inner.complete(id, evidence)
    }
    pub fn complete_exact(&self, id: &str, evidence: &CompletionEvidence) -> bool {
        self.inner.complete_exact(id, evidence)
    }
    pub fn shutdown(&self) {
        self.inner.shutdown();
    }
    pub async fn close(&self) {
        self.inner.close().await;
    }
    pub fn continuity(&self) -> Value {
        let snapshot = self.inner.continuity_snapshot();
        json!({"records":snapshot.records,"attempts":snapshot.attempts,"aliases":snapshot.aliases})
    }
    pub fn attempts(&self) -> usize {
        self.inner.continuity_snapshot().attempts
    }
    pub fn calls(&self) -> Vec<Value> {
        self.evaluator.calls.lock().unwrap().clone()
    }
}
pub struct NeverReady {
    polled: Arc<std::sync::atomic::AtomicUsize>,
    dropped: Arc<std::sync::atomic::AtomicUsize>,
    first: bool,
}
impl NeverReady {
    pub fn new(
        polled: Arc<std::sync::atomic::AtomicUsize>,
        dropped: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        Self {
            polled,
            dropped,
            first: true,
        }
    }
}
impl std::future::Future for NeverReady {
    type Output = Result<(), autorouter_runtime::observer_tasks::ObserverRejected>;
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if self.first {
            self.first = false;
            self.polled
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        std::task::Poll::Pending
    }
}
impl Drop for NeverReady {
    fn drop(&mut self) {
        self.dropped
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
pub fn snapshot(s: autorouter_runtime::observer_tasks::ObserverSnapshot) -> Value {
    json!({"closed":s.closed,"worker_finished":s.worker_finished,"pending":s.pending,"factories":s.factories,"charged_bytes":s.charged_bytes,"completed":s.completed,"rejected":s.rejected,"factory_panicked":s.factory_panicked,"poll_panicked":s.poll_panicked,"destructor_panicked":s.destructor_panicked,"timed_out":s.timed_out,"cancelled":s.cancelled,"declined_closed":s.declined_closed,"declined_cancelled":s.declined_cancelled,"declined_payload":s.declined_payload,"declined_capacity":s.declined_capacity,"declined_deadline":s.declined_deadline})
}
