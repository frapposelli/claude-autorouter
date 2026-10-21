//! Unexecuted integration controls: real TLS peers and acknowledged wire bytes.
//! Requires the separately reviewed RawOutput/ciphertext adapter integration.
use super::*;
use super::{
    TlsFixtureOwner as Owner, fixture_credit_blocked as credit_blocked, fixture_output as output,
    fixture_run_owned as owned,
};
use openssl::ssl::SslVersion;
use std::task::{Wake, Waker};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Notify, mpsc, watch};
use tokio::time::{Instant, timeout_at};

const CASE: Duration = Duration::from_secs(10);
const CHUNK: usize = 16 * 1024;
const MAX_POLLS: usize = 1024;
const MAX_ACCEPTED: usize = CHUNK * MAX_POLLS;
const RESPONSE: &[u8] =
    b"HTTP/1.1 413 Content Too Large\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n";
const HEADER: &[u8] =
    b"POST /bounded-fixture HTTP/1.1\r\nHost: localhost\r\nContent-Length: 67108864\r\n\r\n";
const TAIL: &[u8] = b"replacement-after-cancel";

#[derive(Default)]
struct WakeCount {
    calls: AtomicUsize,
    changed: Notify,
}
impl Wake for WakeCount {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.changed.notify_waiters();
    }
}
impl WakeCount {
    fn count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    async fn after(&self, before: usize) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.count() > before {
                return;
            }
            changed.await;
        }
    }
}

#[derive(Clone, Copy)]
enum Mode {
    EarlyHttp,
    Verify,
    Hold,
}
enum Command {
    Respond413,
    Verify(Vec<(u8, usize)>),
    Tail,
}
struct Peer {
    commands: mpsc::Sender<Command>,
    headers: watch::Receiver<bool>,
    body_reads: Arc<AtomicUsize>,
}

async fn signal(value: &mut watch::Receiver<bool>) {
    while !*value.borrow_and_update() {
        value.changed().await.expect("positive peer signal");
    }
}
async fn peer(
    version: SslVersion,
    mode: Mode,
) -> (TrustSnapshot, std::net::SocketAddr, Peer, Owner) {
    let (trust, context) = super::super::lifecycle_tests::isolated_tls(Some(version));
    let (listener, _) = super::super::loopback_peer::bind_listener(true).await;
    let address = listener.local_addr().unwrap();
    let (send, mut commands) = mpsc::channel(2);
    let (headers_send, headers) = watch::channel(false);
    let (closed_send, closed) = watch::channel(false);
    let (release, released) = tokio::sync::oneshot::channel();
    let body_reads = Arc::new(AtomicUsize::new(0));
    let observed_reads = body_reads.clone();
    let task = tokio::spawn(async move {
        let mut stream =
            super::super::loopback_peer::accept_peer(&listener, context.as_ref()).await;
        match mode {
            Mode::EarlyHttp => {
                // Headers are fully flushed and acknowledged before the client
                // starts body admission. Reading one byte here cannot consume
                // body data from a future TLS application record.
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    assert!(header.len() < 1024);
                    let mut byte = [0];
                    stream.read_exact(&mut byte).await.unwrap();
                    header.push(byte[0]);
                }
                assert_eq!(header, HEADER);
                let _ = headers_send.send(true);
                match commands.recv().await {
                    Some(Command::Respond413) => {}
                    None => return,
                    _ => panic!("early HTTP command"),
                }
                stream.write_all(RESPONSE).await.unwrap();
                stream.flush().await.unwrap();
                // Never read request-body bytes, even after the early reply.
                let _ = released.await;
            }
            Mode::Verify | Mode::Hold => {
                let mut byte = [0];
                stream.read_exact(&mut byte).await.unwrap();
                assert_eq!(&byte, b"q");
                stream.write_all(b"r").await.unwrap();
                stream.flush().await.unwrap();
                match (mode, commands.recv().await) {
                    (_, None) => return,
                    (Mode::Verify, Some(Command::Verify(segments))) => {
                        let total = segments.iter().map(|(_, count)| *count).sum::<usize>();
                        assert!(total > 0 && total <= MAX_ACCEPTED);
                        assert!(segments.len() <= MAX_POLLS);
                        let mut buffer = vec![0; CHUNK];
                        for (byte, count) in segments {
                            assert!(count > 0 && count <= CHUNK);
                            stream.read_exact(&mut buffer[..count]).await.unwrap();
                            observed_reads.fetch_add(count, Ordering::SeqCst);
                            assert!(
                                buffer[..count].iter().all(|actual| *actual == byte),
                                "peer plaintext differs from acknowledged prefix"
                            );
                        }
                        stream.write_all(b"a").await.unwrap();
                        stream.flush().await.unwrap();
                        match commands.recv().await {
                            Some(Command::Tail) => {}
                            None => return,
                            _ => panic!("tail command"),
                        }
                        let mut tail = [0; TAIL.len()];
                        stream.read_exact(&mut tail).await.unwrap();
                        observed_reads.fetch_add(tail.len(), Ordering::SeqCst);
                        assert_eq!(&tail, TAIL);
                        stream.write_all(b"b").await.unwrap();
                        stream.flush().await.unwrap();
                        let mut extra = [0];
                        assert_eq!(
                            stream.read(&mut extra).await.unwrap(),
                            0,
                            "no duplicated/unacknowledged plaintext before real TLS EOF"
                        );
                    }
                    (Mode::Hold, Some(_)) => panic!("hold command"),
                    _ => panic!("verify command"),
                }
            }
        }
        drop(stream); // Positive close signal follows actual owned stream Drop.
        let _ = closed_send.send(true);
    });
    // There is no await after spawning until the owner is returned.
    (
        trust,
        address,
        Peer {
            commands: send,
            headers,
            body_reads,
        },
        Owner {
            release: Arc::new(std::sync::Mutex::new(Some(release))),
            peer: Some(task),
            closed,
        },
    )
}

struct Fixture {
    io: Option<TransportIo>,
    connector: Option<Connector>,
    cache: RawCache,
    counts: Arc<Counts>,
    probe: super::super::idle_close::Probe,
}
impl Fixture {
    async fn new(
        version: SslVersion,
        aborts: bool,
        trust: TrustSnapshot,
        address: std::net::SocketAddr,
        qr: bool,
    ) -> Self {
        let probe = super::super::idle_close::Probe::default();
        let f =
            super::fixture_connect_raw(version, aborts, trust, address, Some(probe.clone()), qr)
                .await;
        assert!(
            f.transport.as_ref().unwrap().raw_output.is_some(),
            "new reviewed Raw output integration required"
        );
        Self {
            io: f.transport,
            connector: f.connector,
            cache: f.cache,
            counts: f.counts,
            probe,
        }
    }
    fn clean(&mut self) {
        drop(self.io.take());
        drop(self.connector.take());
        super::fixture_clean(&self.counts, &self.cache, Some(&self.probe));
        assert_eq!(self.counts.attempts.load(Ordering::SeqCst), 1);
        assert_eq!(self.counts.completed.load(Ordering::SeqCst), 1);
        super::fixture_no_write_hold(&self.probe);
    }
}

fn read_once(io: &mut TransportIo, waker: &Waker, bytes: &mut [u8]) -> Poll<io::Result<usize>> {
    let mut cx = Context::from_waker(waker);
    let mut read = ReadBuf::new(bytes);
    match Pin::new(io).poll_read(&mut cx, &mut read) {
        Poll::Pending => {
            assert!(read.filled().is_empty());
            Poll::Pending
        }
        Poll::Ready(result) => Poll::Ready(result.map(|()| read.filled().len())),
    }
}

#[tokio::test]
async fn raw_early_413_progresses_without_resuming_request_body_reads() {
    for version in [SslVersion::TLS1_2, SslVersion::TLS1_3] {
        for vectored in [false, true] {
            let deadline = Instant::now() + CASE;
            let (trust, address, peer, owner) =
                timeout_at(deadline, peer(version, Mode::EarlyHttp))
                    .await
                    .unwrap();
            owned(
                deadline,
                (trust, address, peer),
                owner,
                move |(trust, address, mut peer)| async move {
                    let mut f = Fixture::new(version, false, trust, address, false).await;
                    let io = f.io.as_mut().unwrap();
                    io.write_all(HEADER).await.unwrap();
                    io.flush().await.unwrap();
                    signal(&mut peer.headers).await;
                    let writer = Arc::new(WakeCount::default());
                    let reader = Arc::new(WakeCount::default());
                    let ww = Waker::from(writer.clone());
                    let rw = Waker::from(reader.clone());
                    let ledger = credit_blocked(io, &ww, vectored).await;
                    assert!(!ledger.is_empty());
                    let mut first = [0; 1];
                    assert!(read_once(io, &rw, &mut first).is_pending());
                    let wb = writer.count();
                    let rb = reader.count();
                    peer.commands.send(Command::Respond413).await.unwrap();
                    reader.after(rb).await;
                    writer.after(wb).await;
                    let mut response = vec![0; RESPONSE.len()];
                    io.read_exact(&mut response).await.unwrap();
                    assert_eq!(response, RESPONSE);
                    assert_eq!(
                        peer.body_reads.load(Ordering::SeqCst),
                        0,
                        "the peer never resumed request-body reads"
                    );
                    let _ = output(io); // Queue movement during the read is legal.
                    assert!(
                        ledger.iter().map(|(_, n)| *n).sum::<usize>() < 67108864,
                        "request remains incomplete at early response"
                    );
                    assert_eq!(f.counts.session_error_closes.load(Ordering::SeqCst), 0);
                    f.clean();
                },
            )
            .await;
        }
    }
}

#[tokio::test]
async fn raw_acknowledged_wire_bytes_survive_mutation_cancel_and_new_input() {
    for version in [SslVersion::TLS1_2, SslVersion::TLS1_3] {
        for vectored in [false, true] {
            let deadline = Instant::now() + CASE;
            let (trust, address, peer, owner) = timeout_at(deadline, peer(version, Mode::Verify))
                .await
                .unwrap();
            owned(
                deadline,
                (trust, address, peer),
                owner,
                move |(trust, address, peer)| async move {
                    let mut f = Fixture::new(version, true, trust, address, true).await;
                    let io = f.io.as_mut().unwrap();
                    let writer = Arc::new(WakeCount::default());
                    let reader = Arc::new(WakeCount::default());
                    let ww = Waker::from(writer.clone());
                    let rw = Waker::from(reader.clone());
                    let ledger = credit_blocked(io, &ww, vectored).await;
                    let accepted = ledger.iter().map(|(_, n)| *n).sum::<usize>();
                    let mut abandoned = b"cancelled-input-must-never-reach-peer".to_vec();
                    // A real AsyncWriteExt future is polled once and cancelled.
                    // Pending grants no new input credit, so replacement is legal.
                    {
                        let mut write = Box::pin(io.write(&abandoned));
                        let result = {
                            let mut cx = Context::from_waker(&ww);
                            write.as_mut().poll(&mut cx)
                        };
                        assert!(result.is_pending());
                    }
                    abandoned.fill(0xdd);
                    drop(abandoned);
                    let mut byte = [0];
                    assert!(read_once(io, &rw, &mut byte).is_pending());
                    let wb = writer.count();
                    let rb = reader.count();
                    assert_eq!(peer.body_reads.load(Ordering::SeqCst), 0);
                    peer.commands.send(Command::Verify(ledger)).await.unwrap();
                    writer.after(wb).await;
                    // This is a real writer continuation, not just a counted wake:
                    // drain the accepted plaintext slot as peer reads create room.
                    // The peer cannot acknowledge the ledger until it gets every
                    // Ready-credited byte, including the owned pending prefix.
                    io.flush().await.unwrap();
                    reader.after(rb).await;
                    io.read_exact(&mut byte).await.unwrap();
                    assert_eq!(&byte, b"a");
                    assert_eq!(peer.body_reads.load(Ordering::SeqCst), accepted);
                    peer.commands.send(Command::Tail).await.unwrap();
                    io.write_all(TAIL).await.unwrap();
                    io.flush().await.unwrap();
                    io.read_exact(&mut byte).await.unwrap();
                    assert_eq!(&byte, b"b");
                    assert_eq!(
                        peer.body_reads.load(Ordering::SeqCst),
                        accepted + TAIL.len()
                    );
                    io.shutdown().await.unwrap();
                    f.clean(); // Owner join additionally requires peer's no-extra-byte EOF assertion.
                },
            )
            .await;
        }
    }
}

#[tokio::test]
async fn raw_owned_output_local_write_halfclose_and_peer_close_keeps_actual_fault_and_eviction() {
    // Same eight physical-fault cases, with both original terminal-repeat
    // checks and empty-operation guards in the shared family driver.
    super::node_shutdown_origin_tests::buffered_peer_close_fault(
        super::node_shutdown_origin_tests::BufferedFaultAt::Shutdown,
    )
    .await;
}

#[tokio::test]
async fn raw_dropping_credit_blocked_output_releases_owned_resources() {
    for version in [SslVersion::TLS1_2, SslVersion::TLS1_3] {
        for aborts in [false, true] {
            let deadline = Instant::now() + CASE;
            let (trust, address, peer, owner) = timeout_at(deadline, peer(version, Mode::Hold))
                .await
                .unwrap();
            owned(
                deadline,
                (trust, address, peer),
                owner,
                move |(trust, address, peer)| async move {
                    let mut f = Fixture::new(version, aborts, trust, address, true).await;
                    let io = f.io.as_mut().unwrap();
                    let wake = Waker::from(Arc::new(WakeCount::default()));
                    let _ = credit_blocked(io, &wake, false).await;
                    assert_eq!(peer.body_reads.load(Ordering::SeqCst), 0);
                    assert_eq!(f.counts.session_error_closes.load(Ordering::SeqCst), 0);
                    f.clean();
                    assert_eq!(
                        f.counts.session_error_closes.load(Ordering::SeqCst),
                        0,
                        "Drop is not newly qualified error-close authority"
                    );
                    // Keep the whole peer, including its command sender, alive
                    // until the intended client Drop. Disjoint async captures of
                    // body_reads alone would close the peer before this schedule.
                    drop(peer);
                },
            )
            .await;
        }
    }
}
