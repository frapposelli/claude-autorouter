//! Deterministic IO publication controls for the first failure's dispatch.
//! All peers are synthetic; no provider, network, production backend or timer
//! policy is exercised. An intentionally oversized partial-error path is
//! explicitly observer-failed and is never evidence of qualified shutdown.
use super::*;
use std::sync::Condvar;
use std::task::{Wake, Waker};
use std::time::Duration;

#[derive(Default)]
struct HoldState {
    entered: bool,
    released: bool,
    timed_out: bool,
    cause: Option<FailureCause>,
}
#[derive(Default)]
struct ActionHold {
    state: Mutex<HoldState>,
    changed: Condvar,
}
impl ActionHold {
    fn run(&self, cause: FailureCause) {
        let mut state = self.state.lock().unwrap();
        state.entered = true;
        state.cause = Some(cause);
        self.changed.notify_all();
        let (mut state, _) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(5), |state| !state.released)
            .unwrap();
        if !state.released {
            state.timed_out = true;
        }
    }
    fn wait_entered(&self) {
        let state = self.state.lock().unwrap();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(2), |state| !state.entered)
            .unwrap();
        assert!(
            state.entered,
            "the real deadline monitor did not enter its action"
        );
        assert_eq!(state.cause, Some(FailureCause::Deadline));
        assert!(!state.released);
    }
    fn release(&self) {
        self.state.lock().unwrap().released = true;
        self.changed.notify_all();
    }
}
struct HeldMonitor {
    connection: Option<ConnectionTerminal>,
    owner: Option<RequestOwner>,
    terminal: RequestTerminal,
    token: CancellationToken,
    hold: Arc<ActionHold>,
    probe: Probe,
}
impl HeldMonitor {
    fn new() -> Self {
        let probe = Probe::default();
        let connection = ConnectionTerminal::new(probe.clone(), 1);
        let owner = connection.handle().register().unwrap();
        let terminal = owner.publisher();
        let hold = Arc::new(ActionHold::default());
        let action = hold.clone();
        terminal
            .bind_transport_action(move |cause| action.run(cause))
            .unwrap();
        terminal.admit_transport().unwrap();
        owner.attach();
        Self {
            connection: Some(connection),
            owner: Some(owner),
            terminal,
            token: CancellationToken::new(),
            hold,
            probe,
        }
    }
    fn handle(&self) -> ConnectionHandle {
        self.connection.as_ref().unwrap().handle()
    }
    fn deadline(&self) -> StartDeadline<'_> {
        StartDeadline {
            owner: self.owner.as_ref().unwrap(),
            token: self.token.clone(),
            hold: self.hold.clone(),
        }
    }
    fn assert_pending(&self) {
        assert_eq!(self.terminal.failure_cause(), Some(FailureCause::Deadline));
        assert_eq!(self.handle().pending_dispatches().len(), 1);
        assert!(
            !self.token.is_cancelled(),
            "cancellation escaped the held first action"
        );
        assert_eq!(
            self.probe
                .events()
                .iter()
                .filter(|event| matches!(event, Event::Failed(_, FailureCause::Deadline)))
                .count(),
            1
        );
        assert_eq!(
            self.probe
                .events()
                .iter()
                .filter(|event| matches!(event, Event::Failed(_, FailureCause::Downstream(_))))
                .count(),
            0
        );
    }
    async fn settle(&self) {
        self.hold.release();
        crate::bounded(self.terminal.settle_failure()).await;
        assert!(self.token.is_cancelled());
        assert!(self.handle().pending_dispatches().is_empty());
    }
    async fn finish(mut self, diagnostic_expected: bool) {
        self.settle().await;
        drop(self.owner.take());
        crate::bounded(self.connection.take().unwrap().close()).await;
        assert!(
            !self.hold.state.lock().unwrap().timed_out,
            "action fallback elapsed"
        );
        assert_eq!(self.probe.tasks(), 0);
        assert_eq!(self.probe.writer_snapshot().live_io, 0);
        assert_eq!(self.probe.observer_failed(), diagnostic_expected);
    }
}
impl Drop for HeldMonitor {
    fn drop(&mut self) {
        // This guard also releases a blocked monitor on assertion/timeout unwind
        // before the connection owner's fields are destroyed.
        self.hold.release();
    }
}
struct StartDeadline<'a> {
    owner: &'a RequestOwner,
    token: CancellationToken,
    hold: Arc<ActionHold>,
}
impl StartDeadline<'_> {
    fn fire(self) {
        // Starting inside the physical IO poll proves that TerminalIo's entry
        // snapshot saw no pending failure. The real independent monitor wins
        // Deadline before this same poll returns its EOF/error to TerminalIo.
        self.owner.start(Instant::now(), self.token, |_| {}, |_| {});
        self.hold.wait_entered();
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    Read,
    Write,
    Flush,
}
#[derive(Default)]
struct IoCalls {
    read: AtomicUsize,
    write: AtomicUsize,
    vectored: AtomicUsize,
    flush: AtomicUsize,
}
enum ReadResult {
    Eof,
    Error { bytes: Vec<u8>, code: i32 },
}
#[derive(Clone, Copy)]
enum WriteResult {
    Zero,
    Error(i32),
}
struct ScriptedIo<'a> {
    deadline: Option<StartDeadline<'a>>,
    trigger: Operation,
    read: Option<ReadResult>,
    write: WriteResult,
    calls: Arc<IoCalls>,
}
impl<'a> ScriptedIo<'a> {
    fn new(
        fixture: &'a HeldMonitor,
        trigger: Operation,
        read: ReadResult,
        write: WriteResult,
    ) -> (Self, Arc<IoCalls>) {
        let calls = Arc::new(IoCalls::default());
        (
            Self {
                deadline: Some(fixture.deadline()),
                trigger,
                read: Some(read),
                write,
                calls: calls.clone(),
            },
            calls,
        )
    }
    fn fire(&mut self, operation: Operation) {
        if self.trigger == operation
            && let Some(deadline) = self.deadline.take()
        {
            deadline.fire();
        }
    }
    fn write_result(&self) -> Poll<io::Result<usize>> {
        Poll::Ready(match self.write {
            WriteResult::Zero => Ok(0),
            WriteResult::Error(code) => Err(io::Error::from_raw_os_error(code)),
        })
    }
}
impl AsyncRead for ScriptedIo<'_> {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.calls.read.fetch_add(1, Ordering::SeqCst);
        this.fire(Operation::Read);
        Poll::Ready(match this.read.take().unwrap_or(ReadResult::Eof) {
            ReadResult::Eof => Ok(()),
            ReadResult::Error { bytes, code } => {
                buffer.put_slice(&bytes);
                Err(io::Error::from_raw_os_error(code))
            }
        })
    }
}
impl AsyncWrite for ScriptedIo<'_> {
    fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, _: &[u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.calls.write.fetch_add(1, Ordering::SeqCst);
        this.fire(Operation::Write);
        this.write_result()
    }
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.calls.vectored.fetch_add(1, Ordering::SeqCst);
        this.fire(Operation::Write);
        this.write_result()
    }
    fn is_write_vectored(&self) -> bool {
        true
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.calls.flush.fetch_add(1, Ordering::SeqCst);
        this.fire(Operation::Flush);
        Poll::Ready(Err(io::Error::from_raw_os_error(75)))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
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
fn counting_context() -> (Arc<CountWake>, Waker) {
    let count = Arc::new(CountWake::default());
    let waker = Waker::from(count.clone());
    (count, waker)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn held_monitor_deadline_hides_eof_from_the_same_physical_poll() {
    let fixture = HeldMonitor::new();
    let (inner, calls) = ScriptedIo::new(
        &fixture,
        Operation::Read,
        ReadResult::Eof,
        WriteResult::Zero,
    );
    let mut stream = TerminalIo::new(inner, Some(fixture.handle()));
    let (_, waker) = counting_context();
    let mut cx = Context::from_waker(&waker);
    let mut bytes = [0; 8];
    let mut buffer = ReadBuf::new(&mut bytes);
    assert!(
        Pin::new(&mut stream)
            .poll_read(&mut cx, &mut buffer)
            .is_pending()
    );
    assert!(buffer.filled().is_empty());
    assert_eq!(calls.read.load(Ordering::SeqCst), 1);
    fixture.assert_pending();
    assert!(
        Pin::new(&mut stream)
            .poll_read(&mut cx, &mut buffer)
            .is_pending()
    );
    assert_eq!(
        calls.read.load(Ordering::SeqCst),
        1,
        "EOF was re-polled while held"
    );
    {
        let failure = fixture
            .connection
            .as_ref()
            .unwrap()
            .claim_connection_failure();
        tokio::pin!(failure);
        assert!(std::future::Future::poll(failure.as_mut(), &mut cx).is_pending());
    }
    fixture.settle().await;
    assert!(matches!(
        Pin::new(&mut stream).poll_read(&mut cx, &mut buffer),
        Poll::Ready(Ok(()))
    ));
    assert!(buffer.filled().is_empty());
    assert_eq!(calls.read.load(Ordering::SeqCst), 1);
    drop(stream);
    fixture.finish(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_error_progress_is_hidden_then_replayed_into_smaller_and_zero_buffers() {
    let fixture = HeldMonitor::new();
    let (inner, calls) = ScriptedIo::new(
        &fixture,
        Operation::Read,
        ReadResult::Error {
            bytes: b"abcdefg".to_vec(),
            code: 71,
        },
        WriteResult::Zero,
    );
    let mut stream = TerminalIo::new(inner, Some(fixture.handle()));
    let (_, waker) = counting_context();
    let mut cx = Context::from_waker(&waker);
    let mut storage = [0; 16];
    let mut original = ReadBuf::new(&mut storage);
    original.put_slice(b"pre");
    assert!(
        Pin::new(&mut stream)
            .poll_read(&mut cx, &mut original)
            .is_pending()
    );
    assert_eq!(
        original.filled(),
        b"pre",
        "Pending exposed new ReadBuf bytes"
    );
    fixture.assert_pending();
    fixture.settle().await;
    let mut empty = [];
    let mut zero = ReadBuf::new(&mut empty);
    assert!(matches!(
        Pin::new(&mut stream).poll_read(&mut cx, &mut zero),
        Poll::Ready(Ok(()))
    ));
    assert!(zero.filled().is_empty());
    let mut collected = Vec::new();
    for _ in 0..2 {
        let mut small = [0; 3];
        let mut buffer = ReadBuf::new(&mut small);
        assert!(matches!(
            Pin::new(&mut stream).poll_read(&mut cx, &mut buffer),
            Poll::Ready(Ok(()))
        ));
        collected.extend_from_slice(buffer.filled());
        let mut empty = [];
        let mut zero = ReadBuf::new(&mut empty);
        assert!(matches!(
            Pin::new(&mut stream).poll_read(&mut cx, &mut zero),
            Poll::Ready(Ok(()))
        ));
    }
    let mut last = [0; 4];
    let mut buffer = ReadBuf::new(&mut last);
    buffer.put_slice(b"x");
    let Poll::Ready(Err(error)) = Pin::new(&mut stream).poll_read(&mut cx, &mut buffer) else {
        panic!("original held error was not returned with the final byte")
    };
    assert_eq!(error.raw_os_error(), Some(71));
    assert_eq!(buffer.filled(), b"xg");
    collected.extend_from_slice(&buffer.filled()[1..]);
    assert_eq!(collected, b"abcdefg");
    assert_eq!(
        calls.read.load(Ordering::SeqCst),
        1,
        "replay polled the physical reader"
    );
    let mut next = [0; 1];
    let mut next = ReadBuf::new(&mut next);
    assert!(matches!(
        Pin::new(&mut stream).poll_read(&mut cx, &mut next),
        Poll::Ready(Ok(()))
    ));
    assert!(next.filled().is_empty());
    assert_eq!(
        calls.read.load(Ordering::SeqCst),
        2,
        "error was replayed twice"
    );
    drop(stream);
    fixture.finish(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_dispatch_wait_wakes_distinct_reader_and_writer_tasks() {
    let fixture = HeldMonitor::new();
    let (inner, calls) = ScriptedIo::new(
        &fixture,
        Operation::Read,
        ReadResult::Eof,
        WriteResult::Zero,
    );
    let mut stream = TerminalIo::new(inner, Some(fixture.handle()));
    let (reader, reader_waker) = counting_context();
    let (writer, writer_waker) = counting_context();
    let mut reader_cx = Context::from_waker(&reader_waker);
    let mut writer_cx = Context::from_waker(&writer_waker);
    let mut storage = [0; 8];
    let mut buffer = ReadBuf::new(&mut storage);
    assert!(
        Pin::new(&mut stream)
            .poll_read(&mut reader_cx, &mut buffer)
            .is_pending()
    );
    assert!(
        Pin::new(&mut stream)
            .poll_write(&mut writer_cx, b"request")
            .is_pending()
    );
    assert_eq!(calls.read.load(Ordering::SeqCst), 1);
    assert_eq!(
        calls.write.load(Ordering::SeqCst),
        0,
        "writer bypassed a pending dispatch"
    );
    fixture.assert_pending();
    fixture.settle().await;
    crate::bounded(async {
        while reader.0.load(Ordering::SeqCst) == 0 || writer.0.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        reader.0.load(Ordering::SeqCst) > 0,
        "reader wake was replaced"
    );
    assert!(
        writer.0.load(Ordering::SeqCst) > 0,
        "writer wake was replaced"
    );
    assert!(matches!(
        Pin::new(&mut stream).poll_read(&mut reader_cx, &mut buffer),
        Poll::Ready(Ok(()))
    ));
    assert!(matches!(
        Pin::new(&mut stream).poll_write(&mut writer_cx, b"request"),
        Poll::Ready(Ok(0))
    ));
    assert_eq!(calls.read.load(Ordering::SeqCst), 1);
    assert_eq!(calls.write.load(Ordering::SeqCst), 1);
    drop(stream);
    fixture.finish(false).await;
}

#[derive(Clone, Copy, Debug)]
enum WriteCase {
    ScalarZero,
    ScalarError,
    VectoredZero,
    VectoredError,
    FlushError,
}
fn poll_write_case(
    stream: &mut TerminalIo<ScriptedIo<'_>>,
    cx: &mut Context<'_>,
    case: WriteCase,
) -> Poll<io::Result<usize>> {
    match case {
        WriteCase::ScalarZero | WriteCase::ScalarError => Pin::new(stream).poll_write(cx, b"abc"),
        WriteCase::VectoredZero | WriteCase::VectoredError => {
            Pin::new(stream).poll_write_vectored(cx, &[IoSlice::new(b"a"), IoSlice::new(b"bc")])
        }
        WriteCase::FlushError => Pin::new(stream)
            .poll_flush(cx)
            .map(|result| result.map(|()| 0)),
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_zero_errors_and_flush_errors_wait_without_repolling_inner() {
    for case in [
        WriteCase::ScalarZero,
        WriteCase::ScalarError,
        WriteCase::VectoredZero,
        WriteCase::VectoredError,
        WriteCase::FlushError,
    ] {
        let fixture = HeldMonitor::new();
        let trigger = if matches!(case, WriteCase::FlushError) {
            Operation::Flush
        } else {
            Operation::Write
        };
        let result = if matches!(case, WriteCase::ScalarError | WriteCase::VectoredError) {
            WriteResult::Error(74)
        } else {
            WriteResult::Zero
        };
        let (inner, calls) = ScriptedIo::new(&fixture, trigger, ReadResult::Eof, result);
        let mut stream = TerminalIo::new(inner, Some(fixture.handle()));
        let (_, waker) = counting_context();
        let mut cx = Context::from_waker(&waker);
        assert!(
            poll_write_case(&mut stream, &mut cx, case).is_pending(),
            "{case:?}"
        );
        fixture.assert_pending();
        assert!(
            poll_write_case(&mut stream, &mut cx, case).is_pending(),
            "{case:?}"
        );
        let count = || {
            calls.write.load(Ordering::SeqCst)
                + calls.vectored.load(Ordering::SeqCst)
                + calls.flush.load(Ordering::SeqCst)
        };
        assert_eq!(count(), 1, "{case:?} repeated physical IO while held");
        fixture.settle().await;
        let ready = poll_write_case(&mut stream, &mut cx, case);
        match case {
            WriteCase::ScalarZero | WriteCase::VectoredZero => {
                assert!(matches!(ready, Poll::Ready(Ok(0))))
            }
            WriteCase::ScalarError | WriteCase::VectoredError | WriteCase::FlushError => {
                let Poll::Ready(Err(error)) = ready else {
                    panic!("lost original {case:?} error")
                };
                assert_eq!(
                    error.raw_os_error(),
                    Some(if matches!(case, WriteCase::FlushError) {
                        75
                    } else {
                        74
                    })
                );
            }
        }
        assert_eq!(
            count(),
            1,
            "{case:?} reproduced its held result by re-polling IO"
        );
        assert_eq!(fixture.probe.writer_snapshot().written_bytes, 0);
        drop(stream);
        fixture.finish(false).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_artificial_partial_error_is_preserved_but_explicitly_unqualified() {
    let fixture = HeldMonitor::new();
    let payload = vec![0x6d; MAX_HELD_TERMINAL_READ_BYTES + 1];
    let (inner, calls) = ScriptedIo::new(
        &fixture,
        Operation::Read,
        ReadResult::Error {
            bytes: payload.clone(),
            code: 71,
        },
        WriteResult::Zero,
    );
    let mut stream = TerminalIo::new(inner, Some(fixture.handle()));
    let (_, waker) = counting_context();
    let mut cx = Context::from_waker(&waker);
    let mut storage = vec![0; payload.len() + 1];
    let mut buffer = ReadBuf::new(&mut storage);
    buffer.put_slice(b"p");
    let Poll::Ready(Err(error)) = Pin::new(&mut stream).poll_read(&mut cx, &mut buffer) else {
        panic!("oversized artificial progress must not allocate a hidden held copy")
    };
    assert_eq!(error.raw_os_error(), Some(71));
    assert_eq!(buffer.filled()[0], b'p');
    assert_eq!(&buffer.filled()[1..], payload);
    assert!(
        fixture.probe.observer_failed(),
        "unqualified bypass was reported clean"
    );
    assert_eq!(calls.read.load(Ordering::SeqCst), 1);
    fixture.assert_pending();
    fixture.settle().await;
    drop(stream);
    fixture.finish(true).await;
}
