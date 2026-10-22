//! Real TLS operations with stale thread-local errors; no fabricated IO error.
use super::*;
use hyper::rt::{Read as HyperRead, ReadBufCursor, Write as HyperWrite};
use openssl::error::ErrorStack;
use openssl::ssl::{SslContextBuilder, SslMethod, SslVersion};
use std::future::poll_fn;
use std::sync::Mutex;
use std::task::Waker;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const LIMIT: Duration = Duration::from_secs(10);
type WriteError = (io::ErrorKind, Option<i32>);

#[derive(Default)]
struct GateState {
    held: bool,
    polls: usize,
    waker: Option<Waker>,
}
#[derive(Default)]
struct WriteGate(Mutex<GateState>);
struct WriteHold(Arc<WriteGate>);
impl WriteGate {
    fn hold(self: &Arc<Self>) -> WriteHold {
        let mut state = self.0.lock().unwrap();
        assert!(!state.held);
        state.held = true;
        WriteHold(self.clone())
    }
    fn poll(&self, cx: &Context<'_>) -> Poll<()> {
        let next = cx.waker().clone();
        let (held, old) = {
            let mut state = self.0.lock().unwrap();
            if state.held {
                state.polls += 1;
                (true, state.waker.replace(next))
            } else {
                (false, Some(next))
            }
        };
        drop(old);
        if held { Poll::Pending } else { Poll::Ready(()) }
    }
}
impl Drop for WriteHold {
    fn drop(&mut self) {
        let wake = {
            let mut state = self.0.0.lock().unwrap_or_else(|e| e.into_inner());
            state.held = false;
            state.waker.take()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
    }
}
struct BioIo {
    inner: Tcp,
    gate: Arc<WriteGate>,
    writes: Arc<AtomicUsize>,
    write_error: Arc<Mutex<Option<WriteError>>>,
}
impl HyperRead for BioIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        HyperRead::poll_read(Pin::new(&mut self.get_mut().inner), cx, buf)
    }
}
impl HyperWrite for BioIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.gate.poll(cx).is_pending() {
            return Poll::Pending;
        }
        this.writes.fetch_add(1, Ordering::SeqCst);
        let result = HyperWrite::poll_write(Pin::new(&mut this.inner), cx, bytes);
        if let Poll::Ready(Err(error)) = &result {
            *this.write_error.lock().unwrap() = Some((error.kind(), error.raw_os_error()));
        }
        result
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        HyperWrite::poll_flush(Pin::new(&mut self.get_mut().inner), cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        HyperWrite::poll_shutdown(Pin::new(&mut self.get_mut().inner), cx)
    }
}
struct TlsFixture {
    stream: Option<TokioIo<SslStream<BioIo>>>,
    release: Option<oneshot::Sender<()>>,
    peer: Option<JoinHandle<()>>,
    gate: Arc<WriteGate>,
    writes: Arc<AtomicUsize>,
    write_error: Arc<Mutex<Option<WriteError>>>,
}
impl Drop for TlsFixture {
    fn drop(&mut self) {
        self.release.take();
        if let Some(peer) = self.peer.take() {
            peer.abort();
        }
    }
}
impl TlsFixture {
    async fn new(version: SslVersion) -> Self {
        let (trust, server) = super::super::lifecycle_tests::isolated_tls(Some(version));
        let (listener, _) = super::super::loopback_peer::bind_listener(true).await;
        let address = listener.local_addr().unwrap();
        let (release, released) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let mut stream =
                super::super::loopback_peer::accept_peer(&listener, server.as_ref()).await;
            let mut byte = [0];
            stream.read_exact(&mut byte).await.unwrap();
            assert_eq!(&byte, b"q");
            stream.write_all(b"r").await.unwrap();
            stream.flush().await.unwrap();
            let _ = released.await;
        });
        let gate = Arc::new(WriteGate::default());
        let writes = Arc::new(AtomicUsize::new(0));
        let write_error = Arc::new(Mutex::new(None));
        // Own the peer task before any client handshake/assertion can fail.
        let mut fixture = Self {
            stream: None,
            release: Some(release),
            peer: Some(peer),
            gate,
            writes,
            write_error,
        };
        let policy = context(&trust, Profile::Raw, false).unwrap();
        let stream = TcpStream::connect(address).await.unwrap();
        let mut stream = SslStream::new(
            Ssl::new(&policy.tls).unwrap(),
            BioIo {
                inner: TokioIo::new(stream),
                gate: fixture.gate.clone(),
                writes: fixture.writes.clone(),
                write_error: fixture.write_error.clone(),
            },
        )
        .unwrap();
        Pin::new(&mut stream).connect().await.unwrap();
        assert_eq!(stream.ssl().version2(), Some(version));
        assert!(stream.ssl().is_init_finished());
        fixture.stream = Some(TokioIo::new(stream));
        let stream = fixture.stream.as_mut().unwrap();
        stream.write_all(b"q").await.unwrap();
        stream.flush().await.unwrap();
        let mut byte = [0];
        stream.read_exact(&mut byte).await.unwrap();
        assert_eq!(&byte, b"r");
        fixture
    }
    async fn finish(mut self) {
        self.stream.take();
        let _ = self.release.take().unwrap().send(());
        let peer = self.peer.take().unwrap();
        tokio::time::timeout(LIMIT, peer).await.unwrap().unwrap();
    }
}
fn stale_sys_enoent() -> ErrorStack {
    // No user file: this exclusive private directory has no CA member.
    use std::os::unix::fs::DirBuilderExt;
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::path::Path::new("/tmp").join(format!(
        "autorouter-shutdown-queue-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .unwrap();
    let mut context = SslContextBuilder::new(SslMethod::tls_client()).unwrap();
    let stale = context.set_ca_file(path.join("missing.pem")).unwrap_err();
    drop(context);
    std::fs::remove_dir(&path).unwrap();
    assert!(
        stale
            .errors()
            .iter()
            .any(|e| e.library_code() == 2 && e.reason_code() == 2)
    );
    drop(ErrorStack::get());
    stale
}

#[tokio::test]
async fn stale_sys_error_cannot_turn_real_shutdown_write_pending_into_fatal_error() {
    tokio::time::timeout(LIMIT, async {
        for version in [SslVersion::TLS1_2, SslVersion::TLS1_3] {
            let mut fixture = TlsFixture::new(version).await;
            let hold = fixture.gate.hold();
            let stale = stale_sys_enoent();
            let result = poll_fn(|cx| {
                stale.put(); // Same task, immediately before this exact TLS poll.
                Poll::Ready(poll_tls_shutdown(fixture.stream.as_mut().unwrap(), cx))
            })
            .await;
            assert_eq!(
                fixture.gate.0.lock().unwrap().polls,
                1,
                "actual below-SSL close-notify write must reach the held gate"
            );
            assert!(
                result.is_pending(),
                "unrelated ERRqueue must not turn WANT_WRITE into fatal IO"
            );
            drop(hold);
            poll_fn(|cx| poll_tls_shutdown(fixture.stream.as_mut().unwrap(), cx))
                .await
                .unwrap();
            fixture.finish().await;
        }
    })
    .await
    .expect("stale ERRqueue real TLS fixture deadline");
}

#[tokio::test]
async fn stale_sys_error_does_not_hide_a_genuine_shutdown_bio_socket_error() {
    tokio::time::timeout(LIMIT, async {
        for version in [SslVersion::TLS1_2, SslVersion::TLS1_3] {
            let mut fixture = TlsFixture::new(version).await;
            // Real half-close of this actual TCP socket; no injected Err value.
            fixture
                .stream
                .as_mut()
                .unwrap()
                .inner_mut()
                .get_mut()
                .inner
                .inner_mut()
                .shutdown()
                .await
                .unwrap();
            let before = fixture.writes.load(Ordering::SeqCst);
            assert!(fixture.write_error.lock().unwrap().is_none());
            let stale = stale_sys_enoent();
            let result = poll_fn(|cx| {
                stale.put();
                Poll::Ready(poll_tls_shutdown(fixture.stream.as_mut().unwrap(), cx))
            })
            .await;
            assert!(
                fixture.writes.load(Ordering::SeqCst) > before,
                "actual BIO socket write must be attempted"
            );
            let error = match result {
                Poll::Ready(Err(error)) => error,
                _ => panic!("real write-half-close must remain an error"),
            };
            let bio = fixture
                .write_error
                .lock()
                .unwrap()
                .expect("actual BIO write error");
            assert_eq!(bio.0, io::ErrorKind::BrokenPipe);
            let actual_errno = bio.1.expect("actual socket errno");
            eprintln!(
                "TLS shutdown {version:?}: BIO={bio:?}, outer_kind={:?}, outer_errno={:?}",
                error.kind(),
                error.raw_os_error()
            );
            if let Some(returned_errno) = error.raw_os_error() {
                assert_eq!(returned_errno, actual_errno);
                assert_eq!(error.kind(), bio.0);
            } else {
                // OpenSSL can put the actual BIO errno in its SYS queue;
                // rust-openssl then returns an SSL error owning that stack.
                let tls_error = error
                    .get_ref()
                    .and_then(|source| source.downcast_ref::<openssl::ssl::Error>())
                    .expect("direct errno or a positively identified SSL error");
                assert_eq!(tls_error.code(), openssl::ssl::ErrorCode::SYSCALL);
                let stack = tls_error.ssl_error().expect("fresh SSL error stack");
                let mut actual_sys_error = false;
                for entry in stack.errors() {
                    eprintln!(
                        "TLS shutdown SSL code={} library={} reason={}",
                        tls_error.code().as_raw(),
                        entry.library_code(),
                        entry.reason_code()
                    );
                    if entry.library_code() == 2 {
                        assert_eq!(entry.reason_code(), actual_errno, "no stale SYS error");
                        actual_sys_error = true;
                    }
                }
                assert!(actual_sys_error, "preserve the exact BIO socket errno");
            }
            fixture.finish().await;
        }
    })
    .await
    .expect("real shutdown BIO error fixture deadline");
}
