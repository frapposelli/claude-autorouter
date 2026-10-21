//! Accepted Raw TLS owns bounded output until SSL and the socket consume it.
//! Buffered acceptance is never evidence of successful provider forwarding.
use std::{
    io,
    pin::Pin,
    task::{Context, Poll, ready},
};

use hyper::rt::{Read, Write};
use hyper_openssl::SslStream;
use hyper_util::rt::TokioIo;
use openssl::ssl::SslWant;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::handshake_io::{EgressRole, HandshakeIo, RAW_WRITE_CREDIT};
use super::tls_application_slot::Slot;

type Tls<S> = TokioIo<SslStream<HandshakeIo<S>>>;

// The slot may hold admitted bytes BEFORE credit is available, or immutable
// retry bytes for an actual SSL_write. Only the latter owns an SSL operation.
#[derive(Default)]
pub(super) struct RawOutput {
    slot: Slot,
    inflight: Option<SslWant>,
    failed: bool,
}

pub(super) fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "TLS transport previously failed")
}

impl RawOutput {
    pub(super) fn failed(&self) -> bool {
        self.failed
    }

    pub(super) fn poison<S>(&mut self, io: &Tls<S>) {
        if !self.failed {
            self.failed = true;
            io.inner().get_ref().fail_egress_waiters();
        }
    }

    #[cfg(test)]
    pub(super) fn state(&self) -> (usize, usize, bool) {
        (
            self.slot.retained_bytes(),
            self.slot.pending_bytes(),
            self.inflight.is_some(),
        )
    }

    pub(super) fn poll_write<S: Read + Write + Unpin>(
        &mut self,
        io: &mut Tls<S>,
        cx: &mut Context<'_>,
        input: &[u8],
    ) -> Poll<io::Result<usize>> {
        // Zero-length operations neither settle nor abandon an SSL retry.
        if input.is_empty() {
            return Poll::Ready(Ok(0));
        }
        if self.failed || io.inner().get_ref().tls_finalization_started() {
            return Poll::Ready(Err(closed()));
        }
        io.inner()
            .get_ref()
            .register_egress_waiter(EgressRole::Writer, cx);
        let result = self.slot.poll_write(
            &mut ApplicationWriter {
                io,
                inflight: &mut self.inflight,
            },
            cx,
            input,
        );
        if result.is_ready() {
            io.inner().get_ref().clear_egress_waiter(EgressRole::Writer);
        }
        if matches!(result, Poll::Ready(Err(_))) {
            self.poison(io);
        }
        result
    }

    pub(super) fn poll_write_vectored<S: Read + Write + Unpin>(
        &mut self,
        io: &mut Tls<S>,
        cx: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match buffers.iter().find(|buffer| !buffer.is_empty()) {
            Some(input) => self.poll_write(io, cx, input),
            None => Poll::Ready(Ok(0)),
        }
    }

    pub(super) fn poll_read<S: Read + Write + Unpin>(
        &mut self,
        io: &mut Tls<S>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        if self.failed {
            return Poll::Ready(Err(closed()));
        }
        io.inner()
            .get_ref()
            .register_egress_waiter(EgressRole::Reader, cx);
        let result = (|| {
            // Pending physical output alone must not block an early response.
            if let Poll::Ready(Err(error)) = io.inner_mut().get_mut().poll_egress_drain(cx) {
                return Poll::Ready(Err(error));
            }
            if self.inflight.is_some() {
                match self.slot.poll_drain(
                    &mut ApplicationWriter {
                        io,
                        inflight: &mut self.inflight,
                    },
                    cx,
                ) {
                    Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                    Poll::Pending if self.inflight.is_some_and(|want| want != SslWant::Reading) => {
                        return Poll::Pending;
                    }
                    _ => {}
                }
            }
            // SSL_write WANT_READ permits this interleaving. WANT_WRITE and
            // unfamiliar states permit only the exact owned write retry above.
            openssl::error::clear_initialized();
            Pin::new(&mut *io).poll_read(cx, buffer)
        })();
        if result.is_ready() {
            io.inner().get_ref().clear_egress_waiter(EgressRole::Reader);
        }
        if matches!(result, Poll::Ready(Err(_))) {
            self.poison(io);
        }
        result
    }

    // This boundary must complete before SSL_shutdown starts. Errors here are
    // application/socket failures, never finalization-only errors to absorb.
    pub(super) fn poll_flush<S: Read + Write + Unpin>(
        &mut self,
        io: &mut Tls<S>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        if self.failed {
            return Poll::Ready(Err(closed()));
        }
        io.inner()
            .get_ref()
            .register_egress_waiter(EgressRole::Writer, cx);
        let result = (|| {
            ready!(self.slot.poll_drain(
                &mut ApplicationWriter {
                    io,
                    inflight: &mut self.inflight
                },
                cx
            ))?;
            io.inner_mut().get_mut().poll_egress_flush(cx)
        })();
        if result.is_ready() {
            io.inner().get_ref().clear_egress_waiter(EgressRole::Writer);
        }
        if matches!(result, Poll::Ready(Err(_))) {
            self.poison(io);
        }
        result
    }
}

struct ApplicationWriter<'a, S> {
    io: &'a mut Tls<S>,
    inflight: &'a mut Option<SslWant>,
}

impl<S: Read + Write + Unpin> AsyncWrite for ApplicationWriter<'_, S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.inflight.is_none() {
            // Every NEW SSL operation (including a partial-write suffix) needs
            // credit. Do not reserve again for an actual in-flight retry.
            ready!(
                this.io
                    .inner_mut()
                    .get_mut()
                    .poll_egress_credit(cx, RAW_WRITE_CREDIT)
            )?;
        } else if let Poll::Ready(Err(error)) = this.io.inner_mut().get_mut().poll_egress_drain(cx)
        {
            return Poll::Ready(Err(error));
        }
        openssl::error::clear_initialized();
        let result = Pin::new(&mut *this.io).poll_write(cx, bytes);
        *this.inflight = if result.is_pending() {
            // hyper-openssl has already called SSL_get_error synchronously.
            // Sample only actual SSL Pending, never pre-credit Pending.
            Some(this.io.inner().ssl().want())
        } else {
            None
        };
        // Once SSL accepts bytes, preserve that exact count. Physical drain
        // belongs before a later write, or to read/flush/shutdown; an error
        // after this point cannot replace successful buffered byte credit.
        result
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        unreachable!("application slot only retries writes")
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        unreachable!("application slot cannot finalize TLS")
    }
}
