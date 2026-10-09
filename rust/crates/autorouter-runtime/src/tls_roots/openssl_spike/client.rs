//! Test-only B1/B2 transport; production keeps its Rustls connector.
//! Raw session evidence does not qualify fetch caches, pooling or configuration.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Request, Response, Uri};
use hyper_openssl::SslStream;
use hyper_util::client::legacy::{
    Client,
    connect::{Connected, Connection, HttpConnector},
};
use hyper_util::rt::TokioIo;
use openssl::pkey::Id;
use openssl::ssl::Ssl;
use openssl::x509::X509VerifyResult;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tower_service::Service;

use super::abort::{AbortControl, DialGate};
use super::lifecycle::{ConnectionIdentity, ConnectionLifetime};
use super::policy::{Context as TlsContext, Profile, TrustSnapshot, context, trust_snapshot};
use super::session::{Cache, Key, RawCache, TicketState, Verification};
use crate::http_client::{HttpError, HttpTransport};

#[derive(Debug, Default)]
pub(super) struct Counts {
    pub attempts: AtomicUsize,
    pub active: AtomicUsize,
    pub completed: AtomicUsize,
    pub tasks: AtomicUsize,
    pub shutdown_handles: AtomicUsize,
    pub shutdown_calls: AtomicUsize,
    pub session_error_closes: AtomicUsize,
    pub read_bytes: AtomicUsize,
    pub published: AtomicUsize,
}

// Track every Hyper-owned task independently from sockets. This does not alter
// task scheduling; it lets fixtures distinguish IO release from task completion.
#[derive(Clone)]
struct Executor(Arc<Counts>);
struct TaskLease(Arc<Counts>);
impl Drop for TaskLease {
    fn drop(&mut self) {
        self.0.tasks.fetch_sub(1, Ordering::SeqCst);
    }
}
impl<F> hyper::rt::Executor<F> for Executor
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, future: F) {
        self.0.tasks.fetch_add(1, Ordering::SeqCst);
        let lease = TaskLease(self.0.clone());
        tokio::spawn(async move {
            let _lease = lease;
            future.await;
        });
    }
}

struct Lease(Arc<Counts>);
impl Lease {
    fn new(counts: Arc<Counts>) -> Self {
        counts.attempts.fetch_add(1, Ordering::SeqCst);
        counts.active.fetch_add(1, Ordering::SeqCst);
        Self(counts)
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

type Tcp = TokioIo<TcpStream>;
enum Stream {
    Plain(TcpStream),
    Tls(TokioIo<SslStream<Tcp>>),
}

pub(super) struct TransportIo {
    stream: Stream,
    _lease: Lease,
    session: Option<Arc<TicketState>>,
    lifetime: Arc<ConnectionLifetime>,
    abort: Option<Arc<AbortControl>>,
}
impl Connection for TransportIo {
    fn connected(&self) -> Connected {
        let connected = match &self.stream {
            Stream::Plain(io) => io.connected(),
            Stream::Tls(io) => io.inner().get_ref().connected(),
        }
        .extra(ConnectionIdentity::new(&self.lifetime));
        match &self.abort {
            Some(control) => connected.extra(control.handle()),
            None => connected,
        }
    }
}
impl TransportIo {
    fn io_error(&self) {
        if let Some(control) = &self.abort {
            control.io_error();
        } else if let Some(state) = &self.session {
            state.close_error();
        }
    }
    fn ordinary_close(&self) {
        if let Some(control) = &self.abort {
            control.ordinary_close();
        }
    }
}
impl Drop for TransportIo {
    fn drop(&mut self) {
        self.ordinary_close();
    }
}
impl AsyncRead for TransportIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buffer.filled().len();
        let capacity = buffer.remaining();
        let result = match &mut self.stream {
            Stream::Plain(io) => Pin::new(io).poll_read(cx, buffer),
            Stream::Tls(io) => Pin::new(io).poll_read(cx, buffer),
        };
        self._lease
            .0
            .read_bytes
            .fetch_add(buffer.filled().len() - before, Ordering::SeqCst);
        match &result {
            Poll::Ready(Err(_)) => self.io_error(),
            Poll::Ready(Ok(())) if capacity != 0 && buffer.filled().len() == before => {
                self.ordinary_close()
            }
            _ => {}
        }
        result
    }
}
impl AsyncWrite for TransportIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = match &mut self.stream {
            Stream::Plain(io) => Pin::new(io).poll_write(cx, bytes),
            Stream::Tls(io) => Pin::new(io).poll_write(cx, bytes),
        };
        if matches!(result, Poll::Ready(Err(_))) {
            self.io_error();
        }
        result
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = match &mut self.stream {
            Stream::Plain(io) => Pin::new(io).poll_flush(cx),
            Stream::Tls(io) => Pin::new(io).poll_flush(cx),
        };
        if matches!(result, Poll::Ready(Err(_))) {
            self.io_error();
        }
        result
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = match &mut self.stream {
            Stream::Plain(io) => Pin::new(io).poll_shutdown(cx),
            Stream::Tls(io) => Pin::new(io).poll_shutdown(cx),
        };
        match &result {
            Poll::Ready(Err(_)) => self.io_error(),
            Poll::Ready(Ok(())) => self.ordinary_close(),
            Poll::Pending => {}
        }
        result
    }
    fn is_write_vectored(&self) -> bool {
        match &self.stream {
            Stream::Plain(io) => io.is_write_vectored(),
            Stream::Tls(io) => io.is_write_vectored(),
        }
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let result = match &mut self.stream {
            Stream::Plain(io) => Pin::new(io).poll_write_vectored(cx, buffers),
            Stream::Tls(io) => Pin::new(io).poll_write_vectored(cx, buffers),
        };
        if matches!(result, Poll::Ready(Err(_))) {
            self.io_error();
        }
        result
    }
}

#[derive(Clone)]
struct Connector {
    tcp: HttpConnector,
    tls: TlsContext,
    profile: Profile,
    counts: Arc<Counts>,
    sessions: Option<RawCache>,
    aborts: bool,
    dial_gate: Option<Arc<DialGate>>,
}
impl Service<Uri> for Connector {
    type Response = TokioIo<TransportIo>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<TokioIo<TransportIo>, io::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.tcp
            .poll_ready(cx)
            .map_err(|_| io::Error::other("Synthetic TCP connector unavailable"))
    }
    fn call(&mut self, uri: Uri) -> Self::Future {
        let host = uri.host().unwrap_or_default();
        let host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        if host != "localhost"
            && !host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
        {
            return Box::pin(async {
                Err(io::Error::other(
                    "Synthetic transport permits loopback destinations only",
                ))
            });
        }
        // Node reads this setting when a TLS connection is requested, including
        // attempts that fail before TCP. Plain HTTP must not emit its warning.
        let reject = uri.scheme_str() != Some("https") || super::options::reject_unauthorized();
        if uri.scheme_str() == Some("https") && !self.tls.valid_ciphers {
            return Box::pin(async {
                Err(io::Error::other("Synthetic invalid cipher expression"))
            });
        }
        let session = self
            .sessions
            .as_ref()
            .filter(|_| uri.scheme_str() == Some("https"))
            .map(|cache| {
                Arc::new(TicketState::new(
                    cache,
                    Key {
                        policy: self.tls.generation,
                        origin: format!(
                            "{}:{}",
                            host.to_ascii_lowercase(),
                            uri.port_u16().unwrap_or(443)
                        ),
                        server_name: host
                            .parse::<std::net::IpAddr>()
                            .is_err()
                            .then(|| host.to_ascii_lowercase()),
                        verification: if reject {
                            Verification::Required
                        } else {
                            Verification::ExplicitlyDisabled
                        },
                    },
                ))
            });
        let attempt = AttemptGuard(session.clone());
        let lease = Lease::new(self.counts.clone());
        let tls = self.tls.clone();
        let profile = self.profile;
        let aborts = self.aborts;
        let dial_gate = self.dial_gate.clone();
        // Capture the attempt now; a cancelled future owns and releases its IO.
        let tcp = self.tcp.call(uri.clone());
        Box::pin(async move {
            let failure_session = session.clone();
            let connect = async {
                let io = tcp
                    .await
                    .map_err(|_| io::Error::other("Synthetic TCP connection failed"))?;
                if !io.inner().peer_addr()?.ip().is_loopback() {
                    return Err(io::Error::other(
                        "Synthetic transport permits loopback peers only",
                    ));
                }
                let io = io.into_inner();
                let (io, abort) = if aborts {
                    let original = io.into_std()?;
                    let duplicate = original.try_clone()?;
                    let io = TcpStream::from_std(original)?;
                    let control = AbortControl::new(duplicate, session.clone(), lease.0.clone());
                    (io, Some(control))
                } else {
                    (io, None)
                };
                if let Some(gate) = &dial_gate {
                    gate.pause().await;
                }
                if uri.scheme_str() == Some("http") {
                    lease.0.published.fetch_add(1, Ordering::SeqCst);
                    return Ok(TokioIo::new(TransportIo {
                        stream: Stream::Plain(io),
                        _lease: lease,
                        session: None,
                        lifetime: Arc::new(ConnectionLifetime),
                        abort,
                    }));
                }
                if uri.scheme_str() != Some("https") {
                    return Err(io::Error::other("Synthetic unsupported scheme"));
                }
                let host = uri
                    .host()
                    .ok_or_else(|| io::Error::other("Synthetic missing host"))?;
                let host = host
                    .strip_prefix('[')
                    .and_then(|host| host.strip_suffix(']'))
                    .unwrap_or(host);
                let identity = ServerName::try_from(host.to_owned())
                    .map_err(|_| io::Error::other("Synthetic invalid host"))?;
                let mut ssl = Ssl::new(if reject { &tls.tls } else { &tls.unverified })
                    .map_err(|_| io::Error::other("Synthetic SSL allocation failed"))?;
                let offered = if let (Some(state), Some(index)) = (&session, tls.index) {
                    ssl.set_ex_data(index, state.clone());
                    let ticket = state.lookup();
                    if let Some(ticket) = &ticket {
                        ssl.set_bound_session(ticket)
                            .map_err(|_| io::Error::other("Synthetic session ownership failed"))?;
                    }
                    ticket.is_some()
                } else {
                    false
                };
                if !matches!(identity, ServerName::IpAddress(_)) {
                    ssl.set_hostname(host)
                        .map_err(|_| io::Error::other("Synthetic SNI failed"))?;
                }
                let mut stream = SslStream::new(ssl, TokioIo::new(io))
                    .map_err(|_| io::Error::other("Synthetic TLS stream failed"))?;
                Pin::new(&mut stream)
                    .connect()
                    .await
                    .map_err(|_| io::Error::other("Synthetic TLS handshake failed"))?;
                // Explicit post-handshake identity check uses the same exact DNS
                // bytes/partial wildcard rules as the shipping Node verifier.
                if reject {
                    let peer = stream
                        .ssl()
                        .peer_certificate()
                        .ok_or_else(|| io::Error::other("Synthetic missing peer certificate"))?;
                    if stream.ssl().verify_result() != X509VerifyResult::OK
                        || !super::super::valid_identity(&peer, &identity)
                    {
                        return Err(io::Error::other("Synthetic peer verification failed"));
                    }
                }
                if stream
                    .ssl()
                    .peer_tmp_key()
                    .is_ok_and(|key| key.id() == Id::DH && key.bits() < 1024)
                {
                    return Err(io::Error::other("Synthetic DH key too small"));
                }
                if stream.ssl().session_reused() && !offered {
                    return Err(io::Error::other("Synthetic unexpected session reuse"));
                }
                if let Some(state) = &session {
                    state.accept();
                }
                let mut attempt = attempt;
                attempt.0 = None;
                lease.0.completed.fetch_add(1, Ordering::SeqCst);
                lease.0.published.fetch_add(1, Ordering::SeqCst);
                Ok(TokioIo::new(TransportIo {
                    stream: Stream::Tls(TokioIo::new(stream)),
                    _lease: lease,
                    session,
                    lifetime: Arc::new(ConnectionLifetime),
                    abort,
                }))
            };
            let result = if profile == Profile::Fetch {
                tokio::time::timeout(Duration::from_secs(10), connect)
                    .await
                    .map_err(|_| io::Error::other("Synthetic fetch connection deadline"))?
            } else {
                connect.await
            };
            if result.is_err()
                && let Some(state) = failure_session
            {
                state.close_error();
            }
            result
        })
    }
}

pub(super) struct SpikeHttpClient {
    raw: Client<Connector, Full<Bytes>>,
    fetch: Client<Connector, Full<Bytes>>,
    pub raw_counts: Arc<Counts>,
    pub fetch_counts: Arc<Counts>,
    pub raw_sessions: Option<RawCache>,
    pub raw_dial_gate: Option<Arc<DialGate>>,
}
impl SpikeHttpClient {
    pub fn new() -> Result<Self, HttpError> {
        Self::with_sessions(false)
    }
    pub fn with_sessions(sessions: bool) -> Result<Self, HttpError> {
        Self::with_snapshot(sessions, &trust_snapshot()?)
    }
    pub(super) fn with_snapshot(
        sessions: bool,
        snapshot: &TrustSnapshot,
    ) -> Result<Self, HttpError> {
        Self::with_snapshot_aborts(sessions, snapshot, false)
    }
    pub(super) fn with_snapshot_aborts(
        sessions: bool,
        snapshot: &TrustSnapshot,
        aborts: bool,
    ) -> Result<Self, HttpError> {
        fn client(
            profile: Profile,
            counts: Arc<Counts>,
            snapshot: &TrustSnapshot,
            sessions: Option<RawCache>,
            aborts: bool,
            dial_gate: Option<Arc<DialGate>>,
        ) -> Result<Client<Connector, Full<Bytes>>, HttpError> {
            let mut tcp = HttpConnector::new();
            tcp.enforce_http(false);
            let connector = Connector {
                tcp,
                tls: context(snapshot, profile, sessions.is_some())?,
                profile,
                counts: counts.clone(),
                sessions,
                aborts,
                dial_gate,
            };
            let mut builder = Client::builder(Executor(counts));
            builder.retry_canceled_requests(false);
            // Pooling is retained to exercise HTTP ownership, but its expiry and
            // session policies are explicitly not qualified by stage A.
            Ok(builder.build(connector))
        }
        let raw_counts = Arc::new(Counts::default());
        let fetch_counts = Arc::new(Counts::default());
        let raw_dial_gate = aborts.then(|| Arc::new(DialGate::default()));
        let raw_sessions = sessions.then(|| Arc::new(std::sync::Mutex::new(Cache::new(100))));
        Ok(Self {
            raw: client(
                Profile::Raw,
                raw_counts.clone(),
                snapshot,
                raw_sessions.clone(),
                aborts,
                raw_dial_gate.clone(),
            )?,
            fetch: client(
                Profile::Fetch,
                fetch_counts.clone(),
                snapshot,
                None,
                false,
                None,
            )?,
            raw_sessions,
            raw_dial_gate,
            raw_counts,
            fetch_counts,
        })
    }
}
// An abandoned attempt must never publish pending tickets. Drop alone does
// not identify Node's close(hadError); actual connect errors are handled above.
struct AttemptGuard(Option<Arc<TicketState>>);
impl Drop for AttemptGuard {
    fn drop(&mut self) {
        if let Some(state) = &self.0 {
            state.reject();
        }
    }
}

// Incoming and request futures carry no cancellation cause. Their Drop or a
// parser/body error must not be promoted to a socket hadError observation.
// Actual Gateway cancellation therefore remains an explicit B2 limitation.
impl HttpTransport for SpikeHttpClient {
    type ResponseBody = Incoming;
    async fn request_raw(
        &self,
        mut request: Request<Full<Bytes>>,
    ) -> Result<Response<Incoming>, HttpError> {
        if let Some(intent) = request
            .extensions()
            .get::<Arc<super::gateway_intent::Intent>>()
            .cloned()
        {
            let capture =
                hyper_util::client::legacy::connect::capture_http1_assignment(&mut request);
            intent.install(capture);
        }
        request
            .extensions_mut()
            .insert(hyper::ext::NodeHttpResponsePolicy);
        self.raw
            .request(request)
            .await
            .map_err(|_| HttpError::Network)
    }
    async fn request(
        &self,
        mut request: Request<Full<Bytes>>,
    ) -> Result<Response<Incoming>, HttpError> {
        request
            .extensions_mut()
            .insert(hyper::ext::NodeFetchResponsePolicy);
        self.fetch
            .request(request)
            .await
            .map_err(|_| HttpError::Network)
    }
}
