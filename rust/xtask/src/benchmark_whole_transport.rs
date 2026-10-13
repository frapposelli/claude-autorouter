//! Real router/count and genuine inference client, with bounded synthetic IO.
use super::io::{Connector, MAX_REQUEST};
use autorouter_core::js_json::JsDocument;
use autorouter_core::router::{RouteDecision, RouteOptions};
use autorouter_runtime::evaluator::EvaluationError;
use autorouter_runtime::http_client::{
    DevelopmentHttpClient, HttpError, HttpTransport, NativeHttpClient,
};
use autorouter_runtime::response_observer::CompletionEvidence;
use autorouter_runtime::router::Router;
use autorouter_runtime::server::{GatewayDecision, GatewayRouter};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::UnsyncBoxBody};
use hyper::body::{Body, Frame};
use hyper::{HeaderMap, Request, Response};
use serde_json::Value;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(super) struct Calls {
    pub evaluator: Vec<Bytes>,
    pub count: Vec<Bytes>,
}
pub(super) enum InferenceClient {
    Synthetic(DevelopmentHttpClient<Connector>),
    Loopback(NativeHttpClient),
}
pub(super) struct Transport {
    pub calls: Mutex<Calls>,
    pub count_done: Arc<AtomicUsize>,
    pub count_abandoned: Arc<AtomicUsize>,
    pub inference: InferenceClient,
    pub authority: String,
}
struct CountBody {
    inner: Full<Bytes>,
    done: Option<Arc<AtomicUsize>>,
    abandoned: Arc<AtomicUsize>,
    ended: bool,
}
impl Body for CountBody {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let result = Pin::new(&mut self.inner)
            .poll_frame(cx)
            .map(|frame| frame.map(|result| result.map_err(|never| match never {})));
        if matches!(result, Poll::Ready(None)) && !self.ended {
            self.ended = true;
            if let Some(done) = &self.done {
                done.fetch_add(1, Ordering::SeqCst);
            }
        }
        result
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}
impl Drop for CountBody {
    fn drop(&mut self) {
        if self.done.is_some() && !self.ended {
            self.abandoned.fetch_add(1, Ordering::SeqCst);
        }
    }
}
impl HttpTransport for Transport {
    type ResponseBody = UnsyncBoxBody<Bytes, io::Error>;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        if request.uri().scheme_str() != Some("http")
            || ![Some("127.0.0.1:1"), Some(self.authority.as_str())].contains(
                &request
                    .uri()
                    .authority()
                    .map(|authority| authority.as_str()),
            )
            || request.method() != hyper::Method::POST
        {
            return Err(HttpError::InvalidRequest);
        }
        let path = request.uri().path().to_owned();
        let bytes = request
            .into_body()
            .collect()
            .await
            .map_err(|_| HttpError::InvalidRequest)?
            .to_bytes();
        if bytes.len() > MAX_REQUEST {
            return Err(HttpError::InvalidRequest);
        }
        let count = path == "/v1/messages/count_tokens";
        let response = {
            let mut calls = self.calls.lock().unwrap();
            let (calls, body) = match path.as_str() {
                "/v1/systemone" => (
                    &mut calls.evaluator,
                    b"{\"answers\":{\"tier\":{\"choice\":\"sonnet\",\"confidence\":1}}}".as_slice(),
                ),
                "/v1/messages/count_tokens" => {
                    (&mut calls.count, b"{\"input_tokens\":1000}".as_slice())
                }
                _ => return Err(HttpError::InvalidRequest),
            };
            if !calls.is_empty() {
                return Err(HttpError::InvalidRequest);
            }
            calls.push(bytes);
            Bytes::copy_from_slice(body)
        };
        Ok(Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(
                CountBody {
                    inner: Full::new(response),
                    done: count.then(|| self.count_done.clone()),
                    abandoned: self.count_abandoned.clone(),
                    ended: false,
                }
                .boxed_unsync(),
            )
            .unwrap())
    }
    async fn request_raw(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        if request.uri().scheme_str() != Some("http")
            || request
                .uri()
                .authority()
                .map(|authority| authority.as_str())
                != Some(self.authority.as_str())
            || request.uri().path() != "/v1/messages"
            || request.uri().query() != Some("probe=whole")
        {
            return Err(HttpError::InvalidRequest);
        }
        let response = match &self.inference {
            InferenceClient::Synthetic(client) => client.request_raw(request).await?,
            InferenceClient::Loopback(client) => client.request_raw(request).await?,
        };
        Ok(response.map(|body| {
            body.map_err(|_| io::Error::other("Synthetic upstream body failed"))
                .boxed_unsync()
        }))
    }
}

/// Observe accepted delivery commits without changing the production pipeline.
pub(super) struct ObservedRouter {
    pub router: Router<Transport>,
    pub commits: Mutex<Vec<Value>>,
}
impl GatewayRouter for ObservedRouter {
    async fn route(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> Result<Value, EvaluationError> {
        self.router
            .route(document, options, headers, cancellation, search)
            .await
    }
    async fn route_exact(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> Result<RouteDecision, EvaluationError> {
        self.router
            .route_exact(document, options, headers, cancellation, search)
            .await
    }
    async fn route_contract_exact(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> Result<GatewayDecision, EvaluationError> {
        self.router
            .route_typed_exact(document, options, headers, cancellation, search)
            .await
            .map(GatewayDecision::Typed)
    }
    fn complete(&self, id: &str, evidence: &Value) -> bool {
        let accepted = self.router.complete(id, evidence);
        if accepted {
            self.commits.lock().unwrap().push(evidence.clone());
        }
        accepted
    }
    fn complete_exact(&self, id: &str, evidence: &CompletionEvidence) -> bool {
        let accepted = self.router.complete_exact(id, evidence);
        if accepted {
            self.commits
                .lock()
                .unwrap()
                .push(serde_json::to_value(evidence).unwrap());
        }
        accepted
    }
    fn shutdown(&self) {
        self.router.shutdown();
    }
    async fn close(&self) {
        self.router.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::super::io::ProviderState;
    use super::*;
    #[tokio::test]
    async fn body_observation_requires_complete_count_eof_not_only_call_admission() {
        let transport = Transport {
            calls: Mutex::new(Calls::default()),
            count_done: Arc::default(),
            count_abandoned: Arc::default(),
            inference: InferenceClient::Synthetic(DevelopmentHttpClient::new(Connector(Arc::new(
                ProviderState::default(),
            )))),
            authority: "127.0.0.1:1".into(),
        };
        let request = || {
            Request::builder()
                .method("POST")
                .uri("http://127.0.0.1:1/v1/messages/count_tokens")
                .body(Full::new(Bytes::from_static(b"{}")))
                .unwrap()
        };
        let response = transport.request(request()).await.unwrap();
        assert_eq!(transport.calls.lock().unwrap().count.len(), 1);
        assert_eq!(transport.count_done.load(Ordering::SeqCst), 0);
        drop(response);
        assert_eq!(transport.count_abandoned.load(Ordering::SeqCst), 1);
        transport.calls.lock().unwrap().count.clear();
        let response = transport.request(request()).await.unwrap();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(bytes.as_ref(), b"{\"input_tokens\":1000}");
        assert_eq!(transport.count_done.load(Ordering::SeqCst), 1);
        assert_eq!(transport.count_abandoned.load(Ordering::SeqCst), 1);
    }
}
