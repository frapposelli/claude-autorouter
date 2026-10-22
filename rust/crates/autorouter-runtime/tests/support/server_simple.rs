//! Shared simple synthetic peer lifecycle; parent constructors retain their response/config scripts.
use autorouter_core::{config::RouterConfig, js_json::JsDocument, router::RouteOptions};
use autorouter_runtime::{
    evaluator::EvaluationError,
    http_client::{HttpTransport, NativeHttpClient},
    server::{Gateway, GatewayHandle, GatewayRouter},
    server_events::{EventSink, EventSinks},
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{HeaderMap, Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::task::{JoinHandle, JoinSet};
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
    pub async fn start<Prepare, Respond, Reply, Configure>(
        prepare: Prepare,
        configure: Configure,
    ) -> Self
    where
        Prepare: FnOnce() -> Respond,
        Respond:
            Fn(Request<Incoming>, Arc<Mutex<Vec<Seen>>>) -> Reply + Clone + Send + Sync + 'static,
        Reply: Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send + 'static,
        Configure: FnOnce(SocketAddr) -> (RouterConfig, String),
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let stop = CancellationToken::new();
        let peer_stop = stop.clone();
        let active = Arc::new(AtomicUsize::new(0));
        let peer_active = active.clone();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let peer_seen = seen.clone();
        let respond = prepare();
        let peer = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = peer_stop.cancelled() => break,
                    result = tasks.join_next(), if !tasks.is_empty() => { result.unwrap().unwrap(); },
                    socket = listener.accept() => {
                        let (socket, _) = socket.unwrap();
                        assert!(peer_active.fetch_add(1, Ordering::SeqCst) < 8);
                        let lease = PeerLease(peer_active.clone());
                        let seen = peer_seen.clone();
                        let respond = respond.clone();
                        tasks.spawn(async move {
                            let _lease = lease;
                            let service = service_fn(move |request| respond(request, seen.clone()));
                            hyper::server::conn::http1::Builder::new()
                                .serve_connection(TokioIo::new(socket), service).await.unwrap();
                        });
                    }
                }
            }
            tasks.abort_all();
            while let Some(result) = tasks.join_next().await {
                if let Err(error) = result {
                    assert!(error.is_cancelled(), "Unexpected peer panic: {error}");
                }
            }
        });
        let (config, token) = configure(address);
        let client = Arc::new(NativeHttpClient::new().unwrap());
        let router = Arc::new(Router::default());
        let logs = Events::default();
        let statuses = Events::default();
        let decisions = Events::default();
        let gateway = Gateway::with_router(
            config,
            client.clone(),
            router.clone(),
            EventSinks {
                log: Some(sink(&logs)),
                status: Some(sink(&statuses)),
                decision: Some(sink(&decisions)),
                ..Default::default()
            },
        )
        .unwrap();
        let handle = gateway.listen(0).await.unwrap();
        Self {
            handle: Some(handle),
            peer: Some(peer),
            stop,
            active,
            seen,
            logs,
            statuses,
            decisions,
            router,
            client: Some(client),
            token,
        }
    }
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
