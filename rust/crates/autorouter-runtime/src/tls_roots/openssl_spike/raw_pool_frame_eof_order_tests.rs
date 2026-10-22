//! Scripted-IO causal controls, not real TLS or raw-pool parity evidence.
//! Sampling Incoming consumes a ready frame; retain exact synthetic bytes.
//! No driver/IO re-entry, timer, spawn, retry, unsafe, pool or policy change.
use super::*;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::AtomicBool;
use std::task::Wake;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

const HEAD_COMPLETE: &[u8] =
    b"HTTP/1.1 200 OK\r\ncontent-length: 1\r\nconnection: keep-alive\r\n\r\n";
const HEAD_SHORT: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: keep-alive\r\n\r\n";
const ENTITY: &[u8] = b"x";

#[derive(Clone, Default)]
struct Rows(Arc<Mutex<Vec<Trace>>>, Arc<AtomicBool>);
impl Rows {
    fn push(&self, event: &'static str, mut row: Trace) {
        row["event"] = event.into();
        let mut rows = self.0.lock().unwrap();
        if rows.len() >= 64 {
            self.1.store(true, Ordering::SeqCst);
            panic!("mock boundary row bound");
        }
        row["sequence"] = rows.len().into();
        rows.push(row);
    }
    // Drop diagnostics must not create a second panic after a failed assertion.
    // The failure bit is checked on normal success and reported on any unwind.
    fn push_on_drop(&self, event: &'static str) {
        let appended = catch_unwind(AssertUnwindSafe(|| {
            let mut rows = match self.0.lock() {
                Ok(rows) => rows,
                Err(poisoned) => {
                    self.1.store(true, Ordering::SeqCst);
                    poisoned.into_inner()
                }
            };
            if rows.len() >= 64 {
                return false;
            }
            let row = serde_json::json!({"event":event,"sequence":rows.len()});
            rows.push(row);
            true
        }))
        .unwrap_or(false);
        if !appended {
            self.1.store(true, Ordering::SeqCst);
        }
    }
    fn snapshot(&self) -> Vec<Trace> {
        self.0.lock().unwrap().clone()
    }
}
struct CountWake(Arc<AtomicUsize>);
impl Wake for CountWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        let previous = self.0.fetch_add(1, Ordering::SeqCst);
        // On an existing assertion unwind, retain the invalid scalar instead of
        // introducing a second panic from a driver/channel Drop notification.
        if !std::thread::panicking() {
            assert!(previous < 128, "mock aggregate wake bound");
        }
    }
}
fn waker(calls: &Arc<AtomicUsize>) -> Waker {
    Waker::from(Arc::new(CountWake(calls.clone())))
}
// Own only diagnostic rows and scalar metadata, never body/driver/sender/IO.
// Construct before setup; keep last in Fixture so unwinding cleanup is visible.
struct ReportGuard {
    rows: Rows,
    wakes: Arc<AtomicUsize>,
    short: bool,
    terminal: Terminal,
    emitted: bool,
}
impl ReportGuard {
    fn emit(&mut self, requested_complete: bool) -> bool {
        if self.emitted {
            return false;
        }
        self.emitted = true;
        let attempted = catch_unwind(AssertUnwindSafe(|| {
            let unwinding = std::thread::panicking();
            let (rows, poisoned) = match self.rows.0.lock() {
                Ok(rows) => (rows.clone(), false),
                Err(poisoned) => (poisoned.into_inner().clone(), true),
            };
            let wake_calls = self.wakes.load(Ordering::SeqCst);
            let complete = requested_complete
                && !unwinding
                && !poisoned
                && !self.rows.1.load(Ordering::SeqCst)
                && rows.len() <= 64
                && wake_calls <= 128;
            let bytes = serde_json::to_vec(&serde_json::json!({
                "kind":"scripted_client_frame_eof_mock", "complete":complete,
                "unwinding":unwinding, "rows_poisoned":poisoned,
                "row_drop_or_bound_failure":self.rows.1.load(Ordering::SeqCst),
                "short_content_length":self.short,
                "terminal_kind":match self.terminal { Terminal::Eof => "eof", Terminal::ReadError => "read_error" },
                "wake_calls":wake_calls, "rows":rows,
            })).ok()?;
            if bytes.len().checked_add(1)? > 16_384 {
                return None;
            }
            Some((bytes, complete))
        }));
        // Fixed fallback is well below the same 16 KiB cap. No retry/replay of a
        // partially written report: exactly one chosen output buffer is tried.
        let (mut bytes, complete) = match attempted {
            Ok(Some(report)) => report,
            _ => (br#"{"kind":"scripted_client_frame_eof_mock","complete":false,"reason":"diagnostic_snapshot_encoding_or_byte_bound_failed","rows":[]}"#.to_vec(), false),
        };
        bytes.push(b'\n');
        let wrote = catch_unwind(AssertUnwindSafe(|| {
            let stderr = io::stderr();
            let mut sink = stderr.lock();
            let mut offset = 0;
            for _ in 0..64 {
                if offset == bytes.len() {
                    return true;
                }
                match io::Write::write(&mut sink, &bytes[offset..]) {
                    Ok(0) | Err(_) => return false,
                    Ok(count) => offset += count,
                }
            }
            offset == bytes.len()
        }))
        .unwrap_or(false);
        complete && wrote
    }
}
impl Drop for ReportGuard {
    fn drop(&mut self) {
        if !self.emitted {
            let _ = self.emit(false);
        }
    }
}
fn once<F: Future + ?Sized>(future: Pin<&mut F>, wake: &Waker) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(wake))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
enum Sample {
    Missing,
    Pending,
    Data { bytes: usize, ended: bool },
    End,
    Error,
}
#[derive(Default)]
struct BodyState {
    incoming: Option<Pin<Box<Incoming>>>,
    payload: Vec<Bytes>,
    polls: usize,
    errors: usize,
}
// The test is the sole strong owner. IO has only Weak<BodySlot> and never owns
// an OwnedBody, completion callback, pool lease, reservation or RequestTerminal.
struct BodySlot {
    state: Mutex<BodyState>,
    wake: Waker,
    rows: Rows,
}
impl BodySlot {
    fn sample(&self, stage: &'static str) -> Sample {
        let mut state = self.state.lock().unwrap();
        state.polls += 1;
        assert!(state.polls <= 5, "mock body poll bound");
        let (outcome, data) = match state.incoming.as_mut() {
            None => (Sample::Missing, None),
            Some(body) => match body
                .as_mut()
                .poll_frame(&mut Context::from_waker(&self.wake))
            {
                Poll::Pending => (Sample::Pending, None),
                Poll::Ready(None) => (Sample::End, None),
                Poll::Ready(Some(Err(_))) => (Sample::Error, None),
                Poll::Ready(Some(Ok(frame))) => {
                    let data = frame.into_data().expect("fixture has no trailers");
                    let ended = body.is_end_stream();
                    assert!(data.len() <= ENTITY.len(), "mock payload bound");
                    (
                        Sample::Data {
                            bytes: data.len(),
                            ended,
                        },
                        Some(data),
                    )
                }
            },
        };
        if outcome == Sample::Error {
            state.errors += 1;
        }
        if let Some(data) = data {
            assert!(state.payload.len() < 2, "mock frame retention bound");
            state.payload.push(data);
        }
        drop(state);
        self.rows.push(
            "incoming_sample",
            serde_json::json!({"stage":stage,"outcome":outcome}),
        );
        outcome
    }
    fn discard_incoming(&self) {
        let body = self.state.lock().unwrap().incoming.take();
        if let Some(body) = body {
            drop(body);
            self.rows.push("incoming_dropped", serde_json::json!({}));
        }
    }
}
#[derive(Default)]
struct PermitState {
    head: bool,
    body: bool,
    terminal: bool,
    wake: Option<Waker>,
}
struct Permits {
    state: Mutex<PermitState>,
    rows: Rows,
}
impl Permits {
    // Explicit mock-peer stimulus, after the fixed write phase validates the
    // actual request terminator. This is not a claim about real IO scheduling.
    fn release_head(&self, complete_at: usize) {
        assert!((4..=16_384).contains(&complete_at));
        let wake = {
            let mut state = self.state.lock().unwrap();
            assert!(!state.head, "response head released exactly once");
            state.head = true;
            state.wake.take()
        };
        self.rows.push(
            "script_response_head_released",
            serde_json::json!({
                "complete_at_request_byte":complete_at
            }),
        );
        if let Some(wake) = wake {
            wake.wake();
        }
    }
    fn head_ready(&self, cx: &Context<'_>) -> bool {
        let mut state = self.state.lock().unwrap();
        if !state.head {
            state.wake = Some(cx.waker().clone());
        }
        state.head
    }
    fn set(&self, body: bool, terminal: bool) {
        let wake = {
            let mut state = self.state.lock().unwrap();
            state.body = body;
            state.terminal = terminal;
            state.wake.take()
        };
        self.rows.push(
            "script_readiness",
            serde_json::json!({"body":body,"terminal":terminal}),
        );
        if let Some(wake) = wake {
            wake.wake();
        }
    }
    fn ready(&self, cx: &Context<'_>, terminal: bool) -> bool {
        let mut state = self.state.lock().unwrap();
        let ready = if terminal { state.terminal } else { state.body };
        if !ready {
            state.wake = Some(cx.waker().clone());
        }
        ready
    }
}
#[derive(Clone, Copy, Debug)]
enum Terminal {
    Eof,
    ReadError,
}
type SenderSlot = Mutex<Option<SendRequest<Full<Bytes>>>>;
// Scalar progress only. A valid mock peer cannot expose a response head until
// AsyncWrite has actually accepted the complete request head, even across writes.
#[derive(Default)]
struct RequestHeadProgress {
    accepted: AtomicUsize,
    complete_at: AtomicUsize,
    blocked_reads: AtomicUsize,
    response_bytes: AtomicUsize,
}
struct ScriptIo {
    head: &'static [u8],
    head_offset: usize,
    body_sent: bool,
    terminal: Terminal,
    permits: Arc<Permits>,
    body: Weak<BodySlot>,
    sender: Weak<SenderSlot>,
    rows: Rows,
    reads: Arc<AtomicUsize>,
    terminals: Arc<AtomicUsize>,
    live: Arc<AtomicUsize>,
    in_read: Arc<AtomicBool>,
    written: usize,
    request_head_tail: u32,
    request_head: Arc<RequestHeadProgress>,
}
impl ScriptIo {
    fn sample_terminal(&self) {
        self.rows.push(
            "io_terminal_begin",
            serde_json::json!({"kind":format!("{:?}",self.terminal)}),
        );
        let flags = {
            let owner = self.sender.upgrade().unwrap();
            let locked = owner.lock().unwrap();
            let sender = locked.as_ref().unwrap();
            (sender.is_ready(), sender.is_closed())
        };
        assert!(
            !flags.1,
            "sample precedes dispatcher reaction to actual IO terminal result"
        );
        let sample = self
            .body
            .upgrade()
            .unwrap()
            .sample("inside_actual_terminal_read");
        self.rows.push(
            "io_terminal_return",
            serde_json::json!({"kind":format!("{:?}",self.terminal),
            "body":sample,"sender_ready":flags.0,"sender_closed":flags.1}),
        );
    }
}
impl AsyncRead for ScriptIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        assert!(
            !this.in_read.swap(true, Ordering::SeqCst),
            "mock nested IO read"
        );
        struct ReadGuard(Arc<AtomicBool>);
        impl Drop for ReadGuard {
            fn drop(&mut self) {
                assert!(self.0.swap(false, Ordering::SeqCst));
            }
        }
        let _read = ReadGuard(this.in_read.clone());
        assert!(
            this.reads.fetch_add(1, Ordering::SeqCst) < 24,
            "mock IO read bound"
        );
        if this.head_offset < this.head.len() {
            let released = this.permits.head_ready(cx);
            let complete = this.request_head.complete_at.load(Ordering::SeqCst) != 0;
            if !complete || !released {
                this.request_head
                    .blocked_reads
                    .fetch_add(1, Ordering::SeqCst);
                this.rows.push(
                    "io_response_head_waits_for_request",
                    serde_json::json!({
                        "accepted_request_bytes":this.written,
                        "request_head_complete":complete,"response_head_released":released
                    }),
                );
                return Poll::Pending;
            }
            let bytes = output.remaining().min(this.head.len() - this.head_offset);
            output.put_slice(&this.head[this.head_offset..this.head_offset + bytes]);
            this.head_offset += bytes;
            this.request_head
                .response_bytes
                .store(this.head_offset, Ordering::SeqCst);
            this.rows
                .push("io_head_bytes", serde_json::json!({"bytes":bytes}));
            return Poll::Ready(Ok(()));
        }
        if !this.body_sent {
            if !this.permits.ready(cx, false) {
                return Poll::Pending;
            }
            output.put_slice(ENTITY);
            this.body_sent = true;
            this.rows
                .push("io_body_bytes", serde_json::json!({"bytes":ENTITY.len()}));
            return Poll::Ready(Ok(()));
        }
        if !this.permits.ready(cx, true) {
            return Poll::Pending;
        }
        let terminal = this.terminals.fetch_add(1, Ordering::SeqCst);
        assert_eq!(terminal, 0, "one actual terminal read in fixed script");
        this.sample_terminal();
        match this.terminal {
            Terminal::Eof => Poll::Ready(Ok(())),
            Terminal::ReadError => {
                Poll::Ready(Err(io::Error::from(io::ErrorKind::ConnectionReset)))
            }
        }
    }
}
impl AsyncWrite for ScriptIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let previous_written = self.written;
        self.written = self.written.checked_add(bytes.len()).unwrap();
        assert!(self.written <= 16_384, "mock request write bound");
        self.request_head
            .accepted
            .store(self.written, Ordering::SeqCst);
        let mut newly_complete = None;
        if self.request_head.complete_at.load(Ordering::SeqCst) == 0 {
            for (offset, byte) in bytes.iter().enumerate() {
                // Only the last four bytes survive this call; no request bytes
                // or user payload are retained in the mock or diagnostic rows.
                self.request_head_tail = (self.request_head_tail << 8) | u32::from(*byte);
                if self.request_head_tail == 0x0d0a0d0a {
                    let complete_at = previous_written + offset + 1;
                    self.request_head
                        .complete_at
                        .store(complete_at, Ordering::SeqCst);
                    newly_complete = Some(complete_at);
                    break;
                }
            }
        }
        if let Some(complete_at) = newly_complete {
            self.rows.push(
                "io_request_head_accepted",
                serde_json::json!({
                    "complete_at_request_byte":complete_at,"accepted_request_bytes":self.written
                }),
            );
            // Completion alone does not make response bytes ready. The fixture
            // validates this accepted header, then releases its mock peer head.
        }
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.rows.push("io_shutdown", serde_json::json!({}));
        Poll::Ready(Ok(()))
    }
}
impl Drop for ScriptIo {
    fn drop(&mut self) {
        let previous = self.live.fetch_sub(1, Ordering::SeqCst);
        self.rows.push_on_drop("io_dropped");
        if previous != 1 {
            self.rows.1.store(true, Ordering::SeqCst);
            if !std::thread::panicking() {
                assert_eq!(previous, 1);
            }
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DriverPoll {
    Pending,
    Complete,
    Error,
}
type ClientDriver = Pin<Box<dyn Future<Output = Result<(), hyper::Error>> + Send>>;
struct Fixture {
    driver: Option<ClientDriver>,
    sender: Arc<SenderSlot>,
    body: Arc<BodySlot>,
    permits: Arc<Permits>,
    wake: Waker,
    rows: Rows,
    live: Arc<AtomicUsize>,
    terminals: Arc<AtomicUsize>,
    driver_polls: usize,
    request_head: Arc<RequestHeadProgress>,
    report: ReportGuard,
}
impl Fixture {
    fn new(short: bool, terminal: Terminal) -> Self {
        let rows = Rows::default();
        let wake_calls = Arc::new(AtomicUsize::default());
        let report = ReportGuard {
            rows: rows.clone(),
            wakes: wake_calls.clone(),
            short,
            terminal,
            emitted: false,
        };
        let wake = waker(&wake_calls);
        let body = Arc::new(BodySlot {
            state: Mutex::new(BodyState::default()),
            wake: waker(&wake_calls),
            rows: rows.clone(),
        });
        let sender = Arc::new(Mutex::new(None));
        let permits = Arc::new(Permits {
            state: Mutex::default(),
            rows: rows.clone(),
        });
        let live = Arc::new(AtomicUsize::new(1));
        let terminals = Arc::new(AtomicUsize::default());
        let request_head = Arc::new(RequestHeadProgress::default());
        let io = ScriptIo {
            head: if short { HEAD_SHORT } else { HEAD_COMPLETE },
            head_offset: 0,
            body_sent: false,
            terminal,
            permits: permits.clone(),
            body: Arc::downgrade(&body),
            sender: Arc::downgrade(&sender),
            rows: rows.clone(),
            reads: Arc::default(),
            terminals: terminals.clone(),
            live: live.clone(),
            in_read: Arc::default(),
            written: 0,
            request_head_tail: 0,
            request_head: request_head.clone(),
        };
        let builder = hyper::client::conn::http1::Builder::new();
        let mut handshake =
            Box::pin(builder.handshake::<_, Full<Bytes>>(hyper_util::rt::TokioIo::new(io)));
        let (raw_sender, connection) = match once(handshake.as_mut(), &wake) {
            Poll::Ready(Ok(value)) => value,
            _ => panic!("constructing this HTTP/1 client performs no IO"),
        };
        *sender.lock().unwrap() = Some(raw_sender);
        let mut request = Request::get("/synthetic-frame-eof")
            .header("host", "synthetic")
            .body(Full::new(Bytes::new()))
            .unwrap();
        request
            .extensions_mut()
            .insert(hyper::ext::NodeHttpResponsePolicy);
        let mut response = Box::pin(
            sender
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .send_request(request),
        );
        let mut fixture = Self {
            driver: Some(Box::pin(connection)),
            sender,
            body,
            permits,
            wake,
            rows,
            live,
            terminals,
            driver_polls: 0,
            request_head,
            report,
        };
        assert_eq!(
            fixture.drive("write_request_before_response_head"),
            DriverPoll::Pending
        );
        let accepted = fixture.request_head.accepted.load(Ordering::SeqCst);
        let complete_at = fixture.request_head.complete_at.load(Ordering::SeqCst);
        assert!(
            (4..=16_384).contains(&complete_at),
            "actual request terminator was accepted"
        );
        assert_eq!(accepted, complete_at, "this GET has no request body");
        assert!(
            fixture.request_head.blocked_reads.load(Ordering::SeqCst) > 0,
            "response was unavailable before actual request head completion"
        );
        assert_eq!(
            fixture.request_head.response_bytes.load(Ordering::SeqCst),
            0,
            "first setup poll accepted request but exposed no unsolicited response"
        );
        assert_eq!(fixture.terminals.load(Ordering::SeqCst), 0);
        fixture.permits.release_head(complete_at);
        assert_eq!(fixture.drive("head_only"), DriverPoll::Pending);
        assert_eq!(
            fixture.request_head.response_bytes.load(Ordering::SeqCst),
            if short {
                HEAD_SHORT.len()
            } else {
                HEAD_COMPLETE.len()
            }
        );
        let response = match once(response.as_mut(), &fixture.wake) {
            Poll::Ready(Ok(value)) => value,
            _ => panic!("head-only planned poll must acquire actual Incoming"),
        };
        assert_eq!(response.status(), hyper::StatusCode::OK);
        assert!(!response.body().is_end_stream());
        fixture.body.state.lock().unwrap().incoming = Some(Box::pin(response.into_body()));
        fixture
            .rows
            .push("response_head_acquired", serde_json::json!({}));
        fixture
    }
    fn drive(&mut self, stage: &'static str) -> DriverPoll {
        self.driver_polls += 1;
        assert!(self.driver_polls <= 4, "fixed mock driver poll budget");
        let outcome = match once(self.driver.as_mut().unwrap().as_mut(), &self.wake) {
            Poll::Pending => DriverPoll::Pending,
            Poll::Ready(Ok(())) => DriverPoll::Complete,
            Poll::Ready(Err(_)) => DriverPoll::Error,
        };
        let sender = self.sender.lock().unwrap();
        let sender = sender.as_ref().unwrap();
        self.rows.push(
            "driver_poll_return",
            serde_json::json!({"stage":stage,"outcome":format!("{outcome:?}"),
            "sender_ready":sender.is_ready(),"sender_closed":sender.is_closed()}),
        );
        outcome
    }
    fn boundary_sample(&self) -> Sample {
        let rows = self.rows.snapshot();
        let samples: Vec<_> = rows
            .iter()
            .filter(|row| row["event"] == "io_terminal_return")
            .collect();
        assert_eq!(samples.len(), 1);
        serde_json::from_value(samples[0]["body"].clone()).unwrap()
    }
    fn finish(mut self, expected_data: &[u8], expected_errors: usize) {
        self.body.discard_incoming();
        drop(self.driver.take());
        assert_eq!(
            self.live.load(Ordering::SeqCst),
            0,
            "driver Drop released actual scripted IO"
        );
        let sender = self.sender.lock().unwrap().take().unwrap();
        assert!(
            sender.is_closed(),
            "real dispatch receiver closed after driver disposal"
        );
        drop(sender);
        self.permits.state.lock().unwrap().wake.take();
        assert_eq!(
            Arc::strong_count(&self.body),
            1,
            "IO never owns the pinned body"
        );
        assert_eq!(Arc::strong_count(&self.sender), 1);
        assert_eq!(Arc::strong_count(&self.permits), 1);
        assert_eq!(
            Arc::strong_count(&self.request_head),
            1,
            "IO released scalar setup progress"
        );
        let state = self.body.state.lock().unwrap();
        assert!(state.incoming.is_none());
        assert_eq!(
            state
                .payload
                .iter()
                .flat_map(|bytes| bytes.iter().copied())
                .collect::<Vec<_>>(),
            expected_data
        );
        assert_eq!(state.errors, expected_errors);
        drop(state);
        assert!(
            self.report.emit(true),
            "complete bounded mock diagnostic must be written"
        );
    }
}
#[test]
fn immediate_eof_is_observed_before_clean_final_frame_reaches_incoming() {
    let mut fixture = Fixture::new(false, Terminal::Eof);
    fixture.permits.set(true, true);
    assert_eq!(fixture.drive("complete_body_and_eof"), DriverPoll::Complete);
    assert_eq!(fixture.boundary_sample(), Sample::Pending);
    assert_eq!(
        fixture.body.sample("after_driver_final_frame"),
        Sample::Data {
            bytes: 1,
            ended: true
        }
    );
    assert_eq!(fixture.body.sample("after_driver_body_end"), Sample::End);
    fixture.finish(ENTITY, 0);
}
#[test]
fn body_data_can_be_observed_before_terminal_readiness_is_enabled() {
    let mut fixture = Fixture::new(false, Terminal::Eof);
    fixture.permits.set(true, false);
    assert_eq!(
        fixture.drive("complete_body_without_eof"),
        DriverPoll::Pending
    );
    assert_eq!(fixture.terminals.load(Ordering::SeqCst), 0);
    assert_eq!(
        fixture.body.sample("before_enabling_eof"),
        Sample::Data {
            bytes: 1,
            ended: true
        }
    );
    fixture.permits.set(true, true);
    assert_eq!(fixture.drive("later_eof"), DriverPoll::Complete);
    assert_eq!(fixture.boundary_sample(), Sample::End);
    fixture.finish(ENTITY, 0);
}
#[test]
fn eof_boundary_sampler_can_observe_a_previously_published_unconsumed_frame() {
    let mut fixture = Fixture::new(false, Terminal::Eof);
    fixture.permits.set(true, false);
    assert_eq!(
        fixture.drive("complete_body_without_eof"),
        DriverPoll::Pending
    );
    assert_eq!(fixture.terminals.load(Ordering::SeqCst), 0);
    fixture.permits.set(true, true);
    assert_eq!(
        fixture.drive("later_eof_with_unconsumed_frame"),
        DriverPoll::Complete
    );
    assert_eq!(
        fixture.boundary_sample(),
        Sample::Data {
            bytes: 1,
            ended: true
        }
    );
    assert_eq!(
        fixture.body.sample("after_sample_consumed_data"),
        Sample::End
    );
    fixture.finish(ENTITY, 0);
}
fn short_body(terminal: Terminal) {
    let mut fixture = Fixture::new(true, terminal);
    fixture.permits.set(true, true);
    assert_eq!(fixture.drive("partial_body"), DriverPoll::Pending);
    assert_eq!(
        fixture.body.sample("drain_exact_partial_body"),
        Sample::Data {
            bytes: 1,
            ended: false
        }
    );
    assert_eq!(
        fixture.terminals.load(Ordering::SeqCst),
        0,
        "channel backpressure separates the incomplete body from terminal read"
    );
    assert_ne!(
        fixture.drive("terminal_on_incomplete_body"),
        DriverPoll::Pending
    );
    assert_eq!(fixture.boundary_sample(), Sample::Pending);
    assert_eq!(
        fixture.body.sample("after_incomplete_source_terminal"),
        Sample::Error
    );
    fixture.finish(ENTITY, 1);
}
#[test]
fn short_content_length_retains_original_body_error_on_real_scripted_eof() {
    short_body(Terminal::Eof);
}
#[test]
fn short_content_length_retains_original_body_error_on_real_scripted_read_error() {
    short_body(Terminal::ReadError);
}
#[test]
fn cancelled_incoming_does_not_wait_for_disabled_body_or_eof_readiness() {
    let mut fixture = Fixture::new(false, Terminal::Eof);
    fixture.body.discard_incoming();
    assert_ne!(fixture.drive("cancel_before_body"), DriverPoll::Pending);
    assert_eq!(fixture.terminals.load(Ordering::SeqCst), 0);
    fixture.finish(b"", 0);
}
