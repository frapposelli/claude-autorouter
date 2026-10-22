//! Exact shared loopback primitives; each original schedule retains its bounds and teardown policy.
use std::{
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

pub(super) async fn bind_listener(secure: bool) -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!(
        "{}://{}",
        if secure { "https" } else { "http" },
        listener.local_addr().unwrap()
    );
    (listener, origin)
}

pub(super) enum Peer {
    Tcp(TcpStream),
    Tls(hyper_util::rt::TokioIo<hyper_openssl::SslStream<hyper_util::rt::TokioIo<TcpStream>>>),
}
pub(super) async fn accept_peer(
    listener: &TcpListener,
    context: Option<&openssl::ssl::SslContext>,
) -> Peer {
    let (stream, _) = listener.accept().await.unwrap();
    if let Some(context) = context {
        let mut stream = hyper_openssl::SslStream::new(
            openssl::ssl::Ssl::new(context).unwrap(),
            hyper_util::rt::TokioIo::new(stream),
        )
        .unwrap();
        Pin::new(&mut stream).accept().await.unwrap();
        Peer::Tls(hyper_util::rt::TokioIo::new(stream))
    } else {
        Peer::Tcp(stream)
    }
}
impl AsyncRead for Peer {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}
impl AsyncWrite for Peer {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}
pub(super) struct PeerLease(pub(super) Arc<AtomicUsize>);
impl Drop for PeerLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

// Each caller retains its admission/error and request/read/shutdown policy.
// TLS stays in each spawned peer, after admission and active lease creation.
pub(super) struct ListenerPeers {
    pub(super) tasks: tokio::task::JoinSet<io::Result<()>>,
    pub(super) stop: tokio_util::sync::CancellationToken,
    pub(super) active: Arc<AtomicUsize>,
    pub(super) accepted: Arc<AtomicUsize>,
}
impl ListenerPeers {
    pub(super) fn spawn<F, Fut>(
        listener: TcpListener,
        tls: Option<openssl::ssl::SslContext>,
        mut admit: impl FnMut(&AtomicUsize) -> io::Result<usize> + Send + 'static,
        peer_error: &'static str,
        handshake_error: &'static str,
        exchange: F,
    ) -> Self
    where
        F: FnOnce(Peer, usize) -> Fut + Clone + Send + 'static,
        Fut: std::future::Future<Output = io::Result<()>> + Send + 'static,
    {
        let stop = tokio_util::sync::CancellationToken::new();
        let shutdown = stop.clone();
        let active = Arc::new(AtomicUsize::new(0));
        let accepted = Arc::new(AtomicUsize::new(0));
        let active_peer = active.clone();
        let accepted_peer = accepted.clone();
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(async move {
            let mut peers = tokio::task::JoinSet::new();
            let result = loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => break Ok(()),
                    result = peers.join_next(), if !peers.is_empty() => {
                        match result {
                            Some(Ok(Ok(()))) => {},
                            _ => break Err(io::Error::other(peer_error)),
                        }
                    },
                    socket = listener.accept() => {
                        let (socket, _) = socket?;
                        let connection = match admit(&accepted_peer) {
                            Ok(connection) => connection,
                            Err(error) => break Err(error),
                        };
                        active_peer.fetch_add(1, Ordering::SeqCst);
                        let lease = PeerLease(active_peer.clone());
                        let context = tls.clone();
                        // Clone the caller's sender(s) exactly once here, then
                        // consume that clone into the request future below.
                        let exchange = exchange.clone();
                        peers.spawn(async move {
                            let _lease = lease;
                            if let Some(context) = context {
                                let ssl = openssl::ssl::Ssl::new(&context)?;
                                let mut stream = hyper_openssl::SslStream::new(ssl, hyper_util::rt::TokioIo::new(socket))?;
                                Pin::new(&mut stream).accept().await
                                    .map_err(|_| io::Error::other(handshake_error))?;
                                exchange(Peer::Tls(hyper_util::rt::TokioIo::new(stream)), connection).await
                            } else {
                                exchange(Peer::Tcp(socket), connection).await
                            }
                        });
                    }
                }
            };
            peers.abort_all();
            while peers.join_next().await.is_some() {}
            result
        });
        Self {
            tasks,
            stop,
            active,
            accepted,
        }
    }
}
impl Peer {
    pub(super) fn session_reused(&self) -> bool {
        match self {
            Self::Tcp(_) => false,
            Self::Tls(stream) => stream.inner().ssl().session_reused(),
        }
    }
}

pub(super) struct Reply {
    pub(super) first: &'static [u8],
    pub(super) remainder: Option<oneshot::Receiver<&'static [u8]>>,
}
pub(super) async fn read_head(
    stream: &mut (impl AsyncRead + Unpin),
) -> io::Result<Option<Vec<u8>>> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        let count = match stream.read(&mut byte).await {
            Ok(count) => count,
            // The client owns shutdown; TLS Drop need not send close_notify.
            // Only an empty between-request read may treat this as teardown.
            Err(error)
                if head.is_empty()
                    && matches!(
                        error.kind(),
                        io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset
                    ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        if count == 0 {
            return if head.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::other("fixture incomplete request"))
            };
        }
        head.push(byte[0]);
        if head.len() > 16384 {
            return Err(io::Error::other("fixture header bound"));
        }
    }
    Ok(Some(head))
}

pub(super) async fn write_reply(
    stream: &mut (impl AsyncWrite + Unpin),
    receive: oneshot::Receiver<Reply>,
) -> io::Result<bool> {
    let Ok(reply) = receive.await else {
        return Ok(false); // Scenario unwound before authorizing a reply.
    };
    stream.write_all(reply.first).await?;
    stream.flush().await?;
    if let Some(remainder) = reply.remainder {
        let Ok(remainder) = remainder.await else {
            return Ok(false); // Scenario unwound with a partial response.
        };
        stream.write_all(remainder).await?;
        stream.flush().await?;
    }
    Ok(true)
}
