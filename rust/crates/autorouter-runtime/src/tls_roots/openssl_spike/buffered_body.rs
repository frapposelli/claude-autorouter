//! Test-only bounded application storage. Hyper/TLS/input-frame backing is
//! separately charged; this module never reports delivery or returns a pool lease.
use super::gateway_terminal::{DataProducerOwner, RequestTerminal, safe_cancel};
use bytes::{Buf, Bytes};
use hyper::HeaderMap;
use hyper::body::{Body, Frame, SizeHint};
use std::collections::VecDeque;
use std::future::poll_fn;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub(super) const BLOCK_BYTES: usize = 16_384;
pub(super) const BLOCKS: usize = 12;
pub(super) const HIGH_WATER: usize = 65_536;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum End {
    Clean,
    SourceError,
    InvalidMetadata,
    Cancelled,
}
#[derive(Default)]
struct Metrics {
    live_pools: AtomicUsize,
    allocated: AtomicUsize,
    outstanding: AtomicUsize,
    peak_outstanding: AtomicUsize,
    peak_queued_bytes: AtomicUsize,
    producer_tasks: AtomicUsize,
    callback_panics: AtomicUsize,
    waker_panics: AtomicUsize,
    consumer_polls: AtomicUsize,
}
struct PoolState {
    free: Vec<Box<[u8; BLOCK_BYTES]>>,
    allocated: usize,
}
struct Pool {
    state: Mutex<PoolState>,
    changed: Notify,
    metrics: Arc<Metrics>,
}
impl Pool {
    async fn acquire(self: &Arc<Self>, stop: &CancellationToken) -> Option<Block> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let (reused, allocate) = {
                let mut state = self.state.lock().unwrap();
                if let Some(block) = state.free.pop() {
                    (Some(block), false)
                } else if state.allocated < BLOCKS {
                    state.allocated += 1;
                    (None, true)
                } else {
                    (None, false)
                }
            };
            if reused.is_some() || allocate {
                let storage = reused.unwrap_or_else(|| {
                    self.metrics.allocated.fetch_add(1, Ordering::SeqCst);
                    Box::new([0; BLOCK_BYTES])
                });
                let outstanding = self.metrics.outstanding.fetch_add(1, Ordering::SeqCst) + 1;
                self.metrics
                    .peak_outstanding
                    .fetch_max(outstanding, Ordering::SeqCst);
                return Some(Block {
                    storage: Some(storage),
                    length: 0,
                    pool: self.clone(),
                });
            }
            tokio::select! { biased; _ = stop.cancelled() => return None, _ = &mut changed => {} }
        }
    }
}
impl Drop for Pool {
    fn drop(&mut self) {
        let allocated = self.state.get_mut().unwrap().allocated;
        self.metrics
            .allocated
            .fetch_sub(allocated, Ordering::SeqCst);
        self.metrics.live_pools.fetch_sub(1, Ordering::SeqCst);
    }
}
struct Block {
    storage: Option<Box<[u8; BLOCK_BYTES]>>,
    length: usize,
    pool: Arc<Pool>,
}
impl AsRef<[u8]> for Block {
    fn as_ref(&self) -> &[u8] {
        &self.storage.as_ref().unwrap()[..self.length]
    }
}
impl Drop for Block {
    fn drop(&mut self) {
        let storage = self.storage.take().unwrap();
        {
            let mut state = self.pool.state.lock().unwrap();
            assert!(state.free.len() < BLOCKS);
            state.free.push(storage);
        }
        self.pool.metrics.outstanding.fetch_sub(1, Ordering::SeqCst);
        self.pool.changed.notify_one();
    }
}
struct State {
    queue: VecDeque<Block>,
    queued_bytes: usize,
    trailers: Option<HeaderMap>,
    end: Option<End>,
    consumer_gone: bool,
    consumer_waker: Option<Waker>,
}
struct Shared {
    state: Mutex<State>,
    capacity: Notify,
    pool: Arc<Pool>,
    stop: CancellationToken,
}
impl Shared {
    fn finish(&self, end: End) -> (End, Option<Waker>) {
        let mut state = self.state.lock().unwrap();
        let actual = *state.end.get_or_insert(end);
        (actual, state.consumer_waker.take())
    }
    fn wake(&self, waker: Option<Waker>) {
        if let Some(waker) = waker
            && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| waker.wake())).is_err()
        {
            self.pool
                .metrics
                .waker_panics
                .fetch_add(1, Ordering::SeqCst);
        }
    }
    async fn room(&self) -> bool {
        loop {
            let changed = self.capacity.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let ready = {
                let state = self.state.lock().unwrap();
                !state.consumer_gone && state.queued_bytes < HIGH_WATER
            };
            if self.stop.is_cancelled() {
                return false;
            }
            if ready {
                return true;
            }
            tokio::select! { biased; _ = self.stop.cancelled() => return false, _ = &mut changed => {} }
        }
    }
    async fn copy(&self, source: &mut Bytes, last: bool) -> bool {
        while !source.is_empty() {
            if !self.room().await {
                return false;
            }
            let needs_block = {
                let state = self.state.lock().unwrap();
                state
                    .queue
                    .back()
                    .is_none_or(|block| block.length == BLOCK_BYTES)
            };
            let mut new_block = if needs_block {
                let Some(block) = self.pool.acquire(&self.stop).await else {
                    return false;
                };
                Some(block)
            } else {
                None
            };
            let (copied, waker) = {
                let mut state = self.state.lock().unwrap();
                if state.consumer_gone {
                    (0, None)
                } else {
                    // A consumer can remove the partial tail while allocation is
                    // pending. Only the producer appends, so recheck ownership.
                    if state
                        .queue
                        .back()
                        .is_none_or(|block| block.length == BLOCK_BYTES)
                    {
                        if let Some(block) = new_block.take() {
                            assert!(state.queue.len() < BLOCKS);
                            state.queue.push_back(block);
                        } else {
                            // No user-owned value is dropped under this lock.
                            drop(state);
                            continue;
                        }
                    }
                    let block = state.queue.back_mut().unwrap();
                    let copied = source.len().min(BLOCK_BYTES - block.length);
                    block.storage.as_mut().unwrap()[block.length..block.length + copied]
                        .copy_from_slice(&source[..copied]);
                    block.length += copied;
                    state.queued_bytes += copied;
                    assert!(state.queued_bytes < HIGH_WATER + BLOCK_BYTES);
                    self.pool
                        .metrics
                        .peak_queued_bytes
                        .fetch_max(state.queued_bytes, Ordering::SeqCst);
                    if last && copied == source.len() {
                        state.end = Some(End::Clean);
                    }
                    (copied, state.consumer_waker.take())
                }
            };
            drop(new_block);
            if copied == 0 {
                return false;
            }
            // Advancing the last Bytes view may run an embedding owner's Drop.
            // Do it only after releasing the queue lock.
            source.advance(copied);
            self.wake(waker);
        }
        true
    }
    fn trailers(&self, headers: HeaderMap) -> bool {
        let bytes = headers.iter().try_fold(0usize, |total, (name, value)| {
            total
                .checked_add(name.as_str().len())?
                .checked_add(value.len())
        });
        if headers.len() > 16_384 || bytes.is_none_or(|bytes| bytes > 16_384) {
            return false;
        }
        let old = {
            let mut state = self.state.lock().unwrap();
            state.trailers.replace(headers)
        };
        let valid = old.is_none();
        drop(old);
        valid
    }
}
#[derive(Clone)]
pub(super) struct Probe {
    shared: Weak<Shared>,
    metrics: Arc<Metrics>,
}
#[derive(Debug)]
pub(super) struct Snapshot {
    pub live_pools: usize,
    pub queued_bytes: usize,
    pub queued_blocks: usize,
    pub allocated_blocks: usize,
    pub outstanding_blocks: usize,
    pub peak_outstanding_blocks: usize,
    pub peak_queued_bytes: usize,
    pub producer_tasks: usize,
    pub callback_panics: usize,
    pub waker_panics: usize,
    pub consumer_polls: usize,
    pub end: Option<End>,
}
impl Probe {
    pub(super) fn snapshot(&self) -> Snapshot {
        let (queued_bytes, queued_blocks, end) =
            self.shared.upgrade().map_or((0, 0, None), |shared| {
                let state = shared.state.lock().unwrap();
                (state.queued_bytes, state.queue.len(), state.end)
            });
        Snapshot {
            live_pools: self.metrics.live_pools.load(Ordering::SeqCst),
            queued_bytes,
            queued_blocks,
            allocated_blocks: self.metrics.allocated.load(Ordering::SeqCst),
            outstanding_blocks: self.metrics.outstanding.load(Ordering::SeqCst),
            peak_outstanding_blocks: self.metrics.peak_outstanding.load(Ordering::SeqCst),
            peak_queued_bytes: self.metrics.peak_queued_bytes.load(Ordering::SeqCst),
            producer_tasks: self.metrics.producer_tasks.load(Ordering::SeqCst),
            callback_panics: self.metrics.callback_panics.load(Ordering::SeqCst),
            waker_panics: self.metrics.waker_panics.load(Ordering::SeqCst),
            consumer_polls: self.metrics.consumer_polls.load(Ordering::SeqCst),
            end,
        }
    }
}
pub(super) struct Producer {
    task: Option<JoinHandle<()>>,
    stop: CancellationToken,
}
impl Producer {
    pub(super) async fn join(mut self) -> Result<(), tokio::task::JoinError> {
        // Keep the join handle owned while awaiting: cancellation of this join
        // future must still run Producer::drop and abort the underlying task.
        let result = self.task.as_mut().unwrap().await;
        self.task.take();
        result
    }
    pub(super) async fn shutdown(mut self) -> Result<(), tokio::task::JoinError> {
        safe_cancel(&self.stop);
        // Keep the join handle owned while awaiting: cancellation of this join
        // future must still run Producer::drop and abort the underlying task.
        let result = self.task.as_mut().unwrap().await;
        self.task.take();
        result
    }
}
impl Drop for Producer {
    fn drop(&mut self) {
        safe_cancel(&self.stop);
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
struct TaskGuard {
    handoff: Option<DataProducerOwner>,
    shared: Arc<Shared>,
    terminal: Option<Box<dyn FnOnce(End) + Send>>,
    end: End,
}
impl Drop for TaskGuard {
    fn drop(&mut self) {
        if let Some(owner) = &mut self.handoff {
            owner.seal();
        }
        let (end, waker) = self.shared.finish(self.end);
        // The guard owns publication before spawn, including abort-before-poll.
        // Publish independently before waking a potentially panicking consumer.
        if let Some(terminal) = self.terminal.take()
            && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| terminal(end))).is_err()
        {
            self.shared
                .pool
                .metrics
                .callback_panics
                .fetch_add(1, Ordering::SeqCst);
        }
        self.shared.wake(waker);
        self.shared
            .pool
            .metrics
            .producer_tasks
            .fetch_sub(1, Ordering::SeqCst);
    }
}
pub(super) struct BufferedBody {
    shared: Arc<Shared>,
    finished: bool,
}
impl Body for BufferedBody {
    type Data = Bytes;
    type Error = End;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, End>>> {
        let this = self.get_mut();
        this.shared
            .pool
            .metrics
            .consumer_polls
            .fetch_add(1, Ordering::SeqCst);
        if this.finished {
            return Poll::Ready(None);
        }
        let waker = cx.waker().clone();
        let (block, trailers, end, old_waker) = {
            let mut state = this.shared.state.lock().unwrap();
            let block = state.queue.pop_front();
            if let Some(block) = &block {
                state.queued_bytes -= block.length;
            }
            let trailers = if block.is_none() && state.end.is_some() {
                state.trailers.take()
            } else {
                None
            };
            let end = state.end;
            let old = state.consumer_waker.replace(waker);
            (block, trailers, end, old)
        };
        drop(old_waker);
        this.shared.capacity.notify_one();
        if let Some(block) = block {
            return Poll::Ready(Some(Ok(Frame::data(Bytes::from_owner(block)))));
        }
        if let Some(trailers) = trailers {
            return Poll::Ready(Some(Ok(Frame::trailers(trailers))));
        }
        match end {
            None => Poll::Pending,
            Some(End::Clean) => {
                this.finished = true;
                Poll::Ready(None)
            }
            Some(error) => {
                this.finished = true;
                Poll::Ready(Some(Err(error)))
            }
        }
    }
    fn is_end_stream(&self) -> bool {
        if self.finished {
            return true;
        }
        let state = self.shared.state.lock().unwrap();
        state.end == Some(End::Clean) && state.queue.is_empty() && state.trailers.is_none()
    }
    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}
impl Drop for BufferedBody {
    fn drop(&mut self) {
        safe_cancel(&self.shared.stop);
        let (queue, trailers, waker) = {
            let mut state = self.shared.state.lock().unwrap();
            state.consumer_gone = true;
            state.queued_bytes = 0;
            (
                std::mem::take(&mut state.queue),
                state.trailers.take(),
                state.consumer_waker.take(),
            )
        };
        drop((queue, trailers, waker));
        self.shared.capacity.notify_waiters();
        self.shared.pool.changed.notify_waiters();
    }
}
pub(super) fn start<B>(
    body: B,
    terminal: impl FnOnce(End) + Send + 'static,
) -> (BufferedBody, Producer, Probe)
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Send,
{
    start_with_handoff(body, terminal, None)
}
pub(super) fn start_with_handoff<B>(
    body: B,
    terminal: impl FnOnce(End) + Send + 'static,
    publisher: Option<RequestTerminal>,
) -> (BufferedBody, Producer, Probe)
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Send,
{
    // Preserve an already-ended Incoming at the constructor boundary. Waiting
    // for the spawned task would hide HEAD/204/304/CL0's empty handoff from the
    // consumer-owned pool wrapper when no body poll will ever occur.
    let already_ended = body.is_end_stream();
    let metrics = Arc::new(Metrics::default());
    metrics.live_pools.store(1, Ordering::SeqCst);
    let pool = Arc::new(Pool {
        state: Mutex::new(PoolState {
            free: Vec::with_capacity(BLOCKS),
            allocated: 0,
        }),
        changed: Notify::new(),
        metrics: metrics.clone(),
    });
    let stop = CancellationToken::new();
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            queue: VecDeque::with_capacity(BLOCKS),
            queued_bytes: 0,
            trailers: None,
            end: already_ended.then_some(End::Clean),
            consumer_gone: false,
            consumer_waker: None,
        }),
        capacity: Notify::new(),
        pool,
        stop: stop.clone(),
    });
    let probe = Probe {
        shared: Arc::downgrade(&shared),
        metrics: metrics.clone(),
    };
    let mut handoff = publisher.and_then(|publisher| match publisher.producer(stop.clone()) {
        Ok(owner) => Some(owner),
        Err(()) => {
            safe_cancel(&stop);
            None
        }
    });
    if already_ended && let Some(owner) = &mut handoff {
        owner.seal();
    }
    if stop.is_cancelled() {
        shared.state.lock().unwrap().end = None;
    }
    let producer = shared.clone();
    metrics.producer_tasks.fetch_add(1, Ordering::SeqCst);
    let guard = TaskGuard {
        handoff,
        shared: producer.clone(),
        terminal: Some(Box::new(terminal)),
        end: End::Cancelled,
    };
    let task = tokio::spawn(async move {
        let mut guard = guard;
        let mut body = Box::pin(body);
        let mut previous: Option<super::gateway_terminal::handoff::HandoffTicket> = None;
        let end = if producer.stop.is_cancelled() {
            End::Cancelled
        } else if body.is_end_stream() {
            End::Clean
        } else {
            loop {
                if !producer.room().await {
                    break End::Cancelled;
                }
                if guard.handoff.as_ref().is_some_and(|owner| !owner.valid()) {
                    break End::Cancelled;
                }
                let frame = tokio::select! { biased; _ = producer.stop.cancelled() => break End::Cancelled, frame = poll_fn(|cx| {
                    if let Some(ticket) = &mut previous {
                        match ticket.poll_permission(cx) {
                            Poll::Pending => return Poll::Pending,
                            Poll::Ready(Err(())) => return Poll::Ready(Err(())),
                            Poll::Ready(Ok(())) => {}
                        }
                    }
                    body.as_mut().poll_frame(cx).map(Ok)
                }) => frame };
                let Ok(frame) = frame else {
                    break End::Cancelled;
                };
                drop(previous.take());
                match frame {
                    None => break End::Clean,
                    Some(Err(_)) => break End::SourceError,
                    Some(Ok(frame)) => match frame.into_data() {
                        Ok(mut data) => {
                            let input_bytes = data.len();
                            let last = body.is_end_stream();
                            if !producer.copy(&mut data, last).await {
                                break End::Cancelled;
                            }
                            if last {
                                break End::Clean;
                            }
                            if input_bytes != 0
                                && let Some(owner) = &guard.handoff
                            {
                                let Ok(ticket) = owner.ticket(input_bytes) else {
                                    break End::Cancelled;
                                };
                                previous = Some(ticket);
                            }
                        }
                        Err(frame) => {
                            let Ok(headers) = frame.into_trailers() else {
                                break End::InvalidMetadata;
                            };
                            if !producer.trailers(headers) {
                                break End::InvalidMetadata;
                            }
                            // A trailer frame by itself is not clean decoder EOF.
                        }
                    },
                }
            }
        };
        guard.end = end;
    });
    (
        BufferedBody {
            shared,
            finished: false,
        },
        Producer {
            task: Some(task),
            stop,
        },
        probe,
    )
}

#[cfg(test)]
#[path = "buffered_body_tests.rs"]
mod tests;
