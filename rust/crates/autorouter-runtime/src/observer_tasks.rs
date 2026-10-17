//! Owned, bounded futures returned by optional observation callbacks.
//!
//! An admitted factory starts synchronously on its caller's thread. Its future
//! is polled by one owner worker, independently of forwarding. Callback errors
//! and ordinary unwinding factory/poll/destructor panics are ignored; no private
//! rejection or panic payload enters the counters. Embedding hosts explicitly
//! initialize observer privacy before workers; this adapter never changes hooks.
//!
//! Limits cover accepted factory/future count and *charged* storage: the caller's
//! exact serialized payload byte count plus the known inline factory/future
//! sizes. Fixed worker/queue bookkeeping is separately bounded by the count.
//! They cannot bound hidden user allocations inside Box/Vec/Arc or caller-spawned
//! work. Factories/polls/destructors must not block; safe Rust cannot preempt one.
//! A pending-future timeout is optional and disabled by default. Never-ready
//! futures retain bounded admission until cancelled or the owner is closed.
//!
//! Explicit `close` cancels accepted work and joins the worker. Dropping the
//! owner only requests cancellation; shutdown paths must call `close` for joined
//! cleanup. Abort, replaced/aborting panic hooks, and panic payloads whose own
//! destructors panic are outside the ordinary-unwind containment contract.
use crate::callback_privacy::contain_callback;
use std::collections::VecDeque;
use std::future::Future;
use std::mem::size_of;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::{Instant, Sleep};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

/// Payload-free rejection: callbacks must discard private error details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObserverRejected;
/// Boxed callback return type for heterogeneous observation APIs.
pub type ObserverFuture =
    Pin<Box<dyn Future<Output = Result<(), ObserverRejected>> + Send + 'static>>;

const MAX_PENDING: usize = 256;
const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_CHARGED_BYTES: usize = 1024 * 1024;

/// Storage limits may be lowered, but not raised beyond the declared ceilings.
#[derive(Debug, Clone, Copy)]
pub struct ObserverLimits {
    /// Includes factories in progress, queued futures and polled futures.
    pub max_pending: usize,
    pub max_payload_bytes: usize,
    pub max_charged_bytes: usize,
    /// Starts at admission; `None` preserves delayed and never-ready callbacks.
    pub pending_timeout: Option<Duration>,
}
impl Default for ObserverLimits {
    fn default() -> Self {
        Self {
            max_pending: MAX_PENDING,
            max_payload_bytes: MAX_PAYLOAD_BYTES,
            max_charged_bytes: MAX_CHARGED_BYTES,
            pending_timeout: None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObserverStartError {
    InvalidLimits,
    NoRuntime,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObserverAdmission {
    Accepted,
    Closed,
    Cancelled,
    PayloadTooLarge,
    Capacity,
    DeadlineOverflow,
    FactoryPanicked,
}
/// Numeric-only diagnostics, never payloads, keys, error details or panics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObserverSnapshot {
    pub closed: bool,
    pub worker_finished: bool,
    pub pending: usize,
    pub factories: usize,
    pub charged_bytes: usize,
    pub completed: u64,
    pub rejected: u64,
    pub factory_panicked: u64,
    pub poll_panicked: u64,
    pub destructor_panicked: u64,
    pub timed_out: u64,
    pub cancelled: u64,
    pub declined_closed: u64,
    pub declined_cancelled: u64,
    pub declined_payload: u64,
    pub declined_capacity: u64,
    pub declined_deadline: u64,
}
struct State {
    snapshot: ObserverSnapshot,
    queued: VecDeque<Tracked>,
    waker: Option<Waker>,
}
struct Inner {
    limits: ObserverLimits,
    state: Mutex<State>,
}
impl Inner {
    fn locked(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
    fn wake(&self) {
        let waker = self.locked().waker.clone();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
    fn close(&self) {
        self.locked().snapshot.closed = true;
        self.wake();
    }
    fn drop_contained<T>(&self, value: T) {
        if contain_callback(|| drop(value)).is_err() {
            let mut state = self.locked();
            state.snapshot.destructor_panicked =
                state.snapshot.destructor_panicked.saturating_add(1);
        }
    }
}

/// A clonable admission/cancellation handle; it does not own worker lifetime.
#[derive(Clone)]
pub struct ObserverHandle {
    inner: Arc<Inner>,
}
impl ObserverHandle {
    pub(crate) fn payload_limit(&self) -> usize {
        self.inner.limits.max_payload_bytes
    }
    pub fn snapshot(&self) -> ObserverSnapshot {
        self.inner.locked().snapshot
    }
    /// Stop admission and request cancellation, without awaiting shutdown.
    pub fn cancel(&self) {
        self.inner.close();
    }
    pub fn submit<F, Fut>(&self, payload_bytes: usize, factory: F) -> ObserverAdmission
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(), ObserverRejected>> + Send + 'static,
    {
        self.submit_inner(payload_bytes, None, factory)
    }
    /// Caller cancellation is independent of other accepted observations.
    pub fn submit_with_cancellation<F, Fut>(
        &self,
        payload_bytes: usize,
        cancellation: CancellationToken,
        factory: F,
    ) -> ObserverAdmission
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(), ObserverRejected>> + Send + 'static,
    {
        self.submit_inner(payload_bytes, Some(cancellation), factory)
    }
    fn submit_inner<F, Fut>(
        &self,
        payload_bytes: usize,
        cancellation: Option<CancellationToken>,
        factory: F,
    ) -> ObserverAdmission
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(), ObserverRejected>> + Send + 'static,
    {
        let charge = payload_bytes
            .checked_add(size_of::<F>())
            .and_then(|bytes| bytes.checked_add(size_of::<Fut>()));
        let deadline = match self.inner.limits.pending_timeout {
            None => Ok(None),
            Some(timeout) => Instant::now().checked_add(timeout).map(Some).ok_or(()),
        };
        let admission = {
            let mut state = self.inner.locked();
            let snapshot = &mut state.snapshot;
            if snapshot.closed {
                snapshot.declined_closed = snapshot.declined_closed.saturating_add(1);
                Err(ObserverAdmission::Closed)
            } else if cancellation
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
                snapshot.declined_cancelled = snapshot.declined_cancelled.saturating_add(1);
                Err(ObserverAdmission::Cancelled)
            } else if payload_bytes > self.inner.limits.max_payload_bytes {
                snapshot.declined_payload = snapshot.declined_payload.saturating_add(1);
                Err(ObserverAdmission::PayloadTooLarge)
            } else if deadline.is_err() {
                snapshot.declined_deadline = snapshot.declined_deadline.saturating_add(1);
                Err(ObserverAdmission::DeadlineOverflow)
            } else if snapshot.pending >= self.inner.limits.max_pending
                || charge.is_none_or(|charge| {
                    charge
                        > self
                            .inner
                            .limits
                            .max_charged_bytes
                            .saturating_sub(snapshot.charged_bytes)
                })
            {
                snapshot.declined_capacity = snapshot.declined_capacity.saturating_add(1);
                Err(ObserverAdmission::Capacity)
            } else {
                let charge = charge.expect("checked admission charge");
                snapshot.pending += 1;
                snapshot.factories += 1;
                snapshot.charged_bytes += charge;
                Ok(Reservation {
                    inner: self.inner.clone(),
                    charge,
                    factory: true,
                    outcome: Outcome::Cancelled,
                })
            }
        };
        let Ok(mut reservation) = admission else {
            self.inner.drop_contained(factory);
            return admission.err().expect("declined admission");
        };
        let deadline = deadline.expect("checked pending observation deadline");
        let future = match contain_callback(factory) {
            Ok(future) => future,
            Err(_) => {
                reservation.outcome = Outcome::FactoryPanicked;
                return ObserverAdmission::FactoryPanicked;
            }
        };
        let mut tracked = Tracked {
            future: Some(Box::pin(future)),
            cancellation: cancellation.map(|token| Box::pin(token.cancelled_owned())),
            deadline,
            reservation: Some(reservation),
        };
        {
            let mut state = self.inner.locked();
            tracked
                .reservation
                .as_mut()
                .expect("admitted reservation")
                .factory = false;
            state.snapshot.factories -= 1;
            if !state.snapshot.closed {
                state.queued.push_back(tracked);
                drop(state);
                self.inner.wake();
                return ObserverAdmission::Accepted;
            }
        }
        // A concurrent close cannot allow a late factory to enqueue afterward.
        tracked.finish(Outcome::Cancelled);
        ObserverAdmission::Accepted
    }
}

/// One owned worker; explicit close is cancellation plus joined cleanup.
pub struct ObserverTasks {
    handle: ObserverHandle,
    worker: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}
impl ObserverTasks {
    pub fn new(limits: ObserverLimits) -> Result<Self, ObserverStartError> {
        if limits.max_pending == 0
            || limits.max_pending > MAX_PENDING
            || limits.max_payload_bytes == 0
            || limits.max_payload_bytes > MAX_PAYLOAD_BYTES
            || limits.max_charged_bytes == 0
            || limits.max_charged_bytes > MAX_CHARGED_BYTES
            || limits.max_payload_bytes > limits.max_charged_bytes
            || limits.pending_timeout.is_some_and(|timeout| {
                timeout.is_zero() || Instant::now().checked_add(timeout).is_none()
            })
        {
            return Err(ObserverStartError::InvalidLimits);
        }
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| ObserverStartError::NoRuntime)?;
        let inner = Arc::new(Inner {
            limits,
            state: Mutex::new(State {
                snapshot: ObserverSnapshot::default(),
                queued: VecDeque::new(),
                waker: None,
            }),
        });
        let worker = Worker {
            inner: inner.clone(),
            active: Vec::new(),
            timer: None,
        };
        Ok(Self {
            handle: ObserverHandle { inner },
            worker: tokio::sync::Mutex::new(Some(runtime.spawn(worker))),
        })
    }
    pub fn handle(&self) -> ObserverHandle {
        self.handle.clone()
    }
    pub fn snapshot(&self) -> ObserverSnapshot {
        self.handle.snapshot()
    }
    /// Idempotent and cancellation-safe: a cancelled closer leaves the handle
    /// stored so another close still joins the same owned worker.
    pub async fn close(&self) {
        self.handle.cancel();
        let mut worker = self.worker.lock().await;
        if let Some(task) = worker.as_mut() {
            let _ = task.await;
        }
        worker.take();
    }
}
impl Drop for ObserverTasks {
    fn drop(&mut self) {
        self.handle.cancel();
        if let Some(worker) = self.worker.get_mut().as_ref() {
            worker.abort();
        }
    }
}

#[derive(Clone, Copy)]
enum Outcome {
    Completed,
    Rejected,
    FactoryPanicked,
    PollPanicked,
    TimedOut,
    Cancelled,
}
struct Reservation {
    inner: Arc<Inner>,
    charge: usize,
    factory: bool,
    outcome: Outcome,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        {
            let mut state = self.inner.locked();
            let snapshot = &mut state.snapshot;
            snapshot.pending -= 1;
            snapshot.charged_bytes -= self.charge;
            if self.factory {
                snapshot.factories -= 1;
            }
            let counter = match self.outcome {
                Outcome::Completed => &mut snapshot.completed,
                Outcome::Rejected => &mut snapshot.rejected,
                Outcome::FactoryPanicked => &mut snapshot.factory_panicked,
                Outcome::PollPanicked => &mut snapshot.poll_panicked,
                Outcome::TimedOut => &mut snapshot.timed_out,
                Outcome::Cancelled => &mut snapshot.cancelled,
            };
            *counter = counter.saturating_add(1);
        }
        self.inner.wake();
    }
}
struct Tracked {
    future: Option<ObserverFuture>,
    cancellation: Option<Pin<Box<WaitForCancellationFutureOwned>>>,
    deadline: Option<Instant>,
    reservation: Option<Reservation>,
}
impl Tracked {
    fn finish(&mut self, outcome: Outcome) {
        if let Some(reservation) = &mut self.reservation {
            reservation.outcome = outcome;
            if let Some(future) = self.future.take() {
                reservation.inner.drop_contained(future);
            }
        }
        self.reservation.take();
    }
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Outcome> {
        if self
            .cancellation
            .as_mut()
            .is_some_and(|cancelled| cancelled.as_mut().poll(cx).is_ready())
        {
            return Poll::Ready(Outcome::Cancelled);
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Poll::Ready(Outcome::TimedOut);
        }
        match contain_callback(|| {
            self.future
                .as_mut()
                .expect("accepted observation future")
                .as_mut()
                .poll(cx)
        }) {
            Ok(Poll::Ready(Ok(()))) => Poll::Ready(Outcome::Completed),
            Ok(Poll::Ready(Err(ObserverRejected))) => Poll::Ready(Outcome::Rejected),
            Ok(Poll::Pending) => Poll::Pending,
            Err(_) => Poll::Ready(Outcome::PollPanicked),
        }
    }
}
impl Drop for Tracked {
    fn drop(&mut self) {
        self.finish(Outcome::Cancelled);
    }
}
struct Worker {
    inner: Arc<Inner>,
    active: Vec<Tracked>,
    timer: Option<Pin<Box<Sleep>>>,
}
impl Future for Worker {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        let closed = {
            let mut state = this.inner.locked();
            state.waker = Some(cx.waker().clone());
            this.active.extend(state.queued.drain(..));
            state.snapshot.closed
        };
        let mut index = 0;
        while index < this.active.len() {
            let outcome = if closed || this.inner.locked().snapshot.closed {
                Poll::Ready(Outcome::Cancelled)
            } else {
                this.active[index].poll(cx)
            };
            if let Poll::Ready(outcome) = outcome {
                let mut completed = this.active.swap_remove(index);
                completed.finish(outcome);
            } else {
                index += 1;
            }
        }
        if closed && this.inner.locked().snapshot.pending == 0 {
            return Poll::Ready(());
        }
        if let Some(deadline) = this.active.iter().filter_map(|task| task.deadline).min() {
            let timer = this
                .timer
                .get_or_insert_with(|| Box::pin(tokio::time::sleep_until(deadline)));
            timer.as_mut().reset(deadline);
            if timer.as_mut().poll(cx).is_ready() {
                cx.waker().wake_by_ref();
            }
        } else {
            this.timer = None;
        }
        Poll::Pending
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let queued = {
            let mut state = self.inner.locked();
            state.snapshot.closed = true;
            state.waker = None;
            std::mem::take(&mut state.queued)
        };
        drop(queued);
        self.active.clear();
        self.inner.locked().snapshot.worker_finished = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    async fn settled(handle: &ObserverHandle, pending: usize) {
        for _ in 0..100 {
            if handle.snapshot().pending == pending {
                return;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(handle.snapshot().pending, pending);
    }
    struct Never {
        dropped: Arc<AtomicUsize>,
    }
    impl Future for Never {
        type Output = Result<(), ObserverRejected>;
        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
            Poll::Pending
        }
    }
    impl Drop for Never {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn factories_start_synchronously_and_pending_futures_never_block_forwarding() {
        let owner = ObserverTasks::new(ObserverLimits::default()).unwrap();
        let handle = owner.handle();
        let started = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicUsize::new(0));
        let (release, waiting) = tokio::sync::oneshot::channel();
        let observed = started.clone();
        assert_eq!(
            handle.submit(8, move || {
                observed.store(true, Ordering::SeqCst);
                async move {
                    let _ = waiting.await;
                    Ok(())
                }
            }),
            ObserverAdmission::Accepted
        );
        assert!(started.load(Ordering::SeqCst));
        let never = dropped.clone();
        assert_eq!(
            handle.submit(8, || Never { dropped: never }),
            ObserverAdmission::Accepted
        );
        assert_eq!(
            handle.submit(8, || async { Err(ObserverRejected) }),
            ObserverAdmission::Accepted
        );
        // This unrelated continuation proceeds while the first two stay pending.
        tokio::task::yield_now().await;
        assert_eq!(handle.snapshot().rejected, 1);
        assert_eq!(handle.snapshot().pending, 2);
        release.send(()).unwrap();
        settled(&handle, 1).await;
        owner.close().await;
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        let snapshot = handle.snapshot();
        assert_eq!(
            (snapshot.completed, snapshot.rejected, snapshot.cancelled),
            (1, 1, 1)
        );
        assert_eq!(
            (snapshot.pending, snapshot.charged_bytes, snapshot.factories),
            (0, 0, 0)
        );
        assert!(snapshot.closed && snapshot.worker_finished);
        owner.close().await;
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_is_disabled_by_default_and_optional_timeout_preserves_cancellation() {
        let owner = ObserverTasks::new(ObserverLimits::default()).unwrap();
        let handle = owner.handle();
        let dropped = Arc::new(AtomicUsize::new(0));
        let never = dropped.clone();
        handle.submit(10, || Never { dropped: never });
        tokio::time::advance(Duration::from_secs(60 * 60)).await;
        assert_eq!(handle.snapshot().pending, 1);
        assert_eq!(handle.snapshot().timed_out, 0);
        owner.close().await;
        assert_eq!(dropped.load(Ordering::SeqCst), 1);

        let timed = ObserverTasks::new(ObserverLimits {
            pending_timeout: Some(Duration::from_millis(100)),
            ..ObserverLimits::default()
        })
        .unwrap();
        let handle = timed.handle();
        let cancellation = CancellationToken::new();
        handle.submit_with_cancellation(10, cancellation.clone(), || Never {
            dropped: dropped.clone(),
        });
        handle.submit(10, || Never {
            dropped: dropped.clone(),
        });
        cancellation.cancel();
        settled(&handle, 1).await;
        tokio::time::advance(Duration::from_millis(100)).await;
        settled(&handle, 0).await;
        timed.close().await;
        assert_eq!(
            (handle.snapshot().cancelled, handle.snapshot().timed_out),
            (1, 1)
        );
        assert_eq!(dropped.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn cancelling_one_future_leaves_other_observers_owned_until_close() {
        let owner = ObserverTasks::new(ObserverLimits::default()).unwrap();
        let handle = owner.handle();
        let cancellation = CancellationToken::new();
        let drops = Arc::new(AtomicUsize::new(0));
        handle.submit_with_cancellation(10, cancellation.clone(), || Never {
            dropped: drops.clone(),
        });
        handle.submit(10, || Never {
            dropped: drops.clone(),
        });
        tokio::task::yield_now().await;
        cancellation.cancel();
        settled(&handle, 1).await;
        assert_eq!(handle.snapshot().cancelled, 1);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        owner.close().await;
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        assert_eq!(handle.snapshot().cancelled, 2);
    }

    #[tokio::test]
    async fn saturation_oversize_closed_and_cancelled_decline_before_factory_execution() {
        let owner = ObserverTasks::new(ObserverLimits {
            max_pending: 1,
            ..ObserverLimits::default()
        })
        .unwrap();
        let handle = owner.handle();
        handle.submit(10, std::future::pending);
        let started = AtomicBool::new(false);
        assert_eq!(
            handle.submit(10, || {
                started.store(true, Ordering::SeqCst);
                async { Ok(()) }
            }),
            ObserverAdmission::Capacity
        );
        assert!(!started.load(Ordering::SeqCst));
        owner.close().await;
        assert_eq!(
            handle.submit(10, || {
                started.store(true, Ordering::SeqCst);
                async { Ok(()) }
            }),
            ObserverAdmission::Closed
        );
        assert!(!started.load(Ordering::SeqCst));

        let owner = ObserverTasks::new(ObserverLimits::default()).unwrap();
        let handle = owner.handle();
        assert_eq!(
            handle.submit(64 * 1024 + 1, || {
                started.store(true, Ordering::SeqCst);
                async { Ok(()) }
            }),
            ObserverAdmission::PayloadTooLarge
        );
        assert_eq!(
            handle.submit(usize::MAX, || {
                started.store(true, Ordering::SeqCst);
                async { Ok(()) }
            }),
            ObserverAdmission::PayloadTooLarge
        );
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(
            handle.submit_with_cancellation(10, cancelled, || {
                started.store(true, Ordering::SeqCst);
                async { Ok(()) }
            }),
            ObserverAdmission::Cancelled
        );
        assert!(!started.load(Ordering::SeqCst));
        owner.close().await;
        assert_eq!(
            (
                handle.snapshot().declined_payload,
                handle.snapshot().declined_cancelled
            ),
            (2, 1)
        );
    }

    #[tokio::test]
    async fn aggregate_charged_bytes_include_known_inline_storage_and_recover() {
        let payload = 100;
        let future_bytes = size_of::<std::future::Pending<Result<(), ObserverRejected>>>();
        let owner = ObserverTasks::new(ObserverLimits {
            max_pending: 3,
            max_payload_bytes: payload,
            max_charged_bytes: payload + future_bytes,
            ..ObserverLimits::default()
        })
        .unwrap();
        let handle = owner.handle();
        assert_eq!(
            handle.submit(payload, std::future::pending),
            ObserverAdmission::Accepted
        );
        assert_eq!(handle.snapshot().charged_bytes, payload + future_bytes);
        assert_eq!(
            handle.submit(payload, std::future::pending),
            ObserverAdmission::Capacity
        );
        owner.close().await;
        assert_eq!(handle.snapshot().charged_bytes, 0);
    }

    struct PanicPoll;
    impl Future for PanicPoll {
        type Output = Result<(), ObserverRejected>;
        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
            panic!("synthetic private poll marker");
        }
    }
    struct PanicDrop;
    impl Future for PanicDrop {
        type Output = Result<(), ObserverRejected>;
        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
            Poll::Ready(Ok(()))
        }
    }
    impl Drop for PanicDrop {
        fn drop(&mut self) {
            panic!("synthetic private destructor marker");
        }
    }
    #[tokio::test]
    async fn ordinary_factory_poll_and_destructor_panics_do_not_abort_the_owner() {
        let owner = ObserverTasks::new(ObserverLimits::default()).unwrap();
        let handle = owner.handle();
        assert_eq!(
            handle.submit(
                10,
                || -> std::future::Ready<Result<(), ObserverRejected>> {
                    panic!("synthetic private factory marker")
                }
            ),
            ObserverAdmission::FactoryPanicked
        );
        assert_eq!(
            (
                handle.snapshot().pending,
                handle.snapshot().factory_panicked
            ),
            (0, 1)
        );
        handle.submit(10, || PanicPoll);
        handle.submit(10, || PanicDrop);
        handle.submit(10, || async { Ok(()) });
        settled(&handle, 0).await;
        assert_eq!(
            (
                handle.snapshot().poll_panicked,
                handle.snapshot().destructor_panicked,
                handle.snapshot().completed
            ),
            (1, 1, 2)
        );
        owner.close().await;
        assert_eq!(handle.snapshot().charged_bytes, 0);
    }

    #[tokio::test]
    async fn close_is_joined_and_late_factory_completion_cannot_enqueue() {
        let owner = Arc::new(ObserverTasks::new(ObserverLimits::default()).unwrap());
        let handle = owner.handle();
        let (factory_started, waiting) = tokio::sync::oneshot::channel();
        let (release, wait_release) = std::sync::mpsc::channel();
        let dropped = Arc::new(AtomicUsize::new(0));
        let observer_dropped = dropped.clone();
        let submitter = handle.clone();
        let submit = std::thread::spawn(move || {
            submitter.submit(10, || {
                factory_started.send(()).unwrap();
                wait_release.recv().unwrap();
                Never {
                    dropped: observer_dropped,
                }
            })
        });
        waiting.await.unwrap();
        assert_eq!(
            (handle.snapshot().pending, handle.snapshot().factories),
            (1, 1)
        );
        let closing = owner.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let observed = closed.clone();
        let close = tokio::spawn(async move {
            closing.close().await;
            observed.store(true, Ordering::SeqCst);
        });
        tokio::task::yield_now().await;
        assert!(!closed.load(Ordering::SeqCst));
        assert!(handle.snapshot().closed);
        release.send(()).unwrap();
        close.await.unwrap();
        assert_eq!(submit.join().unwrap(), ObserverAdmission::Accepted);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert_eq!(
            (
                handle.snapshot().pending,
                handle.snapshot().factories,
                handle.snapshot().cancelled
            ),
            (0, 0, 1)
        );
        assert!(handle.snapshot().worker_finished);
    }

    #[tokio::test]
    async fn drop_requests_cancellation_and_queued_futures_are_not_detached() {
        let owner = ObserverTasks::new(ObserverLimits::default()).unwrap();
        let handle = owner.handle();
        let dropped = Arc::new(AtomicUsize::new(0));
        handle.submit(10, || Never {
            dropped: dropped.clone(),
        });
        drop(owner);
        settled(&handle, 0).await;
        assert!(handle.snapshot().closed && handle.snapshot().worker_finished);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancelling_a_closer_preserves_the_worker_for_a_later_join() {
        let owner = Arc::new(ObserverTasks::new(ObserverLimits::default()).unwrap());
        let handle = owner.handle();
        let (factory_started, waiting) = tokio::sync::oneshot::channel();
        let (release, wait_release) = std::sync::mpsc::channel();
        let dropped = Arc::new(AtomicUsize::new(0));
        let observed = dropped.clone();
        let submitter = handle.clone();
        let submit = std::thread::spawn(move || {
            submitter.submit(10, || {
                factory_started.send(()).unwrap();
                wait_release.recv().unwrap();
                Never { dropped: observed }
            })
        });
        waiting.await.unwrap();
        let closer_owner = owner.clone();
        let closer = tokio::spawn(async move { closer_owner.close().await });
        tokio::task::yield_now().await;
        assert!(handle.snapshot().closed);
        closer.abort();
        assert!(closer.await.unwrap_err().is_cancelled());
        // The first cancelled close did not take/detach the worker's JoinHandle.
        release.send(()).unwrap();
        owner.close().await;
        assert_eq!(submit.join().unwrap(), ObserverAdmission::Accepted);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert!(handle.snapshot().worker_finished);
        assert_eq!(
            (handle.snapshot().pending, handle.snapshot().factories),
            (0, 0)
        );
    }

    #[tokio::test]
    async fn a_declined_factory_is_not_started_and_its_destructor_is_contained() {
        struct Captured;
        impl Drop for Captured {
            fn drop(&mut self) {
                panic!("synthetic private declined-factory marker");
            }
        }
        let owner = ObserverTasks::new(ObserverLimits {
            max_pending: 1,
            ..ObserverLimits::default()
        })
        .unwrap();
        let handle = owner.handle();
        handle.submit(10, std::future::pending);
        let started = Arc::new(AtomicBool::new(false));
        let observed = started.clone();
        let captured = Captured;
        assert_eq!(
            handle.submit(10, move || {
                observed.store(true, Ordering::SeqCst);
                drop(captured);
                async { Ok(()) }
            }),
            ObserverAdmission::Capacity
        );
        assert!(!started.load(Ordering::SeqCst));
        assert_eq!(handle.snapshot().destructor_panicked, 1);
        owner.close().await;
        assert_eq!(handle.snapshot().pending, 0);
    }

    #[test]
    fn startup_requires_valid_bounds_and_an_explicit_runtime() {
        assert!(matches!(
            ObserverTasks::new(ObserverLimits::default()),
            Err(ObserverStartError::NoRuntime)
        ));
        for limits in [
            ObserverLimits {
                max_pending: 0,
                ..ObserverLimits::default()
            },
            ObserverLimits {
                max_pending: MAX_PENDING + 1,
                ..ObserverLimits::default()
            },
            ObserverLimits {
                max_payload_bytes: 0,
                ..ObserverLimits::default()
            },
            ObserverLimits {
                max_payload_bytes: MAX_PAYLOAD_BYTES + 1,
                ..ObserverLimits::default()
            },
            ObserverLimits {
                max_charged_bytes: 1,
                ..ObserverLimits::default()
            },
            ObserverLimits {
                max_charged_bytes: MAX_CHARGED_BYTES + 1,
                ..ObserverLimits::default()
            },
            ObserverLimits {
                pending_timeout: Some(Duration::ZERO),
                ..ObserverLimits::default()
            },
            ObserverLimits {
                pending_timeout: Some(Duration::MAX),
                ..ObserverLimits::default()
            },
        ] {
            assert!(matches!(
                ObserverTasks::new(limits),
                Err(ObserverStartError::InvalidLimits)
            ));
        }
    }
}
