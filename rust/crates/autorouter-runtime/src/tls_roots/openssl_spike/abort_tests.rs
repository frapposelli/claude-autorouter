//! Explicit direct-client intent only. These tests do not arm Gateway requests.
use super::abort::{self, Cause, Handle, Outcome, Terminal};
use super::client::SpikeHttpClient;
use super::lifecycle_tests::isolated_tls;
use crate::http_client::HttpTransport;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, body::Body, http::Extensions};
use hyper_openssl::SslStream;
use hyper_util::{
    client::legacy::connect::{Assignment, CaptureAssignment, capture_http1_assignment},
    rt::TokioIo,
};
use openssl::ssl::{Ssl, SslVersion};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

const LIMIT: Duration = Duration::from_secs(5);
const BULK: usize = 512 * 1024;

enum Reply {
    Complete,
    ObserveClose,
    Prefix {
        finish: oneshot::Receiver<()>,
    },
    Held {
        bulk: bool,
        written: oneshot::Sender<()>,
    },
}
struct RequestEvent {
    connection: usize,
    resumed: bool,
    path: String,
    reply: oneshot::Sender<Reply>,
}
struct PeerLease(Arc<AtomicUsize>);
impl Drop for PeerLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn peer<S: AsyncRead + AsyncWrite + Unpin>(
    mut io: S,
    connection: usize,
    resumed: bool,
    events: mpsc::Sender<RequestEvent>,
    closed: mpsc::Sender<usize>,
) -> io::Result<()> {
    loop {
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            match io.read(&mut byte).await {
                Ok(0) => {
                    let _ = closed.send(connection).await;
                    return Ok(());
                }
                Ok(_) => head.push(byte[0]),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset
                    ) =>
                {
                    let _ = closed.send(connection).await;
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
            if head.len() > 16384 {
                return Err(io::Error::other("fixture header bound"));
            }
        }
        let path = std::str::from_utf8(&head)
            .unwrap()
            .split_ascii_whitespace()
            .nth(1)
            .unwrap()
            .to_owned();
        let (reply, receive) = oneshot::channel();
        if events
            .send(RequestEvent {
                connection,
                resumed,
                path,
                reply,
            })
            .await
            .is_err()
        {
            return Ok(());
        }
        let Ok(reply) = receive.await else {
            return Ok(());
        };
        match reply {
            Reply::ObserveClose => {}
            Reply::Complete => {
                io.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\n\r\nDONE")
                    .await?
            }
            Reply::Prefix { finish } => {
                io.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 8\r\n\r\nPART")
                    .await?;
                io.flush().await?;
                if finish.await.is_err() {
                    return Ok(());
                }
                io.write_all(b"DONE").await?;
            }
            Reply::Held { bulk, written } => {
                io.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 1048576\r\n\r\n")
                    .await?;
                if bulk {
                    for _ in 0..BULK / 16384 {
                        io.write_all(&[b'x'; 16384]).await?;
                    }
                } else {
                    io.write_all(b"partial").await?;
                }
                io.flush().await?;
                let _ = written.send(());
                // No more response bytes. A physical socket shutdown must reach
                // this read even while the client holds Incoming without polls.
            }
        }
        io.flush().await?;
    }
}

struct Fixture {
    client: Arc<SpikeHttpClient>,
    origin: String,
    events: mpsc::Receiver<RequestEvent>,
    closed: mpsc::Receiver<usize>,
    tasks: JoinSet<io::Result<()>>,
    stop: CancellationToken,
    active: Arc<AtomicUsize>,
}
impl Fixture {
    async fn new(version: Option<SslVersion>) -> Self {
        let (trust, tls) = isolated_tls(version);
        let client = Arc::new(SpikeHttpClient::with_snapshot_aborts(true, &trust, true).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!(
            "{}://{}",
            if version.is_some() { "https" } else { "http" },
            listener.local_addr().unwrap()
        );
        let (send, events) = mpsc::channel(16);
        let (send_closed, closed) = mpsc::channel(16);
        let active = Arc::new(AtomicUsize::new(0));
        let peer_active = active.clone();
        let stop = CancellationToken::new();
        let shutdown = stop.clone();
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            let mut peers = JoinSet::new();
            let mut accepted = 0;
            let result = loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => break Ok(()),
                    result = peers.join_next(), if !peers.is_empty() => {
                        if !matches!(result, Some(Ok(Ok(())))) { break Err(io::Error::other("fixture peer failed")); }
                    }
                    result = listener.accept() => {
                        let (socket, _) = result?;
                        if accepted == 16 { break Err(io::Error::other("fixture connection bound")); }
                        let connection = accepted; accepted += 1;
                        peer_active.fetch_add(1, Ordering::SeqCst);
                        let lease = PeerLease(peer_active.clone());
                        let context = tls.clone(); let send = send.clone(); let closed = send_closed.clone();
                        peers.spawn(async move {
                            let _lease = lease;
                            if let Some(context) = context {
                                let mut stream = SslStream::new(Ssl::new(&context)?, TokioIo::new(socket))?;
                                std::pin::Pin::new(&mut stream).accept().await.map_err(|_| io::Error::other("fixture TLS failed"))?;
                                let resumed = stream.ssl().session_reused();
                                peer(TokioIo::new(stream), connection, resumed, send, closed).await
                            } else { peer(socket, connection, false, send, closed).await }
                        });
                    }
                }
            };
            peers.abort_all(); while peers.join_next().await.is_some() {}
            result
        });
        Self {
            client,
            origin,
            events,
            closed,
            tasks,
            stop,
            active,
        }
    }
    async fn finish(mut self) {
        let counts = self.client.raw_counts.clone();
        let cache = Arc::downgrade(self.client.raw_sessions.as_ref().unwrap());
        drop(self.client);
        let clean = tokio::time::timeout(LIMIT, async {
            while counts.active.load(Ordering::SeqCst) != 0
                || counts.tasks.load(Ordering::SeqCst) != 0
                || counts.shutdown_handles.load(Ordering::SeqCst) != 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await;
        self.stop.cancel();
        let peers = tokio::time::timeout(LIMIT, async {
            while let Some(result) = self.tasks.join_next().await {
                assert!(matches!(result, Ok(Ok(()))));
            }
        })
        .await;
        self.tasks.abort_all();
        assert!(clean.is_ok(), "client tasks/socket/duplicate handle leak");
        assert!(peers.is_ok(), "peer cleanup deadline");
        assert_eq!(self.active.load(Ordering::SeqCst), 0);
        assert!(cache.upgrade().is_none());
    }
}

fn request(origin: &str, path: &str) -> (Request<Full<Bytes>>, CaptureAssignment) {
    let mut request = Request::get(format!("{origin}/{path}"))
        .body(Full::new(Bytes::new()))
        .unwrap();
    let capture = capture_http1_assignment(&mut request);
    (request, capture)
}
fn handle(assignment: &Assignment) -> Handle {
    let mut extra = Extensions::new();
    assignment.get_extras(&mut extra);
    extra.remove().unwrap()
}
fn claim(capture: &CaptureAssignment, cause: Cause) -> Outcome {
    abort::consume(
        capture
            .assignment()
            .unwrap()
            .claim_abort()
            .expect("active reservation"),
        cause,
    )
}

#[derive(Clone, Copy)]
enum Scenario {
    Backpressure,
    Reassigned,
    Sibling,
    OrdinaryDrop,
    DelayedClaim,
    LosingDial,
}

async fn run(version: Option<SslVersion>, scenario: Scenario) {
    let mut fixture = Fixture::new(version).await;
    let client = fixture.client.clone();
    let origin = fixture.origin.clone();
    let (_, replacement) = mpsc::channel(1);
    let mut events = std::mem::replace(&mut fixture.events, replacement);
    let (_, replacement) = mpsc::channel(1);
    let mut closed = std::mem::replace(&mut fixture.closed, replacement);
    let mut scenarios = JoinSet::new();
    scenarios.spawn(async move {
        let (a, capture_a) = request(&origin, "a");
        let response_a = client.request_raw(a); tokio::pin!(response_a);
        let a = tokio::select! { event = events.recv() => event.unwrap(), _ = &mut response_a => panic!("early response") };
        assert_eq!(a.path, "/a"); assert!(!a.resumed);
        let (written, wrote) = oneshot::channel();
        let (complete_a, finish_a) = oneshot::channel();
        a.reply.send(if matches!(scenario, Scenario::Reassigned) { Reply::Complete } else if matches!(scenario, Scenario::LosingDial) { Reply::Prefix { finish: finish_a } } else { Reply::Held { bulk: matches!(scenario, Scenario::Backpressure), written } }).ok().unwrap();
        let body_a = response_a.await.unwrap().into_body();
        let assignment_a = capture_a.assignment().unwrap(); let identity_a = handle(&assignment_a);
        assert!(identity_a.is_alive()); assert_eq!(identity_a.terminal(), None);
        assert!(!body_a.is_end_stream());
        if matches!(scenario, Scenario::Backpressure) {
            wrote.await.unwrap();
            // The peer completed 512KiB; bytes remain in the real socket while
            // the entire Incoming is deliberately unpolled. No sleep is used.
            while !identity_a.unread_socket_bytes() { tokio::task::yield_now().await; }
            assert!(client.raw_counts.read_bytes.load(Ordering::SeqCst) < BULK);
            if version.is_some() { assert_eq!(client.raw_sessions.as_ref().unwrap().lock().unwrap().snapshot().0, 1); }
            assert_eq!(claim(&capture_a, Cause::ExplicitFixtureCancellation), Outcome::Requested);
            assert_eq!(identity_a.terminal(), Some(Terminal::Abort(Cause::ExplicitFixtureCancellation)));
            assert!(assignment_a.claim_abort().is_none());
            assert_eq!(tokio::time::timeout(Duration::from_secs(1), closed.recv()).await
                .expect("claimed socket did not physically close before Incoming repoll/drop"), Some(a.connection));
            assert_eq!(client.raw_counts.shutdown_calls.load(Ordering::SeqCst), 1);
            assert_eq!(client.raw_sessions.as_ref().unwrap().lock().unwrap().snapshot().0, 0);
            drop(body_a);
        } else if matches!(scenario, Scenario::OrdinaryDrop | Scenario::DelayedClaim) {
            wrote.await.unwrap();
            let held_claim = matches!(scenario, Scenario::DelayedClaim).then(|| assignment_a.claim_abort().unwrap());
            drop(body_a);
            assert_eq!(closed.recv().await, Some(a.connection));
            assert_eq!(client.raw_counts.shutdown_calls.load(Ordering::SeqCst), 0);
            assert_eq!(client.raw_counts.session_error_closes.load(Ordering::SeqCst), 0);
            if let Some(held_claim) = held_claim {
                while identity_a.is_alive() { tokio::task::yield_now().await; }
                assert_eq!(abort::consume(held_claim, Cause::ExplicitFixtureCancellation), Outcome::MissingTransport);
                assert_eq!(client.raw_counts.session_error_closes.load(Ordering::SeqCst), 0);
            }
        } else if matches!(scenario, Scenario::LosingDial) {
            let held = client.raw_dial_gate.as_ref().unwrap().hold_next();
            let (b, capture_b) = request(&origin, "b");
            let response_b = client.request_raw(b); tokio::pin!(response_b);
            let release = tokio::select! {
                release = held => release.unwrap(),
                _ = &mut response_b => panic!("response before selected connection"),
                _ = events.recv() => panic!("held dial dispatched request"),
            };
            assert!(capture_b.assignment().is_none());
            assert_eq!(client.raw_counts.shutdown_handles.load(Ordering::SeqCst), 2);
            complete_a.send(()).unwrap();
            assert_eq!(body_a.collect().await.unwrap().to_bytes(), "PARTDONE");
            let b = tokio::select! { event = events.recv() => event.unwrap(), _ = &mut response_b => panic!("early response") };
            assert_eq!(a.connection, b.connection, "pool winner was not selected");
            assert!(assignment_a.claim_abort().is_none());
            b.reply.send(Reply::ObserveClose).ok().unwrap();
            assert_eq!(claim(&capture_b, Cause::ExplicitFixtureCancellation), Outcome::Requested);
            assert_eq!(closed.recv().await, Some(b.connection));
            assert!(response_b.await.is_err());
            // The losing dial remains independently owned by Hyper's background
            // task and has no assignment belonging to the aborted request.
            release.send(()).unwrap();
            while client.raw_counts.published.load(Ordering::SeqCst) != 2
                || client.raw_counts.active.load(Ordering::SeqCst) != 1
                || client.raw_counts.tasks.load(Ordering::SeqCst) != 1 {
                tokio::task::yield_now().await;
            }
            assert_eq!(client.raw_counts.shutdown_calls.load(Ordering::SeqCst), 1);
        } else {
            let (b, capture_b) = request(&origin, "b");
            let response_b = client.request_raw(b); tokio::pin!(response_b);
            let b = tokio::select! { event = events.recv() => event.unwrap(), _ = &mut response_b => panic!("early response") };
            assert_eq!(b.path, "/b");
            if matches!(scenario, Scenario::Reassigned) {
                assert_eq!(a.connection, b.connection);
                assert_eq!(body_a.size_hint().exact(), Some(4));
                assert!(assignment_a.claim_abort().is_none(), "old A obtained destructive authority over B");
                assert_eq!(client.raw_counts.shutdown_calls.load(Ordering::SeqCst), 0);
                assert_eq!(identity_a.terminal(), None);
                assert_eq!(body_a.collect().await.unwrap().to_bytes(), "DONE");
            } else {
                assert_ne!(a.connection, b.connection);
                assert_eq!(claim(&capture_a, Cause::ExplicitFixtureDeadline), Outcome::Requested);
                assert_eq!(closed.recv().await, Some(a.connection));
                assert_eq!(handle(&capture_b.assignment().unwrap()).terminal(), None);
                drop(body_a);
            }
            b.reply.send(Reply::Complete).ok().unwrap();
            assert_eq!(response_b.await.unwrap().into_body().collect().await.unwrap().to_bytes(), "DONE");
        }
        // A poisoned connection cannot reenter the pool. A body Drop can close
        // IO, but must not evict its TLS session just because it was abandoned.
        let (c, _) = request(&origin, "c"); let response_c = client.request_raw(c); tokio::pin!(response_c);
        let c = tokio::select! { event = events.recv() => event.unwrap(), _ = &mut response_c => panic!("early response") };
        if matches!(scenario, Scenario::LosingDial) {
            assert_eq!(c.connection, 1, "request did not use surviving losing dial");
            assert_eq!(client.raw_counts.attempts.load(Ordering::SeqCst), 2);
        }
        if matches!(scenario, Scenario::Backpressure | Scenario::OrdinaryDrop | Scenario::DelayedClaim) {
            assert_ne!(c.connection, a.connection);
            if version.is_some() { assert_eq!(c.resumed, matches!(scenario, Scenario::OrdinaryDrop | Scenario::DelayedClaim)); }
        }
        c.reply.send(Reply::Complete).ok().unwrap();
        assert_eq!(response_c.await.unwrap().into_body().collect().await.unwrap().to_bytes(), "DONE");
        identity_a
    });
    let result = tokio::time::timeout(LIMIT, scenarios.join_next()).await;
    scenarios.abort_all();
    while scenarios.join_next().await.is_some() {}
    fixture.finish().await;
    let identity = result.expect("abort scenario deadline").unwrap().unwrap();
    assert!(
        !identity.is_alive(),
        "weak metadata retained duplicate descriptor"
    );
}

#[tokio::test]
async fn http_backpressured_abort_closes_peer_before_body_repoll() {
    run(None, Scenario::Backpressure).await;
}
#[tokio::test]
async fn tls12_backpressured_abort_closes_peer_before_body_repoll() {
    run(Some(SslVersion::TLS1_2), Scenario::Backpressure).await;
}
#[tokio::test]
async fn tls13_backpressured_abort_closes_peer_before_body_repoll() {
    run(Some(SslVersion::TLS1_3), Scenario::Backpressure).await;
}
#[tokio::test]
async fn http_old_body_cannot_abort_reassigned_socket() {
    run(None, Scenario::Reassigned).await;
}
#[tokio::test]
async fn tls12_old_body_cannot_abort_reassigned_socket() {
    run(Some(SslVersion::TLS1_2), Scenario::Reassigned).await;
}
#[tokio::test]
async fn tls13_old_body_cannot_abort_reassigned_socket() {
    run(Some(SslVersion::TLS1_3), Scenario::Reassigned).await;
}
#[tokio::test]
async fn http_claimed_abort_preserves_sibling() {
    run(None, Scenario::Sibling).await;
}
#[tokio::test]
async fn tls12_claimed_abort_preserves_sibling() {
    run(Some(SslVersion::TLS1_2), Scenario::Sibling).await;
}
#[tokio::test]
async fn tls13_claimed_abort_preserves_sibling() {
    run(Some(SslVersion::TLS1_3), Scenario::Sibling).await;
}
#[tokio::test]
async fn http_ordinary_body_drop_never_arms_abort() {
    run(None, Scenario::OrdinaryDrop).await;
}
#[tokio::test]
async fn tls12_ordinary_body_drop_preserves_session() {
    run(Some(SslVersion::TLS1_2), Scenario::OrdinaryDrop).await;
}
#[tokio::test]
async fn tls13_ordinary_body_drop_preserves_session() {
    run(Some(SslVersion::TLS1_3), Scenario::OrdinaryDrop).await;
}

#[tokio::test]
async fn http_abort_closes_pool_winner_and_preserves_held_losing_dial() {
    run(None, Scenario::LosingDial).await;
}
#[tokio::test]
async fn tls12_abort_closes_pool_winner_and_preserves_held_losing_dial() {
    run(Some(SslVersion::TLS1_2), Scenario::LosingDial).await;
}
#[tokio::test]
async fn tls13_abort_closes_pool_winner_and_preserves_held_losing_dial() {
    run(Some(SslVersion::TLS1_3), Scenario::LosingDial).await;
}

#[tokio::test]
async fn http_delayed_claim_cannot_retain_or_abort_dropped_io() {
    run(None, Scenario::DelayedClaim).await;
}
#[tokio::test]
async fn tls12_delayed_claim_cannot_retain_or_abort_dropped_io() {
    run(Some(SslVersion::TLS1_2), Scenario::DelayedClaim).await;
}
#[tokio::test]
async fn tls13_delayed_claim_cannot_retain_or_abort_dropped_io() {
    run(Some(SslVersion::TLS1_3), Scenario::DelayedClaim).await;
}
