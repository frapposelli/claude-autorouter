//! Test-only scheduling control around the actual complete classifier worker.
//! It cannot publish an evaluation result or modify cancellation semantics.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Waker},
};
use tokio::sync::{Notify, Semaphore};

pub(crate) struct WorkerSpawnGate {
    entered: AtomicBool,
    changed: Notify,
    release: Semaphore,
}
impl Default for WorkerSpawnGate {
    fn default() -> Self {
        Self {
            entered: AtomicBool::new(false),
            changed: Notify::new(),
            release: Semaphore::new(0),
        }
    }
}
impl WorkerSpawnGate {
    pub(super) async fn before_spawn(&self) {
        self.entered.store(true, Ordering::Release);
        self.changed.notify_waiters();
        self.release.acquire().await.unwrap().forget();
    }
    pub(crate) fn entered(&self) -> bool {
        self.entered.load(Ordering::Acquire)
    }
    pub(crate) fn release(&self) {
        self.release.add_permits(1);
    }
}

#[derive(Default)]
pub(crate) struct WorkerPollGate {
    held: AtomicBool,
    blocked: AtomicBool,
    finished: AtomicBool,
    waker: Mutex<Option<Waker>>,
    changed: Notify,
}
impl WorkerPollGate {
    pub(crate) fn hold(self: &Arc<Self>) -> ReleaseOnDrop {
        self.held.store(true, Ordering::Release);
        ReleaseOnDrop(self.clone())
    }
    pub(crate) fn release(&self) {
        self.held.store(false, Ordering::Release);
        let waker = self.waker.lock().unwrap().take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
    pub(crate) async fn wait_blocked(&self) {
        self.wait_flag(&self.blocked).await;
    }
    pub(crate) async fn wait_finished(&self) {
        self.wait_flag(&self.finished).await;
    }
    async fn wait_flag(&self, flag: &AtomicBool) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if flag.load(Ordering::Acquire) {
                return;
            }
            changed.await;
        }
    }
    fn blocks(&self, cx: &Context<'_>) -> bool {
        if !self.held.load(Ordering::Acquire) {
            return false;
        }
        // Clone/drop/wake arbitrary wakers only outside the gate's mutex.
        let next = cx.waker().clone();
        let old = self.waker.lock().unwrap().replace(next);
        drop(old);
        if !self.held.load(Ordering::Acquire) {
            return false;
        }
        self.blocked.store(true, Ordering::Release);
        self.changed.notify_waiters();
        true
    }
}
pub(crate) struct ReleaseOnDrop(Arc<WorkerPollGate>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}
pub(crate) struct ScheduledWorker<F> {
    future: Option<Pin<Box<F>>>,
    gate: Option<Arc<WorkerPollGate>>,
}
impl<F: Future> Future for ScheduledWorker<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if this.gate.as_ref().is_some_and(|gate| gate.blocks(cx)) {
            return Poll::Pending;
        }
        this.future.as_mut().unwrap().as_mut().poll(cx)
    }
}
impl<F> Drop for ScheduledWorker<F> {
    fn drop(&mut self) {
        // The completion observation follows actual evaluator future teardown.
        drop(self.future.take());
        if let Some(gate) = &self.gate {
            gate.finished.store(true, Ordering::Release);
            gate.changed.notify_waiters();
        }
    }
}
pub(crate) fn schedule<F: Future>(
    future: F,
    gate: Option<Arc<WorkerPollGate>>,
) -> ScheduledWorker<F> {
    ScheduledWorker {
        future: Some(Box::pin(future)),
        gate,
    }
}
