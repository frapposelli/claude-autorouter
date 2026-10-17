//! Shared simple synthetic peer storage; original parent constructors retain their response scripts.
use autorouter_core::{js_json::JsDocument, router::RouteOptions};
use autorouter_runtime::{
    evaluator::EvaluationError,
    http_client::{HttpTransport, NativeHttpClient},
    server::{GatewayHandle, GatewayRouter},
    server_events::EventSink,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{HeaderMap, Request};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub type Events = Arc<Mutex<Vec<Value>>>;
pub fn sink(events: &Events) -> EventSink {
    let events = events.clone();
    Arc::new(move |value| {
        let mut rows = events.lock().unwrap();
        assert!(rows.len() < 128);
        rows.push(value.to_serde_observation_lossy());
    })
}
#[derive(Default)]
pub struct Router {
    pub calls: AtomicUsize,
    pub shutdown: AtomicBool,
}
impl GatewayRouter for Router {
    async fn route(
        &self,
        _: Arc<JsDocument>,
        _: RouteOptions,
        _: &HeaderMap,
        _: &CancellationToken,
        _: &str,
    ) -> Result<Value, EvaluationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"model":"claude-haiku-4-5-20251001","source":"test"}))
    }
    fn complete(&self, _: &str, _: &Value) -> bool {
        false
    }
    fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
    async fn close(&self) {
        self.shutdown();
    }
}
pub struct PeerLease(pub Arc<AtomicUsize>);
impl Drop for PeerLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
pub struct Seen {
    pub method: String,
    pub path: String,
    pub headers: HeaderMap,
    pub body: Bytes,
}
pub struct Fixture {
    pub handle: Option<GatewayHandle>,
    pub peer: Option<JoinHandle<()>>,
    pub stop: CancellationToken,
    pub active: Arc<AtomicUsize>,
    pub seen: Arc<Mutex<Vec<Seen>>>,
    pub logs: Events,
    pub statuses: Events,
    pub decisions: Events,
    pub router: Arc<Router>,
    pub client: Option<Arc<NativeHttpClient>>,
    pub token: String,
}
impl Fixture {
    pub async fn send(
        &self,
        path: &str,
        method: &str,
        body: String,
        extra: &[(&str, &str)],
    ) -> (u16, HeaderMap, Bytes) {
        let mut request = Request::builder()
            .method(method)
            .uri(format!(
                "http://{}{path}",
                self.handle.as_ref().unwrap().address
            ))
            .header(super::LOCAL_AUTH_HEADER, &self.token)
            .body(Full::new(Bytes::from(body)))
            .unwrap();
        for (name, value) in extra {
            request.headers_mut().insert(
                hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        let response = self
            .client
            .as_ref()
            .unwrap()
            .request(request)
            .await
            .unwrap();
        let (parts, body) = response.into_parts();
        (
            parts.status.as_u16(),
            parts.headers,
            Limited::new(body, 65536)
                .collect()
                .await
                .unwrap()
                .to_bytes(),
        )
    }
    pub async fn close_connections(&mut self) {
        self.client.take();
        self.handle.take().unwrap().close().await;
        assert!(self.router.shutdown.load(Ordering::SeqCst));
        self.stop.cancel();
        self.peer.take().unwrap().await.unwrap();
        assert_eq!(self.active.load(Ordering::SeqCst), 0);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(task) = self.peer.take() {
            task.abort();
        }
    }
}
