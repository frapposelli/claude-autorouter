//! Loopback gateway with request-scoped credentials and original provider bytes.
//! Completion requires both clean protocol evidence and the connection writer's
//! flush acknowledgement. Every abandoned attempt is explicitly released.
use crate::evaluator::EvaluationError;
use crate::http_client::{HttpError, HttpTransport};
use crate::response_observer::{
    CompletionEvidence, DEFAULT_BUFFER_BYTES, Observation, ObservedBody, ResponseObserver,
};
use crate::route_counter::RouteTokenCounter;
use crate::router::Router;
use crate::server_events::{
    EventSink, EventSinks, RequestEvents, emit, emit_document, event_document,
};
#[cfg(test)]
use crate::tls_roots::openssl_spike::gateway_intent::{
    Cause as IntentCause, Event as IntentEvent, Intent, IntentIo, Probe as IntentProbe,
    Registry as IntentRegistry,
};
#[cfg(test)]
use crate::tls_roots::openssl_spike::gateway_terminal::{
    ConnectionHandle, ConnectionTerminal, FailureCause, Probe as TerminalProbe, RequestTerminal,
    TerminalIo,
};
use crate::transport_completion::{
    CompletionRegistry, Delivery, Failure, RequestReceiveDeadline, serve_http1_scanned,
};
use autorouter_core::auth::{LOCAL_AUTH_HEADER, is_subscription_request};
use autorouter_core::config::{AuthMode, Credential, RouterConfig, js_trim};
use autorouter_core::contracts::{
    BlockShape, DiagnosticEvent, DiagnosticName, ExactModelFields, InvalidRequestShape,
    LifecycleName, MessageShape, PricingContext, RequestContext, RouteLog, TelemetryFields,
    TypedRouteDecision, UsageTelemetry,
};
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

/// Custom JSON adapters retain their historical raw observation boundary.
/// The production implementation supplies a closed typed contract instead.
pub enum GatewayDecision {
    Typed(TypedRouteDecision),
    Legacy(RouteDecision),
}
impl GatewayDecision {
    fn model(&self) -> &JsString {
        match self {
            Self::Typed(value) => &value.model,
            Self::Legacy(value) => &value.model,
        }
    }
}
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
    fn route_contract_exact(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> impl Future<Output = Result<GatewayDecision, EvaluationError>> + Send {
        async move {
            self.route_exact(document, options, headers, cancellation, search)
                .await
                .map(GatewayDecision::Legacy)
        }
    }
    /// Optional per-route count override. Existing adapters keep their prior
    /// contract; adapters that own counting can override this additive method.
    fn route_contract_exact_with_counter(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
        _counter: Option<RouteTokenCounter>,
    ) -> impl Future<Output = Result<GatewayDecision, EvaluationError>> + Send {
        self.route_contract_exact(document, options, headers, cancellation, search)
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
    /// Stop admitting work and await all internally spawned task destructors.
    /// Dropping this waiter must preserve the adapter's cleanup ownership.
    fn close(&self) -> impl Future<Output = ()> + Send;
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
    async fn route_contract_exact(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> Result<GatewayDecision, EvaluationError> {
        self.route_typed_exact(document, options, headers, cancellation, search)
            .await
            .map(GatewayDecision::Typed)
    }
    async fn route_contract_exact_with_counter(
        &self,
        document: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
        counter: Option<RouteTokenCounter>,
    ) -> Result<GatewayDecision, EvaluationError> {
        match counter {
            Some(counter) => {
                self.route_typed_exact_with_counter(
                    document,
                    options,
                    headers,
                    cancellation,
                    search,
                    counter,
                )
                .await
            }
            None => {
                self.route_typed_exact(document, options, headers, cancellation, search)
                    .await
            }
        }
        .map(GatewayDecision::Typed)
    }
    fn shutdown(&self) {
        self.shutdown();
    }
    async fn close(&self) {
        self.close().await;
    }
}
type GatewayBody = UnsyncBoxBody<Bytes, io::Error>;
#[derive(Clone)]
struct ScannedReceive;
#[derive(Clone)]
struct PreheaderFailure;
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
fn upstream_header_base(incoming: &HeaderMap, config: &RouterConfig) -> HeaderMap {
    let mut headers = clean_headers(incoming);
    headers.remove(LOCAL_AUTH_HEADER);
    headers.remove("cookie");
    headers.insert(
        "accept-encoding",
        hyper::header::HeaderValue::from_static("identity"),
    );
    headers.remove("x-api-key");
    if config.auth_mode != AuthMode::Subscription {
        headers.remove("authorization");
    }
    headers
}
pub fn upstream_headers(
    incoming: &HeaderMap,
    config: &RouterConfig,
) -> Result<HeaderMap, HttpError> {
    let mut headers = upstream_header_base(incoming, config);
    if config.auth_mode != AuthMode::Subscription {
        let values = config
            .credential_node_http_values(Credential::Anthropic)
            .map_err(|_| HttpError::InvalidRequest)?
            .ok_or(HttpError::InvalidRequest)?;
        for value in values {
            headers.append("x-api-key", js_header(&value)?);
        }
    }
    Ok(headers)
}
/// Fetch count credentials have a separate scalar conversion/whitespace rule.
/// Failure makes an advisory count unavailable; it must not reject routing.
pub fn count_headers(incoming: &HeaderMap, config: &RouterConfig) -> Result<HeaderMap, HttpError> {
    let mut headers = upstream_header_base(incoming, config);
    if config.auth_mode != AuthMode::Subscription {
        let value = config
            .credential_text(Credential::Anthropic)
            .map_err(|_| HttpError::InvalidRequest)?;
        headers.insert(
            "x-api-key",
            crate::http_client::fetch_header_value(value.as_deref().unwrap_or("undefined"))?,
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
        self.events.lifecycle(
            if delivered {
                LifecycleName::RequestComplete
            } else {
                LifecycleName::RequestCancelled
            },
            TelemetryFields::default(),
        );
        self.finished = true;
    }
}
impl<R: GatewayRouter> Drop for RequestLease<R> {
    fn drop(&mut self) {
        if !self.finished {
            self.cancellation.cancel();
            self.router.complete(&self.id, &Value::Null);
            self.events
                .lifecycle(LifecycleName::RequestCancelled, TelemetryFields::default());
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
    #[cfg(test)]
    terminal: Option<RequestTerminal>,
    prefix: Option<Frame<Bytes>>,
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
            #[cfg(test)]
            terminal: None,
            prefix: None,
        }
    }
    #[cfg(test)]
    fn with_terminal(mut self, terminal: Option<RequestTerminal>) -> Self {
        self.terminal = terminal;
        self
    }
    #[cfg(test)]
    fn with_intent(mut self, intent: Option<Arc<Intent>>) -> Self {
        self.intent = intent;
        self
    }
}
impl<B: Body<Data = Bytes>> ForwardBody<B> {
    async fn stage_first(&mut self) -> Result<(), io::Error> {
        let frame = std::future::poll_fn(|cx| {
            // Empty data is not a first write. Bound work for arbitrary fake
            // bodies; the isolated application buffer emits only nonempty data.
            for _ in 0..16 {
                match Pin::new(&mut *self).poll_frame(cx) {
                    Poll::Ready(Some(Ok(frame)))
                        if frame.data_ref().is_some_and(Bytes::is_empty) => {}
                    result => return result,
                }
            }
            cx.waker().wake_by_ref();
            Poll::Pending
        })
        .await;
        self.prefix = frame.transpose()?;
        Ok(())
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
        #[cfg(test)]
        if this.terminal.as_ref().is_some_and(RequestTerminal::failed) {
            this.finished = true;
            return Poll::Ready(Some(Err(io::Error::other("Upstream body failed"))));
        }
        if this.cancelled.as_mut().poll(cx).is_ready() {
            #[cfg(test)]
            if let Some(terminal) = &this.terminal {
                terminal.fail(FailureCause::Cancelled);
            }
            #[cfg(test)]
            if let Some(intent) = &this.intent {
                intent.observe(IntentEvent::GenericCancellation);
            }
            this.finished = true;
            if let Some(events) = &this.events {
                events.lifecycle(LifecycleName::RequestCancelled, TelemetryFields::default());
            }
            return Poll::Ready(Some(Err(io::Error::other("Request cancelled"))));
        }
        if this.deadline.as_mut().poll(cx).is_ready() {
            #[cfg(test)]
            if let Some(terminal) = &this.terminal {
                terminal.fail(FailureCause::Deadline);
                this.finished = true;
                return Poll::Ready(Some(Err(io::Error::other("Upstream deadline exceeded"))));
            }
            #[cfg(test)]
            if let Some(intent) = &this.intent {
                intent.finish(IntentCause::Deadline);
            }
            this.finished = true;
            emit(
                &this.log,
                DiagnosticEvent::status(DiagnosticName::ProxyError, 502).observation(),
            );
            if let Some(events) = &this.events {
                events.lifecycle(
                    LifecycleName::RequestError,
                    TelemetryFields {
                        status: Some(502),
                        ..TelemetryFields::default()
                    },
                );
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
        if let Some(frame) = this.prefix.take() {
            // Replay runs after the same terminal/cancellation/deadline checks,
            // without polling or observing the upstream frame a second time.
            return Poll::Ready(Some(Ok(frame)));
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
                if let Some(terminal) = &this.terminal {
                    terminal.fail(FailureCause::Upstream);
                    this.finished = true;
                    return Poll::Ready(Some(Err(io::Error::other("Upstream body failed"))));
                }
                #[cfg(test)]
                if let Some(intent) = &this.intent {
                    intent.finish(IntentCause::UpstreamFailure);
                }
                this.finished = true;
                emit(
                    &this.log,
                    DiagnosticEvent::status(DiagnosticName::ProxyError, 502).observation(),
                );
                if let Some(events) = &this.events {
                    events.lifecycle(
                        LifecycleName::RequestError,
                        TelemetryFields {
                            status: Some(502),
                            ..TelemetryFields::default()
                        },
                    );
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
    #[cfg(test)]
    terminal_probe: Option<TerminalProbe>,
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
            #[cfg(test)]
            terminal_probe: None,
        }))
    }
    #[cfg(test)]
    pub(crate) fn with_test_intent(mut gateway: Arc<Self>, probe: IntentProbe) -> Arc<Self> {
        Arc::get_mut(&mut gateway)
            .expect("unique fixture gateway")
            .intent_probe = Some(probe);
        gateway
    }
    #[cfg(test)]
    pub(crate) fn with_test_terminal(mut gateway: Arc<Self>, probe: TerminalProbe) -> Arc<Self> {
        Arc::get_mut(&mut gateway)
            .expect("unique fixture gateway")
            .terminal_probe = Some(probe);
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
        let scans = crate::http_receive::ScanRegistry::new();
        let first_scan = scans.next_scan();
        let task = tokio::spawn(async move {
            let scan_timer = tokio::time::sleep_until(first_scan);
            tokio::pin!(scan_timer);
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    biased;_=stopping.cancelled()=>break,
                    _=&mut scan_timer=>{
                        let started = Instant::now();
                        scans.scan_due(scan_timer.deadline(), started);
                        scan_timer.as_mut().reset(started + crate::http_receive::SCAN_INTERVAL);
                        tokio::task::yield_now().await;
                    },
                    Some(_)=connections.join_next(),if !connections.is_empty()=>{},
                    incoming=listener.accept()=>{
                        let Ok((stream,_))=incoming else{break};
                        let gateway=gateway.clone();let token=stopping.child_token();
                        let requests=token.child_token();let completion=CompletionRegistry::new(16);
                        let registration=scans.register(completion.clone(),requests.clone());
                        connections.spawn(gateway.serve_connection_with_receipt(
                            stream, completion, token, registration, requests,
                        ));
                    }
                }
            }
            drop(listener);
            stopping.cancel();
            gateway.router.close().await;
            while connections.join_next().await.is_some() {}
        });
        Ok(GatewayHandle {
            address,
            cancellation,
            task: Some(task),
        })
    }
    /// Synthetic developer probes reuse the ordinary connection owner, HTTP
    /// parser, authentication, body bounds and delivery acknowledgements. This
    /// entry is absent from ordinary shipping builds; it exposes no endpoint.
    #[cfg(feature = "development-probes")]
    pub async fn serve_development_connection<I>(
        self: Arc<Self>,
        stream: I,
        registry: CompletionRegistry,
        cancellation: CancellationToken,
    ) where
        I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        self.serve_connection(stream, registry, cancellation).await;
    }
    // The listener and deterministic writer regression use this same connection
    // owner. It creates no tasks beyond the listener-owned connection task.
    #[cfg(any(test, feature = "development-probes"))]
    async fn serve_connection<I>(
        self: Arc<Self>,
        stream: I,
        registry: CompletionRegistry,
        token: CancellationToken,
    ) where
        I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        // A standalone developer/test connection owns its scanner inside this
        // same future. Listening connections share the listener phase instead.
        let scans = crate::http_receive::ScanRegistry::new();
        let requests = token.child_token();
        let registration = scans.register(registry.clone(), requests.clone());
        let connection =
            self.serve_connection_with_receipt(stream, registry, token, registration, requests);
        tokio::pin!(connection);
        let mut next = scans.next_scan();
        loop {
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(next) => {
                    let started = Instant::now();
                    scans.scan_due(next, started);
                    next = started + crate::http_receive::SCAN_INTERVAL;
                    tokio::task::yield_now().await;
                },
                _ = &mut connection => break,
            }
        }
    }
    async fn serve_connection_with_receipt<I>(
        self: Arc<Self>,
        stream: I,
        registry: CompletionRegistry,
        token: CancellationToken,
        registration: crate::http_receive::Registration,
        request_token: CancellationToken,
    ) where
        I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let control = registration.control();
        let for_service = registry.clone();
        #[cfg(test)]
        let intent_registry = self
            .intent_probe
            .clone()
            .map(|probe| IntentRegistry::new(probe, 16));
        #[cfg(test)]
        let terminal_owner = self
            .terminal_probe
            .clone()
            .map(|probe| ConnectionTerminal::new(probe, 16));
        #[cfg(test)]
        let terminal_service = terminal_owner.as_ref().map(ConnectionTerminal::handle);
        // Observe real downstream IO before IntentIo performs
        // abort effects that can make the upstream body fail.
        #[cfg(test)]
        let stream = TerminalIo::new(stream, terminal_service.clone());
        #[cfg(test)]
        let stream = IntentIo::new(stream, intent_registry.clone());
        let gateway = self.clone();
        let service = service_fn(move |mut request: Request<Incoming>| {
            request.extensions_mut().insert(ScannedReceive);
            let timing = request
                .extensions()
                .get::<hyper::ext::NodeHttpResponseTiming>()
                .cloned();
            #[cfg(test)]
            let request = {
                let mut request = request;
                if let Some(registry) = &intent_registry {
                    request.extensions_mut().insert(registry.clone());
                }
                request
            };
            #[cfg(test)]
            let terminal_service = terminal_service.clone();
            let gateway = gateway.clone();
            let registry = for_service.clone();
            let token = request_token.child_token();
            async move {
                if request.method() == Method::CONNECT {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "CONNECT is not supported",
                    ));
                }
                #[cfg(test)]
                let request = {
                    let mut request = request;
                    if let Some(owner) = terminal_service {
                        request.extensions_mut().insert(owner);
                    }
                    request
                };
                let response = gateway.handle(request, registry, token).await;
                if response.extensions().get::<PreheaderFailure>().is_some() {
                    return Err(io::Error::other(
                        "Upstream body failed before its first write",
                    ));
                }
                // Local end/json responses send immediately. ForwardBody stages
                // a stream's first send separately, before this future resolves.
                if response.body().size_hint().exact().is_some()
                    && let Some(timing) = timing
                {
                    timing.mark_prepared();
                }
                Ok::<_, io::Error>(response)
            }
        });
        let deadline_expired = registry.deadline_expired();
        #[cfg(test)]
        let deadline_expired = async {
            // Keep opt-in experimental terminal ownership unchanged.
            if self.intent_probe.is_none() && self.terminal_probe.is_none() {
                deadline_expired.await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        #[cfg(test)]
        {
            let terminal_failure = async {
                match &terminal_owner {
                    Some(owner) => owner.claim_connection_failure().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {biased;_=terminal_failure=>{},_=token.cancelled()=>{},_=deadline_expired=>{},_=serve_http1_scanned(stream,registry.clone(),service,control)=>{}}
        }
        #[cfg(not(test))]
        tokio::select! {biased;_=token.cancelled()=>{},_=deadline_expired=>{},_=serve_http1_scanned(stream,registry.clone(),service,control)=>{}}
        token.cancel();
        #[cfg(test)]
        if let Some(owner) = terminal_owner {
            owner.close().await;
        }
    }
    async fn handle(
        self: Arc<Self>,
        request: Request<Incoming>,
        registry: CompletionRegistry,
        cancellation: CancellationToken,
    ) -> Response<GatewayBody> {
        let (parts, mut incoming) = request.into_parts();
        #[cfg(test)]
        let terminal_owner = match parts.extensions.get::<ConnectionHandle>() {
            Some(owner) => match owner.register() {
                Ok(owner) => Some(owner),
                Err(()) => {
                    return json_error(502, "Router could not complete the upstream request");
                }
            },
            None => None,
        };
        #[cfg(test)]
        let terminal = terminal_owner.as_ref().map(|owner| owner.publisher());
        let timing = parts
            .extensions
            .get::<hyper::ext::NodeHttpResponseTiming>()
            .cloned();
        let receive_deadline = if parts.extensions.get::<ScannedReceive>().is_some() {
            None
        } else {
            Some(
                parts
                    .extensions
                    .get::<RequestReceiveDeadline>()
                    .map(|bound| bound.deadline)
                    .unwrap_or_else(|| Instant::now() + Duration::from_secs(30)),
            )
        };
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
            let context = RequestContext {
                request_id: id.clone(),
                session_id: header_text(&headers, "x-claude-code-session-id"),
                agent_id: header_text(&headers, "x-claude-code-agent-id"),
                prompt_id: header_text(&headers, "x-claude-code-prompt-id"),
                request_class: header_text(&headers, "x-claude-code-request-class"),
            };
            let state = Arc::new(RequestEvents::new_typed(
                context,
                &self.config.models.opus,
                self.config.session_log_mode,
                self.sinks.clone(),
            ));
            state.lifecycle(LifecycleName::RequestStart, TelemetryFields::default());
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
                events.lifecycle(
                    LifecycleName::RequestError,
                    TelemetryFields {
                        status: Some(status),
                        ..TelemetryFields::default()
                    },
                );
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
            let receive_expiry = async {
                if let Some(deadline) = receive_deadline {
                    tokio::time::sleep_until(deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            body = match tokio::select! {biased;_=cancellation.cancelled()=>Err(499),_=receive_expiry=>Err(408),result=reading=>result}
            {
                Ok(bytes) => bytes,
                Err(499) => {
                    return json_error(502, "Router could not complete the upstream request");
                }
                Err(413) => {
                    emit(
                        &self.sinks.log,
                        DiagnosticEvent::status(DiagnosticName::ProxyError, 413).observation(),
                    );
                    return reject(413, "Request body too large");
                }
                Err(code) => {
                    emit(
                        &self.sinks.log,
                        DiagnosticEvent::status(DiagnosticName::ProxyError, code).observation(),
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
                let (count_headers, counter) = match count_headers(&headers, &self.config) {
                    Ok(headers) => (headers, None),
                    Err(_) => (
                        HeaderMap::new(),
                        Some(RouteTokenCounter::new(|_, _, _| async { None })),
                    ),
                };
                let decision = match self
                    .router
                    .route_contract_exact_with_counter(
                        document.clone(),
                        options,
                        &count_headers,
                        &cancellation,
                        &search,
                        counter,
                    )
                    .await
                {
                    Ok(decision) => decision,
                    Err(EvaluationError::Cancelled) => {
                        return json_error(502, "Router could not complete the upstream request");
                    }
                    Err(_) => {
                        emit(
                            &self.sinks.log,
                            DiagnosticEvent::status(DiagnosticName::ProxyError, 502).observation(),
                        );
                        return reject(502, "Router could not complete the upstream request");
                    }
                };
                if cancellation.is_cancelled() {
                    return json_error(502, "Router could not complete the upstream request");
                }
                let (prepared, adjustments) =
                    prepare_request_document_exact(&document, decision.model());
                let exact_model = decision.model().clone();
                body = Bytes::from(prepared.stringify());
                let state = events.as_ref().unwrap();
                let requested = document
                    .get(document.root(), "model")
                    .and_then(|n| document.string(n))
                    .cloned()
                    .expect("validated request model");
                match decision {
                    GatewayDecision::Typed(decision) => {
                        state.decision_typed(&document, &decision);
                        let requested_display = requested.to_well_formed();
                        let log = serde_json::to_value(RouteLog {
                            decision: &decision.metadata,
                            event: LifecycleName::Route,
                            requested_model: &requested_display,
                            request_adjustments: &adjustments,
                        })
                        .expect("bounded route log contract");
                        state.log_document(event_document(
                            log,
                            &ExactModelFields {
                                model: Some(&exact_model),
                                requested_model: Some(&requested),
                                selected_model: None,
                            }
                            .overrides(),
                        ));
                        state.lifecycle_exact(
                            LifecycleName::Route,
                            TelemetryFields::route(
                                &decision.metadata,
                                &requested,
                                pricing_context(&prepared),
                            ),
                            ExactModelFields {
                                model: Some(&exact_model),
                                requested_model: Some(&requested),
                                selected_model: None,
                            },
                        );
                    }
                    GatewayDecision::Legacy(decision) => {
                        state.use_legacy_observations();
                        let decision = decision.decision;
                        state.decision_exact(&document, &decision, &exact_model);
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
                        fields["pricing_context"] =
                            serde_json::to_value(pricing_context(&prepared))
                                .expect("pricing contract");
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
            }
        }
        if cancellation.is_cancelled() {
            return json_error(502, "Router could not complete the upstream request");
        }
        // Match source forward: reject Node HTTP credentials only after body
        // validation and inference routing/preparation have completed.
        let upstream_headers = match upstream_headers(&headers, &self.config) {
            Ok(headers) => headers,
            Err(_) => return reject(502, "Router could not complete the upstream request"),
        };
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
        #[cfg(test)]
        if let Some(terminal) = &terminal {
            request.extensions_mut().insert(terminal.clone());
            if let Some(intent) = &intent {
                intent.bind_terminal(terminal);
            }
            let terminal_events = events.clone();
            let terminal_log = self.sinks.log.clone();
            let terminal_intent = intent.clone();
            terminal_owner
                .as_ref()
                .expect("terminal admission owner")
                .start(
                    deadline,
                    cancellation.clone(),
                    move |cause| {
                        if let Some(intent) = &terminal_intent {
                            match cause {
                                FailureCause::Upstream => {
                                    intent.finish(IntentCause::UpstreamFailure)
                                }
                                FailureCause::Deadline => intent.finish(IntentCause::Deadline),
                                FailureCause::Downstream(cause) => {
                                    intent.finish(IntentCause::Downstream(cause))
                                }
                                FailureCause::Cancelled => {
                                    intent.observe(IntentEvent::GenericCancellation)
                                }
                            }
                        }
                    },
                    move |cause| {
                        if matches!(cause, FailureCause::Upstream | FailureCause::Deadline) {
                            // A body failure closes the response; the frozen
                            // Node outer catch suppresses proxy_error after that
                            // downstream close. It is not a new pre-header 502.
                            if cause == FailureCause::Deadline {
                                emit(
                                    &terminal_log,
                                    DiagnosticEvent::status(DiagnosticName::ProxyError, 502)
                                        .observation(),
                                );
                            }
                            if let Some(events) = &terminal_events {
                                events.lifecycle(
                                    LifecycleName::RequestError,
                                    TelemetryFields {
                                        status: Some(502),
                                        ..TelemetryFields::default()
                                    },
                                );
                            }
                        } else if let Some(events) = &terminal_events {
                            events.lifecycle(
                                LifecycleName::RequestCancelled,
                                TelemetryFields::default(),
                            );
                        }
                    },
                );
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
                emit(
                    &self.sinks.log,
                    DiagnosticEvent::status(DiagnosticName::ProxyError, 502).observation(),
                );
                return reject(502, "Router could not complete the upstream request");
            }
        };
        let status = response.status();
        emit(
            &self.sinks.log,
            DiagnosticEvent::status(DiagnosticName::UpstreamResponse, status.as_u16())
                .observation(),
        );
        if let Some(events) = &events {
            events.lifecycle(
                LifecycleName::UpstreamResponse,
                TelemetryFields {
                    status: Some(status.as_u16()),
                    ..TelemetryFields::default()
                },
            );
        }
        #[cfg(test)]
        if let Some(owner) = &terminal_owner {
            // Acquired headers remain observable even if the independent body
            // producer latched an error before the response could attach.
            owner.attach();
        }
        let bodyless_method = parts.method == Method::HEAD;
        let (mut parts, body) = response.into_parts();
        let observe = header_text(&parts.headers, "content-encoding")
            .is_none_or(|v| v.is_empty() || v == "identity");
        let content_type = if observe {
            header_text(&parts.headers, "content-type").unwrap_or_default()
        } else {
            String::new()
        };
        parts.headers = clean_headers(&parts.headers);
        #[cfg(test)]
        if let Some(terminal) = &terminal {
            debug_assert!(!parts.headers.contains_key(hyper::header::CONTENT_LENGTH));
            parts.extensions.insert(terminal.handoff_observer());
        }
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
                            event_document(
                                DiagnosticEvent {
                                    event: DiagnosticName::UpstreamModel,
                                    status: None,
                                    model: Some(model.to_well_formed()),
                                    error_type: None,
                                }
                                .observation(),
                                &ExactModelFields {
                                    model: Some(&model),
                                    ..ExactModelFields::default()
                                }
                                .overrides(),
                            ),
                        );
                        if let Some(events) = &events_sink {
                            events.lifecycle_exact(
                                LifecycleName::UpstreamModel,
                                TelemetryFields {
                                    model: Some(model.to_well_formed()),
                                    ..TelemetryFields::default()
                                },
                                ExactModelFields {
                                    model: Some(&model),
                                    ..ExactModelFields::default()
                                },
                            );
                        }
                    }
                    Observation::Error { error_type } => {
                        emit(
                            &sinks.log,
                            DiagnosticEvent {
                                event: DiagnosticName::UpstreamError,
                                status: None,
                                model: None,
                                error_type: Some(error_type.into()),
                            }
                            .observation(),
                        );
                        if let Some(events) = &events_sink {
                            events.lifecycle(
                                LifecycleName::UpstreamError,
                                TelemetryFields {
                                    error_type: Some(error_type.into()),
                                    ..TelemetryFields::default()
                                },
                            );
                        }
                    }
                    Observation::Usage { usage } => {
                        if status.is_success()
                            && let Some(events) = &events_sink
                        {
                            events.lifecycle(
                                LifecycleName::UpstreamUsage,
                                TelemetryFields {
                                    usage: Some(UsageTelemetry::from_observation(Value::Object(
                                        usage,
                                    ))),
                                    ..TelemetryFields::default()
                                },
                            );
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
        let forward = forward
            .with_intent(intent.clone())
            .with_terminal(terminal.clone());
        let mut forward = forward;
        if let Some(timing) = &timing {
            let bodyless = matches!(parts.status.as_u16(), 204 | 304) || bodyless_method;
            let staged = if bodyless {
                std::future::poll_fn(|cx| {
                    for _ in 0..16 {
                        match Pin::new(&mut forward).poll_frame(cx) {
                            Poll::Ready(Some(Ok(_))) => {}
                            Poll::Ready(Some(Err(error))) => return Poll::Ready(Err(error)),
                            Poll::Ready(None) => return Poll::Ready(Ok(())),
                            Poll::Pending => return Poll::Pending,
                        }
                    }
                    cx.waker().wake_by_ref();
                    Poll::Pending
                })
                .await
            } else {
                forward.stage_first().await
            };
            if staged.is_err() {
                let mut failed = Response::new(full(Bytes::new()));
                failed.extensions_mut().insert(PreheaderFailure);
                return failed;
            }
            timing.mark_prepared();
        }
        #[cfg(test)]
        if terminal.is_some() && timing.is_none() && forward.stage_first().await.is_err() {
            // Headers were acquired and status recorded. A failure before the
            // first write remains a connection-owned close, not a new 502 head.
            // The retained request owner retires when serve_http1 is destroyed.
            debug_assert!(terminal.as_ref().is_some_and(RequestTerminal::failed));
            std::future::pending::<()>().await;
        }
        let response = Response::from_parts(parts, forward);
        let delivery_deadline = Some(deadline);
        #[cfg(test)]
        let delivery_deadline = if self.intent_probe.is_some() || self.terminal_probe.is_some() {
            None
        } else {
            delivery_deadline
        };
        let deadline_log = self.sinks.log.clone();
        if let Some(lease) = lease {
            match registry.track_with_deadline(response, delivery_deadline, move |delivery| {
                #[cfg(test)]
                let mut delivery_claim = terminal_owner.map(|owner| owner.claim_delivery(delivery));
                #[cfg(test)]
                let delivery = delivery_claim
                    .as_ref()
                    .map_or(delivery, |claim| claim.delivery());
                #[cfg(test)]
                if let Some(intent) = &intent {
                    if delivery == Delivery::Flushed {
                        intent.finish(IntentCause::Delivered);
                    } else {
                        intent.observe(IntentEvent::DeliveryFailed);
                    }
                }
                if delivery == Delivery::Failed(Failure::Deadline) {
                    emit(
                        &deadline_log,
                        DiagnosticEvent::status(DiagnosticName::ProxyError, 502).observation(),
                    );
                    lease.events.lifecycle(
                        LifecycleName::RequestError,
                        TelemetryFields {
                            status: Some(502),
                            ..TelemetryFields::default()
                        },
                    );
                }
                let evidence = evidence.lock().unwrap().take();
                lease.finish(delivery, evidence, status.is_success());
                #[cfg(test)]
                if let Some(claim) = &mut delivery_claim {
                    claim.callback_finished();
                }
            }) {
                Ok(response) => response.map(boxed),
                Err(_) => json_error(502, "Router could not complete the upstream request"),
            }
        } else {
            #[cfg(test)]
            if let Some(intent) = intent {
                return match registry.track(response, move |delivery| {
                    let mut delivery_claim =
                        terminal_owner.map(|owner| owner.claim_delivery(delivery));
                    let delivery = delivery_claim
                        .as_ref()
                        .map_or(delivery, |claim| claim.delivery());
                    if delivery == Delivery::Flushed {
                        intent.finish(IntentCause::Delivered);
                    } else {
                        intent.observe(IntentEvent::DeliveryFailed);
                    }
                    if let Some(claim) = &mut delivery_claim {
                        claim.callback_finished();
                    }
                }) {
                    Ok(response) => response.map(boxed),
                    Err(_) => json_error(502, "Router could not complete the upstream request"),
                };
            }
            #[cfg(test)]
            if let Some(owner) = terminal_owner {
                return match registry.track(response, move |delivery| {
                    let mut claim = owner.claim_delivery(delivery);
                    claim.callback_finished();
                }) {
                    Ok(response) => response.map(boxed),
                    Err(_) => json_error(502, "Router could not complete the upstream request"),
                };
            }
            match registry.track_with_deadline(response, delivery_deadline, move |delivery| {
                if delivery == Delivery::Failed(Failure::Deadline) {
                    emit(
                        &deadline_log,
                        DiagnosticEvent::status(DiagnosticName::ProxyError, 502).observation(),
                    );
                }
            }) {
                Ok(response) => response.map(boxed),
                Err(_) => json_error(502, "Router could not complete the upstream request"),
            }
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
    let array = messages.and_then(|node| document.node(node));
    let rows = match array {
        Some(JsNode::Array(messages)) => Some(
            messages
                .iter()
                .take(10)
                .map(|node| {
                    let role = document.get(*node, "role");
                    let role_text = role
                        .and_then(|node| document.string(node))
                        .and_then(|s| s.to_scalar());
                    let content = document.get(*node, "content");
                    let blocks = content.and_then(|node| document.node(node));
                    MessageShape {
                        role: role_text
                            .filter(|s| ["user", "assistant", "system"].contains(&s.as_str()))
                            .unwrap_or_else(|| kind(document, role).into()),
                        content_type: if matches!(blocks, Some(JsNode::Array(_))) {
                            "array"
                        } else {
                            kind(document, content)
                        },
                        blocks: match blocks {
                            Some(JsNode::Array(blocks)) => Some(
                                blocks
                                    .iter()
                                    .take(10)
                                    .map(|node| BlockShape {
                                        value_type: if matches!(
                                            document.node(*node),
                                            Some(JsNode::Null)
                                        ) {
                                            "null"
                                        } else {
                                            kind(document, Some(*node))
                                        },
                                        type_type: kind(document, document.get(*node, "type")),
                                    })
                                    .collect(),
                            ),
                            _ => None,
                        },
                    }
                })
                .collect(),
        ),
        _ => None,
    };
    serde_json::to_value(InvalidRequestShape {
        event: DiagnosticName::InvalidRequestShape,
        model_type: kind(document, document.get(root, "model")),
        messages_type: if matches!(array, Some(JsNode::Array(_))) {
            "array"
        } else {
            kind(document, messages)
        },
        messages: rows,
    })
    .expect("bounded shape contract")
}

fn pricing_context(document: &JsDocument) -> PricingContext {
    let root = document.root();
    let field = |name, allowed: &[&str], fallback: &str| match document.get(root, name) {
        None => fallback.to_owned(),
        Some(node) => document
            .string(node)
            .and_then(|s| s.to_scalar())
            .filter(|s| allowed.contains(&s.as_str()))
            .unwrap_or_else(|| "unknown".into()),
    };
    let mut context = PricingContext {
        speed: field("speed", &["standard", "fast"], "standard"),
        inference_geo: field("inference_geo", &["global", "us"], "global"),
        service_tier: field("service_tier", &["auto", "standard_only"], "auto"),
        pricing_unsupported: None,
    };
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
        context.pricing_unsupported = Some(true)
    }
    context
}

#[cfg(test)]
mod forwarding_tests {
    use super::*;
    use std::collections::VecDeque;

    pub(super) fn credential_config(raw: Option<&str>) -> RouterConfig {
        let member = raw
            .map(|raw| format!(",\"ANTHROPIC_API_KEY\":{raw}"))
            .unwrap_or_default();
        let document = JsDocument::parse(format!(
            "{{\"AUTOROUTER_AUTH_MODE\":\"api-key\",\"AUTOROUTER_TOKEN\":\"synthetic-local-token\"{member}}}"
        ).as_bytes()).unwrap();
        crate::test_config_document(&document)
    }

    #[test]
    fn node_http_credentials_preserve_absence_null_and_repeated_fields() {
        let mut actual = Vec::new();
        let cases = [
            None,
            Some("null"),
            Some("[]"),
            Some(r#"["synthetic-a","synthetic-b"]"#),
        ];
        for raw in cases {
            let config = credential_config(raw);
            let before = serde_json::to_string(&config).unwrap();
            let mut incoming = HeaderMap::new();
            incoming.insert("x-api-key", "synthetic-local-token".parse().unwrap());
            incoming.insert(
                "authorization",
                "Bearer synthetic-local-token".parse().unwrap(),
            );
            let result = upstream_headers(&incoming, &config).map(|headers| {
                assert!(!headers.contains_key("authorization"));
                headers
                    .get_all("x-api-key")
                    .iter()
                    .map(|v| v.as_bytes().to_vec())
                    .collect::<Vec<_>>()
            });
            assert_eq!(serde_json::to_string(&config).unwrap(), before);
            actual.push(result);
        }
        assert_eq!(
            actual,
            vec![
                Err(HttpError::InvalidRequest),
                Ok(vec![b"null".to_vec()]),
                Ok(vec![]),
                Ok(vec![b"synthetic-a".to_vec(), b"synthetic-b".to_vec()]),
            ]
        );
    }

    // The connection writer proves that a final HTTP/1 framing write can fail
    // after a prior pipelined response was flushed. Exercise the request lease
    // against the real core task state for both resulting delivery verdicts.
    struct ContinuityRouter {
        turns: Mutex<autorouter_core::turn_state::TurnState>,
        exact_completions: std::sync::atomic::AtomicUsize,
    }
    impl GatewayRouter for ContinuityRouter {
        async fn route(
            &self,
            _: Arc<JsDocument>,
            _: RouteOptions,
            _: &HeaderMap,
            _: &CancellationToken,
            _: &str,
        ) -> Result<Value, EvaluationError> {
            unreachable!("the request is already staged")
        }
        fn complete(&self, id: &str, evidence: &Value) -> bool {
            self.turns
                .lock()
                .unwrap()
                .complete(id, (!evidence.is_null()).then_some(evidence), 2)
        }
        fn complete_exact(&self, id: &str, evidence: &CompletionEvidence) -> bool {
            use autorouter_core::turn_state::{ContinuationEvidence, ToolIdentity};
            self.exact_completions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.turns.lock().unwrap().complete_exact(
                id,
                Some(&ContinuationEvidence {
                    model: evidence
                        .continuation_model
                        .clone()
                        .unwrap_or_else(|| evidence.model.clone()),
                    tools: evidence
                        .tool_uses
                        .iter()
                        .map(|tool| ToolIdentity {
                            id: tool.id.as_str().into(),
                            model: tool.model.clone(),
                        })
                        .collect(),
                }),
                2,
            )
        }
        fn shutdown(&self) {}
        async fn close(&self) {}
    }

    #[test]
    fn failed_final_delivery_cannot_replace_confirmed_tool_owner() {
        use crate::response_observer::ToolUse;
        use crate::server_events::RecordSink;
        use autorouter_core::config::SessionLogMode;
        use autorouter_core::turn_state::{
            ContinuationEvidence, Pin as TaskPin, Selection, ToolIdentity, ToolOwner, TurnState,
        };
        use std::sync::atomic::Ordering;

        for (delivery, confirmed) in [
            (Delivery::Failed(Failure::Io), false),
            (Delivery::Flushed, true),
        ] {
            let mut turns = TurnState::default();
            let key = ["synthetic-task".to_owned()];
            assert!(turns.select_exact(
                &key,
                TaskPin::new("claude-opus-5-5".into(), "claude-haiku-4-5-20251001".into()),
                Selection {
                    scope: "synthetic-session",
                    request_id: Some("first"),
                    sequence: 1,
                },
                0,
            ));
            assert!(turns.complete_exact(
                "first",
                Some(&ContinuationEvidence {
                    model: "claude-opus-5-5".into(),
                    tools: vec![ToolIdentity {
                        id: "original-tool".into(),
                        model: "claude-opus-5-5".into(),
                    }],
                }),
                1,
            ));
            assert!(turns.select_exact(
                &key,
                TaskPin::new("claude-sonnet-5".into(), "claude-opus-5-5".into()),
                Selection {
                    scope: "synthetic-session",
                    request_id: Some("second"),
                    sequence: 2,
                },
                1,
            ));
            let router = Arc::new(ContinuityRouter {
                turns: Mutex::new(turns),
                exact_completions: std::sync::atomic::AtomicUsize::new(0),
            });
            let records = Arc::new(Mutex::new(Vec::<Value>::new()));
            let captured = records.clone();
            let record: RecordSink = Arc::new(move |value| {
                captured.lock().unwrap().push(value);
            });
            let events = Arc::new(RequestEvents::new_typed(
                RequestContext {
                    request_id: "second".into(),
                    session_id: Some("synthetic-session".into()),
                    agent_id: None,
                    prompt_id: Some("synthetic-task".into()),
                    request_class: None,
                },
                "claude-opus-5-5",
                SessionLogMode::Metadata,
                EventSinks {
                    record: Some(record),
                    ..EventSinks::default()
                },
            ));
            events.lifecycle(LifecycleName::RequestStart, TelemetryFields::default());
            events.lifecycle(
                LifecycleName::Route,
                TelemetryFields {
                    model: Some("claude-sonnet-5".into()),
                    ..TelemetryFields::default()
                },
            );
            events.forwarding();
            let lease = RequestLease {
                router: router.clone(),
                id: "second".into(),
                events,
                cancellation: CancellationToken::new(),
                finished: false,
            };
            lease.finish(
                delivery,
                Some(CompletionEvidence {
                    model: "claude-sonnet-5".into(),
                    continuation_model: Some("claude-sonnet-5".into()),
                    stop_reason: "tool_use".into(),
                    tool_uses: vec![ToolUse {
                        id: "replacement-tool".into(),
                        model: "claude-sonnet-5".into(),
                    }],
                }),
                true,
            );
            assert_eq!(
                router.exact_completions.load(Ordering::SeqCst),
                usize::from(confirmed)
            );
            let mut turns = router.turns.lock().unwrap();
            assert_eq!(turns.attempt_count(), 0, "failed attempts must be released");
            let original = turns.tool_owner_exact("synthetic-session", &["original-tool".into()]);
            let replacement =
                turns.tool_owner_exact("synthetic-session", &["replacement-tool".into()]);
            if confirmed {
                assert!(original.is_none());
            } else {
                assert!(replacement.is_none());
            }
            // The core router reads this exact tool-owner surface for the next
            // tool_result turn. Stage that continuation with the surviving
            // owner's model, then verify its visible pin before forwarding.
            let owner = if confirmed { replacement } else { original };
            let (key, selected) = match owner {
                Some(ToolOwner::Owned { key, pin }) => (key, pin.model),
                _ => panic!("the next tool continuation lost its confirmed owner"),
            };
            assert_eq!(
                selected,
                if confirmed {
                    JsString::from_scalar("claude-sonnet-5")
                } else {
                    JsString::from_scalar("claude-opus-5-5")
                },
            );
            assert!(turns.select_exact(
                std::slice::from_ref(&key),
                TaskPin::new(selected.clone(), selected.clone()),
                Selection {
                    scope: "synthetic-session",
                    request_id: Some("third"),
                    sequence: 3,
                },
                3,
            ));
            assert_eq!(turns.get_exact(&key, 3).unwrap().model, selected);
            assert_eq!(turns.attempt_count(), 1);
            assert!(!turns.complete_exact("third", None, 3));
            assert_eq!(turns.attempt_count(), 0);
            let records = records.lock().unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0]["completion_confirmed"], confirmed);
            assert_eq!(
                records[0]["status"],
                if confirmed { "completed" } else { "cancelled" },
            );
        }
    }

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

    struct CountedFrames {
        frames: VecDeque<Result<Frame<Bytes>, io::Error>>,
        polls: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl Body for CountedFrames {
        type Data = Bytes;
        type Error = io::Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
            self.polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Poll::Ready(self.frames.pop_front())
        }
    }
    #[tokio::test]
    async fn staged_prefix_rechecks_every_failure_without_another_inner_poll() {
        use crate::tls_roots::openssl_spike::gateway_terminal::{ConnectionTerminal, Probe};
        use std::sync::atomic::{AtomicUsize, Ordering};
        for cause in [
            FailureCause::Upstream,
            FailureCause::Cancelled,
            FailureCause::Deadline,
        ] {
            let terminal_probe = Probe::default();
            let connection = ConnectionTerminal::new(terminal_probe.clone(), 1);
            let owner = connection.handle().register().unwrap();
            owner.attach();
            let publisher = owner.publisher();
            let token = CancellationToken::new();
            let polls = Arc::new(AtomicUsize::new(0));
            let mut body = ForwardBody::new(
                CountedFrames {
                    frames: VecDeque::from([Ok(Frame::data(Bytes::from_static(b"prefix")))]),
                    polls: polls.clone(),
                },
                Instant::now() + Duration::from_secs(30),
                token.clone(),
                None,
                None,
            )
            .with_terminal(Some(publisher.clone()));
            body.stage_first().await.unwrap();
            assert_eq!(polls.load(Ordering::SeqCst), 1);
            match cause {
                FailureCause::Upstream => {
                    publisher.fail(cause);
                }
                FailureCause::Cancelled => token.cancel(),
                FailureCause::Deadline => {
                    body.deadline.as_mut().reset(Instant::now());
                    // A reset schedules the timer; first observe it ready before
                    // claiming that replay is racing an elapsed deadline.
                    body.deadline.as_mut().await;
                }
                _ => unreachable!(),
            }
            let frame = crate::bounded(body.frame()).await;
            assert!(frame.unwrap().is_err(), "staged replay after {cause:?}");
            assert!(publisher.failed());
            assert_eq!(polls.load(Ordering::SeqCst), 1, "no second observation");
            drop((body, owner));
            connection.close().await;
            assert_eq!(terminal_probe.tasks(), 0);
        }
    }
    #[tokio::test]
    async fn staged_prefix_replays_once_and_empty_frames_do_not_submit_a_head() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let polls = Arc::new(AtomicUsize::new(0));
        let mut frames = VecDeque::new();
        for _ in 0..33 {
            frames.push_back(Ok(Frame::data(Bytes::new())));
        }
        frames.push_back(Ok(Frame::trailers(HeaderMap::new())));
        frames.push_back(Ok(Frame::data(Bytes::from_static(b"first"))));
        frames.push_back(Ok(Frame::data(Bytes::from_static(b"second"))));
        let mut body = ForwardBody::new(
            CountedFrames {
                frames,
                polls: polls.clone(),
            },
            Instant::now() + Duration::from_secs(30),
            CancellationToken::new(),
            None,
            None,
        );
        body.stage_first().await.unwrap();
        assert_eq!(polls.load(Ordering::SeqCst), 35);
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            "first"
        );
        assert_eq!(polls.load(Ordering::SeqCst), 35);
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            "second"
        );
        assert!(body.frame().await.is_none());
    }
    #[tokio::test]
    async fn staged_clean_empty_eof_and_preframe_cancel_keep_distinct_outcomes() {
        use crate::tls_roots::openssl_spike::gateway_terminal::{ConnectionTerminal, Probe};
        let mut body = ForwardBody::new(
            Frames(VecDeque::new()),
            Instant::now() + Duration::from_secs(30),
            CancellationToken::new(),
            None,
            None,
        );
        body.stage_first().await.unwrap();
        assert!(body.is_end_stream());
        assert!(body.frame().await.is_none());
        let connection = ConnectionTerminal::new(Probe::default(), 1);
        let owner = connection.handle().register().unwrap();
        owner.attach();
        let publisher = owner.publisher();
        let token = CancellationToken::new();
        token.cancel();
        let mut body = ForwardBody::new(
            Frames(VecDeque::new()),
            Instant::now() + Duration::from_secs(30),
            token,
            None,
            None,
        )
        .with_terminal(Some(publisher.clone()));
        assert!(body.stage_first().await.is_err());
        assert!(publisher.failed());
        drop((body, owner));
        connection.close().await;
    }
}

#[cfg(test)]
#[path = "server_deadline_contracts.rs"]
mod deadline_contracts;

#[cfg(test)]
#[path = "server_failed_flush_contracts.rs"]
mod failed_flush_contracts;

#[cfg(test)]
#[path = "server_credential_contracts.rs"]
mod credential_contracts;

#[cfg(test)]
#[path = "server_receive_timer_contracts.rs"]
mod receive_timer_contracts;
