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
use super::handshake_io::HandshakeIo;
use super::lifecycle::{ConnectionIdentity, ConnectionLifetime};
use super::policy::{Context as TlsContext, Profile, TrustSnapshot, context, trust_snapshot};
use super::raw_tls_output::RawOutput;
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
    Tls(TokioIo<SslStream<HandshakeIo<Tcp>>>),
}

// Present only for the explicitly armed idle fixture. Delivered retains the
// actual EOF result, so subsequent polls cannot touch the underlying IO again.
#[derive(Clone, Copy)]
enum LatchedEof {
    Observed,
    Delivered,
}

pub(super) struct TransportIo {
    stream: Stream,
    _lease: Lease,
    session: Option<Arc<TicketState>>,
    lifetime: Arc<ConnectionLifetime>,
    abort: Option<Arc<AbortControl>>,
    idle: Option<Arc<super::idle_close::Connection>>,
    eof_latched: Option<LatchedEof>,
    // Only raw Node-compatible HTTPS uses the error-only finalization boundary.
    // Once TCP shutdown starts, a pending poll must not repeat SSL_shutdown.
    node_shutdown_tcp_only: Option<bool>,
    // Only polls which reached SSL can leave application output in progress.
    // Empty writes cannot complete pending SSL output; only a nonempty
    // Ready result clears that write form's bit.
    tls_write_pending: bool,
    tls_vectored_write_pending: bool,
    raw_output: Option<RawOutput>,
}
impl Connection for TransportIo {
    fn connected(&self) -> Connected {
        let connected = match &self.stream {
            Stream::Plain(io) => io.connected(),
            Stream::Tls(io) => io.inner().get_ref().connected(),
        }
        .extra(ConnectionIdentity::new(&self.lifetime));
        let connected = match &self.abort {
            Some(control) => connected.extra(control.handle()),
            None => connected,
        };
        match &self.idle {
            Some(connection) => connected.extra(connection.identity()),
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
        if let Some(idle) = &self.idle
            && idle.read_ready(cx).is_pending()
        {
            return Poll::Pending;
        }
        if let Some(phase) = self.eof_latched {
            // A zero-capacity poll is not an EOF-delivery observation.
            if buffer.remaining() == 0 {
                return Poll::Ready(Ok(()));
            }
            if matches!(phase, LatchedEof::Observed) {
                let idle = self.idle.as_ref().expect("armed EOF fixture identity");
                if idle.eof_delivery_ready(cx).is_pending() {
                    return Poll::Pending;
                }
                idle.event(super::idle_close::Event::ReadEofDelivered);
                idle.event(super::idle_close::Event::ReadEof);
                self.eof_latched = Some(LatchedEof::Delivered);
            }
            return Poll::Ready(Ok(()));
        }
        let before = buffer.filled().len();
        let capacity = buffer.remaining();
        let this = self.as_mut().get_mut();
        let result = match (&mut this.stream, &mut this.raw_output) {
            (Stream::Plain(io), _) => Pin::new(io).poll_read(cx, buffer),
            (Stream::Tls(io), Some(output)) => output.poll_read(io, cx, buffer),
            (Stream::Tls(io), None) => Pin::new(io).poll_read(cx, buffer),
        };
        self._lease
            .0
            .read_bytes
            .fetch_add(buffer.filled().len() - before, Ordering::SeqCst);
        match &result {
            Poll::Ready(Err(error)) => {
                if let Some(idle) = &self.idle {
                    idle.event(|id| super::idle_close::Event::ReadError(id, error.kind()));
                }
                self.io_error();
            }
            Poll::Ready(Ok(())) if capacity != 0 && buffer.filled().len() == before => {
                if let Some(idle) = &self.idle {
                    if idle.defer_observed_eof(cx) {
                        // The physical EOF is real even while HTTP notification
                        // is held. It must not become an error-close/abort.
                        self.eof_latched = Some(LatchedEof::Observed);
                        self.ordinary_close();
                        return Poll::Pending;
                    }
                    idle.event(super::idle_close::Event::ReadEof);
                }
                self.ordinary_close()
            }
            _ => {}
        }
        result
    }
}
// Keep this boundary separate so the same real TLS shutdown operation can be
// checked with a deterministic below-SSL write readiness control.
fn poll_tls_shutdown<T: AsyncWrite + Unpin>(
    io: &mut T,
    cx: &mut Context<'_>,
) -> Poll<io::Result<()>> {
    // SSL_get_error also inspects the thread-local queue. A pending TLS poll
    // may resume after unrelated OpenSSL work; only this poll's errors belong
    // here. TLS establishment has already completed explicit initialization.
    openssl::error::clear_initialized();
    Pin::new(io).poll_shutdown(cx)
}

// Node's TLSWrap::DoShutdown does not use SSL close-finalization errors as its
// socket-shutdown result. Keep the operation boundary explicit: TLS Pending
// remains Pending, and genuine underlying TCP errors remain errors.
fn poll_node_tls_shutdown<S>(
    io: &mut TokioIo<SslStream<HandshakeIo<S>>>,
    tcp_only: &mut bool,
    allow_finalization_error: bool,
    cx: &mut Context<'_>,
    mut finalization_error: impl FnMut(&io::Error),
) -> Poll<io::Result<()>>
where
    S: hyper::rt::Read + hyper::rt::Write + Unpin,
{
    let mut result = if *tcp_only {
        hyper::rt::Write::poll_shutdown(Pin::new(io.inner_mut().get_mut()), cx)
    } else {
        io.inner_mut().get_mut().reset_shutdown_entry();
        poll_tls_shutdown(io, cx)
    };
    // At most one SSL finalization error and one subsequent ciphertext-write
    // error can be absorbed. After explicit discard no ciphertext remains;
    // physical flush and TCP shutdown errors are always returned unchanged.
    for _ in 0..2 {
        let lower_started = io.inner().get_ref().lower_shutdown_started();
        if lower_started {
            *tcp_only = true;
        }
        match result {
            Poll::Ready(Err(error))
                if allow_finalization_error
                    && (!lower_started
                        || (io.inner().get_ref().finalization_write_failed()
                            && !io.inner().get_ref().shutdown_entered())) =>
            {
                *tcp_only = true;
                finalization_error(&error);
                if io.inner().get_ref().finalization_write_failed() {
                    io.inner_mut().get_mut().discard_failed_finalization();
                }
                result = hyper::rt::Write::poll_shutdown(Pin::new(io.inner_mut().get_mut()), cx);
            }
            result => return result,
        }
    }
    result
}

fn record_tls_shutdown_error(idle: Option<&super::idle_close::Connection>, error: &io::Error) {
    if let Some(idle) = idle
        && let Some(tls_error) = error
            .get_ref()
            .and_then(|source| source.downcast_ref::<openssl::ssl::Error>())
    {
        idle.event(|id| super::idle_close::Event::ShutdownTlsError(id, tls_error.code().as_raw()));
        if let Some(stack) = tls_error.ssl_error() {
            for entry in stack.errors() {
                idle.event(|id| {
                    super::idle_close::Event::ShutdownTlsReason(
                        id,
                        entry.library_code(),
                        entry.reason_code(),
                    )
                });
            }
        }
    }
}

impl AsyncWrite for TransportIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        if let Some(idle) = &self.idle
            && idle.write_ready(cx).is_pending()
        {
            return Poll::Pending;
        }
        let this = self.as_mut().get_mut();
        let result = match (&mut this.stream, &mut this.raw_output) {
            (Stream::Plain(io), _) => Pin::new(io).poll_write(cx, bytes),
            (Stream::Tls(io), Some(output)) => output.poll_write(io, cx, bytes),
            (Stream::Tls(io), None) => Pin::new(io).poll_write(cx, bytes),
        };
        if !bytes.is_empty() && matches!(&self.stream, Stream::Tls(_)) {
            self.tls_write_pending = result.is_pending();
        }
        if let Poll::Ready(Err(error)) = &result {
            if let Some(idle) = &self.idle {
                idle.event(|id| super::idle_close::Event::WriteError(id, error.kind()));
            }
            self.io_error();
        }
        result
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.as_mut().get_mut();
        let result = match (&mut this.stream, &mut this.raw_output) {
            (Stream::Plain(io), _) => Pin::new(io).poll_flush(cx),
            (Stream::Tls(io), Some(output)) => output.poll_flush(io, cx),
            (Stream::Tls(io), None) => Pin::new(io).poll_flush(cx),
        };
        if let Poll::Ready(Err(error)) = &result {
            if let Some(idle) = &self.idle {
                idle.event(|id| super::idle_close::Event::FlushError(id, error.kind()));
            }
            self.io_error();
        }
        result
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.as_mut().get_mut();
        let idle = this.idle.as_deref();
        let application_write_pending = this.tls_write_pending || this.tls_vectored_write_pending;
        let result = match (
            &mut this.stream,
            &mut this.node_shutdown_tcp_only,
            &mut this.raw_output,
        ) {
            (Stream::Plain(io), _, _) => Pin::new(io).poll_shutdown(cx),
            // Pending application output makes a TLS failure ambiguous. Keep
            // its original error, while still recording actual TCP stage entry.
            (Stream::Tls(io), Some(tcp_only), output) => {
                if output.as_ref().is_some_and(|output| output.failed()) {
                    return Poll::Ready(Err(super::raw_tls_output::closed()));
                }
                let has_owner = output.is_some();
                let application = if !*tcp_only && !io.inner().get_ref().tls_finalization_started()
                {
                    match output.as_mut() {
                        Some(output) => match output.poll_flush(io, cx) {
                            Poll::Ready(Ok(())) => {
                                io.inner_mut().get_mut().begin_tls_finalization();
                                Poll::Ready(Ok(()))
                            }
                            result => result,
                        },
                        None => Poll::Ready(Ok(())),
                    }
                } else {
                    Poll::Ready(Ok(()))
                };
                if application.is_pending() {
                    return Poll::Pending;
                }
                if let Poll::Ready(Err(error)) = application {
                    if let Some(idle) = idle {
                        idle.event(|id| super::idle_close::Event::ShutdownError(id, error.kind()));
                    }
                    this.io_error();
                    return Poll::Ready(Err(error));
                }
                io.inner()
                    .get_ref()
                    .register_egress_waiter(super::handshake_io::EgressRole::Writer, cx);
                let result = poll_node_tls_shutdown(
                    io,
                    tcp_only,
                    has_owner || !application_write_pending,
                    cx,
                    |error| {
                        record_tls_shutdown_error(idle, error);
                        if let Some(idle) = idle {
                            idle.event(|id| {
                                super::idle_close::Event::ShutdownTlsFinalizationError(
                                    id,
                                    error.kind(),
                                )
                            });
                        }
                    },
                );
                if result.is_ready() {
                    io.inner()
                        .get_ref()
                        .clear_egress_waiter(super::handshake_io::EgressRole::Writer);
                }
                if matches!(result, Poll::Ready(Err(_)))
                    && let Some(output) = output
                {
                    output.poison(io);
                }
                result
            }
            (Stream::Tls(io), None, _) => poll_tls_shutdown(io, cx),
        };
        match &result {
            Poll::Ready(Err(error)) => {
                record_tls_shutdown_error(this.idle.as_deref(), error);
                if let Some(idle) = &this.idle {
                    idle.event(|id| super::idle_close::Event::ShutdownError(id, error.kind()));
                }
                this.io_error();
            }
            Poll::Ready(Ok(())) => this.ordinary_close(),
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
        if let Some(idle) = &self.idle
            && idle.write_ready(cx).is_pending()
        {
            return Poll::Pending;
        }
        let this = self.as_mut().get_mut();
        let result = match (&mut this.stream, &mut this.raw_output) {
            (Stream::Plain(io), _) => Pin::new(io).poll_write_vectored(cx, buffers),
            (Stream::Tls(io), Some(output)) => output.poll_write_vectored(io, cx, buffers),
            (Stream::Tls(io), None) => Pin::new(io).poll_write_vectored(cx, buffers),
        };
        if buffers.iter().any(|buffer| !buffer.is_empty()) && matches!(&self.stream, Stream::Tls(_))
        {
            self.tls_vectored_write_pending = result.is_pending();
        }
        if let Poll::Ready(Err(error)) = &result {
            if let Some(idle) = &self.idle {
                idle.event(|id| super::idle_close::Event::WriteError(id, error.kind()));
            }
            self.io_error();
        }
        result
    }
}

#[derive(Clone)]
pub(super) struct Connector {
    tcp: HttpConnector,
    tls: TlsContext,
    profile: Profile,
    counts: Arc<Counts>,
    sessions: Option<RawCache>,
    aborts: bool,
    dial_gate: Option<Arc<DialGate>>,
    idle: Option<super::idle_close::Probe>,
}

// The separate raw-agent experiment reuses this connector and TLS/session
// machinery; it does not change any existing legacy-client constructor.
pub(super) fn raw_pool_connector(
    snapshot: &TrustSnapshot,
    counts: Arc<Counts>,
    sessions: RawCache,
    dial_gate: Option<Arc<DialGate>>,
) -> Result<Connector, HttpError> {
    let mut tcp = HttpConnector::new();
    tcp.enforce_http(false);
    Ok(Connector {
        tcp,
        tls: context(snapshot, Profile::Raw, true)?,
        profile: Profile::Raw,
        counts,
        sessions: Some(sessions),
        aborts: true,
        dial_gate,
        idle: None,
    })
}
impl Connector {
    pub(super) fn observe_idle(&mut self, probe: super::idle_close::Probe) {
        assert!(self.idle.is_none(), "idle observer installed once");
        self.idle = Some(probe);
    }
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
        let idle = self.idle.clone();
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
                        idle: idle.as_ref().map(super::idle_close::Probe::connected),
                        eof_latched: None,
                        node_shutdown_tcp_only: None,
                        tls_write_pending: false,
                        tls_vectored_write_pending: false,
                        raw_output: None,
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
                let mut stream = SslStream::new(ssl, HandshakeIo::new(TokioIo::new(io)))
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
                stream.get_mut().release().await?;
                if profile == Profile::Raw {
                    stream.get_mut().enable_raw_egress();
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
                    idle: idle.as_ref().map(super::idle_close::Probe::connected),
                    eof_latched: None,
                    node_shutdown_tcp_only: (profile == Profile::Raw).then_some(false),
                    tls_write_pending: false,
                    tls_vectored_write_pending: false,
                    raw_output: (profile == Profile::Raw).then(RawOutput::default),
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
    idle: Option<super::idle_close::Probe>,
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
        Self::with_snapshot_controls(sessions, snapshot, aborts, None)
    }
    pub(super) fn with_snapshot_idle(
        sessions: bool,
        snapshot: &TrustSnapshot,
        probe: super::idle_close::Probe,
    ) -> Result<Self, HttpError> {
        Self::with_snapshot_controls(sessions, snapshot, false, Some(probe))
    }
    fn with_snapshot_controls(
        sessions: bool,
        snapshot: &TrustSnapshot,
        aborts: bool,
        idle: Option<super::idle_close::Probe>,
    ) -> Result<Self, HttpError> {
        fn client(
            profile: Profile,
            counts: Arc<Counts>,
            snapshot: &TrustSnapshot,
            sessions: Option<RawCache>,
            aborts: bool,
            dial_gate: Option<Arc<DialGate>>,
            idle: Option<super::idle_close::Probe>,
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
                idle,
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
                idle.clone(),
            )?,
            fetch: client(
                Profile::Fetch,
                fetch_counts.clone(),
                snapshot,
                None,
                false,
                None,
                None,
            )?,
            raw_sessions,
            raw_dial_gate,
            idle,
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
        let observed = self.idle.as_ref().map(|probe| probe.attach(&mut request));
        request
            .extensions_mut()
            .insert(hyper::ext::NodeHttpResponsePolicy);
        let result = self.raw.request(request).await;
        if let (Some(probe), Some(id)) = (&self.idle, observed) {
            probe.record(if result.is_ok() {
                super::idle_close::Event::ResponseHead(id)
            } else {
                super::idle_close::Event::ResponseError(id)
            });
        }
        result.map_err(|_| HttpError::Network)
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

#[cfg(test)]
#[path = "shutdown_queue_tests.rs"]
mod shutdown_queue_tests;

#[cfg(test)]
#[path = "node_shutdown_origin_tests.rs"]
mod node_shutdown_origin_tests;

#[cfg(test)]
#[path = "raw_output_product_tests.rs"]
mod raw_output_product_tests;

// Shared fixture-only fault: a real owned duplicate of this exact TCP socket.
// Do not call the draining HandshakeIo shutdown as an immediate socket fault.
#[cfg(test)]
fn shutdown_raw_tls_write_half_for_test(transport: &TransportIo) -> io::Result<()> {
    use std::os::fd::AsFd;
    let tcp = match &transport.stream {
        Stream::Tls(tls) => tls.inner().get_ref().inner_transport().inner(),
        Stream::Plain(_) => panic!("fixture requires the existing accepted TLS socket"),
    };
    let duplicate = std::net::TcpStream::from(tcp.as_fd().try_clone_to_owned()?);
    assert_eq!(
        duplicate.local_addr()?,
        tcp.local_addr()?,
        "duplicate local socket identity"
    );
    assert_eq!(
        duplicate.peer_addr()?,
        tcp.peer_addr()?,
        "duplicate peer socket identity"
    );
    // This succeeds only after the kernel handles SHUT_WR on the real socket.
    // It does not touch/flush the plaintext or ciphertext owners above it.
    duplicate.shutdown(std::net::Shutdown::Write)
}

// Shared fixture machinery; the enclosing OpenSSL spike is cfg(test).
use openssl::ssl::{ErrorCode, SslVersion};
use std::future::poll_fn;
use std::sync::Mutex;
use std::task::Waker;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout, timeout_at};

const FIXTURE_CLEANUP: Duration = Duration::from_secs(2);
pub(super) struct TlsFixtureOwner {
    pub(super) release: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    pub(super) peer: Option<JoinHandle<()>>,
    pub(super) closed: tokio::sync::watch::Receiver<bool>,
}
pub(super) fn fixture_release_peer(release: &Arc<Mutex<Option<oneshot::Sender<()>>>>) {
    let send = release.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(send) = send {
        let _ = send.send(());
    }
}
impl TlsFixtureOwner {
    async fn finish(mut self) {
        fixture_release_peer(&self.release);
        let mut peer = self.peer.take().unwrap();
        match timeout(FIXTURE_CLEANUP, &mut peer).await {
            Ok(result) => result.expect("owned peer panic"),
            Err(_) => {
                peer.abort();
                let _ = timeout(FIXTURE_CLEANUP, &mut peer)
                    .await
                    .expect("owned peer abort/join deadline");
                panic!("owned peer completion deadline");
            }
        }
    }
}
impl Drop for TlsFixtureOwner {
    fn drop(&mut self) {
        fixture_release_peer(&self.release);
        if let Some(peer) = self.peer.take() {
            peer.abort();
        }
    }
}
pub(super) async fn fixture_run_owned<T, F, Fut>(
    deadline: Instant,
    value: T,
    owner: TlsFixtureOwner,
    check: F,
) where
    T: Send + 'static,
    F: FnOnce(T) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut task = tokio::spawn(check(value));
    let mut cancelled_joined = true;
    let outcome = match timeout_at(deadline, &mut task).await {
        Ok(outcome) => Some(outcome),
        Err(_) => {
            task.abort();
            cancelled_joined = timeout(FIXTURE_CLEANUP, &mut task).await.is_ok();
            None
        }
    };
    owner.finish().await;
    assert!(cancelled_joined, "assertion task abort/join deadline");
    match outcome {
        Some(Ok(())) => {}
        Some(Err(error)) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Some(Err(_)) => panic!("assertion task unexpectedly cancelled"),
        None => panic!("real TLS case deadline"),
    }
}
pub(super) struct TlsFixtureConnection {
    pub(super) transport: Option<TransportIo>,
    pub(super) connector: Option<Connector>,
    pub(super) cache: RawCache,
    pub(super) counts: Arc<Counts>,
}
pub(super) async fn fixture_connect_raw(
    version: SslVersion,
    aborts: bool,
    trust: TrustSnapshot,
    address: std::net::SocketAddr,
    probe: Option<super::idle_close::Probe>,
    qr: bool,
) -> TlsFixtureConnection {
    let cache = Arc::new(std::sync::Mutex::new(Cache::new(8)));
    let counts = Arc::new(Counts::default());
    let mut connector = raw_pool_connector(&trust, counts.clone(), cache.clone(), None).unwrap();
    connector.aborts = aborts;
    if let Some(probe) = probe {
        connector.observe_idle(probe);
    }
    let uri: Uri = format!("https://{address}/").parse().unwrap();
    let mut transport = connector.call(uri).await.unwrap().into_inner();
    match &transport.stream {
        Stream::Tls(stream) => assert_eq!(stream.inner().ssl().version2(), Some(version)),
        Stream::Plain(_) => panic!("real TLS required"),
    }
    if qr {
        transport.write_all(b"q").await.unwrap();
        transport.flush().await.unwrap();
        let mut byte = [0];
        transport.read_exact(&mut byte).await.unwrap();
        assert_eq!(&byte, b"r");
    }
    TlsFixtureConnection {
        transport: Some(transport),
        connector: Some(connector),
        cache,
        counts,
    }
}

pub(super) fn fixture_output(io: &TransportIo) -> (usize, usize, bool, usize) {
    let (retained, pending, inflight) = io
        .raw_output
        .as_ref()
        .expect("reviewed Raw output owner")
        .state();
    let cipher = match &io.stream {
        Stream::Tls(tls) => tls.inner().get_ref().egress_queued_bytes(),
        Stream::Plain(_) => panic!("real TLS required"),
    };
    assert!(retained <= 16 * 1024 && pending <= retained && cipher <= 64 * 1024);
    (retained, pending, inflight, cipher)
}
pub(super) fn fixture_pipe_reset(error: &io::Error) {
    let expected = [
        nix::errno::Errno::EPIPE as i32,
        nix::errno::Errno::ECONNRESET as i32,
    ];
    if let Some(errno) = error.raw_os_error() {
        assert!(
            expected.contains(&errno),
            "actual socket pipe/reset failure"
        );
        return;
    }
    let tls = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<openssl::ssl::Error>())
        .expect("identified SSL SYSCALL carrying actual socket errno");
    assert_eq!(tls.code(), ErrorCode::SYSCALL);
    let mut found = false;
    for entry in tls.ssl_error().expect("actual SYS stack").errors() {
        if entry.library_code() == 2 {
            assert!(
                expected.contains(&entry.reason_code()),
                "actual SYS pipe/reset reason"
            );
            found = true;
        }
    }
    assert!(found, "positive socket error evidence required");
}
pub(super) fn fixture_clean(
    counts: &Counts,
    cache: &RawCache,
    probe: Option<&super::idle_close::Probe>,
) {
    assert_eq!(counts.active.load(Ordering::SeqCst), 0);
    assert_eq!(counts.tasks.load(Ordering::SeqCst), 0);
    assert_eq!(counts.shutdown_handles.load(Ordering::SeqCst), 0);
    if let Some(probe) = probe {
        assert_eq!(probe.live(), 0);
    }
    assert_eq!(Arc::strong_count(cache), 1, "no hidden strong cache owner");
}
pub(super) fn fixture_no_write_hold(probe: &super::idle_close::Probe) {
    assert!(
        !probe
            .rows()
            .iter()
            .any(|event| matches!(event, super::idle_close::Event::WriteHeld(_))),
        "no above-SSL fixture output hold"
    );
}
const FIXTURE_CHUNK: usize = 16 * 1024;
const FIXTURE_MAX_POLLS: usize = 1024;
const FIXTURE_MAX_ACCEPTED: usize = FIXTURE_CHUNK * FIXTURE_MAX_POLLS;
fn fixture_write_once(
    io: &mut TransportIo,
    waker: &Waker,
    bytes: &[u8],
    vectored: bool,
) -> Poll<io::Result<usize>> {
    let mut cx = Context::from_waker(waker);
    if vectored {
        let slices = [
            io::IoSlice::new(&[]),
            io::IoSlice::new(bytes),
            io::IoSlice::new(b"must-not-be-gathered"),
        ];
        Pin::new(io).poll_write_vectored(&mut cx, &slices)
    } else {
        Pin::new(io).poll_write(&mut cx, bytes)
    }
}
pub(super) async fn fixture_credit_blocked(
    io: &mut TransportIo,
    waker: &Waker,
    vectored: bool,
) -> Vec<(u8, usize)> {
    let mut caller = vec![0; FIXTURE_CHUNK];
    let mut ledger = Vec::new();
    let mut accepted = 0;
    tokio::task::unconstrained(poll_fn(|_| {
        for index in 0..FIXTURE_MAX_POLLS {
            let byte = (index % 251 + 1) as u8;
            caller.fill(byte);
            match fixture_write_once(io, waker, &caller, vectored) {
                Poll::Ready(Ok(count)) => {
                    assert!(count > 0 && count <= FIXTURE_CHUNK);
                    accepted += count;
                    assert!(accepted <= FIXTURE_MAX_ACCEPTED);
                    ledger.push((byte, count));
                    caller.fill(0xee); // Every acknowledged caller prefix may now change.
                }
                Poll::Ready(Err(error)) => {
                    panic!("actual write failed before required credit barrier: {error:?}")
                }
                Poll::Pending => {
                    let (_, pending, inflight, cipher) = fixture_output(io);
                    assert!(
                        pending > 0 && !inflight && cipher > 32 * 1024,
                        "required pre-SSL credit barrier, not an in-flight WANT_WRITE"
                    );
                    assert!(accepted > 0);
                    return Poll::Ready(std::mem::take(&mut ledger));
                }
            }
        }
        panic!("did not observe declared credit barrier within byte/poll budget");
    }))
    .await
}
pub(super) fn fixture_evicted(cache: &RawCache, counts: &Counts, peak: usize, aborts: bool) {
    assert_eq!(cache.lock().unwrap().snapshot(), (0, 0, peak));
    assert_eq!(
        counts.session_error_closes.load(Ordering::SeqCst),
        usize::from(aborts)
    );
}
