//! Loopback gateway with request-scoped credentials and original provider bytes.
//! Completion requires both clean protocol evidence and the connection writer's
//! flush acknowledgement. Every abandoned attempt is explicitly released.
use crate::evaluator::EvaluationError;
use crate::http_client::{HttpError, HttpTransport};
use crate::response_observer::{
    CompletionEvidence, DEFAULT_BUFFER_BYTES, Observation, ObservedBody, ResponseObserver,
};
use crate::router::Router;
use crate::server_events::{
    EventSink, EventSinks, RequestEvents, emit, emit_document, event_document,
};
#[cfg(test)]
use crate::tls_roots::openssl_spike::gateway_intent::{
    Cause as IntentCause, Event as IntentEvent, Intent, IntentIo, Probe as IntentProbe,
    Registry as IntentRegistry,
};
use crate::transport_completion::{
    CompletionRegistry, Delivery, RequestReceiveDeadline, serve_http1,
};
use autorouter_core::auth::{LOCAL_AUTH_HEADER, is_subscription_request};
use autorouter_core::config::{AuthMode, RouterConfig, js_trim};
use autorouter_core::js_json::{JsDocument, JsNode, JsString, NodeId};
use autorouter_core::model_request::prepare_request_document_exact;
use autorouter_core::request_validation::validate_request_document;
use autorouter_core::router::{RouteDecision, RouteOptions};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::UnsyncBoxBody};
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::service::service_fn;
use hyper::{HeaderMap, Method, Request, Response};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use subtle::ConstantTimeEq;
use tokio::time::{Instant, Sleep};
use tokio_util::sync::CancellationToken;

pub trait GatewayRouter: Send + Sync + 'static {
    /// Development adapters may make an explicit request transformation before
    /// routing. The product router keeps the original document unchanged.
    fn transform_document(&self, document: Arc<JsDocument>) -> Arc<JsDocument> {
        document
    }
    fn transform_document_for_request(
        &self,
        document: Arc<JsDocument>,
        _request_class: &str,
    ) -> Arc<JsDocument> {
        self.transform_document(document)
    }
    fn route(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> impl Future<Output = Result<Value, EvaluationError>> + Send;
    fn route_exact(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> impl Future<Output = Result<RouteDecision, EvaluationError>> + Send {
        async move {
            let decision = self
                .route(document, options, headers, cancellation, search)
                .await?;
            let model = JsString::from_scalar(
                decision["model"]
                    .as_str()
                    .ok_or(EvaluationError::InvalidResponse)?,
            );
            Ok(RouteDecision { decision, model })
        }
    }
    fn complete(&self, id: &str, evidence: &Value) -> bool;
    fn complete_exact(&self, id: &str, evidence: &CompletionEvidence) -> bool {
        // Older development adapters can only accept scalar JSON metadata.
        // Production overrides this method and keeps exact UTF-16 identities.
        match serde_json::to_value(evidence) {
            Ok(value) => self.complete(id, &value),
            Err(_) => self.complete(id, &Value::Null),
        }
    }
    fn shutdown(&self);
}
impl<T: HttpTransport + 'static> GatewayRouter for Router<T>
where
    <T::ResponseBody as Body>::Error: Send + Sync + 'static,
{
    async fn route(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> Result<Value, EvaluationError> {
        self.route(document, options, headers, cancellation, search)
            .await
    }
    fn complete(&self, id: &str, evidence: &Value) -> bool {
        self.complete(id, evidence)
    }
    fn complete_exact(&self, id: &str, evidence: &CompletionEvidence) -> bool {
        self.complete_exact(id, evidence)
    }
    async fn route_exact(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> Result<RouteDecision, EvaluationError> {
        self.route_exact(document, options, headers, cancellation, search)
            .await
    }
    fn shutdown(&self) {
        self.shutdown();
    }
}
type GatewayBody = UnsyncBoxBody<Bytes, io::Error>;
fn boxed<B: Body<Data = Bytes> + Send + 'static>(body: B) -> GatewayBody
where
    B::Error: Send + Sync + 'static,
{
    body.map_err(|_| io::Error::other("Upstream body failed"))
        .boxed_unsync()
}
fn full(bytes: impl Into<Bytes>) -> GatewayBody {
    Full::new(bytes.into())
        .map_err(|never: Infallible| match never {})
        .boxed_unsync()
}
fn json_error(status: u16, message: &str) -> Response<GatewayBody> {
    let mut response = Response::builder()
        .status(status)
        .header("content-type", "application/json");
    if status == 413 {
        response = response.header("connection", "close");
    }
    response
        .body(full(
            json!({"type":"error","error":{"type":"api_error","message":message}}).to_string(),
        ))
        .expect("fixed error response")
}
fn header_text(headers: &HeaderMap, key: &str) -> Option<String> {
    headers
        .get(key)
        .map(|v| v.as_bytes().iter().map(|b| char::from(*b)).collect())
}
fn js_header(value: &str) -> Result<hyper::header::HeaderValue, HttpError> {
    let bytes: Option<Vec<u8>> = value.chars().map(|c| u8::try_from(c as u32).ok()).collect();
    hyper::header::HeaderValue::from_bytes(&bytes.ok_or(HttpError::InvalidRequest)?)
        .map_err(|_| HttpError::InvalidRequest)
}

/// Reproduce Node's IncomingMessage header view: selected singleton fields
/// retain the first value, Cookie joins with semicolons, other duplicates join.
fn incoming_headers(headers: &HeaderMap) -> HeaderMap {
    const FIRST: &[&str] = &[
        "age",
        "authorization",
        "content-length",
        "content-type",
        "etag",
        "expires",
        "from",
        "host",
        "if-modified-since",
        "if-unmodified-since",
        "last-modified",
        "location",
        "max-forwards",
        "proxy-authorization",
        "referer",
        "retry-after",
        "server",
        "user-agent",
    ];
    let mut result = HeaderMap::new();
    for key in headers.keys() {
        let values: Vec<_> = headers.get_all(key).iter().collect();
        if key == "set-cookie" {
            for value in values {
                result.append(key.clone(), value.clone());
            }
        } else if values.len() == 1 || FIRST.contains(&key.as_str()) {
            result.insert(key.clone(), values[0].clone());
        } else {
            let delimiter = if key == "cookie" {
                b"; ".as_slice()
            } else {
                b", ".as_slice()
            };
            let mut joined = Vec::new();
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    joined.extend_from_slice(delimiter);
                }
                joined.extend_from_slice(value.as_bytes());
            }
            if let Ok(value) = hyper::header::HeaderValue::from_bytes(&joined) {
                result.insert(key.clone(), value);
            }
        }
    }
    result
}
pub fn clean_headers(headers: &HeaderMap) -> HeaderMap {
    let mut blocked: HashSet<String> = [
        "host",
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "content-length",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    if let Some(connection) = header_text(headers, "connection") {
        for name in connection.split(',') {
            blocked.insert(js_trim(name).to_ascii_lowercase());
        }
    }
    let mut result = HeaderMap::new();
    for (name, value) in headers {
        if !blocked.contains(name.as_str()) {
            result.append(name.clone(), value.clone());
        }
    }
    result
}
pub fn upstream_headers(
    incoming: &HeaderMap,
    config: &RouterConfig,
) -> Result<HeaderMap, HttpError> {
    let mut headers = clean_headers(incoming);
    headers.remove(LOCAL_AUTH_HEADER);
    headers.remove("cookie");
    headers.insert(
        "accept-encoding",
        hyper::header::HeaderValue::from_static("identity"),
    );
    if config.auth_mode == AuthMode::Subscription {
        headers.remove("x-api-key");
    } else {
        headers.remove("authorization");
        headers.insert(
            "x-api-key",
            js_header(config.anthropic_key.as_deref().unwrap_or("undefined"))?,
        );
    }
    Ok(headers)
}
fn authorized(headers: &HeaderMap, config: &RouterConfig) -> bool {
    let credential = if config.auth_mode == AuthMode::Subscription {
        header_text(headers, LOCAL_AUTH_HEADER)
    } else {
        header_text(headers, "x-api-key").or_else(|| {
            header_text(headers, "authorization").map(|text| {
                if text
                    .get(..7)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Bearer "))
                {
                    text[7..].to_owned()
                } else {
                    text
                }
            })
        })
    };
    let Some(actual) = credential else {
        return false;
    };
    let expected = config.local_token.as_deref().unwrap_or_default();
    actual.len() == expected.len() && bool::from(actual.as_bytes().ct_eq(expected.as_bytes()))
}
fn request_id() -> Result<String, ()> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(|_| ())?;
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let mut id = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if [4, 6, 8, 10].contains(&index) {
            id.push('-');
        }
        id.push_str(&format!("{byte:02x}"));
    }
    Ok(id)
}

struct RequestLease<R: GatewayRouter> {
    router: Arc<R>,
    id: String,
    events: Arc<RequestEvents>,
    cancellation: CancellationToken,
    finished: bool,
}
impl<R: GatewayRouter> RequestLease<R> {
    fn finish(mut self, delivery: Delivery, evidence: Option<CompletionEvidence>, success: bool) {
        let delivered = delivery == Delivery::Flushed && !self.cancellation.is_cancelled();
        let execution = if delivered && success { evidence } else { None };
        self.events.confirmed(execution.is_some());
        if let Some(execution) = &execution {
            self.router.complete_exact(&self.id, execution);
        } else {
            self.router.complete(&self.id, &Value::Null);
        }
        self.events.status(
            if delivered {
                "request_complete"
            } else {
                "request_cancelled"
            },
            json!({}),
        );
        self.finished = true;
    }
}
impl<R: GatewayRouter> Drop for RequestLease<R> {
    fn drop(&mut self) {
        if !self.finished {
            self.cancellation.cancel();
            self.router.complete(&self.id, &Value::Null);
            self.events.status("request_cancelled", json!({}));
        }
    }
}

struct ForwardBody<B> {
    inner: Pin<Box<B>>,
    deadline: Pin<Box<Sleep>>,
    cancelled: Pin<Box<dyn Future<Output = ()> + Send>>,
    events: Option<Arc<RequestEvents>>,
    log: Option<EventSink>,
    finished: bool,
    #[cfg(test)]
    intent: Option<Arc<Intent>>,
}
impl<B> ForwardBody<B> {
    fn new(
        body: B,
        deadline: Instant,
        cancellation: CancellationToken,
        events: Option<Arc<RequestEvents>>,
        log: Option<EventSink>,
    ) -> Self {
        Self {
            inner: Box::pin(body),
            deadline: Box::pin(tokio::time::sleep_until(deadline)),
            cancelled: Box::pin(cancellation.cancelled_owned()),
            events,
            log,
            finished: false,
            #[cfg(test)]
            intent: None,
        }
    }
    #[cfg(test)]
    fn with_intent(mut self, intent: Option<Arc<Intent>>) -> Self {
        self.intent = intent;
        self
    }
}
impl<B: Body<Data = Bytes>> Body for ForwardBody<B> {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }
        if this.cancelled.as_mut().poll(cx).is_ready() {
            #[cfg(test)]
            if let Some(intent) = &this.intent {
                intent.observe(IntentEvent::GenericCancellation);
            }
            this.finished = true;
            if let Some(events) = &this.events {
                events.status("request_cancelled", json!({}));
            }
            return Poll::Ready(Some(Err(io::Error::other("Request cancelled"))));
        }
        if this.deadline.as_mut().poll(cx).is_ready() {
            #[cfg(test)]
            if let Some(intent) = &this.intent {
                intent.finish(IntentCause::Deadline);
            }
            this.finished = true;
            emit(&this.log, json!({"event":"proxy_error","status":502}));
            if let Some(events) = &this.events {
                events.status("request_error", json!({"status":502}));
            }
            return Poll::Ready(Some(Err(io::Error::other("Upstream deadline exceeded"))));
        }
        #[cfg(test)]
        if this
            .intent
            .as_ref()
            .is_some_and(|intent| !intent.body_ready(cx))
        {
            return Poll::Pending;
        }
        match this.inner.as_mut().poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) if frame.is_trailers() => {
                // Node's upstream pipeline consumes trailers without calling
                // ServerResponse.addTrailers. Still poll for a clean upstream
                // EOF before completion can be armed; trailers are not EOF.
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Poll::Ready(Some(Err(_))) => {
                #[cfg(test)]
                if let Some(intent) = &this.intent {
                    intent.finish(IntentCause::UpstreamFailure);
                }
                this.finished = true;
                emit(&this.log, json!({"event":"proxy_error","status":502}));
                if let Some(events) = &this.events {
                    events.status("request_error", json!({"status":502}));
                }
                Poll::Ready(Some(Err(io::Error::other("Upstream body failed"))))
            }
            Poll::Ready(None) => {
                #[cfg(test)]
                if let Some(intent) = &this.intent {
                    intent.observe(IntentEvent::UpstreamEof);
                }
                this.finished = true;
                Poll::Ready(None)
            }
            other => other.map(|frame| {
                frame.map(|result| result.map_err(|_| io::Error::other("Upstream body failed")))
            }),
        }
    }
    fn is_end_stream(&self) -> bool {
        self.finished
    }
    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}

pub struct Gateway<T, R = Router<T>> {
    config: RouterConfig,
    transport: Arc<T>,
    router: Arc<R>,
    sinks: EventSinks,
    #[cfg(test)]
    intent_probe: Option<IntentProbe>,
}
impl<T: HttpTransport + 'static> Gateway<T>
where
    <T::ResponseBody as Body>::Error: Send + Sync + 'static,
{
    pub fn new(
        config: RouterConfig,
        transport: Arc<T>,
        sinks: EventSinks,
    ) -> Result<Arc<Self>, String> {
        let router = Arc::new(Router::new(transport.clone(), config.clone()));
        Self::with_router(config, transport, router, sinks)
    }
}
impl<T: HttpTransport + 'static, R: GatewayRouter> Gateway<T, R>
where
    <T::ResponseBody as Body>::Error: Send + Sync + 'static,
{
    pub fn with_router(
        config: RouterConfig,
        transport: Arc<T>,
        router: Arc<R>,
        sinks: EventSinks,
    ) -> Result<Arc<Self>, String> {
        if config
            .local_token
            .as_ref()
            .is_none_or(|token| token.encode_utf16().count() < 16)
        {
            return Err("AUTOROUTER_TOKEN must contain at least 16 characters".into());
        }
        Ok(Arc::new(Self {
            config,
            transport,
            router,
            sinks,
            #[cfg(test)]
            intent_probe: None,
        }))
    }
    #[cfg(test)]
    pub(crate) fn with_test_intent(mut gateway: Arc<Self>, probe: IntentProbe) -> Arc<Self> {
        Arc::get_mut(&mut gateway)
            .expect("unique fixture gateway")
            .intent_probe = Some(probe);
        gateway
    }
    pub async fn listen(self: &Arc<Self>, port: u16) -> Result<GatewayHandle, String> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|_| "Could not listen on the local AutoRouter port.")?;
        let address = listener
            .local_addr()
            .map_err(|_| "Could not inspect the local AutoRouter port.")?;
        let cancellation = CancellationToken::new();
        let stopping = cancellation.clone();
        let gateway = self.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    biased;_=stopping.cancelled()=>break,
                    Some(_)=connections.join_next(),if !connections.is_empty()=>{},
                    incoming=listener.accept()=>{
                        let Ok((stream,_))=incoming else{break};let gateway=gateway.clone();let token=stopping.child_token();
                        connections.spawn(async move{
                            let registry=CompletionRegistry::new(16);let for_service=registry.clone();let request_token=token.clone();
                            #[cfg(test)]
                            let intent_registry = gateway.intent_probe.clone().map(|probe| IntentRegistry::new(probe, 16));
                            #[cfg(test)]
                            let stream = IntentIo::new(stream, intent_registry.clone());
                            let service=service_fn(move|request: Request<Incoming>|{
                                #[cfg(test)]
                                let request = { let mut request = request; if let Some(registry) = &intent_registry { request.extensions_mut().insert(registry.clone()); } request };
                                let gateway=gateway.clone();let registry=for_service.clone();let token=request_token.child_token();async move{if request.method() == Method::CONNECT { return Err(io::Error::new(io::ErrorKind::ConnectionAborted,"CONNECT is not supported")); } Ok::<_,io::Error>(gateway.handle(request,registry,token).await)}});
                            tokio::select!{biased;_=token.cancelled()=>{},_=serve_http1(stream,registry,service)=>{}}
                            token.cancel();
                        });
                    }
                }
            }
            drop(listener);
            stopping.cancel();
            gateway.router.shutdown();
            while connections.join_next().await.is_some() {}
        });
        Ok(GatewayHandle {
            address,
            cancellation,
            task: Some(task),
        })
    }
    async fn handle(
        self: Arc<Self>,
        request: Request<Incoming>,
        registry: CompletionRegistry,
        cancellation: CancellationToken,
    ) -> Response<GatewayBody> {
        let (parts, mut incoming) = request.into_parts();
        let receive_deadline = parts
            .extensions
            .get::<RequestReceiveDeadline>()
            .map(|receipt| receipt.deadline)
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(30));
        let headers = incoming_headers(&parts.headers);
        if header_text(&headers, "origin").is_some_and(|v| !v.is_empty()) {
            return json_error(403, "Browser requests are not supported");
        }
        if !authorized(&headers, &self.config) {
            return json_error(401, "Invalid local router credential");
        }
        let url = match url::Url::options()
            .base_url(Some(&url::Url::parse("http://127.0.0.1").unwrap()))
            .parse(&parts.uri.to_string())
        {
            Ok(url) => url,
            Err(_) => return json_error(502, "Router could not complete the upstream request"),
        };
        let path = url.path();
        let search = url
            .query()
            .filter(|q| !q.is_empty())
            .map(|q| format!("?{q}"))
            .unwrap_or_default();
        if parts.method == Method::GET && path == "/health" {
            return Response::builder()
                .header("content-type", "application/json")
                .body(full(b"{\"status\":\"ok\"}".as_slice()))
                .unwrap();
        }
        if parts.method == Method::HEAD && path == "/api/hello" {
            return Response::new(full(Bytes::new()));
        }
        let inference = parts.method == Method::POST && path == "/v1/messages";
        let count = parts.method == Method::POST && path == "/v1/messages/count_tokens";
        let models = parts.method == Method::GET
            && (path == "/v1/models"
                || path
                    .strip_prefix("/v1/models/")
                    .is_some_and(|s| !s.is_empty() && !s.contains('/')));
        if !inference && !count && !models {
            return json_error(404, "Unsupported endpoint");
        }
        let mut lease = None;
        let mut events = None;
        if inference {
            let id = match request_id() {
                Ok(id) => id,
                Err(()) => {
                    return json_error(502, "Router could not complete the upstream request");
                }
            };
            let mut context = json!({"request_id":id});
            for (to, from) in [
                ("session_id", "x-claude-code-session-id"),
                ("agent_id", "x-claude-code-agent-id"),
                ("prompt_id", "x-claude-code-prompt-id"),
                ("request_class", "x-claude-code-request-class"),
            ] {
                if let Some(value) = header_text(&headers, from) {
                    context[to] = json!(value);
                }
            }
            let state = Arc::new(RequestEvents::new(
                context,
                &self.config.models.opus,
                self.config.session_log_mode,
                self.sinks.clone(),
            ));
            state.status("request_start", json!({}));
            lease = Some(RequestLease {
                router: self.router.clone(),
                id,
                events: state.clone(),
                cancellation: cancellation.clone(),
                finished: false,
            });
            events = Some(state);
        }
        let reject = |status, message| {
            if let Some(events) = &events {
                events.status("request_error", json!({"status":status}));
            }
            json_error(status, message)
        };
        if self.config.auth_mode == AuthMode::Subscription
            && !is_subscription_request(
                header_text(&headers, "x-api-key").as_deref(),
                header_text(&headers, "authorization").as_deref(),
                header_text(&headers, "anthropic-beta").as_deref(),
            )
        {
            return reject(
                401,
                "Subscription mode requires Claude Code OAuth authentication and its OAuth beta header, without an API key. Sign in with claude auth login and remove conflicting API-key, auth-token, custom-header, or apiKeyHelper settings.",
            );
        }
        if header_text(&headers, "content-encoding")
            .is_some_and(|v| !v.is_empty() && v != "identity")
        {
            return reject(415, "Compressed request bodies are not supported");
        }
        if header_text(&headers, "content-length")
            .and_then(|v| v.parse::<f64>().ok())
            .is_some_and(|length| length > self.config.max_body_bytes as f64)
        {
            return reject(413, "Request body too large");
        }
        let upstream_headers = match upstream_headers(&headers, &self.config) {
            Ok(headers) => headers,
            Err(_) => return reject(502, "Router could not complete the upstream request"),
        };
        let mut body = Bytes::new();
        if inference || count {
            let reading = async {
                let mut bytes = Vec::new();
                while let Some(frame) = incoming.frame().await {
                    let frame = frame.map_err(|_| 502u16)?;
                    if let Ok(chunk) = frame.into_data() {
                        if chunk.len() > self.config.max_body_bytes.saturating_sub(bytes.len()) {
                            return Err(413);
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                    tokio::task::consume_budget().await;
                }
                Ok::<_, u16>(Bytes::from(bytes))
            };
            body = match tokio::select! {biased;_=cancellation.cancelled()=>Err(499),_=tokio::time::sleep_until(receive_deadline)=>Err(408),result=reading=>result}
            {
                Ok(bytes) => bytes,
                Err(499) => {
                    return json_error(502, "Router could not complete the upstream request");
                }
                Err(413) => {
                    emit(&self.sinks.log, json!({"event":"proxy_error","status":413}));
                    return reject(413, "Request body too large");
                }
                Err(code) => {
                    emit(
                        &self.sinks.log,
                        json!({"event":"proxy_error","status":code}),
                    );
                    return reject(code, "Router could not complete the upstream request");
                }
            };
            let document = match JsDocument::parse(&body) {
                Ok(document) => Arc::new(document),
                Err(_) => return reject(400, "Invalid JSON body"),
            };
            let shape = validate_request_document(&document);
            if shape["valid"] != true {
                emit(&self.sinks.log, invalid_shape_log(&document));
                return reject(
                    400,
                    shape["error"].as_str().unwrap_or("Invalid request shape"),
                );
            }
            if inference {
                let request_class =
                    header_text(&headers, "x-claude-code-request-class").unwrap_or_default();
                let document = self
                    .router
                    .transform_document_for_request(document, &request_class);
                let context = &events.as_ref().unwrap().context;
                let options = RouteOptions {
                    scope: json!([
                        header_text(&headers, "x-claude-code-session-id"),
                        header_text(&headers, "x-claude-code-agent-id")
                    ])
                    .to_string(),
                    request_class,
                    prompt_id: header_text(&headers, "x-claude-code-prompt-id").unwrap_or_default(),
                    request_id: context["request_id"].as_str().map(str::to_owned),
                    count_tokens: true,
                };
                let decision = match self
                    .router
                    .route_exact(
                        document.clone(),
                        options,
                        &upstream_headers,
                        &cancellation,
                        &search,
                    )
                    .await
                {
                    Ok(decision) => decision,
                    Err(EvaluationError::Cancelled) => {
                        return json_error(502, "Router could not complete the upstream request");
                    }
                    Err(_) => {
                        emit(&self.sinks.log, json!({"event":"proxy_error","status":502}));
                        return reject(502, "Router could not complete the upstream request");
                    }
                };
                if cancellation.is_cancelled() {
                    return json_error(502, "Router could not complete the upstream request");
                }
                let (prepared, adjustments) =
                    prepare_request_document_exact(&document, &decision.model);
                let exact_model = decision.model;
                let decision = decision.decision;
                body = Bytes::from(prepared.stringify());
                let state = events.as_ref().unwrap();
                state.decision_exact(&document, &decision, &exact_model);
                let requested = document
                    .get(document.root(), "model")
                    .and_then(|n| document.string(n))
                    .cloned()
                    .expect("validated request model");
                let mut log = decision.clone();
                log["event"] = json!("route");
                log["requested_model"] = json!(requested.to_well_formed());
                log["request_adjustments"] = json!(adjustments);
                state.log_document(event_document(
                    log,
                    &[("model", &exact_model), ("requested_model", &requested)],
                ));
                let mut fields = json!({});
                for name in [
                    "model",
                    "source",
                    "evaluator",
                    "reason",
                    "latency_ms",
                    "evaluation_latency_ms",
                    "classifier_error",
                    "classifier_status",
                    "classified_tier",
                    "context_check",
                    "counted_input_tokens",
                    "continuity_state",
                    "compatibility_reason",
                ] {
                    if let Some(value) = decision.get(name) {
                        fields[name] = value.clone();
                    }
                }
                fields["requested_model"] = json!(requested.to_well_formed());
                fields["pricing_context"] = pricing_context(&prepared);
                if let Some(latency) = fields.get("latency_ms").cloned() {
                    fields["routing_latency_ms"] = latency;
                }
                state.status_document(
                    "route",
                    event_document(
                        fields,
                        &[("model", &exact_model), ("requested_model", &requested)],
                    ),
                );
            }
        }
        if cancellation.is_cancelled() {
            return json_error(502, "Router could not complete the upstream request");
        }
        let target = match url::Url::parse(&format!("{}{path}{search}", self.config.upstream)) {
            Ok(target) => target,
            Err(_) => return reject(502, "Router could not complete the upstream request"),
        };
        let mut request = match Request::builder()
            .method(parts.method.clone())
            .uri(target.as_str())
            .body(Full::new(body.clone()))
        {
            Ok(request) => request,
            Err(_) => return reject(502, "Router could not complete the upstream request"),
        };
        *request.headers_mut() = upstream_headers;
        if inference || count {
            request.headers_mut().insert(
                "content-length",
                hyper::header::HeaderValue::from(body.len()),
            );
        }
        #[cfg(test)]
        let intent = match parts.extensions.get::<IntentRegistry>() {
            Some(registry) => match registry.register() {
                Ok(intent) => {
                    request.extensions_mut().insert(intent.clone());
                    Some(intent)
                }
                Err(()) => return reject(502, "Router could not complete the upstream request"),
            },
            None => None,
        };
        let deadline = Instant::now() + Duration::from_millis(self.config.upstream_timeout_ms);
        if let Some(events) = &events {
            events.forwarding();
        }
        let response_deadline = tokio::time::sleep_until(deadline);
        #[cfg(test)]
        let response_deadline = async {
            response_deadline.await;
            // select! drops losing futures before its branch body. The test
            // intent must claim while the request future still owns its lease.
            if let Some(intent) = &intent {
                intent.finish(IntentCause::Deadline);
            }
        };
        let response = match tokio::select! {
            biased;
            _=cancellation.cancelled()=>{
                #[cfg(test)]
                if let Some(intent) = &intent { intent.observe(IntentEvent::GenericCancellation); }
                Err(())
            },
            _=response_deadline=>Err(()),
            result=self.transport.request_raw(request)=>result.map_err(|_| {
                #[cfg(test)]
                if let Some(intent) = &intent { intent.finish(IntentCause::UpstreamFailure); }
            })
        } {
            Ok(response) => response,
            Err(()) => {
                emit(&self.sinks.log, json!({"event":"proxy_error","status":502}));
                return reject(502, "Router could not complete the upstream request");
            }
        };
        let status = response.status();
        emit(
            &self.sinks.log,
            json!({"event":"upstream_response","status":status.as_u16()}),
        );
        if let Some(events) = &events {
            events.status("upstream_response", json!({"status":status.as_u16()}));
        }
        let (mut parts, body) = response.into_parts();
        let observe = header_text(&parts.headers, "content-encoding")
            .is_none_or(|v| v.is_empty() || v == "identity");
        let content_type = if observe {
            header_text(&parts.headers, "content-type").unwrap_or_default()
        } else {
            String::new()
        };
        parts.headers = clean_headers(&parts.headers);
        let evidence = Arc::new(Mutex::new(None));
        let evidence_sink = evidence.clone();
        let events_sink = events.clone();
        let sinks = self.sinks.clone();
        let observer =
            ResponseObserver::new(&content_type, DEFAULT_BUFFER_BYTES, move |observation| {
                match observation {
                    Observation::Model { model } => {
                        emit_document(
                            &sinks.log,
                            event_document(json!({"event":"upstream_model"}), &[("model", &model)]),
                        );
                        if let Some(events) = &events_sink {
                            events.status_document(
                                "upstream_model",
                                event_document(json!({}), &[("model", &model)]),
                            );
                        }
                    }
                    Observation::Error { error_type } => {
                        emit(
                            &sinks.log,
                            json!({"event":"upstream_error","error_type":error_type}),
                        );
                        if let Some(events) = &events_sink {
                            events.status("upstream_error", json!({"error_type":error_type}));
                        }
                    }
                    Observation::Usage { usage } => {
                        if status.is_success()
                            && let Some(events) = &events_sink
                        {
                            events.status("upstream_usage", json!({"usage":usage}));
                        }
                    }
                    Observation::Complete(value) => {
                        *evidence_sink.lock().unwrap() = Some(value);
                    }
                    Observation::Execution { .. } => {}
                }
            })
            .expect("fixed observer bound");
        let forward = ForwardBody::new(
            ObservedBody::new(body, observer),
            deadline,
            cancellation,
            events,
            self.sinks.log.clone(),
        );
        #[cfg(test)]
        let forward = forward.with_intent(intent.clone());
        let response = Response::from_parts(parts, forward);
        if let Some(lease) = lease {
            match registry.track(response, move |delivery| {
                #[cfg(test)]
                if let Some(intent) = &intent {
                    if delivery == Delivery::Flushed {
                        intent.finish(IntentCause::Delivered);
                    } else {
                        intent.observe(IntentEvent::DeliveryFailed);
                    }
                }
                let evidence = evidence.lock().unwrap().take();
                lease.finish(delivery, evidence, status.is_success());
            }) {
                Ok(response) => response.map(boxed),
                Err(_) => json_error(502, "Router could not complete the upstream request"),
            }
        } else {
            #[cfg(test)]
            if let Some(intent) = intent {
                return match registry.track(response, move |delivery| {
                    if delivery == Delivery::Flushed {
                        intent.finish(IntentCause::Delivered);
                    } else {
                        intent.observe(IntentEvent::DeliveryFailed);
                    }
                }) {
                    Ok(response) => response.map(boxed),
                    Err(_) => json_error(502, "Router could not complete the upstream request"),
                };
            }
            response.map(boxed)
        }
    }
}

pub struct GatewayHandle {
    pub address: SocketAddr,
    cancellation: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl GatewayHandle {
    pub async fn close(mut self) {
        self.cancellation.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
impl Drop for GatewayHandle {
    fn drop(&mut self) {
        self.cancellation.cancel();
        // Let the owned listener task run its cancellation cleanup. Aborting
        // it here would skip router shutdown, leaving shared zero-deadline
        // evaluator work alive. Explicit close additionally joins that work.
    }
}

fn kind(document: &JsDocument, node: Option<NodeId>) -> &'static str {
    match node.and_then(|n| document.node(n)) {
        None => "undefined",
        Some(JsNode::Null | JsNode::Array(_) | JsNode::Object(_)) => "object",
        Some(JsNode::Bool(_)) => "boolean",
        Some(JsNode::Number(_)) => "number",
        Some(JsNode::String(_)) => "string",
    }
}
fn invalid_shape_log(document: &JsDocument) -> Value {
    let root = document.root();
    let messages = document.get(root, "messages");
    let array = messages.and_then(|n| document.node(n));
    let mut log = json!({"event":"invalid_request_shape","model_type":kind(document,document.get(root,"model")),"messages_type":if matches!(array,Some(JsNode::Array(_))){"array"}else{kind(document,messages)}});
    if let Some(JsNode::Array(messages)) = array {
        log["messages"]=json!(messages.iter().take(10).map(|node|{
        let role=document.get(*node,"role");let role_text=role.and_then(|n|document.string(n)).and_then(|s|s.to_scalar());let content=document.get(*node,"content");let blocks=content.and_then(|n|document.node(n));
        let mut row=json!({"role":role_text.filter(|s|["user","assistant","system"].contains(&s.as_str())).unwrap_or_else(||kind(document,role).into()),"content_type":if matches!(blocks,Some(JsNode::Array(_))){"array"}else{kind(document,content)}});
        if let Some(JsNode::Array(blocks))=blocks{row["blocks"]=json!(blocks.iter().take(10).map(|node|json!({"value_type":if matches!(document.node(*node),Some(JsNode::Null)){"null"}else{kind(document,Some(*node))},"type_type":kind(document,document.get(*node,"type"))})).collect::<Vec<_>>());}row
    }).collect::<Vec<_>>());
    }
    log
}
fn pricing_context(document: &JsDocument) -> Value {
    let root = document.root();
    let mut context = json!({});
    for (field, allowed, fallback) in [
        ("speed", ["standard", "fast"], "standard"),
        ("inference_geo", ["global", "us"], "global"),
        ("service_tier", ["auto", "standard_only"], "auto"),
    ] {
        context[field] = match document.get(root, field) {
            None => json!(fallback),
            Some(node) => document
                .string(node)
                .and_then(|s| s.to_scalar())
                .filter(|s| allowed.contains(&s.as_str()))
                .map_or_else(|| json!("unknown"), |s| json!(s)),
        };
    }
    let nonnull = |field| {
        document
            .get(root, field)
            .is_some_and(|id| !matches!(document.node(id), Some(JsNode::Null)))
    };
    let advisor = document
        .get(root, "tools")
        .and_then(|id| document.node(id))
        .is_some_and(|node| match node {
            JsNode::Array(tools) => tools.iter().any(|tool| {
                document
                    .get(*tool, "type")
                    .and_then(|id| document.string(id))
                    .is_some_and(|s| {
                        let units = s.units();
                        let prefix: Vec<u16> = "advisor".encode_utf16().collect();
                        units.starts_with(&prefix)
                            && (units.len() == prefix.len()
                                || units.get(prefix.len()) == Some(&u16::from(b'_')))
                    })
            }),
            _ => false,
        });
    if nonnull("fallbacks") || nonnull("fallback_credit_token") || advisor {
        context["pricing_unsupported"] = json!(true)
    }
    context
}

#[cfg(test)]
mod forwarding_tests {
    use super::*;
    use std::collections::VecDeque;

    struct Frames(VecDeque<Result<Frame<Bytes>, io::Error>>);
    impl Body for Frames {
        type Data = Bytes;
        type Error = io::Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
            Poll::Ready(self.0.pop_front())
        }
    }

    #[tokio::test]
    async fn upstream_trailers_are_consumed_and_do_not_hide_a_later_body_failure() {
        for fail_after_trailers in [false, true] {
            let mut trailers = HeaderMap::new();
            trailers.insert("x-synthetic-trailer", "value".parse().unwrap());
            let mut frames = VecDeque::from([
                Ok(Frame::data(Bytes::from_static(b"synthetic body"))),
                Ok(Frame::trailers(trailers)),
            ]);
            if fail_after_trailers {
                frames.push_back(Err(io::Error::other("synthetic failure")));
            }
            let mut body = ForwardBody::new(
                Frames(frames),
                Instant::now() + Duration::from_secs(1),
                CancellationToken::new(),
                None,
                None,
            );
            assert_eq!(
                body.frame().await.unwrap().unwrap().into_data().unwrap(),
                "synthetic body"
            );
            assert!(!body.is_end_stream());
            let next = tokio::time::timeout(Duration::from_secs(1), body.frame())
                .await
                .unwrap();
            if fail_after_trailers {
                assert_eq!(
                    next.unwrap().unwrap_err().to_string(),
                    "Upstream body failed"
                );
            } else {
                assert!(next.is_none());
            }
            assert!(body.is_end_stream());
        }
    }
}
