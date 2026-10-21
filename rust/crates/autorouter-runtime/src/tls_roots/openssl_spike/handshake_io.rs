//! Test-only memory-BIO egress boundary. Handshake writes needed for another
//! read are released first; the final flight stays owned until peer acceptance.
//! Accepted Raw TLS may additionally own one bounded ciphertext queue.
use std::{
    future::poll_fn,
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Wake, Waker, ready},
};

use hyper::rt::{Read, ReadBufCursor, Write};
use hyper_util::client::legacy::connect::{Connected, Connection};

const MAX_PENDING: usize = 64 * 1024;
pub(super) const RAW_WRITE_CREDIT: usize = 32 * 1024;

// These are logical callers, not BIO directions: an SSL read can need a write,
// and an SSL write can need a read. Outer TransportIo polls register the role.
#[derive(Clone, Copy)]
pub(super) enum EgressRole {
    Reader,
    Writer,
}

#[derive(Default)]
struct Waiters {
    reader: Option<Arc<Waker>>,
    writer: Option<Arc<Waker>>,
}

#[derive(Default)]
struct Readiness(Mutex<Waiters>);

impl Readiness {
    fn register(&self, role: EgressRole, cx: &Context<'_>) {
        let next = Arc::new(cx.waker().clone());
        let old = {
            let mut waiters = self.0.lock().unwrap_or_else(|error| error.into_inner());
            match role {
                EgressRole::Reader => waiters.reader.replace(next),
                EgressRole::Writer => waiters.writer.replace(next),
            }
        };
        drop(old);
    }

    fn clear(&self, role: EgressRole) {
        let old = {
            let mut waiters = self.0.lock().unwrap_or_else(|error| error.into_inner());
            match role {
                EgressRole::Reader => waiters.reader.take(),
                EgressRole::Writer => waiters.writer.take(),
            }
        };
        drop(old);
    }

    fn clear_all(&self) {
        let old = {
            let mut waiters = self.0.lock().unwrap_or_else(|error| error.into_inner());
            (waiters.reader.take(), waiters.writer.take())
        };
        drop(old);
    }
}

impl Wake for Readiness {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let (reader, writer) = {
            let waiters = self.0.lock().unwrap_or_else(|error| error.into_inner());
            (waiters.reader.clone(), waiters.writer.clone())
        };
        // Only Arc clones occur under the mutex. Waker clone/drop/wake calls
        // may invoke arbitrary code and therefore always happen outside it.
        if let Some(reader) = reader {
            reader.wake_by_ref();
        }
        if let Some(writer) = writer {
            writer.wake_by_ref();
        }
    }
}

struct RawEgress {
    readiness: Arc<Readiness>,
    waker: Waker,
    shutdown_started: bool,
    finalizing: bool,
    finalization_write_failed: bool,
}

impl RawEgress {
    fn new() -> Self {
        let readiness = Arc::new(Readiness::default());
        Self {
            waker: Waker::from(readiness.clone()),
            readiness,
            shutdown_started: false,
            finalizing: false,
            finalization_write_failed: false,
        }
    }
}

impl Drop for RawEgress {
    fn drop(&mut self) {
        self.readiness.clear_all();
    }
}

pub(super) struct HandshakeIo<S> {
    inner: S,
    pending: Vec<u8>,
    written: usize,
    handshaking: bool,
    shutdown_entered: bool,
    raw_egress: Option<RawEgress>,
}

impl<S> HandshakeIo<S> {
    // Test fixtures may duplicate this exact owned socket to apply a real OS
    // fault without first draining the ciphertext queue. No runtime accessor.
    #[cfg(test)]
    pub(super) fn inner_transport(&self) -> &S {
        &self.inner
    }

    pub(super) fn tls_finalization_started(&self) -> bool {
        self.raw_egress
            .as_ref()
            .is_some_and(|egress| egress.finalizing)
    }

    // The caller has finished every admitted application SSL_write, drained
    // its ciphertext and physically flushed. Only subsequent TLS-generated
    // ciphertext can cross this fence; new application admission is forbidden.
    pub(super) fn begin_tls_finalization(&mut self) {
        assert_eq!(
            self.egress_queued_bytes(),
            0,
            "application output must settle first"
        );
        if let Some(egress) = &mut self.raw_egress {
            assert!(!egress.finalizing, "TLS finalization starts once");
            egress.finalizing = true;
        }
    }

    pub(super) fn finalization_write_failed(&self) -> bool {
        self.raw_egress
            .as_ref()
            .is_some_and(|egress| egress.finalization_write_failed)
    }

    pub(super) fn discard_failed_finalization(&mut self) {
        let egress = self.raw_egress.as_mut().expect("accepted Raw egress");
        assert!(egress.finalizing && egress.finalization_write_failed && !self.shutdown_entered);
        self.pending.clear();
        self.written = 0;
        egress.finalization_write_failed = false;
    }

    // This scalar witnesses only entry into the underlying shutdown operation.
    // Reset before each TLS poll so an earlier attempt cannot classify it.
    pub(super) fn reset_shutdown_entry(&mut self) {
        self.shutdown_entered = false;
    }

    pub(super) fn shutdown_entered(&self) -> bool {
        self.shutdown_entered
    }

    // This separate phase starts before draining close-notify ciphertext. It
    // must not be reported as physical TCP shutdown entry while drain is Pending.
    pub(super) fn lower_shutdown_started(&self) -> bool {
        self.shutdown_entered
            || self
                .raw_egress
                .as_ref()
                .is_some_and(|egress| egress.shutdown_started)
    }

    pub(super) fn enable_raw_egress(&mut self) {
        assert!(
            !self.handshaking,
            "peer acceptance must release the final flight first"
        );
        assert!(self.pending.is_empty() && self.written == 0);
        assert!(self.raw_egress.is_none(), "Raw egress is enabled once");
        self.raw_egress = Some(RawEgress::new());
    }

    pub(super) fn register_egress_waiter(&self, role: EgressRole, cx: &Context<'_>) {
        if let Some(egress) = &self.raw_egress {
            // The aggregate is for socket registration, never a logical role.
            // Retaining it here would make its own wake recurse indefinitely.
            if cx.waker().will_wake(&egress.waker) {
                return;
            }
            egress.readiness.register(role, cx);
        }
    }

    pub(super) fn clear_egress_waiter(&self, role: EgressRole) {
        if let Some(egress) = &self.raw_egress {
            egress.readiness.clear(role);
        }
    }

    pub(super) fn fail_egress_waiters(&self) {
        if let Some(egress) = &self.raw_egress {
            egress.waker.wake_by_ref();
            egress.readiness.clear_all();
        }
    }

    pub(super) fn egress_queued_bytes(&self) -> usize {
        if self.raw_egress.is_some() {
            self.pending.len() - self.written
        } else {
            0
        }
    }

    fn egress_waker(&self) -> Option<Waker> {
        self.raw_egress.as_ref().map(|egress| egress.waker.clone())
    }

    pub(super) fn new(inner: S) -> Self {
        Self {
            inner,
            pending: Vec::new(),
            written: 0,
            handshaking: true,
            shutdown_entered: false,
            raw_egress: None,
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

    // At most MAX_PENDING bytes are retried per poll; every successful step
    // consumes positive bytes. No background task, self-wake or polling loop
    // waits for unavailable socket readiness.
    fn drain_ciphertext(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.raw_egress.as_mut().unwrap().finalization_write_failed = false;
        let mut progressed = false;
        let result = loop {
            if self.written == self.pending.len() {
                self.pending.clear();
                self.written = 0;
                break Poll::Ready(Ok(()));
            }
            match Pin::new(&mut self.inner).poll_write(cx, &self.pending[self.written..]) {
                Poll::Pending => break Poll::Pending,
                Poll::Ready(Err(error)) => {
                    let egress = self.raw_egress.as_mut().unwrap();
                    egress.finalization_write_failed = egress.finalizing;
                    break Poll::Ready(Err(error));
                }
                Poll::Ready(Ok(0)) => {
                    break Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
                }
                Poll::Ready(Ok(count)) => {
                    if count > self.pending.len() - self.written {
                        break Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "TLS ciphertext count exceeds the queued bytes",
                        )));
                    }
                    self.written += count;
                    progressed = true;
                }
            }
        };
        if progressed {
            // A reader may have freed a writer's memory credit without a new
            // socket readiness edge. Wake both bounded logical roles once.
            self.raw_egress.as_ref().unwrap().waker.wake_by_ref();
        }
        result
    }

    pub(super) fn poll_egress_drain(&mut self, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let Some(waker) = self.egress_waker() else {
            return Poll::Ready(Ok(()));
        };
        self.drain_ciphertext(&mut Context::from_waker(&waker))
    }

    // Call before each NEW capped application SSL_write, not before a retry
    // of an already-started SSL operation. A slot driver can start more than
    // one SSL operation in a single outer poll, so one preflight is insufficient.
    pub(super) fn poll_egress_credit(
        &mut self,
        cx: &mut Context<'_>,
        minimum: usize,
    ) -> Poll<io::Result<()>> {
        if self.raw_egress.is_none() {
            return Poll::Ready(Ok(()));
        }
        assert!(minimum <= MAX_PENDING);
        if let Poll::Ready(Err(error)) = self.poll_egress_drain(cx) {
            return Poll::Ready(Err(error));
        }
        if MAX_PENDING - self.egress_queued_bytes() >= minimum {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }

    // BIO_flush below SSL only acknowledges owned memory. Outer flush and
    // shutdown must explicitly drive this real socket drain+flush boundary.
    pub(super) fn poll_egress_flush(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        ready!(self.poll_egress_drain(cx))?;
        match self.egress_waker() {
            Some(waker) => Pin::new(&mut self.inner).poll_flush(&mut Context::from_waker(&waker)),
            None => Pin::new(&mut self.inner).poll_flush(cx),
        }
    }

    fn queue_ciphertext(&mut self, bytes: &[u8]) {
        if self.written != 0 {
            self.pending.copy_within(self.written.., 0);
            self.pending.truncate(self.pending.len() - self.written);
            self.written = 0;
        }
        if !bytes.is_empty() {
            if self.pending.capacity() == 0 {
                self.pending = Vec::with_capacity(MAX_PENDING);
            }
            self.pending.extend_from_slice(bytes);
            // A reader already awaiting a response must repoll its opportunistic
            // drain when a writer adds request ciphertext to the memory BIO.
            self.raw_egress.as_ref().unwrap().waker.wake_by_ref();
        }
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
        match this.egress_waker() {
            Some(waker) => {
                Pin::new(&mut this.inner).poll_read(&mut Context::from_waker(&waker), buffer)
            }
            None => Pin::new(&mut this.inner).poll_read(cx, buffer),
        }
    }
}

impl<S: Write + Unpin> Write for HandshakeIo<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.raw_egress.is_some() {
            if bytes.len() > MAX_PENDING {
                return Poll::Ready(Err(io::Error::other(
                    "Synthetic TLS ciphertext write exceeds bounded egress",
                )));
            }
            if bytes.len() > MAX_PENDING - this.egress_queued_bytes() {
                ready!(this.poll_egress_drain(cx))?;
            }
            this.queue_ciphertext(bytes);
            return Poll::Ready(Ok(bytes.len()));
        }
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
        if this.handshaking || this.raw_egress.is_some() {
            // Memory BIO acknowledgement, not proof of physical delivery.
            Poll::Ready(Ok(()))
        } else {
            Pin::new(&mut this.inner).poll_flush(cx)
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Teardown of a rejected peer cannot flush unaccepted final-flight
        // bytes. Accepted Raw egress must deliver its queued close-notify first.
        let this = self.get_mut();
        if let Some(egress) = &mut this.raw_egress {
            egress.shutdown_started = true;
            ready!(this.poll_egress_flush(cx))?;
        }
        this.shutdown_entered = true;
        match this.egress_waker() {
            Some(waker) => {
                Pin::new(&mut this.inner).poll_shutdown(&mut Context::from_waker(&waker))
            }
            None => Pin::new(&mut this.inner).poll_shutdown(cx),
        }
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

#[cfg(test)]
#[path = "raw_egress_tests.rs"]
mod raw_egress_tests;
