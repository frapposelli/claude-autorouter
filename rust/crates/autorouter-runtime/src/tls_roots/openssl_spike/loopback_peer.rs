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
