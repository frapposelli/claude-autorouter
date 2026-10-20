//! Test-only memory-BIO egress boundary. Handshake writes needed for another
//! read are released first; the final flight stays owned until peer acceptance.
//! No timer, extra read, retry, certificate policy or session publication here.
use std::{
    future::poll_fn,
    io,
    pin::Pin,
    task::{Context, Poll, ready},
};

use hyper::rt::{Read, ReadBufCursor, Write};
use hyper_util::client::legacy::connect::{Connected, Connection};

const MAX_PENDING: usize = 64 * 1024;

pub(super) struct HandshakeIo<S> {
    inner: S,
    pending: Vec<u8>,
    written: usize,
    handshaking: bool,
}

impl<S> HandshakeIo<S> {
    pub(super) fn new(inner: S) -> Self {
        Self {
            inner,
            pending: Vec::new(),
            written: 0,
            handshaking: true,
        }
    }
}

impl<S: Write + Unpin> HandshakeIo<S> {
    fn drain(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.written < self.pending.len() {
            let count =
                ready!(Pin::new(&mut self.inner).poll_write(cx, &self.pending[self.written..]))?;
            if count == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
            self.written += count;
        }
        ready!(Pin::new(&mut self.inner).poll_flush(cx))?;
        self.pending.clear();
        self.written = 0;
        Poll::Ready(Ok(()))
    }

    // The caller has checked the chain, exact hostname, ephemeral key and
    // offered session. Cancellation/error still drops this exact socket and
    // its pending bytes; no other owner can publish them or cache acceptance.
    pub(super) async fn release(&mut self) -> io::Result<()> {
        poll_fn(|cx| self.drain(cx)).await?;
        self.handshaking = false;
        self.pending = Vec::new();
        Ok(())
    }
}

impl<S: Read + Write + Unpin> Read for HandshakeIo<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let this = self.get_mut();
        if this.handshaking {
            // This read cannot make progress until our preceding flight reaches
            // the peer. A read after SSL_connect succeeds is never requested.
            ready!(this.drain(cx))?;
        }
        Pin::new(&mut this.inner).poll_read(cx, buffer)
    }
}

impl<S: Write + Unpin> Write for HandshakeIo<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if !this.handshaking {
            return Pin::new(&mut this.inner).poll_write(cx, bytes);
        }
        if bytes.len() > MAX_PENDING - this.pending.len() {
            return Poll::Ready(Err(io::Error::other(
                "Synthetic TLS handshake flight exceeds bounded egress",
            )));
        }
        if !bytes.is_empty() {
            // Allocate a fixed maximum once, rather than letting Vec growth
            // reserve more than the declared per-handshake payload budget.
            if this.pending.capacity() == 0 {
                this.pending = Vec::with_capacity(MAX_PENDING);
            }
            this.pending.extend_from_slice(bytes);
        }
        Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.handshaking {
            // BIO_flush acknowledges the owned memory buffer. Peer acceptance
            // releases the final flight; a preceding read drains other flights.
            Poll::Ready(Ok(()))
        } else {
            Pin::new(&mut this.inner).poll_flush(cx)
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // In particular, teardown of a rejected peer must not flush pending
        // final-handshake bytes as a side effect of closing the transport.
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

impl<S: Connection> Connection for HandshakeIo<S> {
    fn connected(&self) -> Connected {
        self.inner.connected()
    }
}

#[cfg(test)]
#[path = "handshake_io_tests.rs"]
mod tests;
