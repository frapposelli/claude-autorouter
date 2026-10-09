//! Stage-B1 synthetic transport only. No production selection path.
//! Session resumption, Node pool expiry, and config initialization remain gates.

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
use openssl::ssl::{Ssl, SslVerifyMode};
use openssl::x509::X509VerifyResult;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tower_service::Service;

use super::policy::{Context as TlsContext, Profile, context};
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
        match &mut self.stream {
            Stream::Plain(io) => Pin::new(io).poll_read(cx, buffer),
            Stream::Tls(io) => Pin::new(io).poll_read(cx, buffer),
        }
    }
}
impl Write for TransportIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut self.stream {
            Stream::Plain(io) => Pin::new(io).poll_write(cx, bytes),
            Stream::Tls(io) => Pin::new(io).poll_write(cx, bytes),
        }
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.stream {
            Stream::Plain(io) => Pin::new(io).poll_flush(cx),
            Stream::Tls(io) => Pin::new(io).poll_flush(cx),
        }
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.stream {
            Stream::Plain(io) => Pin::new(io).poll_shutdown(cx),
            Stream::Tls(io) => Pin::new(io).poll_shutdown(cx),
        }
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
        match &mut self.stream {
            Stream::Plain(io) => Pin::new(io).poll_write_vectored(cx, buffers),
            Stream::Tls(io) => Pin::new(io).poll_write_vectored(cx, buffers),
        }
    }
}

#[derive(Clone)]
struct Connector {
    tcp: HttpConnector,
    tls: TlsContext,
    profile: Profile,
    counts: Arc<Counts>,
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
        let lease = Lease::new(self.counts.clone());
        let tls = self.tls.clone();
        let profile = self.profile;
        // Capture the attempt now; a cancelled future owns and releases its IO.
        let tcp = self.tcp.call(uri.clone());
        Box::pin(async move {
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
                let mut ssl = Ssl::new(&tls.tls)
                    .map_err(|_| io::Error::other("Synthetic SSL allocation failed"))?;
                if !reject {
                    ssl.set_verify(SslVerifyMode::NONE);
                }
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
                if stream.ssl().session_reused() {
                    return Err(io::Error::other(
                        "Synthetic transport unexpectedly resumed a session",
                    ));
                }
                lease.0.completed.fetch_add(1, Ordering::SeqCst);
                Ok(TransportIo {
                    stream: Stream::Tls(stream),
                    _lease: lease,
                })
            };
            if profile == Profile::Fetch {
                tokio::time::timeout(Duration::from_secs(10), connect)
                    .await
                    .map_err(|_| io::Error::other("Synthetic fetch connection deadline"))?
            } else {
                connect.await
            }
        })
    }
}

pub(super) struct SpikeHttpClient {
    raw: Client<Connector, Full<Bytes>>,
    fetch: Client<Connector, Full<Bytes>>,
    pub raw_counts: Arc<Counts>,
    pub fetch_counts: Arc<Counts>,
}
impl SpikeHttpClient {
    pub fn new() -> Result<Self, HttpError> {
        fn client(
            profile: Profile,
            counts: Arc<Counts>,
        ) -> Result<Client<Connector, Full<Bytes>>, HttpError> {
            let mut tcp = HttpConnector::new();
            tcp.enforce_http(false);
            let connector = Connector {
                tcp,
                tls: context(profile)?,
                profile,
                counts,
            };
            let mut builder = Client::builder(TokioExecutor::new());
            builder.retry_canceled_requests(false);
            // Pooling is retained to exercise HTTP ownership, but its expiry and
            // session policies are explicitly not qualified by stage A.
            Ok(builder.build(connector))
        }
        let raw_counts = Arc::new(Counts::default());
        let fetch_counts = Arc::new(Counts::default());
        Ok(Self {
            raw: client(Profile::Raw, raw_counts.clone())?,
            fetch: client(Profile::Fetch, fetch_counts.clone())?,
            raw_counts,
            fetch_counts,
        })
    }
}
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
