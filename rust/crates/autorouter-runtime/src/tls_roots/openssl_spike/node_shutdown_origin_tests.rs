//! Separate real TLS operation-origin controls. No fabricated SSL result/ERR queue.
//! Raw-shutdown Pending and custom payload are explicitly synthetic stage controls.
use super::*;
use super::{
    TlsFixtureOwner as PeerOwner, fixture_output as buffered_output_state,
    fixture_pipe_reset as require_pending_write_socket_error, fixture_release_peer as release_peer,
    fixture_run_owned as run_owned,
};
use hyper::rt::{Read as HyperRead, ReadBufCursor, Write as HyperWrite};
use openssl::ssl::{ErrorCode, SslVersion};
use std::future::{Future, poll_fn};
use std::net::{Shutdown, TcpStream as StdTcp};
use std::sync::Mutex;
use std::task::{Wake, Waker};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio::time::{Instant, timeout_at};

const LIMIT: Duration = Duration::from_secs(10);
type WriteError = (io::ErrorKind, Option<i32>);
type TestedTls = TokioIo<SslStream<HandshakeIo<ScriptIo>>>;

#[derive(Default)]
struct GateState {
    held: bool,
    waker: Option<Waker>,
}
#[derive(Default)]
struct Gate(Mutex<GateState>);
struct Hold(Arc<Gate>);
impl Gate {
    fn hold(self: &Arc<Self>) -> Hold {
        let old = {
            let mut s = self.0.lock().unwrap();
            assert!(!s.held);
            s.held = true;
            s.waker.take()
        };
        drop(old);
        Hold(self.clone())
    }
    fn poll(&self, cx: &Context<'_>) -> Poll<()> {
        let next = cx.waker().clone();
        let (held, old) = {
            let mut s = self.0.lock().unwrap();
            if s.held {
                (true, s.waker.replace(next))
            } else {
                (false, Some(next))
            }
        };
        drop(old);
        if held { Poll::Pending } else { Poll::Ready(()) }
    }
}
impl Drop for Hold {
    fn drop(&mut self) {
        let wake = {
            let mut s = self.0.0.lock().unwrap_or_else(|e| e.into_inner());
            s.held = false;
            s.waker.take()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
    }
}
#[derive(Default)]
struct CountWake(AtomicUsize);
impl Wake for CountWake {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
#[derive(Debug)]
struct SyntheticRawError(Arc<()>);
impl std::fmt::Display for SyntheticRawError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("synthetic raw-shutdown payload")
    }
}
impl std::error::Error for SyntheticRawError {}
#[derive(Default)]
struct Control {
    write_gate: Arc<Gate>,
    raw_gate: Arc<Gate>,
    writes_entered: AtomicUsize,
    writes_forwarded: AtomicUsize,
    flushes: AtomicUsize,
    raw_entered: AtomicUsize,
    raw_forwarded: AtomicUsize,
    actual_write_error: Mutex<Option<WriteError>>,
    synthetic_raw_error: Mutex<Option<Arc<()>>>,
}
struct ScriptIo {
    tcp: Tcp,
    control: Arc<Control>,
}
impl HyperRead for ScriptIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        HyperRead::poll_read(Pin::new(&mut self.get_mut().tcp), cx, buf)
    }
}
impl HyperWrite for ScriptIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.control.writes_entered.fetch_add(1, Ordering::SeqCst);
        if this.control.write_gate.poll(cx).is_pending() {
            return Poll::Pending;
        }
        this.control.writes_forwarded.fetch_add(1, Ordering::SeqCst);
        let result = HyperWrite::poll_write(Pin::new(&mut this.tcp), cx, bytes);
        if let Poll::Ready(Err(error)) = &result {
            *this.control.actual_write_error.lock().unwrap() =
                Some((error.kind(), error.raw_os_error()));
        }
        result
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.control.flushes.fetch_add(1, Ordering::SeqCst);
        HyperWrite::poll_flush(Pin::new(&mut this.tcp), cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.control.raw_entered.fetch_add(1, Ordering::SeqCst);
        if this.control.raw_gate.poll(cx).is_pending() {
            return Poll::Pending;
        }
        let synthetic = this.control.synthetic_raw_error.lock().unwrap().take();
        if let Some(payload) = synthetic {
            return Poll::Ready(Err(io::Error::other(SyntheticRawError(payload))));
        }
        this.control.raw_forwarded.fetch_add(1, Ordering::SeqCst);
        HyperWrite::poll_shutdown(Pin::new(&mut this.tcp), cx)
    }
}

// Peer ownership survives the assertion task: even a panic/timeout is followed
// by release and a bounded join before the original panic is resumed.

async fn start_peer(
    version: SslVersion,
    graceful: bool,
) -> (TrustSnapshot, std::net::SocketAddr, PeerOwner) {
    let (trust, context) = super::super::lifecycle_tests::isolated_tls(Some(version));
    let (listener, _) = super::super::loopback_peer::bind_listener(true).await;
    let address = listener.local_addr().unwrap();
    let (release, released) = oneshot::channel();
    let (closed_send, closed) = tokio::sync::watch::channel(false);
    let peer = tokio::spawn(async move {
        let mut stream =
            super::super::loopback_peer::accept_peer(&listener, context.as_ref()).await;
        let mut byte = [0];
        stream.read_exact(&mut byte).await.unwrap();
        assert_eq!(&byte, b"q");
        stream.write_all(b"r").await.unwrap();
        stream.flush().await.unwrap();
        let _ = released.await;
        if graceful {
            stream.shutdown().await.unwrap();
        }
        drop(stream);
        let _ = closed_send.send(true);
    });
    (
        trust,
        address,
        PeerOwner {
            release: Arc::new(Mutex::new(Some(release))),
            peer: Some(peer),
            closed,
        },
    )
}
struct TlsFixture {
    stream: Option<TestedTls>,
    duplicate: Option<StdTcp>,
    control: Arc<Control>,
}
impl TlsFixture {
    async fn new(version: SslVersion, trust: TrustSnapshot, address: std::net::SocketAddr) -> Self {
        let mut fixture = Self {
            stream: None,
            duplicate: None,
            control: Arc::new(Control::default()),
        };
        let policy = context(&trust, Profile::Raw, false).unwrap();
        let socket = TcpStream::connect(address)
            .await
            .unwrap()
            .into_std()
            .unwrap();
        fixture.duplicate = Some(socket.try_clone().unwrap());
        let socket = TcpStream::from_std(socket).unwrap();
        let mut stream = SslStream::new(
            Ssl::new(&policy.tls).unwrap(),
            HandshakeIo::new(ScriptIo {
                tcp: TokioIo::new(socket),
                control: fixture.control.clone(),
            }),
        )
        .unwrap();
        Pin::new(&mut stream).connect().await.unwrap();
        assert_eq!(stream.ssl().version2(), Some(version));
        assert!(stream.ssl().is_init_finished());
        stream.get_mut().release().await.unwrap();
        fixture.stream = Some(TokioIo::new(stream));
        let stream = fixture.stream.as_mut().unwrap();
        stream.write_all(b"q").await.unwrap();
        stream.flush().await.unwrap();
        let mut byte = [0];
        stream.read_exact(&mut byte).await.unwrap();
        assert_eq!(&byte, b"r");
        fixture
    }
}

async fn run_tls<F, Fut>(version: SslVersion, check: F)
where
    F: FnOnce(TlsFixture) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let deadline = Instant::now() + LIMIT;
    // start_peer has no await after spawning: no peer escapes without its owner.
    let (trust, address, owner) = timeout_at(deadline, start_peer(version, false))
        .await
        .expect("peer setup deadline");
    run_owned(
        deadline,
        (trust, address),
        owner,
        move |(trust, address)| async move {
            let fixture = TlsFixture::new(version, trust, address).await;
            check(fixture).await;
        },
    )
    .await;
}
fn require_actual_errno(error: &io::Error, actual: WriteError) {
    let errno = actual.1.expect("actual socket OS errno");
    assert_eq!(actual.0, io::ErrorKind::BrokenPipe);
    if let Some(returned) = error.raw_os_error() {
        assert_eq!(returned, errno);
        assert_eq!(error.kind(), actual.0);
        return;
    }
    let tls = error
        .get_ref()
        .and_then(|e| e.downcast_ref::<openssl::ssl::Error>())
        .expect("identified TLS error or direct errno");
    assert_eq!(tls.code(), ErrorCode::SYSCALL);
    let mut found = false;
    for entry in tls.ssl_error().expect("actual TLS error stack").errors() {
        if entry.library_code() == 2 {
            assert_eq!(entry.reason_code(), errno);
            found = true;
        }
    }
    assert!(
        found,
        "exact actual OS error must remain in returned SYS stack"
    );
}

#[tokio::test]
async fn real_tls_finalization_error_calls_back_once_before_actual_raw_shutdown() {
    for version in [SslVersion::TLS1_2, SslVersion::TLS1_3] {
        run_tls(version, |mut fixture| async move {
            fixture
                .duplicate
                .as_ref()
                .unwrap()
                .shutdown(Shutdown::Write)
                .unwrap();
            let control = fixture.control.clone();
            let stream = fixture.stream.as_mut().unwrap();
            let before = control.writes_forwarded.load(Ordering::SeqCst);
            assert!(!stream.inner().get_ref().shutdown_entered());
            let mut tcp_only = false;
            let mut callbacks = 0;
            poll_fn(|cx| {
                poll_node_tls_shutdown(stream, &mut tcp_only, true, cx, |error| {
                    callbacks += 1;
                    assert_eq!(callbacks, 1);
                    assert_eq!(
                        control.raw_entered.load(Ordering::SeqCst),
                        0,
                        "callback precedes raw-stage entry"
                    );
                    require_actual_errno(
                        error,
                        control
                            .actual_write_error
                            .lock()
                            .unwrap()
                            .expect("actual BIO socket failure"),
                    );
                })
            })
            .await
            .unwrap();
            assert!(control.writes_forwarded.load(Ordering::SeqCst) > before);
            assert_eq!(callbacks, 1);
            assert!(tcp_only);
            assert!(stream.inner().get_ref().shutdown_entered());
            assert!(
                control.raw_forwarded.load(Ordering::SeqCst) > 0,
                "actual TCP shutdown called"
            );
            let writes = control.writes_entered.load(Ordering::SeqCst);
            poll_fn(|cx| {
                poll_node_tls_shutdown(stream, &mut tcp_only, true, cx, |_| {
                    callbacks += 1;
                })
            })
            .await
            .unwrap();
            assert_eq!(callbacks, 1);
            assert_eq!(control.writes_entered.load(Ordering::SeqCst), writes);
        })
        .await;
    }
}
#[tokio::test]
async fn pending_raw_stage_resumes_without_repeating_tls_or_callback() {
    for version in [SslVersion::TLS1_2, SslVersion::TLS1_3] {
        for allow_finalization_error in [true, false] {
            run_tls(version, move |mut fixture| async move {
                let control = fixture.control.clone();
                let hold = control.raw_gate.hold();
                let stream = fixture.stream.as_mut().unwrap();
                let mut tcp_only = false;
                let mut callbacks = 0;
                let observed = poll_fn(|cx| {
                    let result = poll_node_tls_shutdown(
                        stream,
                        &mut tcp_only,
                        allow_finalization_error,
                        cx,
                        |_| {
                            callbacks += 1;
                        },
                    );
                    if control.raw_entered.load(Ordering::SeqCst) > 0 {
                        Poll::Ready(result)
                    } else {
                        assert!(result.is_pending());
                        Poll::Pending
                    }
                })
                .await;
                assert!(observed.is_pending());
                assert!(tcp_only);
                assert!(stream.inner().get_ref().shutdown_entered());
                assert_eq!(callbacks, 0);
                assert_eq!(control.raw_forwarded.load(Ordering::SeqCst), 0);
                let writes = control.writes_entered.load(Ordering::SeqCst);
                let flushes = control.flushes.load(Ordering::SeqCst);
                let wake = Arc::new(CountWake::default());
                let waker = Waker::from(wake.clone());
                assert!(
                    poll_node_tls_shutdown(
                        stream,
                        &mut tcp_only,
                        allow_finalization_error,
                        &mut Context::from_waker(&waker),
                        |_| {
                            callbacks += 1;
                        }
                    )
                    .is_pending()
                );
                drop(hold);
                assert!(
                    wake.0.load(Ordering::SeqCst) > 0,
                    "owned raw gate release wakes waiter"
                );
                poll_fn(|cx| {
                    poll_node_tls_shutdown(
                        stream,
                        &mut tcp_only,
                        allow_finalization_error,
                        cx,
                        |_| {
                            callbacks += 1;
                        },
                    )
                })
                .await
                .unwrap();
                assert!(control.raw_forwarded.load(Ordering::SeqCst) > 0);
                assert_eq!(callbacks, 0);
                assert_eq!(control.writes_entered.load(Ordering::SeqCst), writes);
                assert_eq!(control.flushes.load(Ordering::SeqCst), flushes);
            })
            .await;
        }
    }
}
#[tokio::test]
async fn synthetic_raw_stage_error_preserves_payload_without_finalization_callback() {
    for version in [SslVersion::TLS1_2, SslVersion::TLS1_3] {
        run_tls(version, |mut fixture| async move {
            let control = fixture.control.clone();
            let payload = Arc::new(());
            *control.synthetic_raw_error.lock().unwrap() = Some(payload.clone());
            let stream = fixture.stream.as_mut().unwrap();
            let mut tcp_only = false;
            let mut callbacks = 0;
            let error = poll_fn(|cx| {
                poll_node_tls_shutdown(stream, &mut tcp_only, true, cx, |_| {
                    callbacks += 1;
                })
            })
            .await
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Other);
            assert!(error.raw_os_error().is_none());
            let returned = error
                .get_ref()
                .and_then(|e| e.downcast_ref::<SyntheticRawError>())
                .expect("same synthetic raw payload type");
            assert!(Arc::ptr_eq(&returned.0, &payload));
            assert_eq!(callbacks, 0);
            assert!(tcp_only);
            assert!(stream.inner().get_ref().shutdown_entered());
            assert!(control.raw_entered.load(Ordering::SeqCst) > 0);
            assert_eq!(
                control.raw_forwarded.load(Ordering::SeqCst),
                0,
                "fault is synthetic, not a real OS shutdown error"
            );
            let writes = control.writes_entered.load(Ordering::SeqCst);
            let flushes = control.flushes.load(Ordering::SeqCst);
            poll_fn(|cx| {
                poll_node_tls_shutdown(stream, &mut tcp_only, true, cx, |_| {
                    callbacks += 1;
                })
            })
            .await
            .unwrap();
            assert!(control.raw_forwarded.load(Ordering::SeqCst) > 0);
            assert_eq!(callbacks, 0);
            assert_eq!(control.writes_entered.load(Ordering::SeqCst), writes);
            assert_eq!(control.flushes.load(Ordering::SeqCst), flushes);
        })
        .await;
    }
}
#[tokio::test]
async fn pending_tls_write_does_not_enter_raw_shutdown_or_claim_finalization() {
    for version in [SslVersion::TLS1_2, SslVersion::TLS1_3] {
        run_tls(version, |mut fixture| async move {
            let control = fixture.control.clone();
            let hold = control.write_gate.hold();
            let writes = control.writes_entered.load(Ordering::SeqCst);
            let forwarded = control.writes_forwarded.load(Ordering::SeqCst);
            let stream = fixture.stream.as_mut().unwrap();
            let mut tcp_only = false;
            let mut callbacks = 0;
            let wake = Arc::new(CountWake::default());
            let waker = Waker::from(wake.clone());
            let result = poll_node_tls_shutdown(
                stream,
                &mut tcp_only,
                true,
                &mut Context::from_waker(&waker),
                |_| {
                    callbacks += 1;
                },
            );
            assert!(result.is_pending());
            assert!(control.writes_entered.load(Ordering::SeqCst) > writes);
            assert_eq!(control.writes_forwarded.load(Ordering::SeqCst), forwarded);
            assert!(!tcp_only);
            assert!(!stream.inner().get_ref().shutdown_entered());
            assert_eq!(control.raw_entered.load(Ordering::SeqCst), 0);
            assert_eq!(callbacks, 0);
            drop(hold);
            assert!(wake.0.load(Ordering::SeqCst) > 0);
            poll_fn(|cx| {
                poll_node_tls_shutdown(stream, &mut tcp_only, true, cx, |_| {
                    callbacks += 1;
                })
            })
            .await
            .unwrap();
            assert!(tcp_only);
            assert!(control.raw_forwarded.load(Ordering::SeqCst) > 0);
            assert_eq!(callbacks, 0);
        })
        .await;
    }
}

// This control uses the exact fixed production-candidate TransportIo type.
// It cannot insert ScriptIo or a fabricated session into that connector.
struct CacheFixture {
    transport: Option<TransportIo>,
    connector: Option<Connector>,
    cache: RawCache,
    counts: Arc<Counts>,
    release: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}
impl CacheFixture {
    async fn new(
        version: SslVersion,
        aborts: bool,
        trust: TrustSnapshot,
        address: std::net::SocketAddr,
        release: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    ) -> Self {
        let f = super::fixture_connect_raw(version, aborts, trust, address, None, true).await;
        Self {
            transport: f.transport,
            connector: f.connector,
            cache: f.cache,
            counts: f.counts,
            release,
        }
    }

    fn accepted(&self, transport: &TransportIo) -> (usize, usize, usize) {
        let accepted = transport
            .session
            .as_ref()
            .unwrap()
            .lookup()
            .expect("real accepted TLS ticket");
        let before = self.cache.lock().unwrap().snapshot();
        assert_eq!(before.0, 1);
        assert_eq!(before.1, accepted.encoded_len());
        assert!(before.1 > 0);
        before
    }

    fn clean(mut self, transport: TransportIo, probe: &super::super::idle_close::Probe) {
        assert_eq!(
            self.counts.attempts.load(Ordering::SeqCst),
            1,
            "no extra dial or retry"
        );
        assert_eq!(self.counts.completed.load(Ordering::SeqCst), 1);
        drop(transport);
        drop(self.connector.take());
        super::fixture_clean(&self.counts, &self.cache, Some(probe));
    }
}

#[tokio::test]
async fn disallowed_real_tls_finalization_error_stays_error_without_raw_entry() {
    for version in [SslVersion::TLS1_2, SslVersion::TLS1_3] {
        run_tls(version, |mut fixture| async move {
            fixture
                .duplicate
                .as_ref()
                .unwrap()
                .shutdown(Shutdown::Write)
                .unwrap();
            let control = fixture.control.clone();
            let stream = fixture.stream.as_mut().unwrap();
            let writes = control.writes_forwarded.load(Ordering::SeqCst);
            let mut tcp_only = false;
            let mut callbacks = 0;
            let error = poll_fn(|cx| {
                poll_node_tls_shutdown(stream, &mut tcp_only, false, cx, |_| {
                    callbacks += 1;
                })
            })
            .await
            .unwrap_err();
            require_actual_errno(
                &error,
                control
                    .actual_write_error
                    .lock()
                    .unwrap()
                    .expect("actual BIO socket failure"),
            );
            assert!(control.writes_forwarded.load(Ordering::SeqCst) > writes);
            assert_eq!(callbacks, 0);
            assert!(!tcp_only);
            assert!(!stream.inner().get_ref().shutdown_entered());
            assert_eq!(control.raw_entered.load(Ordering::SeqCst), 0);
            assert_eq!(control.raw_forwarded.load(Ordering::SeqCst), 0);
        })
        .await;
    }
}

// A positive-capacity TLS write, through the exact production TransportIo, must
// actually return Pending. No IO wrapper, fixture gate, injected errno or fake
// accepted cache entry is used. Disabling cooperative budgeting for this finite
// poll loop avoids treating a task-budget yield as socket-output backpressure.
const APP_CHUNK: usize = 16 * 1024;
const APP_ATTEMPTS: usize = 1024;
const APP_BYTES: usize = APP_CHUNK * (APP_ATTEMPTS + 1);

async fn observed_cache_fixture(
    version: SslVersion,
    aborts: bool,
    trust: TrustSnapshot,
    address: std::net::SocketAddr,
    release: Arc<Mutex<Option<oneshot::Sender<()>>>>,
) -> (CacheFixture, super::super::idle_close::Probe) {
    let probe = super::super::idle_close::Probe::default();
    let f = super::fixture_connect_raw(version, aborts, trust, address, Some(probe.clone()), true)
        .await;
    (
        CacheFixture {
            transport: f.transport,
            connector: f.connector,
            cache: f.cache,
            counts: f.counts,
            release,
        },
        probe,
    )
}

fn require_no_finalization(rows: &[super::super::idle_close::Event]) {
    assert!(
        !rows.iter().any(|event| matches!(
            event,
            super::super::idle_close::Event::ShutdownTlsFinalizationError(_, _)
        )),
        "application write/flush failure is not close-finalization"
    );
}

fn require_real_fatal_shutdown(error: &io::Error) {
    if error.raw_os_error().is_some() {
        return;
    }
    let tls = error
        .get_ref()
        .and_then(|e| e.downcast_ref::<openssl::ssl::Error>())
        .expect("identified real TLS failure or actual OS errno");
    assert!(
        matches!(tls.code(), ErrorCode::SSL | ErrorCode::SYSCALL),
        "actual fatal TLS shutdown result, not retry/ordinary EOF"
    );
    let has_io = tls.io_error().is_some_and(|e| e.raw_os_error().is_some());
    let has_stack = tls
        .ssl_error()
        .is_some_and(|stack| !stack.errors().is_empty());
    assert!(
        has_io || has_stack,
        "positive actual BIO errno or nonempty current SSL error stack"
    );
}

// Distinct diagnostic: retry the original immutable write before any SSL
// shutdown. Positive OS failure, not a fabricated shutdown error, is required.

#[tokio::test]
async fn buffered_application_after_ordinary_eof_preserves_actual_error_on_write_or_flush() {
    for version in [SslVersion::TLS1_2, SslVersion::TLS1_3] {
        for aborts in [false, true] {
            let deadline = Instant::now() + LIMIT;
            let (trust, address, owner) = timeout_at(deadline, start_peer(version, true))
                .await
                .expect("cache peer setup deadline");
            let release = owner.release.clone();
            run_owned(
                deadline,
                (trust, address, release),
                owner,
                move |(trust, address, release)| async move {
                    let mut fixture =
                        CacheFixture::new(version, aborts, trust, address, release).await;
                    let mut transport = fixture.transport.take().unwrap();
                    // Attach only a diagnostic observer to this existing real
                    // IO. The event is not another dial/acceptance observation.
                    let probe = super::super::idle_close::Probe::default();
                    assert!(transport.idle.is_none());
                    transport.idle = Some(probe.connected());
                    let before = fixture.accepted(&transport);
                    let handle = transport.abort.as_ref().map(|control| control.handle());
                    assert_eq!(handle.is_some(), aborts);
                    release_peer(&fixture.release);
                    let mut byte = [0];
                    assert_eq!(
                        transport.read(&mut byte).await.unwrap(),
                        0,
                        "actual positive-capacity TLS EOF"
                    );
                    assert_eq!(
                        fixture.cache.lock().unwrap().snapshot(),
                        before,
                        "ordinary EOF preserves accepted cache"
                    );
                    if let Some(handle) = &handle {
                        assert_eq!(
                            handle.terminal(),
                            Some(super::super::abort::Terminal::Ordinary)
                        );
                    }
                    assert_eq!(
                        fixture.counts.session_error_closes.load(Ordering::SeqCst),
                        0
                    );
                    // With all owned output empty, lower shutdown really enters
                    // the physical socket phase; it cannot block on ciphertext.
                    let state = buffered_output_state(&transport);
                    assert_eq!((state.1, state.2, state.3), (0, false, 0));
                    match &mut transport.stream {
                        Stream::Tls(stream) => poll_fn(|cx| {
                            HyperWrite::poll_shutdown(Pin::new(stream.inner_mut().get_mut()), cx)
                        })
                        .await
                        .unwrap(),
                        Stream::Plain(_) => panic!("real TLS required"),
                    }
                    match &transport.stream {
                        Stream::Tls(stream) => assert!(
                            stream.inner().get_ref().shutdown_entered(),
                            "actual physical TCP shutdown entry"
                        ),
                        Stream::Plain(_) => panic!("real TLS required"),
                    }
                    let result = poll_fn(|cx| Pin::new(&mut transport).poll_write(cx, b"x")).await;
                    let error = match result {
                        Err(error) => error,
                        Ok(count) => {
                            assert_eq!(
                                count, 1,
                                "only this caller's single buffered byte may be credited"
                            );
                            assert_eq!(
                                fixture.cache.lock().unwrap().snapshot(),
                                before,
                                "buffered credit is not a socket error or completed delivery"
                            );
                            assert_eq!(
                                fixture.counts.session_error_closes.load(Ordering::SeqCst),
                                0
                            );
                            transport.flush().await.unwrap_err()
                        }
                    };
                    require_actual_errno(
                        &error,
                        (
                            io::ErrorKind::BrokenPipe,
                            Some(nix::errno::Errno::EPIPE as i32),
                        ),
                    );
                    require_no_finalization(&probe.rows());
                    super::fixture_evicted(&fixture.cache, &fixture.counts, before.2, aborts);
                    assert!(
                        poll_fn(|cx| Pin::new(&mut transport).poll_write(cx, b"y"))
                            .await
                            .is_err()
                    );
                    super::fixture_evicted(&fixture.cache, &fixture.counts, before.2, aborts);
                    fixture.clean(transport, &probe);
                },
            )
            .await;
        }
    }
}

// New buffered API controls. Historical H/I whole sources and failed results
// remain separate; logical caller Pending is not an actual SSL-write retry.

#[derive(Clone, Copy)]
pub(super) enum BufferedFaultAt {
    Shutdown,
    WriteThenFlush,
}

pub(super) async fn buffered_peer_close_fault(at: BufferedFaultAt) {
    use super::super::idle_close::Event;
    for version in [SslVersion::TLS1_2, SslVersion::TLS1_3] {
        for aborts in [false, true] {
            for vectored in [false, true] {
                let deadline = Instant::now() + LIMIT;
                let (trust, address, owner) = timeout_at(deadline, start_peer(version, false))
                    .await
                    .expect("owned nonreading peer setup deadline");
                let release = owner.release.clone();
                let closed = owner.closed.clone();
                run_owned(
                    deadline,
                    (trust, address, release, closed),
                    owner,
                    move |(trust, address, release, mut closed)| async move {
                        let (mut fixture, probe) =
                            observed_cache_fixture(version, aborts, trust, address, release).await;
                        let mut transport = fixture.transport.take().unwrap();
                        let before = fixture.accepted(&transport);
                        assert_eq!(transport.abort.is_some(), aborts);
                        assert_eq!(transport.node_shutdown_tcp_only, Some(false));
                        let bytes = vec![0x5a; APP_CHUNK];
                        transport.write_all(&bytes).await.unwrap();
                        let waker = Waker::from(Arc::new(CountWake::default()));
                        let ledger =
                            super::fixture_credit_blocked(&mut transport, &waker, vectored).await;
                        assert!((1..=APP_ATTEMPTS).contains(&(ledger.len() + 1)));
                        assert!(
                            ledger.iter().map(|(_, count)| *count).sum::<usize>() + APP_CHUNK
                                <= APP_BYTES
                        );
                        let state = buffered_output_state(&transport);
                        assert!(
                            state.1 > 0 && !state.2 && state.3 > 32 * 1024,
                            "positive owned pre-SSL credit barrier, not an inferred SSL WANT_WRITE"
                        );
                        // Both forms are real no-ops: no ownership, credited
                        // bytes or actual SSL retry state can be erased.
                        let empty =
                            poll_fn(|cx| Poll::Ready(Pin::new(&mut transport).poll_write(cx, &[])))
                                .await;
                        assert!(matches!(empty, Poll::Ready(Ok(0))));
                        assert_eq!(buffered_output_state(&transport), state);
                        let vectors = [io::IoSlice::new(&[]), io::IoSlice::new(&[])];
                        let empty = poll_fn(|cx| {
                            Poll::Ready(Pin::new(&mut transport).poll_write_vectored(cx, &vectors))
                        })
                        .await;
                        assert!(matches!(empty, Poll::Ready(Ok(0))));
                        assert_eq!(buffered_output_state(&transport), state);
                        assert_eq!(fixture.cache.lock().unwrap().snapshot(), before);
                        super::fixture_no_write_hold(&probe);
                        // No call to the new draining lower-shutdown method as
                        // an immediate fault. Drop the owned real peer and wait
                        // for its post-Drop watch signal instead.
                        let before_halfclose = buffered_output_state(&transport);
                        shutdown_raw_tls_write_half_for_test(&transport)
                            .expect("actual OS socket write-half closure");
                        assert_eq!(
                            buffered_output_state(&transport),
                            before_halfclose,
                            "OS fault preserves both admitted output owners"
                        );
                        release_peer(&fixture.release);
                        drop(
                            closed
                                .wait_for(|closed| *closed)
                                .await
                                .expect("actual owned peer-drop barrier"),
                        );
                        let error = match at {
                            BufferedFaultAt::Shutdown => transport.shutdown().await.unwrap_err(),
                            BufferedFaultAt::WriteThenFlush => {
                                // This is NEW unconsumed caller input. Internal
                                // owned bytes are retried first; do not recredit
                                // them as consumption of this different slice.
                                let fresh = b"new-uncredited-caller";
                                let result = poll_fn(|cx| {
                                    if vectored {
                                        let vectors = [io::IoSlice::new(fresh)];
                                        Pin::new(&mut transport).poll_write_vectored(cx, &vectors)
                                    } else {
                                        Pin::new(&mut transport).poll_write(cx, fresh)
                                    }
                                })
                                .await;
                                match result {
                                    Err(error) => error,
                                    Ok(count) => {
                                        assert!(
                                            count > 0 && count <= fresh.len(),
                                            "only actual new-input acceptance may be credited"
                                        );
                                        // As with any buffered writer, accepted
                                        // bytes are not physical success. Flush
                                        // must expose the actual socket fault.
                                        transport.flush().await.unwrap_err()
                                    }
                                }
                            }
                        };
                        require_real_fatal_shutdown(&error);
                        require_pending_write_socket_error(&error); // Exact EPIPE/ECONNRESET or positive corresponding SYS reason.
                        assert_eq!(
                            transport.node_shutdown_tcp_only,
                            Some(false),
                            "application drain failed before TLS finalization/raw phase"
                        );
                        let rows = probe.rows();
                        assert!(rows.iter().any(|event| matches!(
                            event,
                            Event::ShutdownError(_, _)
                                | Event::WriteError(_, _)
                                | Event::FlushError(_, _)
                        )));
                        require_no_finalization(&rows);
                        super::fixture_evicted(&fixture.cache, &fixture.counts, before.2, aborts);
                        assert!(
                            transport.flush().await.is_err(),
                            "failed output remains terminal"
                        );
                        super::fixture_evicted(&fixture.cache, &fixture.counts, before.2, aborts);
                        assert!(
                            transport.shutdown().await.is_err(),
                            "terminal application failure cannot become clean shutdown"
                        );
                        super::fixture_evicted(&fixture.cache, &fixture.counts, before.2, aborts);
                        drop(bytes);
                        fixture.clean(transport, &probe);
                    },
                )
                .await;
            }
        }
    }
}

#[tokio::test]
async fn buffered_owned_output_local_halfclose_and_peer_drop_fails_shutdown_without_finalization_absorption()
 {
    buffered_peer_close_fault(BufferedFaultAt::Shutdown).await;
}

#[tokio::test]
async fn buffered_owned_output_local_halfclose_and_peer_drop_keeps_actual_fault_on_write_or_flush()
{
    buffered_peer_close_fault(BufferedFaultAt::WriteThenFlush).await;
}
