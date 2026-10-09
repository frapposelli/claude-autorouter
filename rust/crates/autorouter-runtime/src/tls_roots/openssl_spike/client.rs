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
use hyper::rt::{Read, ReadBufCursor, Write};
use hyper::{Request, Response, Uri};
use hyper_openssl::SslStream;
use hyper_util::client::legacy::{
    Client,
    connect::{Connected, Connection, HttpConnector},
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use openssl::pkey::Id;
use openssl::ssl::Ssl;
use openssl::x509::X509VerifyResult;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tower_service::Service;

use super::policy::{Context as TlsContext, Profile, TrustSnapshot, context, trust_snapshot};
use super::session::{Cache, Key, RawCache, TicketState, Verification};
use crate::http_client::{HttpError, HttpTransport};

#[derive(Debug, Default)]
pub(super) struct Counts {
    pub attempts: AtomicUsize,
    pub active: AtomicUsize,
    pub completed: AtomicUsize,
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
    Plain(Tcp),
    Tls(SslStream<Tcp>),
}

pub(super) struct TransportIo {
    stream: Stream,
    _lease: Lease,
    session: Option<Arc<TicketState>>,
}
impl Connection for TransportIo {
    fn connected(&self) -> Connected {
        match &self.stream {
            Stream::Plain(io) => io.connected(),
            Stream::Tls(io) => io.get_ref().connected(),
        }
    }
}
impl Read for TransportIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        let result = match &mut self.stream {
            Stream::Plain(io) => Pin::new(io).poll_read(cx, buffer),
            Stream::Tls(io) => Pin::new(io).poll_read(cx, buffer),
        };
        if matches!(result, Poll::Ready(Err(_)))
            && let Some(state) = &self.session
        {
            state.close_error();
        }
        result
    }
}
impl Write for TransportIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = match &mut self.stream {
            Stream::Plain(io) => Pin::new(io).poll_write(cx, bytes),
            Stream::Tls(io) => Pin::new(io).poll_write(cx, bytes),
        };
        if matches!(result, Poll::Ready(Err(_)))
            && let Some(state) = &self.session
        {
            state.close_error();
        }
        result
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = match &mut self.stream {
            Stream::Plain(io) => Pin::new(io).poll_flush(cx),
            Stream::Tls(io) => Pin::new(io).poll_flush(cx),
        };
        if matches!(result, Poll::Ready(Err(_)))
            && let Some(state) = &self.session
        {
            state.close_error();
        }
        result
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = match &mut self.stream {
            Stream::Plain(io) => Pin::new(io).poll_shutdown(cx),
            Stream::Tls(io) => Pin::new(io).poll_shutdown(cx),
        };
        if matches!(result, Poll::Ready(Err(_)))
            && let Some(state) = &self.session
        {
            state.close_error();
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
        if matches!(result, Poll::Ready(Err(_)))
            && let Some(state) = &self.session
        {
            state.close_error();
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
}
impl Service<Uri> for Connector {
    type Response = TransportIo;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<TransportIo, io::Error>> + Send>>;

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
                if uri.scheme_str() == Some("http") {
                    return Ok(TransportIo {
                        stream: Stream::Plain(io),
                        _lease: lease,
                        session: None,
                    });
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
                let mut stream = SslStream::new(ssl, io)
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
                Ok(TransportIo {
                    stream: Stream::Tls(stream),
                    _lease: lease,
                    session,
                })
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
}
impl SpikeHttpClient {
    pub fn new() -> Result<Self, HttpError> {
        Self::with_sessions(false)
    }
    pub fn with_sessions(sessions: bool) -> Result<Self, HttpError> {
        fn client(
            profile: Profile,
            counts: Arc<Counts>,
            snapshot: &TrustSnapshot,
            sessions: Option<RawCache>,
        ) -> Result<Client<Connector, Full<Bytes>>, HttpError> {
            let mut tcp = HttpConnector::new();
            tcp.enforce_http(false);
            let connector = Connector {
                tcp,
                tls: context(snapshot, profile, sessions.is_some())?,
                profile,
                counts,
                sessions,
            };
            let mut builder = Client::builder(TokioExecutor::new());
            builder.retry_canceled_requests(false);
            // Pooling is retained to exercise HTTP ownership, but its expiry and
            // session policies are explicitly not qualified by stage A.
            Ok(builder.build(connector))
        }
        let raw_counts = Arc::new(Counts::default());
        let fetch_counts = Arc::new(Counts::default());
        let snapshot = trust_snapshot()?;
        let raw_sessions = sessions.then(|| Arc::new(std::sync::Mutex::new(Cache::new(100))));
        Ok(Self {
            raw: client(
                Profile::Raw,
                raw_counts.clone(),
                &snapshot,
                raw_sessions.clone(),
            )?,
            fetch: client(Profile::Fetch, fetch_counts.clone(), &snapshot, None)?,
            raw_sessions,
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
