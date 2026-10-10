//! Explicit test-only downstream terminal ownership. Publication is weak and
//! generation checked; only a connection owner can end its serve_http1 future.
use super::gateway_intent::Disconnect;
use crate::transport_completion::{Delivery, Failure};
use std::collections::BTreeMap;
use std::io::{self, IoSlice};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[path = "gateway_terminal_handoff.rs"]
pub(super) mod handoff;
#[path = "gateway_terminal_writer.rs"]
mod writer;
pub(crate) use handoff::{DataProducerOwner, PauseGate, PauseKind, PauseRegistration};

pub(super) fn safe_cancel(token: &CancellationToken) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| token.cancel()));
}
fn notify_one(notify: &Notify) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| notify.notify_one()));
}
fn notify_all(notify: &Notify) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| notify.notify_waiters()));
}

static CONNECTION: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailureCause {
    Upstream,
    Deadline,
    Downstream(Disconnect),
    Cancelled,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Identity {
    pub connection: u64,
    pub request: u64,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Event {
    Registered(Identity),
    Attached(Identity),
    Failed(Identity, FailureCause),
    DeliveryClaimed(Identity, Delivery),
    CallbackFinished(Identity),
    CallbackAbandoned(Identity),
    Retired(Identity),
    ConnectionClaimed(Identity),
    Closed(u64),
    WriteHeld(u64),
}
#[derive(Default)]
struct ProbeState {
    events: Vec<Event>,
    handoff_failures: Vec<(Identity, handoff::HandoffSnapshot)>,
    observer_failed: bool,
}
#[derive(Clone, Default)]
pub(crate) struct Probe {
    state: Arc<Mutex<ProbeState>>,
    tasks: Arc<AtomicUsize>,
    writer: writer::WriterProbe,
}
impl Probe {
    pub(crate) fn hold_writes(&self) {
        self.writer.hold();
    }
    pub(crate) fn release_writes(&self) {
        self.writer.release();
    }
    pub(crate) fn writer_snapshot(&self) -> writer::WriterSnapshot {
        self.writer.snapshot()
    }
    pub(crate) fn handoff_failures(&self) -> Vec<(Identity, handoff::HandoffSnapshot)> {
        self.state.lock().unwrap().handoff_failures.clone()
    }
    pub(crate) fn observer_failed(&self) -> bool {
        self.state.lock().unwrap().observer_failed || self.writer.snapshot().observer_failed
    }
    fn handoff_failure(&self, identity: Identity, snapshot: handoff::HandoffSnapshot) {
        let mut state = self.state.lock().unwrap();
        if state.handoff_failures.len() == 128 {
            state.observer_failed = true;
        } else {
            state.handoff_failures.push((identity, snapshot));
        }
    }
    fn record(&self, event: Event) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        // Test fixtures inspect a bounded suffix; this observer never controls
        // the connection's failure latch or allocation/admission authority.
        if state.events.len() == 1024 {
            state.events.remove(0);
        }
        state.events.push(event);
    }
    pub(crate) fn events(&self) -> Vec<Event> {
        self.state.lock().unwrap().events.clone()
    }
    pub(crate) fn tasks(&self) -> usize {
        self.tasks.load(Ordering::SeqCst)
    }
}
type CauseCallback = Box<dyn FnOnce(FailureCause) + Send>;
struct Record {
    handoff: handoff::HandoffState,
    attached: bool,
    failure: Option<FailureCause>,
    deadline: Option<Instant>,
    token: Option<CancellationToken>,
    cause: Option<CauseCallback>,
    report: Option<CauseCallback>,
}
struct State {
    closed: bool,
    next: u64,
    records: BTreeMap<u64, Record>,
    close_latch: Option<Identity>,
    close_claimed: bool,
}
struct Shared {
    generation: u64,
    limit: usize,
    state: Mutex<State>,
    changed: Notify,
    failed: Notify,
    stop: CancellationToken,
    probe: Probe,
}
struct Callouts {
    sender: Option<handoff::Sender>,
    producer_stop: Option<CancellationToken>,
    cause: FailureCause,
    callback: Option<CauseCallback>,
    report: Option<CauseCallback>,
    token: Option<CancellationToken>,
}
impl Callouts {
    fn run(self) {
        for callback in [self.callback, self.report].into_iter().flatten() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(self.cause)));
        }
        // Cause callbacks must run while upstream abort ownership still exists.
        // Only then release producer/ticket/request cancellation wakes.
        if let Some(stop) = self.producer_stop {
            safe_cancel(&stop);
        }
        handoff::complete_sender(self.sender, Err(()), None);
        if let Some(token) = self.token {
            safe_cancel(&token);
        }
    }
}
impl Shared {
    fn fail(&self, identity: Identity, cause: FailureCause) -> bool {
        self.fail_owned(identity, cause, None)
    }
    fn fail_owned(&self, identity: Identity, cause: FailureCause, producer: Option<u64>) -> bool {
        if identity.connection != self.generation {
            return false;
        }
        let (callouts, handoff) = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return false;
            }
            let Some(record) = state.records.get_mut(&identity.request) else {
                return false;
            };
            if record.failure.is_some()
                || producer.is_some_and(|epoch| !record.handoff.live_producer(epoch))
            {
                return false;
            }
            record.failure = Some(cause);
            let attached = record.attached;
            let handoff = record.handoff.snapshot();
            let callouts = Callouts {
                sender: record.handoff.take_sender(),
                producer_stop: record.handoff.stop(),
                cause,
                callback: record.cause.take(),
                report: if attached { record.report.take() } else { None },
                // A body-source failure after acquired headers must not make
                // the request future lose to cancellation before attachment.
                token: if attached || cause != FailureCause::Upstream {
                    record.token.clone()
                } else {
                    None
                },
            };
            if attached && state.close_latch.is_none() {
                state.close_latch = Some(identity);
            }
            (callouts, handoff)
        };
        self.probe.handoff_failure(identity, handoff);
        self.probe.record(Event::Failed(identity, cause));
        // Panic-contained callbacks complete before wake/drop can destroy a
        // still-owned upstream lease needed by an explicit deadline abort.
        callouts.run();
        notify_one(&self.failed);
        notify_one(&self.changed);
        true
    }
    fn retire(&self, identity: Identity) {
        let record = {
            let mut state = self.state.lock().unwrap();
            if identity.connection != self.generation {
                return;
            }
            state.records.remove(&identity.request)
        };
        if record.is_some() {
            self.probe.record(Event::Retired(identity));
        }
        if let Some(record) = record {
            handoff::retire_record(record);
        }
        notify_one(&self.changed);
    }
    fn disconnect(&self, cause: Disconnect) {
        let identities = {
            let state = self.state.lock().unwrap();
            state
                .records
                .keys()
                .map(|request| Identity {
                    connection: self.generation,
                    request: *request,
                })
                .collect::<Vec<_>>()
        };
        for identity in identities {
            self.fail(identity, FailureCause::Downstream(cause));
        }
    }
}
#[derive(Clone)]
pub(crate) struct ConnectionHandle(Weak<Shared>);
impl ConnectionHandle {
    pub(crate) fn register(&self) -> Result<RequestOwner, ()> {
        let shared = self.0.upgrade().ok_or(())?;
        let identity = {
            let mut state = shared.state.lock().unwrap();
            if state.closed || state.close_latch.is_some() || state.records.len() >= shared.limit {
                return Err(());
            }
            let request = state.next;
            state.next = request.checked_add(1).ok_or(())?;
            state.records.insert(
                request,
                Record {
                    handoff: handoff::HandoffState::default(),
                    attached: false,
                    failure: None,
                    deadline: None,
                    token: None,
                    cause: None,
                    report: None,
                },
            );
            Identity {
                connection: shared.generation,
                request,
            }
        };
        shared.probe.record(Event::Registered(identity));
        Ok(RequestOwner {
            shared,
            identity,
            retired: false,
        })
    }
}
#[derive(Clone)]
pub(crate) struct RequestTerminal {
    shared: Weak<Shared>,
    identity: Identity,
}
impl RequestTerminal {
    pub(crate) fn fail(&self, cause: FailureCause) -> bool {
        self.shared
            .upgrade()
            .is_some_and(|shared| shared.fail(self.identity, cause))
    }
    pub(crate) fn failed(&self) -> bool {
        self.shared.upgrade().is_none_or(|shared| {
            let state = shared.state.lock().unwrap();
            state.closed
                || state
                    .records
                    .get(&self.identity.request)
                    .is_none_or(|record| record.failure.is_some())
        })
    }
}
/// The one admission owner. It is moved into a completion callback, never cloned
/// into request extensions or inferred from reference counts.
pub(crate) struct RequestOwner {
    shared: Arc<Shared>,
    identity: Identity,
    retired: bool,
}
impl RequestOwner {
    pub(crate) fn publisher(&self) -> RequestTerminal {
        RequestTerminal {
            shared: Arc::downgrade(&self.shared),
            identity: self.identity,
        }
    }
    pub(crate) fn start(
        &self,
        deadline: Instant,
        token: CancellationToken,
        cause: impl FnOnce(FailureCause) + Send + 'static,
        report: impl FnOnce(FailureCause) + Send + 'static,
    ) {
        let cause: CauseCallback = Box::new(cause);
        let report: CauseCallback = Box::new(report);
        let callouts = {
            let mut state = self.shared.state.lock().unwrap();
            let record = state
                .records
                .get_mut(&self.identity.request)
                .expect("admitted request");
            assert!(record.deadline.is_none(), "request monitor installed twice");
            record.deadline = Some(deadline);
            record.token = Some(token.clone());
            if let Some(failure) = record.failure {
                record.report = Some(report);
                Some(Callouts {
                    sender: None,
                    producer_stop: None,
                    cause: failure,
                    callback: Some(cause),
                    report: None,
                    token: if record.attached || failure != FailureCause::Upstream {
                        Some(token)
                    } else {
                        None
                    },
                })
            } else {
                record.cause = Some(cause);
                record.report = Some(report);
                None
            }
        };
        if let Some(callouts) = callouts {
            callouts.run();
        }
        notify_one(&self.shared.changed);
    }
    pub(crate) fn attach(&self) {
        let callouts = {
            let mut state = self.shared.state.lock().unwrap();
            let record = state
                .records
                .get_mut(&self.identity.request)
                .expect("live request attachment");
            assert!(!record.attached, "response attached twice");
            record.attached = true;
            let callouts = record.failure.map(|cause| Callouts {
                sender: None,
                producer_stop: None,
                cause,
                callback: None,
                report: record.report.take(),
                token: record.token.clone(),
            });
            if callouts.is_some() && state.close_latch.is_none() {
                state.close_latch = Some(self.identity);
            }
            callouts
        };
        self.shared.probe.record(Event::Attached(self.identity));
        if let Some(callouts) = callouts {
            callouts.run();
            notify_one(&self.shared.failed);
        }
    }
    pub(crate) fn claim_delivery(mut self, delivery: Delivery) -> DeliveryClaim {
        let (record, claimed) = {
            let mut state = self.shared.state.lock().unwrap();
            let record = state.records.remove(&self.identity.request);
            let failed = state.closed
                || state.close_latch.is_some()
                || record
                    .as_ref()
                    .is_none_or(|record| record.failure.is_some());
            let claimed = if failed && delivery == Delivery::Flushed {
                Delivery::Failed(Failure::Body)
            } else {
                delivery
            };
            (record, claimed)
        };
        self.retired = true;
        if let Some(record) = record {
            handoff::retire_record(record);
        }
        notify_one(&self.shared.changed);
        self.shared
            .probe
            .record(Event::DeliveryClaimed(self.identity, claimed));
        DeliveryClaim {
            identity: self.identity,
            delivery: claimed,
            probe: self.shared.probe.clone(),
            finished: false,
        }
    }
}
impl Drop for RequestOwner {
    fn drop(&mut self) {
        if !self.retired {
            self.shared.retire(self.identity);
        }
    }
}
pub(crate) struct DeliveryClaim {
    identity: Identity,
    delivery: Delivery,
    probe: Probe,
    finished: bool,
}
impl DeliveryClaim {
    pub(crate) fn delivery(&self) -> Delivery {
        self.delivery
    }
    pub(crate) fn callback_finished(&mut self) {
        self.finished = true;
        self.probe.record(Event::CallbackFinished(self.identity));
    }
}
impl Drop for DeliveryClaim {
    fn drop(&mut self) {
        if !self.finished {
            self.probe.record(Event::CallbackAbandoned(self.identity));
        }
    }
}
pub(crate) struct ConnectionTerminal {
    shared: Arc<Shared>,
    task: Option<JoinHandle<()>>,
}
impl ConnectionTerminal {
    pub(crate) fn new(probe: Probe, limit: usize) -> Self {
        assert!(limit > 0 && limit <= 16);
        let shared = Arc::new(Shared {
            generation: CONNECTION.fetch_add(1, Ordering::SeqCst),
            limit,
            state: Mutex::new(State {
                closed: false,
                next: 0,
                records: BTreeMap::new(),
                close_latch: None,
                close_claimed: false,
            }),
            changed: Notify::new(),
            failed: Notify::new(),
            stop: CancellationToken::new(),
            probe,
        });
        let monitor = shared.clone();
        monitor.probe.tasks.fetch_add(1, Ordering::SeqCst);
        struct Guard(Probe);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.tasks.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let guard = Guard(monitor.probe.clone());
        let task = tokio::spawn(async move {
            let _guard = guard;
            loop {
                let changed = monitor.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let (expired, next, tokens) = {
                    let state = monitor.state.lock().unwrap();
                    let now = Instant::now();
                    let mut expired = Vec::new();
                    let mut next = None;
                    for (request, record) in &state.records {
                        if record.failure.is_some() {
                            continue;
                        }
                        if record
                            .token
                            .as_ref()
                            .is_some_and(CancellationToken::is_cancelled)
                        {
                            expired.push((
                                Identity {
                                    connection: monitor.generation,
                                    request: *request,
                                },
                                FailureCause::Cancelled,
                            ));
                        } else if let Some(deadline) = record.deadline {
                            if deadline <= now {
                                expired.push((
                                    Identity {
                                        connection: monitor.generation,
                                        request: *request,
                                    },
                                    FailureCause::Deadline,
                                ));
                            } else {
                                next =
                                    Some(next.map_or(deadline, |previous: Instant| {
                                        previous.min(deadline)
                                    }));
                            }
                        }
                    }
                    let tokens = state
                        .records
                        .values()
                        .filter(|record| record.failure.is_none())
                        .filter_map(|record| record.token.clone())
                        .collect::<Vec<_>>();
                    (expired, next, tokens)
                };
                for (identity, cause) in expired {
                    monitor.fail(identity, cause);
                }
                let mut cancelled = tokens
                    .into_iter()
                    .map(|token| Box::pin(token.cancelled_owned()))
                    .collect::<Vec<_>>();
                let cancellation = std::future::poll_fn(|cx| {
                    for token in &mut cancelled {
                        if std::future::Future::poll(token.as_mut(), cx).is_ready() {
                            return Poll::Ready(());
                        }
                    }
                    Poll::Pending
                });
                let deadline = async {
                    if let Some(next) = next {
                        tokio::time::sleep_until(next).await;
                    } else {
                        std::future::pending().await
                    }
                };
                tokio::select! { biased; _ = monitor.stop.cancelled() => break, _ = &mut changed => {}, _ = cancellation => {}, _ = deadline => {} }
            }
        });
        Self {
            shared,
            task: Some(task),
        }
    }
    pub(crate) fn handle(&self) -> ConnectionHandle {
        ConnectionHandle(Arc::downgrade(&self.shared))
    }
    pub(crate) async fn claim_connection_failure(&self) {
        loop {
            let notified = self.shared.failed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let claimed = {
                let mut state = self.shared.state.lock().unwrap();
                if !state.closed && !state.close_claimed {
                    if let Some(identity) = state.close_latch {
                        assert_eq!(identity.connection, self.shared.generation);
                        state.close_claimed = true;
                        Some(identity)
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some(identity) = claimed {
                self.shared.probe.record(Event::ConnectionClaimed(identity));
                return;
            }
            tokio::select! { biased; _ = self.shared.stop.cancelled() => return, _ = &mut notified => {} }
        }
    }
    pub(crate) async fn close(mut self) {
        self.shared.close_records();
        // Retain abort ownership while the monitor join is Pending.
        let _ = self.task.as_mut().unwrap().await;
        self.task.take();
        self.shared
            .probe
            .record(Event::Closed(self.shared.generation));
    }
}
impl Shared {
    fn close_records(&self) {
        let records = {
            let mut state = self.state.lock().unwrap();
            state.closed = true;
            std::mem::take(&mut state.records)
        };
        for (_, record) in records {
            if let Some(token) = &record.token {
                safe_cancel(token);
            }
            handoff::retire_record(record);
        }
        safe_cancel(&self.stop);
        notify_all(&self.changed);
        notify_all(&self.failed);
    }
}
impl Drop for ConnectionTerminal {
    fn drop(&mut self) {
        self.shared.close_records();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

pub(crate) struct TerminalIo<I> {
    inner: I,
    owner: Option<ConnectionHandle>,
    writer: Option<writer::WriterLease>,
}
impl<I> TerminalIo<I> {
    pub(crate) fn new(inner: I, owner: Option<ConnectionHandle>) -> Self {
        let writer = owner
            .as_ref()
            .and_then(|owner| owner.0.upgrade())
            .map(|shared| shared.probe.writer.register(shared.generation));
        Self {
            inner,
            owner,
            writer,
        }
    }
    fn blocked(&self, cx: &Context<'_>, bytes: usize) -> bool {
        let Some(writer) = &self.writer else {
            return false;
        };
        let (blocked, first) = writer.blocked(cx, bytes);
        if first && let Some(shared) = self.owner.as_ref().and_then(|owner| owner.0.upgrade()) {
            shared.probe.record(Event::WriteHeld(shared.generation));
        }
        blocked
    }
    fn wrote(&self, result: &Poll<io::Result<usize>>) {
        if let (Some(writer), Poll::Ready(Ok(bytes))) = (&self.writer, result) {
            writer.wrote(*bytes);
        }
    }
    fn observe(&self, cause: Disconnect) {
        if let Some(shared) = self.owner.as_ref().and_then(|owner| owner.0.upgrade()) {
            shared.disconnect(cause);
        }
    }
}
impl<I: AsyncRead + Unpin> AsyncRead for TerminalIo<I> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buffer.filled().len();
        let capacity = buffer.remaining();
        let result = Pin::new(&mut this.inner).poll_read(cx, buffer);
        match result {
            Poll::Ready(Err(_)) => this.observe(Disconnect::ReadError),
            Poll::Ready(Ok(())) if capacity > 0 && buffer.filled().len() == before => {
                this.observe(Disconnect::ReadEof)
            }
            _ => {}
        }
        result
    }
}
impl<I: AsyncWrite + Unpin> AsyncWrite for TerminalIo<I> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.blocked(cx, bytes.len()) {
            return Poll::Pending;
        }
        let result = Pin::new(&mut this.inner).poll_write(cx, bytes);
        this.wrote(&result);
        if matches!(result, Poll::Ready(Err(_)))
            || (!bytes.is_empty() && matches!(result, Poll::Ready(Ok(0))))
        {
            this.observe(Disconnect::WriteError);
        }
        result
    }
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.blocked(cx, bytes.iter().map(|bytes| bytes.len()).sum()) {
            return Poll::Pending;
        }
        let result = Pin::new(&mut this.inner).poll_write_vectored(cx, bytes);
        this.wrote(&result);
        if matches!(result, Poll::Ready(Err(_)))
            || (bytes.iter().any(|slice| !slice.is_empty()) && matches!(result, Poll::Ready(Ok(0))))
        {
            this.observe(Disconnect::WriteError);
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
            this.observe(Disconnect::WriteError);
        }
        result
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
#[path = "gateway_terminal_tests.rs"]
mod tests;
