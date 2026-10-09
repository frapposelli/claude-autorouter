//! Per-response HTTP/1 write completion, separate from provider completion.
//!
//! This adapter relies on the pinned Hyper 1.12 HTTP/1 driver: with
//! `pipeline_flush(false)`, `proto/h1/io.rs::poll_flush` drains all encoded
//! buffers before calling the underlying writer's flush. `conn.rs::poll_flush`
//! then permits the next ordered response write. Request service futures may
//! execute concurrently, matching Node pipelining. Submission is recorded only
//! at actual head encoding; ready but queued responses cannot be confirmed by
//! an earlier response flush.
//!
//! The body deliberately advertises neither a known length nor early EOF. Its
//! actual EOF only arms a record. Hyper must subsequently encode the final
//! chunk and flush it successfully. Dropping the exhausted body does not discard
//! that record: the connection owns it until flush or failure. This is proof of
//! local transport acceptance, not acknowledgement by the Claude application.
//!
//! Use one registry per connection and [`serve_http1`]; do not wrap this adapter
//! in another buffered writer or enable HTTP/2/pipeline flush aggregation. A
//! transport result alone never proves valid provider execution: the caller
//! must also require successful status, clean observer evidence, a live request
//! and a current attempt sequence before committing a continuation model.

use std::collections::VecDeque;
use std::future::Future;
use std::io::{self, IoSlice};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Buf;
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::header::{CONTENT_LENGTH, TRANSFER_ENCODING};
use hyper::rt::{Sleep, Timer};
use hyper::service::Service;
use hyper::{Request, Response};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Metadata-only transport outcome; never carries provider or request text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Delivery {
    Flushed,
    Failed(Failure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Body,
    Io,
    Abandoned,
    ConnectionClosed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionError {
    Capacity,
    Closed,
}

type Callback = Box<dyn FnOnce(Delivery) + Send + 'static>;

struct Record {
    id: u64,
    eof: bool,
    submitted: bool,
    callback: Callback,
}

struct State {
    records: VecDeque<Record>,
    next_id: u64,
    limit: usize,
    closed: bool,
}

/// Bounded connection-owned records. Callbacks must be short and synchronous.
/// Do not perform I/O or await persistence from the continuity commit callback.
#[derive(Clone)]
pub struct CompletionRegistry(Arc<Mutex<State>>);

#[derive(Clone)]
struct ResponseReceipt {
    registry: CompletionRegistry,
    id: u64,
}

impl CompletionRegistry {
    pub fn new(limit: usize) -> Self {
        Self(Arc::new(Mutex::new(State {
            records: VecDeque::new(),
            next_id: 0,
            limit,
            closed: false,
        })))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn pending(&self) -> usize {
        self.lock().records.len()
    }

    /// Track an inference response, including 204/304 status-only responses.
    /// Use track_empty_response for HEAD. Hyper owns downstream framing;
    /// preserving a length header could let it skip polling the clean body EOF.
    /// Other end-to-end headers and all body frames are retained.
    pub fn track<B>(
        &self,
        response: Response<B>,
        callback: impl FnOnce(Delivery) + Send + 'static,
    ) -> Result<Response<CompletionBody<B>>, AdmissionError> {
        let empty = matches!(response.status().as_u16(), 204 | 304);
        self.track_response(response, callback, empty)
    }

    /// Track a response whose method/status forbids a wire body. Submission to
    /// Hyper and a successful header flush are still required; constructing or
    /// dropping this body cannot establish successful delivery.
    pub fn track_empty_response<B>(
        &self,
        response: Response<B>,
        callback: impl FnOnce(Delivery) + Send + 'static,
    ) -> Result<Response<CompletionBody<B>>, AdmissionError> {
        self.track_response(response, callback, true)
    }

    fn track_response<B>(
        &self,
        response: Response<B>,
        callback: impl FnOnce(Delivery) + Send + 'static,
        empty: bool,
    ) -> Result<Response<CompletionBody<B>>, AdmissionError> {
        let id = {
            let mut state = self.lock();
            if state.closed {
                return Err(AdmissionError::Closed);
            }
            if state.records.len() >= state.limit {
                return Err(AdmissionError::Capacity);
            }
            let id = state.next_id;
            state.next_id = id.checked_add(1).ok_or(AdmissionError::Capacity)?;
            state.records.push_back(Record {
                id,
                eof: empty,
                submitted: false,
                callback: Box::new(callback),
            });
            id
        };
        let (mut parts, body) = response.into_parts();
        parts.headers.remove(CONTENT_LENGTH);
        parts.headers.remove(TRANSFER_ENCODING);
        let receipt = ResponseReceipt {
            registry: self.clone(),
            id,
        };
        parts
            .extensions
            .insert(hyper::ext::NodeHttpResponseSubmission::new(move || {
                receipt.registry.submitted(receipt.id);
            }));
        Ok(Response::from_parts(
            parts,
            CompletionBody {
                inner: Box::pin(body),
                registry: self.clone(),
                id,
                finished: false,
                empty,
                trailers: None,
            },
        ))
    }

    fn submitted(&self, id: u64) {
        let mut state = self.lock();
        if let Some(index) = state.records.iter().position(|record| record.id == id) {
            let mut record = state.records.remove(index).expect("known response record");
            record.submitted = true;
            // Futures can finish out of order. Actual head submission establishes
            // wire order and must precede every response still waiting to write.
            let before = state
                .records
                .iter()
                .position(|row| !row.submitted)
                .unwrap_or(state.records.len());
            state.records.insert(before, record);
        }
    }
    fn was_submitted(&self, id: u64) -> bool {
        self.lock()
            .records
            .iter()
            .any(|record| record.id == id && record.submitted)
    }

    fn eof(&self, id: u64) {
        if let Some(record) = self.lock().records.iter_mut().find(|row| row.id == id) {
            record.eof = true;
        }
    }

    fn fail(&self, id: u64, reason: Failure) {
        let record = {
            let mut state = self.lock();
            state
                .records
                .iter()
                .position(|row| row.id == id)
                .and_then(|index| state.records.remove(index))
        };
        if let Some(record) = record {
            emit(record.callback, Delivery::Failed(reason));
        }
    }

    fn flushed(&self) {
        let ready = {
            let mut state = self.lock();
            let mut ready = Vec::new();
            while state
                .records
                .front()
                .is_some_and(|record| record.eof && record.submitted)
            {
                ready.push(state.records.pop_front().expect("front exists"));
            }
            ready
        };
        for record in ready {
            emit(record.callback, Delivery::Flushed);
        }
    }

    fn close(&self, reason: Failure) {
        let records = {
            let mut state = self.lock();
            state.closed = true;
            std::mem::take(&mut state.records)
        };
        for record in records {
            emit(record.callback, Delivery::Failed(reason));
        }
    }
}

fn emit(callback: Callback, outcome: Delivery) {
    // Observer/embedding callbacks must not interrupt response transport.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(outcome)));
}

/// Clean body EOF transfers completion responsibility to the connection.
pub struct CompletionBody<B> {
    inner: Pin<Box<B>>,
    registry: CompletionRegistry,
    id: u64,
    finished: bool,
    empty: bool,
    trailers: Option<hyper::HeaderMap>,
}

impl<B: Body> Body for CompletionBody<B> {
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if this.empty {
            this.finished = true;
            return Poll::Ready(None);
        }
        loop {
            match this.inner.as_mut().poll_frame(cx) {
                Poll::Ready(None) => {
                    this.finished = true;
                    this.registry.eof(this.id);
                    return Poll::Ready(
                        this.trailers
                            .take()
                            .map(|headers| Ok(Frame::trailers(headers))),
                    );
                }
                Poll::Ready(Some(Err(error))) => {
                    this.finished = true;
                    this.registry.fail(this.id, Failure::Body);
                    return Poll::Ready(Some(Err(error)));
                }
                Poll::Ready(Some(Ok(frame))) if frame.is_trailers() => {
                    // Hyper stops polling a body when it receives trailers.
                    // Retain only this bounded metadata until the source proves
                    // clean EOF, then emit it and let the writer acknowledge it.
                    this.trailers = frame.into_trailers().ok();
                }
                frame => return frame,
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.finished || self.empty
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}

impl<B> Drop for CompletionBody<B> {
    fn drop(&mut self) {
        if (!self.finished && !self.empty) || !self.registry.was_submitted(self.id) {
            self.registry.fail(self.id, Failure::Abandoned);
        }
    }
}

// Hyper1.12 dispatch::poll_write polls this service future to Ready and then
// calls write_head in the same synchronous stack before it can flush again.
// Binding at this boundary prevents unrelated empty flushes during a pending
// service future from acknowledging an empty-status response prematurely.
struct SubmitService<S> {
    inner: S,
    header_phase: Arc<Mutex<Option<tokio::time::Instant>>>,
}

/// Absolute request-receive bound carried from Hyper's header-read phase into
/// the service. This includes time spent waiting for headers, avoiding a fresh
/// 30-second budget after a nearly 10-second header read. Hyper begins that
/// phase before its next read, so this is a conservative bound: it includes
/// pre-request keep-alive idle time and cannot recover arrival times of already
/// buffered pipelined messages. It is not an exact Node message-begin clock.
#[derive(Clone, Copy, Debug)]
pub struct RequestReceiveDeadline {
    pub header_started: tokio::time::Instant,
    pub deadline: tokio::time::Instant,
}

#[derive(Clone)]
struct HeaderTimer {
    header_phase: Arc<Mutex<Option<tokio::time::Instant>>>,
}
impl HeaderTimer {
    fn mark(&self, deadline: std::time::Instant) {
        *self.header_phase.lock().unwrap() =
            Some(tokio::time::Instant::from_std(deadline) - Duration::from_secs(10));
    }
}
impl Timer for HeaderTimer {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Sleep>> {
        self.sleep_until(self.now() + duration)
    }
    fn sleep_until(&self, deadline: std::time::Instant) -> Pin<Box<dyn Sleep>> {
        self.mark(deadline);
        TokioTimer::new().sleep_until(deadline)
    }
    fn reset(&self, sleep: &mut Pin<Box<dyn Sleep>>, deadline: std::time::Instant) {
        self.mark(deadline);
        TokioTimer::new().reset(sleep, deadline);
    }
    fn now(&self) -> std::time::Instant {
        TokioTimer::new().now()
    }
}
impl<S, B> Service<Request<Incoming>> for SubmitService<S>
where
    S: Service<Request<Incoming>, Response = Response<B>>,
{
    type Response = Response<B>;
    type Error = S::Error;
    type Future = S::Future;
    fn call(&self, mut request: Request<Incoming>) -> Self::Future {
        if let Some(header_started) = *self.header_phase.lock().unwrap() {
            request.extensions_mut().insert(RequestReceiveDeadline {
                header_started,
                deadline: header_started + Duration::from_secs(30),
            });
        }
        self.inner.call(request)
    }
}

struct CompletionIo<I> {
    inner: I,
    registry: CompletionRegistry,
}

impl<I: AsyncRead + Unpin> AsyncRead for CompletionIo<I> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_read(cx, buffer);
        if matches!(result, Poll::Ready(Err(_))) {
            this.registry.close(Failure::Io);
        }
        result
    }
}

impl<I: AsyncWrite + Unpin> AsyncWrite for CompletionIo<I> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_write(cx, buffer);
        if matches!(result, Poll::Ready(Err(_)) | Poll::Ready(Ok(0))) {
            this.registry.close(Failure::Io);
        }
        result
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffers: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_write_vectored(cx, buffers);
        if matches!(result, Poll::Ready(Err(_)) | Poll::Ready(Ok(0))) {
            this.registry.close(Failure::Io);
        }
        result
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_flush(cx);
        match result {
            Poll::Ready(Ok(())) => this.registry.flushed(),
            Poll::Ready(Err(_)) => this.registry.close(Failure::Io),
            Poll::Pending => {}
        }
        result
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_shutdown(cx);
        if matches!(result, Poll::Ready(Err(_))) {
            this.registry.close(Failure::Io);
        }
        result
    }
}

impl<I> Drop for CompletionIo<I> {
    fn drop(&mut self) {
        self.registry.close(Failure::ConnectionClosed);
    }
}

/// Drive a connection with the exact HTTP/1 configuration required by the
/// completion protocol. Cancelling/dropping this future fails remaining records.
/// Actual inference still needs its own receive/generation deadlines.
pub fn serve_http1<I, S, B>(
    io: I,
    registry: CompletionRegistry,
    service: S,
) -> impl Future<Output = Result<(), hyper::Error>>
where
    I: AsyncRead + AsyncWrite + Unpin + 'static,
    S: Service<Request<Incoming>, Response = Response<B>>,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    B: Body + 'static,
    B::Data: Buf,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let io = CompletionIo {
        inner: io,
        registry,
    };
    let mut builder = hyper::server::conn::http1::Builder::new();
    let header_phase = Arc::new(Mutex::new(None));
    builder
        .pipeline_flush(false)
        .half_close(false)
        .timer(HeaderTimer {
            header_phase: header_phase.clone(),
        })
        .header_read_timeout(Duration::from_secs(10));
    builder.serve_connection(
        TokioIo::new(io),
        SubmitService {
            inner: service,
            header_phase,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::service::service_fn;
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Waker;

    #[derive(Default)]
    struct FakeState {
        input: VecDeque<u8>,
        output: Vec<u8>,
        write_limit: usize,
        vectored: bool,
        vectored_calls: usize,
        pending_writes: usize,
        flush_blocked: bool,
        flush_failure: bool,
        fail_at: Option<usize>,
        flush_calls: usize,
        write_calls: usize,
        read_waker: Option<Waker>,
        flush_waker: Option<Waker>,
    }

    #[derive(Clone)]
    struct FakeIo(Arc<Mutex<FakeState>>);

    impl FakeIo {
        fn request(count: usize) -> Self {
            Self(Arc::new(Mutex::new(FakeState {
                input:
                    b"POST /v1/messages HTTP/1.1\r\nhost: localhost\r\ncontent-length: 0\r\n\r\n"
                        .repeat(count)
                        .into(),
                write_limit: usize::MAX,
                ..FakeState::default()
            })))
        }

        fn write(&self, cx: &mut Context<'_>, bytes: &[u8]) -> Poll<io::Result<usize>> {
            let mut state = self.0.lock().unwrap();
            state.write_calls += 1;
            if state.pending_writes > 0 {
                state.pending_writes -= 1;
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            if state.fail_at.is_some_and(|at| state.output.len() >= at) {
                return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
            }
            let before_failure = state
                .fail_at
                .map_or(usize::MAX, |at| at.saturating_sub(state.output.len()));
            let count = bytes.len().min(state.write_limit).min(before_failure);
            state.output.extend_from_slice(&bytes[..count]);
            Poll::Ready(Ok(count))
        }
    }

    impl AsyncRead for FakeIo {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            output: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let mut state = self.0.lock().unwrap();
            if state.input.is_empty() {
                state.read_waker = Some(cx.waker().clone());
                return Poll::Pending; // Keep the peer connection alive.
            }
            let count = state.input.len().min(output.remaining());
            let data: Vec<_> = state.input.drain(..count).collect();
            output.put_slice(&data);
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncWrite for FakeIo {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.write(cx, bytes)
        }

        fn poll_write_vectored(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buffers: &[IoSlice<'_>],
        ) -> Poll<io::Result<usize>> {
            self.0.lock().unwrap().vectored_calls += 1;
            let joined: Vec<u8> = buffers
                .iter()
                .flat_map(|part| part.iter().copied())
                .collect();
            self.write(cx, &joined)
        }

        fn is_write_vectored(&self) -> bool {
            self.0.lock().unwrap().vectored
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            let mut state = self.0.lock().unwrap();
            state.flush_calls += 1;
            if state.flush_blocked {
                state.flush_waker = Some(cx.waker().clone());
                Poll::Pending
            } else if state.flush_failure {
                Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()))
            } else {
                Poll::Ready(Ok(()))
            }
        }

        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    type Outcomes = Arc<Mutex<Vec<(usize, Delivery)>>>;

    struct ScriptedBody {
        frames: VecDeque<Result<Frame<Bytes>, io::Error>>,
        wait_after_first: Arc<Mutex<Option<Waker>>>,
        released: Arc<std::sync::atomic::AtomicBool>,
        polled: bool,
    }

    impl Body for ScriptedBody {
        type Data = Bytes;
        type Error = io::Error;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
            if self.polled && !self.released.load(Ordering::SeqCst) {
                *self.wait_after_first.lock().unwrap() = Some(cx.waker().clone());
                return Poll::Pending;
            }
            self.polled = true;
            Poll::Ready(self.frames.pop_front())
        }
    }

    fn start(
        io: FakeIo,
        registry: CompletionRegistry,
        outcomes: Outcomes,
    ) -> tokio::task::JoinHandle<Result<(), hyper::Error>> {
        let next = AtomicUsize::new(0);
        tokio::spawn(serve_http1(
            io,
            registry.clone(),
            service_fn(move |_: Request<Incoming>| {
                let index = next.fetch_add(1, Ordering::SeqCst);
                let outcomes = outcomes.clone();
                let response = registry
                    .track(
                        Response::new(Full::new(Bytes::from_static(b"PAYLOAD"))),
                        move |result| {
                            outcomes.lock().unwrap().push((index, result));
                        },
                    )
                    .unwrap();
                async { Ok::<_, Infallible>(response) }
            }),
        ))
    }

    async fn settle() {
        for _ in 0..30 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_pipeline_keeps_queued_bodyless_response_unsubmitted_until_its_write() {
        for fail_first in [false, true] {
            let io = FakeIo::request(2);
            let registry = CompletionRegistry::new(4);
            let outcomes = Outcomes::default();
            let calls = Arc::new(AtomicUsize::new(0));
            let (release, wait) = tokio::sync::oneshot::channel::<()>();
            let wait = Arc::new(Mutex::new(Some(wait)));
            let registered = registry.clone();
            let observed = outcomes.clone();
            let called = calls.clone();
            let task = tokio::spawn(serve_http1(
                io.clone(),
                registry.clone(),
                service_fn(move |_| {
                    let index = called.fetch_add(1, Ordering::SeqCst);
                    let wait = if index == 0 {
                        wait.lock().unwrap().take()
                    } else {
                        None
                    };
                    let registered = registered.clone();
                    let observed = observed.clone();
                    async move {
                        if let Some(wait) = wait {
                            let _ = wait.await;
                        }
                        let response = registered
                            .track(
                                Response::builder()
                                    .status(204)
                                    .body(Full::new(Bytes::new()))
                                    .unwrap(),
                                move |outcome| observed.lock().unwrap().push((index, outcome)),
                            )
                            .unwrap();
                        Ok::<_, Infallible>(response)
                    }
                }),
            ));
            settle().await;
            assert_eq!(
                calls.load(Ordering::SeqCst),
                2,
                "second service starts before first response"
            );
            assert_eq!(registry.pending(), 1, "second response finishes first");
            registry.flushed();
            assert!(outcomes.lock().unwrap().is_empty());
            assert!(io.0.lock().unwrap().output.is_empty());
            // Idle between complete request heads has no running header timer.
            tokio::time::advance(Duration::from_secs(11)).await;
            settle().await;
            assert!(!task.is_finished());
            io.0.lock().unwrap().flush_blocked = true;
            release.send(()).unwrap();
            settle().await;
            assert_eq!(registry.pending(), 2);
            assert!(outcomes.lock().unwrap().is_empty());
            assert_eq!(
                io.0.lock()
                    .unwrap()
                    .output
                    .windows(12)
                    .filter(|part| *part == b"HTTP/1.1 204")
                    .count(),
                1
            );
            {
                let mut state = io.0.lock().unwrap();
                state.flush_blocked = false;
                state.flush_failure = fail_first;
                state.flush_waker.take().unwrap().wake();
            }
            settle().await;
            if fail_first {
                assert!(task.await.unwrap().is_err());
                assert!(
                    outcomes
                        .lock()
                        .unwrap()
                        .iter()
                        .all(|(_, result)| matches!(result, Delivery::Failed(_)))
                );
            } else {
                assert_eq!(
                    *outcomes.lock().unwrap(),
                    [(0, Delivery::Flushed), (1, Delivery::Flushed)]
                );
                task.abort();
                let _ = task.await;
            }
            assert_eq!(registry.pending(), 0);
        }
    }

    #[tokio::test]
    async fn pipeline_retains_each_request_method_and_body_while_futures_finish_out_of_order() {
        use http_body_util::BodyExt;
        let io = FakeIo::request(0);
        io.0.lock().unwrap().input.extend(b"HEAD /first HTTP/1.1\r\nhost: local\r\n\r\nPOST /second HTTP/1.1\r\nhost: local\r\ncontent-length: 6\r\nconnection: close\r\n\r\nSECOND");
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let recorded = bodies.clone();
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let wait = Arc::new(Mutex::new(Some(wait)));
        let task = tokio::spawn(serve_http1(
            io.clone(),
            CompletionRegistry::new(4),
            service_fn(move |request: Request<Incoming>| {
                let first = request.method() == hyper::Method::HEAD;
                let wait = if first {
                    wait.lock().unwrap().take()
                } else {
                    None
                };
                let recorded = recorded.clone();
                async move {
                    let body = request.into_body().collect().await.unwrap().to_bytes();
                    recorded.lock().unwrap().push(body);
                    if let Some(wait) = wait {
                        let _ = wait.await;
                    }
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(if first {
                        b"FIRST HIDDEN"
                    } else {
                        b"SECOND VISIBLE"
                    }))))
                }
            }),
        ));
        settle().await;
        assert_eq!(
            *bodies.lock().unwrap(),
            [Bytes::new(), Bytes::from_static(b"SECOND")]
        );
        assert!(io.0.lock().unwrap().output.is_empty());
        release.send(()).unwrap();
        task.await.unwrap().unwrap();
        let output = String::from_utf8(io.0.lock().unwrap().output.clone()).unwrap();
        assert_eq!(output.matches("HTTP/1.1 200").count(), 2);
        assert!(!output.contains("FIRST HIDDEN"));
        assert!(output.ends_with("SECOND VISIBLE"));
    }

    #[tokio::test]
    async fn pipeline_backpressure_bounds_pending_services_without_rejecting_later_requests() {
        let io = FakeIo::request(100);
        let registry = CompletionRegistry::new(16);
        let outcomes = Outcomes::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let called = calls.clone();
        let recorded = outcomes.clone();
        let registered = registry.clone();
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let wait = Arc::new(Mutex::new(Some(wait)));
        let task = tokio::spawn(serve_http1(
            io,
            registry.clone(),
            service_fn(move |_| {
                let index = called.fetch_add(1, Ordering::SeqCst);
                let wait = if index == 0 {
                    wait.lock().unwrap().take()
                } else {
                    None
                };
                let registered = registered.clone();
                let recorded = recorded.clone();
                async move {
                    if let Some(wait) = wait {
                        let _ = wait.await;
                    }
                    let response = registered
                        .track(
                            Response::new(Full::new(Bytes::from_static(b"body"))),
                            move |outcome| recorded.lock().unwrap().push((index, outcome)),
                        )
                        .unwrap();
                    Ok::<_, Infallible>(response)
                }
            }),
        ));
        settle().await;
        assert_eq!(calls.load(Ordering::SeqCst), 15);
        assert_eq!(registry.pending(), 14);
        assert!(outcomes.lock().unwrap().is_empty());
        release.send(()).unwrap();
        settle().await;
        assert_eq!(calls.load(Ordering::SeqCst), 100);
        assert_eq!(
            *outcomes.lock().unwrap(),
            (0..100)
                .map(|index| (index, Delivery::Flushed))
                .collect::<Vec<_>>()
        );
        assert_eq!(registry.pending(), 0);
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn keepalive_request_body_delivers_eof_before_waiting_for_another_head() {
        use http_body_util::BodyExt;
        let io = FakeIo::request(0);
        io.0.lock()
            .unwrap()
            .input
            .extend(b"POST / HTTP/1.1\r\nhost: local\r\ncontent-length: 4\r\n\r\nBODY");
        let task = tokio::spawn(serve_http1(
            io.clone(),
            CompletionRegistry::new(4),
            service_fn(|request: Request<Incoming>| async move {
                let body = request.into_body().collect().await.unwrap().to_bytes();
                assert_eq!(body, "BODY");
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"complete"))))
            }),
        ));
        settle().await;
        assert!(io.0.lock().unwrap().output.ends_with(b"complete"));
        assert!(!task.is_finished());
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn node_header_limits_apply_before_truncation_and_before_complete_head() {
        for (count, auth_present) in [(997, true), (998, false), (16300, false)] {
            let io = FakeIo::request(0);
            let raw = format!(
                "GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{}authorization: synthetic\r\n\r\n",
                "z:\r\n".repeat(count)
            );
            io.0.lock().unwrap().input.extend(raw.as_bytes());
            let received = Arc::new(Mutex::new(None));
            let result = received.clone();
            let task = tokio::spawn(serve_http1(
                io,
                CompletionRegistry::new(1),
                service_fn(move |request: Request<Incoming>| {
                    *result.lock().unwrap() = Some(request.headers().clone());
                    async { Ok::<_, Infallible>(Response::new(Full::new(Bytes::new()))) }
                }),
            ));
            task.await.unwrap().unwrap();
            let received = received.lock().unwrap();
            let headers = received.as_ref().unwrap();
            assert_eq!(headers.contains_key("authorization"), auth_present);
            assert_eq!(headers.len(), 1000);
        }
        let io = FakeIo::request(0);
        io.0.lock()
            .unwrap()
            .input
            .extend(b"GET / HTTP/1.1\r\nx:".iter());
        let task = tokio::spawn(serve_http1(
            io.clone(),
            CompletionRegistry::new(1),
            service_fn(|_: Request<Incoming>| async {
                panic!("oversized incomplete headers cannot reach the service");
                #[allow(unreachable_code)]
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
            }),
        ));
        settle().await;
        {
            let mut state = io.0.lock().unwrap();
            state.input.extend(std::iter::repeat_n(b'a', 16382));
            state.read_waker.take().unwrap().wake();
        }
        assert!(task.await.unwrap().is_err());
        assert!(io.0.lock().unwrap().output.starts_with(b"HTTP/1.1 431"));
    }

    #[tokio::test]
    async fn discarded_header_whitespace_never_changes_body_or_next_pipeline_request() {
        use http_body_util::BodyExt;
        let io = FakeIo::request(0);
        let payload = b"  \t\r\n\0\xffPAYLOAD";
        let mut request = format!("POST  /one  HTTP/1.1\r\nhost: localhost\r\nx-padding:{}kept\r\ncontent-length: {}\r\n\r\n", " ".repeat(1_000_000),payload.len()).into_bytes();
        request.extend_from_slice(payload);
        request.extend_from_slice(b"POST /two HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\ncontent-length: 3\r\n\r\n  x");
        io.0.lock().unwrap().input.extend(request);
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = received.clone();
        let task = tokio::spawn(serve_http1(
            io,
            CompletionRegistry::new(1),
            service_fn(move |request: Request<Incoming>| {
                let sink = sink.clone();
                async move {
                    let path = request.uri().path().to_owned();
                    let padding = request.headers().get("x-padding").cloned();
                    let bytes = request.into_body().collect().await.unwrap().to_bytes();
                    sink.lock().unwrap().push((path, padding, bytes));
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
                }
            }),
        ));
        task.await.unwrap().unwrap();
        let received = received.lock().unwrap();
        assert_eq!(received.len(), 2);
        assert_eq!(received[0].0, "/one");
        assert_eq!(received[0].1.as_ref().unwrap(), "kept");
        assert_eq!(received[0].2.as_ref(), payload);
        assert_eq!(received[1].0, "/two");
        assert_eq!(received[1].2.as_ref(), b"  x");
    }

    #[tokio::test(start_paused = true)]
    async fn body_deadline_includes_the_header_read_phase() {
        use http_body_util::BodyExt;
        let io = FakeIo::request(0);
        io.0.lock()
            .unwrap()
            .input
            .extend(b"POST /v1/messages HTTP/1.1\r\nhost:".iter());
        let phase_started = tokio::time::Instant::now();
        let deadlines = Arc::new(Mutex::new(Vec::new()));
        let observed = deadlines.clone();
        let expired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = expired.clone();
        let task = tokio::spawn(serve_http1(
            io.clone(),
            CompletionRegistry::new(1),
            service_fn(move |request: Request<Incoming>| {
                let deadline = *request
                    .extensions()
                    .get::<RequestReceiveDeadline>()
                    .unwrap();
                observed.lock().unwrap().push(deadline);
                let flag = flag.clone();
                async move {
                    assert!(
                        tokio::time::timeout_at(deadline.deadline, request.into_body().collect())
                            .await
                            .is_err()
                    );
                    flag.store(true, Ordering::SeqCst);
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::new())))
                }
            }),
        ));
        settle().await;
        tokio::time::advance(Duration::from_secs(9)).await;
        {
            let mut state = io.0.lock().unwrap();
            state
                .input
                .extend(b" localhost\r\ncontent-length: 1\r\n\r\n".iter());
            state.read_waker.take().unwrap().wake();
        }
        settle().await;
        assert_eq!(deadlines.lock().unwrap()[0].header_started, phase_started);
        assert_eq!(
            deadlines.lock().unwrap()[0].deadline,
            phase_started + Duration::from_secs(30)
        );
        tokio::time::advance(Duration::from_secs(20)).await;
        settle().await;
        assert!(!expired.load(Ordering::SeqCst));
        tokio::time::advance(Duration::from_secs(1)).await;
        settle().await;
        assert!(
            expired.load(Ordering::SeqCst),
            "body cannot acquire another 30 seconds after headers"
        );
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn bodyless_responses_wait_for_service_handoff_and_header_flush() {
        for status in [204, 304] {
            for fail in [false, true] {
                let io = FakeIo::request(1);
                let registry = CompletionRegistry::new(4);
                let outcomes = Outcomes::default();
                let (release, wait) = tokio::sync::oneshot::channel::<()>();
                let wait = Arc::new(Mutex::new(Some(wait)));
                let response_registry = registry.clone();
                let results = outcomes.clone();
                let task = tokio::spawn(serve_http1(
                    io.clone(),
                    registry.clone(),
                    service_fn(move |_: Request<Incoming>| {
                        let results = results.clone();
                        let response = response_registry
                            .track(
                                Response::builder()
                                    .status(status)
                                    .body(Full::new(Bytes::from_static(b"MUST NOT WRITE")))
                                    .unwrap(),
                                move |outcome| results.lock().unwrap().push((0, outcome)),
                            )
                            .unwrap();
                        let wait = wait.lock().unwrap().take().unwrap();
                        async move {
                            let _ = wait.await;
                            Ok::<_, Infallible>(response)
                        }
                    }),
                ));
                settle().await;
                assert_eq!(registry.pending(), 1);
                assert!(io.0.lock().unwrap().output.is_empty());
                assert!(outcomes.lock().unwrap().is_empty());
                // Even an unrelated successful idle flush is insufficient.
                registry.flushed();
                assert!(outcomes.lock().unwrap().is_empty());
                io.0.lock().unwrap().flush_blocked = true;
                release.send(()).unwrap();
                settle().await;
                {
                    let state = io.0.lock().unwrap();
                    assert!(
                        state
                            .output
                            .starts_with(format!("HTTP/1.1 {status}").as_bytes())
                    );
                    assert!(state.output.ends_with(b"\r\n\r\n"));
                    assert!(
                        !state
                            .output
                            .windows(14)
                            .any(|part| part == b"MUST NOT WRITE")
                    );
                }
                assert_eq!(registry.pending(), 1);
                assert!(
                    outcomes.lock().unwrap().is_empty(),
                    "body drop is not delivery"
                );
                {
                    let mut state = io.0.lock().unwrap();
                    state.flush_blocked = false;
                    state.flush_failure = fail;
                    state.flush_waker.take().unwrap().wake();
                }
                settle().await;
                assert_eq!(registry.pending(), 0);
                assert_eq!(
                    *outcomes.lock().unwrap(),
                    [(
                        0,
                        if fail {
                            Delivery::Failed(Failure::Io)
                        } else {
                            Delivery::Flushed
                        }
                    )]
                );
                if fail {
                    assert!(task.await.unwrap().is_err());
                } else {
                    assert!(
                        !task.is_finished(),
                        "header delivery does not close keep-alive"
                    );
                    task.abort();
                    let _ = task.await;
                }
            }
        }
    }

    #[test]
    fn bodyless_response_dropped_before_service_handoff_is_not_delivered() {
        let registry = CompletionRegistry::new(1);
        let outcomes = Outcomes::default();
        let results = outcomes.clone();
        let response = registry
            .track_empty_response(Response::new(Full::new(Bytes::new())), move |outcome| {
                results.lock().unwrap().push((0, outcome))
            })
            .unwrap();
        registry.flushed();
        assert!(outcomes.lock().unwrap().is_empty());
        drop(response);
        assert_eq!(
            *outcomes.lock().unwrap(),
            [(0, Delivery::Failed(Failure::Abandoned))]
        );
    }

    #[tokio::test]
    async fn partial_vectored_and_pending_writes_flush_before_open_keepalive_confirmation() {
        for vectored in [false, true] {
            let io = FakeIo::request(1);
            {
                let mut state = io.0.lock().unwrap();
                state.write_limit = 3;
                state.vectored = vectored;
                state.pending_writes = 3;
                state.flush_blocked = true;
            }
            let registry = CompletionRegistry::new(4);
            let outcomes = Outcomes::default();
            let task = start(io.clone(), registry.clone(), outcomes.clone());
            settle().await;
            assert!(
                io.0.lock()
                    .unwrap()
                    .output
                    .ends_with(b"7\r\nPAYLOAD\r\n0\r\n\r\n")
            );
            assert!(
                outcomes.lock().unwrap().is_empty(),
                "EOF/body drop is not transport completion"
            );
            assert_eq!(registry.pending(), 1);
            {
                let mut state = io.0.lock().unwrap();
                state.flush_blocked = false;
                state.flush_waker.take().unwrap().wake();
            }
            settle().await;
            assert_eq!(*outcomes.lock().unwrap(), [(0, Delivery::Flushed)]);
            assert_eq!(registry.pending(), 0);
            assert!(!task.is_finished(), "keep-alive remains usable");
            assert!(io.0.lock().unwrap().write_calls > 3);
            assert_eq!(io.0.lock().unwrap().vectored_calls > 0, vectored);
            task.abort();
            let _ = task.await;
            assert_eq!(*outcomes.lock().unwrap(), [(0, Delivery::Flushed)]);
        }
    }

    #[tokio::test]
    async fn error_after_all_payload_and_framing_bytes_before_flush_never_confirms() {
        let io = FakeIo::request(1);
        io.0.lock().unwrap().flush_failure = true;
        let registry = CompletionRegistry::new(4);
        let outcomes = Outcomes::default();
        assert!(
            start(io.clone(), registry.clone(), outcomes.clone())
                .await
                .unwrap()
                .is_err()
        );
        assert!(
            io.0.lock()
                .unwrap()
                .output
                .ends_with(b"PAYLOAD\r\n0\r\n\r\n")
        );
        assert_eq!(
            *outcomes.lock().unwrap(),
            [(0, Delivery::Failed(Failure::Io))]
        );
        assert_eq!(registry.pending(), 0);
    }

    #[tokio::test]
    async fn cancellation_after_body_drop_while_flush_pending_releases_attempt() {
        let io = FakeIo::request(1);
        io.0.lock().unwrap().flush_blocked = true;
        let registry = CompletionRegistry::new(4);
        let outcomes = Outcomes::default();
        let task = start(io.clone(), registry.clone(), outcomes.clone());
        settle().await;
        assert!(io.0.lock().unwrap().output.ends_with(b"0\r\n\r\n"));
        assert_eq!(registry.pending(), 1);
        task.abort();
        let _ = task.await;
        assert_eq!(
            *outcomes.lock().unwrap(),
            [(0, Delivery::Failed(Failure::ConnectionClosed))]
        );
        assert_eq!(registry.pending(), 0);
    }

    #[tokio::test]
    async fn partial_flush_does_not_confirm_and_later_body_error_releases_attempt() {
        let io = FakeIo::request(1);
        let registry = CompletionRegistry::new(4);
        let outcomes = Outcomes::default();
        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let waiter = Arc::new(Mutex::new(None));
        let body = Arc::new(Mutex::new(Some(ScriptedBody {
            frames: VecDeque::from([
                Ok(Frame::data(Bytes::from_static(b"FIRST"))),
                Err(io::ErrorKind::UnexpectedEof.into()),
            ]),
            wait_after_first: waiter.clone(),
            released: released.clone(),
            polled: false,
        })));
        let registered = registry.clone();
        let recorded = outcomes.clone();
        let task = tokio::spawn(serve_http1(
            io.clone(),
            registry.clone(),
            service_fn(move |_| {
                let recorded = recorded.clone();
                let response = registered
                    .track(
                        Response::new(body.lock().unwrap().take().unwrap()),
                        move |outcome| {
                            recorded.lock().unwrap().push((0, outcome));
                        },
                    )
                    .unwrap();
                async { Ok::<_, Infallible>(response) }
            }),
        ));
        settle().await;
        assert!(io.0.lock().unwrap().output.ends_with(b"5\r\nFIRST\r\n"));
        assert!(io.0.lock().unwrap().flush_calls > 0);
        assert!(outcomes.lock().unwrap().is_empty());
        released.store(true, Ordering::SeqCst);
        waiter.lock().unwrap().take().unwrap().wake();
        assert!(task.await.unwrap().is_err());
        assert_eq!(
            *outcomes.lock().unwrap(),
            [(0, Delivery::Failed(Failure::Body))]
        );
        assert_eq!(registry.pending(), 0);
    }

    #[tokio::test]
    async fn trailer_frame_does_not_skip_source_eof_or_the_transport_flush() {
        let io = FakeIo::request(1);
        {
            let mut state = io.0.lock().unwrap();
            state.flush_blocked = true;
            state.input = b"POST /v1/messages HTTP/1.1\r\nhost: localhost\r\nte: trailers\r\ncontent-length: 0\r\n\r\n".to_vec().into();
        }
        let registry = CompletionRegistry::new(4);
        let outcomes = Outcomes::default();
        let mut trailers = hyper::HeaderMap::new();
        trailers.insert("x-synthetic-trailer", "complete".parse().unwrap());
        let body = Arc::new(Mutex::new(Some(ScriptedBody {
            frames: VecDeque::from([
                Ok(Frame::data(Bytes::from_static(b"DATA"))),
                Ok(Frame::trailers(trailers)),
            ]),
            wait_after_first: Arc::new(Mutex::new(None)),
            released: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            polled: false,
        })));
        let registered = registry.clone();
        let recorded = outcomes.clone();
        let task = tokio::spawn(serve_http1(
            io.clone(),
            registry.clone(),
            service_fn(move |_| {
                let recorded = recorded.clone();
                let mut response = Response::new(body.lock().unwrap().take().unwrap());
                response
                    .headers_mut()
                    .insert("trailer", "x-synthetic-trailer".parse().unwrap());
                let response = registered
                    .track(response, move |outcome| {
                        recorded.lock().unwrap().push((0, outcome));
                    })
                    .unwrap();
                async { Ok::<_, Infallible>(response) }
            }),
        ));
        settle().await;
        assert!(outcomes.lock().unwrap().is_empty());
        assert_eq!(registry.pending(), 1);
        {
            let mut state = io.0.lock().unwrap();
            assert!(
                state
                    .output
                    .ends_with(b"0\r\nx-synthetic-trailer: complete\r\n\r\n"),
                "{}",
                String::from_utf8_lossy(&state.output)
            );
            state.flush_blocked = false;
            state.flush_waker.take().unwrap().wake();
        }
        settle().await;
        assert_eq!(*outcomes.lock().unwrap(), [(0, Delivery::Flushed)]);
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn two_pipelined_responses_confirm_first_before_second_framing_write_fails() {
        // Measure exact framing size using the same driver, rather than relying
        // on a particular Date header or Hyper serialization implementation.
        let baseline = FakeIo::request(1);
        let task = start(
            baseline.clone(),
            CompletionRegistry::new(4),
            Outcomes::default(),
        );
        settle().await;
        let response_len = baseline.0.lock().unwrap().output.len();
        task.abort();
        let _ = task.await;

        let io = FakeIo::request(2);
        {
            let mut state = io.0.lock().unwrap();
            state.vectored = true;
            state.write_limit = 5;
            // All second response payload bytes are accepted, but the final
            // terminating chunk cannot complete.
            state.fail_at = Some(response_len * 2 - 3);
        }
        let registry = CompletionRegistry::new(4);
        let outcomes = Outcomes::default();
        assert!(
            start(io.clone(), registry.clone(), outcomes.clone())
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(
            *outcomes.lock().unwrap(),
            [(0, Delivery::Flushed), (1, Delivery::Failed(Failure::Io)),]
        );
        assert_eq!(io.0.lock().unwrap().output.len(), response_len * 2 - 3);
        assert_eq!(registry.pending(), 0);
    }

    #[test]
    fn admission_is_bounded_and_unpolled_body_drop_is_failure() {
        let registry = CompletionRegistry::new(1);
        let outcomes = Outcomes::default();
        let recorded = outcomes.clone();
        let response = registry
            .track(Response::new(Full::new(Bytes::new())), move |event| {
                recorded.lock().unwrap().push((0, event));
            })
            .unwrap();
        assert!(matches!(
            registry.track(Response::new(Full::new(Bytes::new())), |_| {}),
            Err(AdmissionError::Capacity)
        ));
        drop(response);
        assert_eq!(
            *outcomes.lock().unwrap(),
            [(0, Delivery::Failed(Failure::Abandoned))]
        );
        assert_eq!(registry.pending(), 0);
        registry.close(Failure::ConnectionClosed);
        assert!(matches!(
            registry.track(Response::new(Full::new(Bytes::new())), |_| {}),
            Err(AdmissionError::Closed)
        ));
    }

    #[tokio::test]
    async fn provider_completion_and_successful_transport_are_independent_required_evidence() {
        use crate::response_observer::{Observation, ObservedBody, ResponseObserver};
        use std::sync::atomic::AtomicBool;

        for fail_flush in [false, true] {
            let io = FakeIo::request(1);
            io.0.lock().unwrap().flush_blocked = true;
            let evidence = Arc::new(Mutex::new(None));
            let committed = Arc::new(AtomicBool::new(false));
            let registry = CompletionRegistry::new(4);
            let registered = registry.clone();
            let observed = evidence.clone();
            let commit = committed.clone();
            let task = tokio::spawn(serve_http1(
                io.clone(),
                registry.clone(),
                service_fn(move |_| {
                    let observed = observed.clone();
                    let completed = observed.clone();
                    let commit = commit.clone();
                    let observer = ResponseObserver::new("application/json", 1024, move |event| {
                        if let Observation::Complete(value) = event {
                            *observed.lock().unwrap() = Some(value);
                        }
                    })
                    .unwrap();
                    let body = ObservedBody::new(Full::new(Bytes::from_static(
                    br#"{"model":"synthetic-opus","content":[],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":2}}"#,
                )), observer);
                    let response = registered
                        .track(Response::new(body), move |delivery| {
                            // Actual gateway integration adds successful HTTP status,
                            // request cancellation and the current attempt-sequence gate.
                            if delivery == Delivery::Flushed
                                && completed
                                    .lock()
                                    .unwrap()
                                    .as_ref()
                                    .is_some_and(|value| value.continuation_model.is_some())
                            {
                                commit.store(true, Ordering::SeqCst);
                            }
                        })
                        .unwrap();
                    async { Ok::<_, Infallible>(response) }
                }),
            ));
            settle().await;
            assert!(
                evidence.lock().unwrap().is_some(),
                "clean provider completion was observed"
            );
            assert!(
                !committed.load(Ordering::SeqCst),
                "buffered terminal bytes cannot commit continuity"
            );
            assert_eq!(registry.pending(), 1);
            {
                let mut state = io.0.lock().unwrap();
                state.flush_blocked = false;
                state.flush_failure = fail_flush;
                state.flush_waker.take().unwrap().wake();
            }
            settle().await;
            assert_eq!(committed.load(Ordering::SeqCst), !fail_flush);
            assert_eq!(registry.pending(), 0);
            if fail_flush {
                assert!(task.await.unwrap().is_err());
            } else {
                task.abort();
                let _ = task.await;
            }
        }
    }
}
