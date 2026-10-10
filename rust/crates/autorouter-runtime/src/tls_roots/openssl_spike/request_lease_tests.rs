//! Lease ownership only: no destructive IO abort or session eviction.
use super::lifecycle_tests::{Fixture, Reply};
use crate::http_client::HttpTransport;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    Request, Uri, Version,
    body::Body,
    rt::{Read, ReadBufCursor, Write},
};
use hyper_util::{
    client::legacy::{
        Client,
        connect::{Connected, Connection, capture_http1_assignment},
    },
    rt::TokioIo,
};
use openssl::ssl::SslVersion;
use std::{
    collections::VecDeque,
    future::Future,
    io,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};
use tokio::{
    io::{AsyncWriteExt, DuplexStream},
    sync::mpsc,
    task::JoinSet,
};
use tower_service::Service;

type Task = Pin<Box<dyn Future<Output = ()> + Send>>;
#[derive(Clone, Default)]
struct HeldExecutor(Arc<Mutex<VecDeque<Task>>>, Arc<AtomicBool>);
impl<F: Future<Output = ()> + Send + 'static> hyper::rt::Executor<F> for HeldExecutor {
    fn execute(&self, future: F) {
        assert!(
            !self.1.swap(false, Ordering::SeqCst),
            "synthetic executor rejection"
        );
        self.0.lock().unwrap().push_back(Box::pin(future));
    }
}
impl HeldExecutor {
    fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }
    fn tick(&self) {
        for _ in 0..self.len() {
            let mut task = self.0.lock().unwrap().pop_front().unwrap();
            if task
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
            {
                self.0.lock().unwrap().push_back(task);
            }
        }
    }
    fn clear(&self) {
        let tasks = std::mem::take(&mut *self.0.lock().unwrap());
        drop(tasks);
    }
}
struct Io {
    stream: TokioIo<DuplexStream>,
    active: Arc<AtomicUsize>,
}
impl Drop for Io {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Connection for Io {
    fn connected(&self) -> Connected {
        Connected::new().extra(17_u64)
    }
}
impl Read for Io {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}
impl Write for Io {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}
struct PendingDial(Arc<AtomicUsize>);
impl Drop for PendingDial {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[derive(Clone)]
struct Connector {
    io: Arc<Mutex<Option<Io>>>,
    calls: Arc<AtomicUsize>,
    pending: bool,
    hold_missing: Arc<AtomicBool>,
    pending_dials: Arc<AtomicUsize>,
}
impl Service<Uri> for Connector {
    type Response = Io;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Io, io::Error>> + Send>>;
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, _: Uri) -> Self::Future {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.pending {
            return Box::pin(std::future::pending());
        }
        let io = self.io.lock().unwrap().take();
        if io.is_none() && self.hold_missing.load(Ordering::SeqCst) {
            self.pending_dials.fetch_add(1, Ordering::SeqCst);
            let lease = PendingDial(self.pending_dials.clone());
            return Box::pin(async move {
                let _lease = lease;
                std::future::pending().await
            });
        }
        Box::pin(
            async move { io.ok_or_else(|| io::Error::other("synthetic unavailable connection")) },
        )
    }
}
struct Memory {
    connector_io: Arc<Mutex<Option<Io>>>,
    hold_missing: Arc<AtomicBool>,
    pending_dials: Arc<AtomicUsize>,
    client: Client<Connector, Full<Bytes>>,
    executor: HeldExecutor,
    peer: DuplexStream,
    calls: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
}
impl Memory {
    fn new(retries: bool, pool: bool, pending: bool) -> Self {
        let (io, peer) = tokio::io::duplex(65536);
        let active = Arc::new(AtomicUsize::new(1));
        let calls = Arc::new(AtomicUsize::new(0));
        let hold_missing = Arc::new(AtomicBool::new(false));
        let pending_dials = Arc::new(AtomicUsize::new(0));
        let connector = Connector {
            io: Arc::new(Mutex::new(Some(Io {
                stream: TokioIo::new(io),
                active: active.clone(),
            }))),
            calls: calls.clone(),
            pending,
            hold_missing: hold_missing.clone(),
            pending_dials: pending_dials.clone(),
        };
        let connector_io = connector.io.clone();
        let executor = HeldExecutor::default();
        let client = Client::builder(executor.clone())
            .retry_canceled_requests(retries)
            .pool_max_idle_per_host(if pool { 1 } else { 0 })
            .build(connector);
        Self {
            connector_io,
            hold_missing,
            pending_dials,
            client,
            executor,
            peer,
            calls,
            active,
        }
    }
    fn finish(self) {
        let active = self.active.clone();
        let pending_dials = self.pending_dials.clone();
        let executor = self.executor.clone();
        drop(self);
        assert_eq!(executor.len(), 0);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(pending_dials.load(Ordering::SeqCst), 0);
    }
}
impl Drop for Memory {
    fn drop(&mut self) {
        self.executor.clear();
    }
}
fn request() -> Request<Full<Bytes>> {
    Request::get("http://synthetic.invalid/request")
        .body(Full::new(Bytes::new()))
        .unwrap()
}
fn poll<F: Future + Unpin>(future: &mut F) -> Poll<F::Output> {
    Pin::new(future).poll(&mut Context::from_waker(Waker::noop()))
}

fn assign(
    memory: &Memory,
    future: &mut hyper_util::client::legacy::ResponseFuture,
    capture: &hyper_util::client::legacy::connect::CaptureAssignment,
) {
    for _ in 0..16 {
        assert!(poll(future).is_pending());
        if capture.assignment().is_some() {
            return;
        }
        memory.executor.tick();
    }
    panic!("in-memory connection did not become assignable");
}

macro_rules! bounded_test {
    ($name:ident, $body:block) => {
        #[tokio::test]
        async fn $name() {
            tokio::time::timeout(Duration::from_secs(3), async $body)
                .await.expect("in-memory lease schedule exceeded deadline");
        }
    };
}

bounded_test!(
    unsubmitted_unpolled_and_preassignment_cancellation_release_pending_record,
    {
        let memory = Memory::new(false, true, true);
        let mut a = request();
        let mut capture = capture_http1_assignment(&mut a);
        drop(a);
        assert!(capture.wait_for_assignment().await.is_none());
        let mut a = request();
        let mut capture = capture_http1_assignment(&mut a);
        drop(memory.client.request(a));
        assert!(capture.wait_for_assignment().await.is_none());
        assert_eq!(memory.calls.load(Ordering::SeqCst), 0);
        let mut a = request();
        let mut capture = capture_http1_assignment(&mut a);
        let mut future = memory.client.request(a);
        assert!(poll(&mut future).is_pending());
        assert_eq!(memory.calls.load(Ordering::SeqCst), 1);
        assert!(capture.assignment().is_none());
        drop(future);
        assert!(capture.wait_for_assignment().await.is_none());
        memory.finish();
    }
);
bounded_test!(
    unsupported_requests_reject_before_connector_and_close_capture,
    {
        for mode in 0..7 {
            let memory = Memory::new(mode == 0, mode != 1, false);
            let mut request = request();
            match mode {
                2 => *request.version_mut() = Version::HTTP_2,
                3 => *request.method_mut() = hyper::Method::CONNECT,
                4 => {
                    request
                        .headers_mut()
                        .insert("upgrade", "websocket".parse().unwrap());
                }
                5 => {
                    request
                        .headers_mut()
                        .insert("connection", "keep-alive, UpGrade".parse().unwrap());
                }
                6 => *request.uri_mut() = "/missing-authority".parse().unwrap(),
                _ => {}
            }
            let mut capture = capture_http1_assignment(&mut request);
            assert!(memory.client.request(request).await.is_err());
            assert!(capture.wait_for_assignment().await.is_none());
            assert_eq!(memory.calls.load(Ordering::SeqCst), 0);
            memory.finish();
        }
    }
);
bounded_test!(cloned_request_second_submission_cannot_retire_first, {
    let memory = Memory::new(false, true, false);
    let mut request = request();
    let capture = capture_http1_assignment(&mut request);
    let duplicate = request.clone();
    let mut first = memory.client.request(request);
    assert!(memory.client.request(duplicate).await.is_err());
    assign(&memory, &mut first, &capture);
    let assignment = capture.assignment().unwrap();
    assert!(assignment.claim_abort().is_some());
    assert!(assignment.claim_abort().is_none());
    drop(first);
    assert!(assignment.claim_abort().is_none());
    memory.finish();
});
bounded_test!(
    actual_hyper_immediate_pool_return_retires_without_background_waiter,
    {
        let mut memory = Memory::new(false, true, false);
        let mut request = request();
        let capture = capture_http1_assignment(&mut request);
        let mut future = memory.client.request(request);
        assign(&memory, &mut future, &capture);
        assert!(capture.assignment().is_some());
        assert_eq!(memory.executor.len(), 1);
        memory.executor.tick(); // Dispatch the request before the peer responds.
        memory
            .peer
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
            .await
            .unwrap();
        memory.executor.tick();
        let Poll::Ready(Ok(response)) = poll(&mut future) else {
            panic!("complete response did not resolve");
        };
        assert_eq!(
            memory.executor.len(),
            1,
            "immediate branch enqueued on_idle"
        );
        assert!(capture.assignment().unwrap().claim_abort().is_none());
        drop(response);
        drop(future);
        memory.finish();
    }
);
bounded_test!(
    actual_hyper_deferred_pool_return_retires_after_body_readiness,
    {
        let mut memory = Memory::new(false, true, false);
        let mut request = request();
        let capture = capture_http1_assignment(&mut request);
        let mut future = memory.client.request(request);
        assign(&memory, &mut future, &capture);
        memory.executor.tick(); // Dispatch the request before the peer responds.
        memory
            .peer
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 8\r\n\r\nAAAA")
            .await
            .unwrap();
        memory.executor.tick();
        let Poll::Ready(Ok(response)) = poll(&mut future) else {
            panic!("headers did not resolve");
        };
        assert_eq!(memory.executor.len(), 2, "driver plus deferred on_idle");
        let assignment = capture.assignment().unwrap();
        let mut extras = hyper::http::Extensions::new();
        assignment.get_extras(&mut extras);
        assert_eq!(extras.get::<u64>(), Some(&17));
        memory.peer.write_all(b"BBBB").await.unwrap();
        let mut body = response.into_body();
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            "AAAA"
        );
        memory.executor.tick();
        memory.executor.tick();
        assert_eq!(
            memory.executor.len(),
            1,
            "deferred reservation was not released"
        );
        assert!(assignment.claim_abort().is_none());
        drop(body);
        drop(future);
        memory.finish();
    }
);
bounded_test!(
    deferred_waiter_drop_retires_and_active_claim_only_poisons,
    {
        let mut memory = Memory::new(false, true, false);
        let mut request = request();
        let capture = capture_http1_assignment(&mut request);
        let mut future = memory.client.request(request);
        assign(&memory, &mut future, &capture);
        memory.executor.tick(); // Dispatch the request before the peer responds.
        memory
            .peer
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 8\r\n\r\nAAAA")
            .await
            .unwrap();
        memory.executor.tick();
        let Poll::Ready(Ok(response)) = poll(&mut future) else {
            panic!("headers did not resolve");
        };
        assert_eq!(memory.executor.len(), 2);
        let assignment = capture.assignment().unwrap();
        let claim = assignment.claim_abort().unwrap();
        assert_eq!(memory.active.load(Ordering::SeqCst), 1, "claim closed IO");
        let mut extras = hyper::http::Extensions::new();
        claim.get_extras(&mut extras);
        assert_eq!(extras.get::<u64>(), Some(&17));
        memory.executor.clear();
        assert!(assignment.claim_abort().is_none());
        drop(response);
        drop(future);
        memory.finish();
    }
);
bounded_test!(
    response_error_and_cancellation_before_headers_retire_assigned_record,
    {
        for corrupt in [false, true] {
            let mut memory = Memory::new(false, true, false);
            let mut request = request();
            let capture = capture_http1_assignment(&mut request);
            let mut future = memory.client.request(request);
            assign(&memory, &mut future, &capture);
            let assignment = capture.assignment().unwrap();
            if corrupt {
                memory
                    .peer
                    .write_all(b"not an HTTP response\r\n\r\n")
                    .await
                    .unwrap();
                memory.executor.tick();
                assert!(matches!(poll(&mut future), Poll::Ready(Err(_))));
            }
            drop(future);
            assert!(assignment.claim_abort().is_none());
            memory.finish();
        }
    }
);
bounded_test!(unsolicited_101_retires_before_response_returns, {
    let mut memory = Memory::new(false, true, false);
    let mut request = request();
    let capture = capture_http1_assignment(&mut request);
    let mut future = memory.client.request(request);
    assign(&memory, &mut future, &capture);
    memory.executor.tick();
    memory.peer.write_all(b"HTTP/1.1 101 Switching Protocols\r\nconnection: upgrade\r\nupgrade: synthetic\r\n\r\n").await.unwrap();
    memory.executor.tick();
    let Poll::Ready(Ok(response)) = poll(&mut future) else {
        panic!("101 did not resolve");
    };
    assert_eq!(response.status(), 101);
    assert!(capture.assignment().unwrap().claim_abort().is_none());
    drop(response);
    drop(future);
    memory.finish();
});

bounded_test!(executor_rejection_unwinds_owned_deferred_reservation, {
    let mut memory = Memory::new(false, true, false);
    let mut request = request();
    let capture = capture_http1_assignment(&mut request);
    let mut future = memory.client.request(request);
    assign(&memory, &mut future, &capture);
    memory.executor.tick();
    memory
        .peer
        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 8\r\n\r\nAAAA")
        .await
        .unwrap();
    memory.executor.tick();
    memory.executor.1.store(true, Ordering::SeqCst);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| poll(&mut future)));
    assert!(result.is_err());
    assert!(
        !memory.executor.1.load(Ordering::SeqCst),
        "panic was not executor rejection"
    );
    assert!(capture.assignment().unwrap().claim_abort().is_none());
    drop(future);
    memory.finish();
});

bounded_test!(
    connection_error_closes_pending_capture_without_assignment,
    {
        let memory = Memory::new(false, true, false);
        let unused = memory.connector_io.lock().unwrap().take();
        drop(unused);
        let mut request = request();
        let mut capture = capture_http1_assignment(&mut request);
        assert!(memory.client.request(request).await.is_err());
        assert!(capture.wait_for_assignment().await.is_none());
        assert_eq!(memory.calls.load(Ordering::SeqCst), 1);
        memory.finish();
    }
);

bounded_test!(
    pooled_winner_is_selected_while_losing_connector_remains_independent,
    {
        let mut memory = Memory::new(false, true, false);
        let mut a = request();
        let capture_a = capture_http1_assignment(&mut a);
        let mut future_a = memory.client.request(a);
        assign(&memory, &mut future_a, &capture_a);
        memory.executor.tick();
        memory
            .peer
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 8\r\n\r\nAAAA")
            .await
            .unwrap();
        memory.executor.tick();
        let Poll::Ready(Ok(response_a)) = poll(&mut future_a) else {
            panic!("A headers missing");
        };
        memory.hold_missing.store(true, Ordering::SeqCst);
        let mut b = request();
        let capture_b = capture_http1_assignment(&mut b);
        let mut future_b = memory.client.request(b);
        assert!(poll(&mut future_b).is_pending());
        assert!(capture_b.assignment().is_none());
        assert_eq!(memory.pending_dials.load(Ordering::SeqCst), 1);
        memory.peer.write_all(b"BBBB").await.unwrap();
        let mut body_a = response_a.into_body();
        assert_eq!(
            body_a.frame().await.unwrap().unwrap().into_data().unwrap(),
            "AAAA"
        );
        memory.executor.tick();
        memory.executor.tick();
        assign(&memory, &mut future_b, &capture_b);
        assert!(capture_a.assignment().unwrap().claim_abort().is_none());
        let selected_b = capture_b.assignment().unwrap();
        let claim = selected_b.claim_abort().unwrap();
        let mut extras = hyper::http::Extensions::new();
        claim.get_extras(&mut extras);
        assert_eq!(extras.get::<u64>(), Some(&17));
        assert_eq!(memory.calls.load(Ordering::SeqCst), 2);
        assert_eq!(memory.active.load(Ordering::SeqCst), 1);
        assert_eq!(
            memory.pending_dials.load(Ordering::SeqCst),
            1,
            "claim touched losing connector"
        );
        drop(future_b);
        assert!(selected_b.claim_abort().is_none());
        drop(future_a);
        drop(body_a);
        memory.finish();
    }
);

async fn run_reassignment(version: Option<SslVersion>) {
    let mut fixture = Fixture::new(version).await;
    let client = fixture.client.clone();
    let origin = fixture.origin.clone();
    let (unused, replacement) = mpsc::channel(1);
    drop(unused);
    let mut requests = std::mem::replace(&mut fixture.requests, replacement);
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        let make = |path| Request::get(format!("{origin}/{path}")).body(Full::new(Bytes::new())).unwrap();
        let mut request_a = make("a"); let capture_a = capture_http1_assignment(&mut request_a); let response_a = client.request_raw(request_a); tokio::pin!(response_a);
        let a = tokio::select! { a = requests.recv() => a.unwrap(), _ = &mut response_a => panic!("early response") };
        a.reply.send(Reply { first: b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\n\r\nAAAA", remainder: None }).ok().unwrap();
        let body_a = response_a.await.unwrap().into_body(); assert_eq!(body_a.size_hint().exact(), Some(4)); assert!(!body_a.is_end_stream());
        let mut request_b = make("b"); let capture_b = capture_http1_assignment(&mut request_b); let response_b = client.request_raw(request_b); tokio::pin!(response_b);
        let b = tokio::select! { b = requests.recv() => b.unwrap(), _ = &mut response_b => panic!("early response") };
        assert_eq!(a.connection, b.connection); assert!(capture_a.assignment().unwrap().claim_abort().is_none(), "old A gained authority after B selected its connection");
        assert!(capture_b.assignment().unwrap().claim_abort().is_some()); assert!(capture_b.assignment().unwrap().claim_abort().is_none()); drop(body_a);
        b.reply.send(Reply { first: b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\n\r\nBBBB", remainder: None }).ok().unwrap();
        assert_eq!(response_b.await.unwrap().into_body().collect().await.unwrap().to_bytes(), "BBBB", "poisoning performed a destructive IO action");
        let response_c = client.request_raw(make("c")); tokio::pin!(response_c);
        let c = tokio::select! { c = requests.recv() => c.unwrap(), _ = &mut response_c => panic!("early response") };
        assert_ne!(c.connection, b.connection, "claimed connection reentered pool"); c.reply.send(Reply { first: b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\n\r\nCCCC", remainder: None }).ok().unwrap();
        assert_eq!(response_c.await.unwrap().into_body().collect().await.unwrap().to_bytes(), "CCCC");
    });
    let result = tokio::time::timeout(Duration::from_secs(5), tasks.join_next()).await;
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    let accepted = fixture.accepted.load(Ordering::SeqCst);
    fixture.finish().await;
    result.unwrap().unwrap().unwrap();
    assert_eq!(accepted, 2);
}
#[tokio::test]
async fn http_old_body_cannot_claim_reassigned_connection() {
    run_reassignment(None).await;
}
#[tokio::test]
async fn tls12_old_body_cannot_claim_reassigned_connection() {
    run_reassignment(Some(SslVersion::TLS1_2)).await;
}
#[tokio::test]
async fn tls13_old_body_cannot_claim_reassigned_connection() {
    run_reassignment(Some(SslVersion::TLS1_3)).await;
}
