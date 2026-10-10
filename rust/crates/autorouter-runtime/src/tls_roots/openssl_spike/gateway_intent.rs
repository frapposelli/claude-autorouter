//! Opt-in gateway cause observation. This entire module is library-test-only.
//! IO observation precedes Hyper teardown; ordinary Drop grants no authority.
use super::abort::{self, Cause as AbortCause, Handle, Outcome, Terminal};
use super::gateway_terminal::{PauseGate, PauseKind, PauseRegistration, RequestTerminal};
use hyper::http::Extensions;
use hyper_util::client::legacy::connect::CaptureAssignment;
use std::{
    io,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::Notify,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Disconnect {
    ReadEof,
    ReadError,
    WriteError,
    FlushError,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Cause {
    Downstream(Disconnect),
    Deadline,
    UpstreamFailure,
    Delivered,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Transport {
    Live,
    Missing,
    Ordinary,
    IoError,
    Explicit,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Action {
    NoAssignment,
    Retired,
    Requested,
    AlreadyClosed,
    Missing,
    ShutdownFailed,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Event {
    Registered,
    CaptureInstalled,
    BodyHeld,
    Terminal(Cause),
    Suppressed(Cause),
    Transport(Transport),
    Action(Action),
    UpstreamEof,
    GenericCancellation,
    DeliveryFailed,
    Released,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Row {
    pub id: usize,
    pub event: Event,
}
#[derive(Default)]
struct Trace {
    next: AtomicUsize,
    live: AtomicUsize,
    rows: Mutex<Vec<Row>>,
    changed: Notify,
    reads_held: AtomicBool,
    read_waker: Mutex<Option<std::task::Waker>>,
    bodies_held: PauseGate,
    body_wakers: Mutex<Vec<std::task::Waker>>,
    intents: Mutex<Vec<Weak<Intent>>>,
}
#[derive(Clone, Default)]
pub(crate) struct Probe(Arc<Trace>);
impl Probe {
    fn record(&self, id: usize, event: Event) {
        {
            let mut rows = self.0.rows.lock().unwrap();
            assert!(rows.len() < 4096, "gateway intent trace bound");
            rows.push(Row { id, event });
        }
        self.0.changed.notify_waiters();
    }
    pub(crate) fn hold_bodies(&self) {
        self.0.bodies_held.hold();
    }
    pub(crate) fn release_bodies(&self) {
        self.0.bodies_held.release();
        let wakers = std::mem::take(&mut *self.0.body_wakers.lock().unwrap());
        for waker in wakers {
            waker.wake();
        }
    }
    pub(crate) fn intent(&self, id: usize) -> Option<Arc<Intent>> {
        let intents = self.0.intents.lock().unwrap().clone();
        intents
            .into_iter()
            .filter_map(|intent| intent.upgrade())
            .find(|intent| intent.id == id)
    }
    pub(crate) fn hold_reads(&self) {
        self.0.reads_held.store(true, Ordering::SeqCst);
    }
    pub(crate) fn release_reads(&self) {
        self.0.reads_held.store(false, Ordering::SeqCst);
        let waker = self.0.read_waker.lock().unwrap().take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
    pub(crate) fn rows(&self) -> Vec<Row> {
        self.0.rows.lock().unwrap().clone()
    }
    pub(crate) fn live(&self) -> usize {
        self.0.live.load(Ordering::SeqCst)
    }
    pub(crate) async fn wait(&self, predicate: impl Fn(&[Row]) -> bool) {
        loop {
            let changed = self.0.changed.notified();
            if predicate(&self.rows()) {
                return;
            }
            changed.await;
        }
    }
}

pub(crate) struct Intent {
    body_pause: Mutex<Option<PauseRegistration>>,
    id: usize,
    probe: Probe,
    terminal: Mutex<Option<Cause>>,
    capture: OnceLock<CaptureAssignment>,
}
impl Intent {
    pub(crate) fn bind_terminal(&self, terminal: &RequestTerminal) {
        match self
            .probe
            .0
            .bodies_held
            .bind(terminal.clone(), PauseKind::Body)
        {
            Ok(registration) => {
                *self.body_pause.lock().unwrap() = Some(registration);
            }
            Err(()) => {
                terminal.fail(super::gateway_terminal::FailureCause::Cancelled);
            }
        }
    }
    pub(crate) fn install(&self, capture: CaptureAssignment) {
        assert!(self.capture.set(capture).is_ok(), "intent submitted twice");
        self.probe.record(self.id, Event::CaptureInstalled);
    }
    pub(crate) fn body_ready(&self, cx: &mut Context<'_>) -> bool {
        if !self.probe.0.bodies_held.held() {
            return true;
        }
        let waker = cx.waker().clone();
        {
            let mut wakers = self.probe.0.body_wakers.lock().unwrap();
            assert!(wakers.len() < 128, "body gate wake bound");
            wakers.push(waker);
        }
        if !self.probe.0.bodies_held.held() {
            return true;
        }
        self.observe(Event::BodyHeld);
        false
    }
    pub(crate) fn transport(&self) -> Transport {
        let Some(assignment) = self.capture.get().and_then(CaptureAssignment::assignment) else {
            return Transport::Missing;
        };
        let mut extras = Extensions::new();
        assignment.get_extras(&mut extras);
        extras
            .remove::<Handle>()
            .map_or(Transport::Missing, |handle| {
                if !handle.is_alive() {
                    Transport::Missing
                } else {
                    match handle.terminal() {
                        None => Transport::Live,
                        Some(Terminal::Ordinary) => Transport::Ordinary,
                        Some(Terminal::IoError) => Transport::IoError,
                        Some(Terminal::Abort(_)) => Transport::Explicit,
                    }
                }
            })
    }
    pub(crate) fn observe(&self, event: Event) {
        self.probe.record(self.id, event);
    }
    pub(crate) fn finish(&self, cause: Cause) {
        let first = {
            let mut terminal = self.terminal.lock().unwrap();
            if terminal.is_some() {
                false
            } else {
                *terminal = Some(cause);
                true
            }
        };
        self.observe(if first {
            Event::Terminal(cause)
        } else {
            Event::Suppressed(cause)
        });
        if !first || cause == Cause::Delivered {
            return;
        }
        // Immutable first cause is selected; all metadata, claim and physical IO
        // operations happen after the context lock is released. Lease retirement
        // independently synchronizes its own exclusive poison operation.
        let Some(assignment) = self.capture.get().and_then(CaptureAssignment::assignment) else {
            if cause != Cause::UpstreamFailure {
                self.observe(Event::Action(Action::NoAssignment));
            }
            return;
        };
        let transport = self.transport();
        self.observe(Event::Transport(transport));
        if cause == Cause::UpstreamFailure {
            return;
        }
        let Some(claim) = assignment.claim_abort() else {
            self.observe(Event::Action(Action::Retired));
            return;
        };
        let outcome = abort::consume(
            claim,
            if cause == Cause::Deadline {
                AbortCause::ExplicitFixtureDeadline
            } else {
                AbortCause::ExplicitFixtureCancellation
            },
        );
        self.observe(Event::Action(match outcome {
            Outcome::Requested => Action::Requested,
            Outcome::AlreadyClosed(_) => Action::AlreadyClosed,
            Outcome::MissingTransport => Action::Missing,
            Outcome::ShutdownFailed(_) => Action::ShutdownFailed,
        }));
    }
}
impl Drop for Intent {
    fn drop(&mut self) {
        self.probe.0.live.fetch_sub(1, Ordering::SeqCst);
        self.probe.record(self.id, Event::Released);
    }
}
#[derive(Default)]
struct Entries {
    closed: bool,
    requests: Vec<Weak<Intent>>,
}
#[derive(Clone)]
pub(crate) struct Registry {
    entries: Arc<Mutex<Entries>>,
    probe: Probe,
    bound: usize,
}
impl Registry {
    pub(crate) fn new(probe: Probe, bound: usize) -> Self {
        assert!(bound > 0);
        Self {
            entries: Arc::new(Mutex::new(Entries::default())),
            probe,
            bound,
        }
    }
    pub(crate) fn register(&self) -> Result<Arc<Intent>, ()> {
        let mut entries = self.entries.lock().unwrap();
        entries.requests.retain(|entry| entry.strong_count() != 0);
        if entries.closed || entries.requests.len() == self.bound {
            return Err(());
        }
        let intent = Arc::new(Intent {
            body_pause: Mutex::new(None),
            id: self.probe.0.next.fetch_add(1, Ordering::SeqCst),
            probe: self.probe.clone(),
            terminal: Mutex::new(None),
            capture: OnceLock::new(),
        });
        self.probe.0.live.fetch_add(1, Ordering::SeqCst);
        entries.requests.push(Arc::downgrade(&intent));
        drop(entries);
        {
            let mut intents = self.probe.0.intents.lock().unwrap();
            intents.retain(|intent| intent.strong_count() != 0);
            assert!(intents.len() < 128, "fixture intent inspection bound");
            intents.push(Arc::downgrade(&intent));
        }
        intent.observe(Event::Registered);
        Ok(intent)
    }
    fn disconnect(&self, cause: Disconnect) {
        let active = {
            let mut entries = self.entries.lock().unwrap();
            entries.closed = true;
            entries
                .requests
                .iter()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>()
        };
        for intent in active {
            intent.finish(Cause::Downstream(cause));
        }
    }
}

pub(crate) struct IntentIo<I> {
    inner: I,
    registry: Option<Registry>,
}
impl<I> IntentIo<I> {
    pub(crate) fn new(inner: I, registry: Option<Registry>) -> Self {
        Self { inner, registry }
    }
    fn disconnect(&self, cause: Disconnect) {
        if let Some(registry) = &self.registry {
            registry.disconnect(cause);
        }
    }
}
impl<I: AsyncRead + Unpin> AsyncRead for IntentIo<I> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Some(registry) = &this.registry
            && registry.probe.0.reads_held.load(Ordering::SeqCst)
        {
            let waker = cx.waker().clone();
            let old = registry.probe.0.read_waker.lock().unwrap().replace(waker);
            drop(old);
            if registry.probe.0.reads_held.load(Ordering::SeqCst) {
                return Poll::Pending;
            }
        }
        let before = buf.filled().len();
        let capacity = buf.remaining();
        let result = Pin::new(&mut this.inner).poll_read(cx, buf);
        match &result {
            Poll::Ready(Err(_)) => this.disconnect(Disconnect::ReadError),
            Poll::Ready(Ok(())) if capacity != 0 && buf.filled().len() == before => {
                this.disconnect(Disconnect::ReadEof)
            }
            _ => {}
        }
        result
    }
}
impl<I: AsyncWrite + Unpin> AsyncWrite for IntentIo<I> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_write(cx, buf);
        if matches!(result, Poll::Ready(Err(_)))
            || (!buf.is_empty() && matches!(result, Poll::Ready(Ok(0))))
        {
            this.disconnect(Disconnect::WriteError);
        }
        result
    }
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_write_vectored(cx, bufs);
        if matches!(result, Poll::Ready(Err(_)))
            || (bufs.iter().any(|b| !b.is_empty()) && matches!(result, Poll::Ready(Ok(0))))
        {
            this.disconnect(Disconnect::WriteError);
        }
        result
    }
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_flush(cx);
        if matches!(result, Poll::Ready(Err(_))) {
            this.disconnect(Disconnect::FlushError);
        }
        result
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}
