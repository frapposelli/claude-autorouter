// Included at item scope inside transport_completion::tests, reusing FakeIo.
// No policy event or timeout is fabricated: requests use the actual parser.

#[derive(Clone, Copy, Debug)]
enum ReceiveWriteMode {
    Healthy,
    Pending,
    PartialSeven,
    Error,
}
struct ReceiveTrace {
    armed: bool,
    mode: ReceiveWriteMode,
    writes: Vec<Vec<u8>>,
    callbacks_at_write: Vec<Vec<(usize, Delivery)>>,
    flushes: usize,
    shutdowns: usize,
    drops: usize,
}
impl ReceiveTrace {
    fn new(mode: ReceiveWriteMode) -> Self {
        Self {
            armed: false,
            mode,
            writes: Vec::new(),
            callbacks_at_write: Vec::new(),
            flushes: 0,
            shutdowns: 0,
            drops: 0,
        }
    }
}
struct ReceiveIo {
    inner: FakeIo,
    trace: Arc<Mutex<ReceiveTrace>>,
    outcomes: Outcomes,
}
impl AsyncRead for ReceiveIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, out)
    }
}
impl AsyncWrite for ReceiveIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let mode = {
            let mut trace = this.trace.lock().unwrap();
            if !trace.armed {
                None
            } else {
                assert!(trace.writes.len() < 4);
                trace.writes.push(bytes.to_vec());
                trace
                    .callbacks_at_write
                    .push(this.outcomes.lock().unwrap().clone());
                Some(trace.mode)
            }
        };
        match mode {
            None => Pin::new(&mut this.inner).poll_write(cx, bytes),
            Some(ReceiveWriteMode::Healthy) => {
                this.inner.0.lock().unwrap().output.extend_from_slice(bytes);
                Poll::Ready(Ok(bytes.len()))
            }
            Some(ReceiveWriteMode::Pending) => Poll::Pending,
            Some(ReceiveWriteMode::PartialSeven) => {
                let count = 7.min(bytes.len());
                this.inner
                    .0
                    .lock()
                    .unwrap()
                    .output
                    .extend_from_slice(&bytes[..count]);
                Poll::Ready(Ok(count))
            }
            Some(ReceiveWriteMode::Error) => Poll::Ready(Err(io::ErrorKind::BrokenPipe.into())),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.trace.lock().unwrap().armed {
            this.trace.lock().unwrap().flushes += 1;
            return Poll::Pending; // It must never be awaited by timeout teardown.
        }
        Pin::new(&mut this.inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.trace.lock().unwrap().armed {
            this.trace.lock().unwrap().shutdowns += 1;
            return Poll::Pending; // It must never be awaited by timeout teardown.
        }
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}
impl Drop for ReceiveIo {
    fn drop(&mut self) {
        self.trace.lock().unwrap().drops += 1;
    }
}

#[derive(Default)]
struct ReceiveBodyTrace {
    polls: AtomicUsize,
    stalled: AtomicUsize,
    drops: AtomicUsize,
}
struct ReceiveBody {
    data: Option<Bytes>,
    hold: bool,
    trace: Arc<ReceiveBodyTrace>,
}
impl Body for ReceiveBody {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let this = self.get_mut();
        this.trace.polls.fetch_add(1, Ordering::SeqCst);
        if let Some(data) = this.data.take() {
            return Poll::Ready(Some(Ok(Frame::data(data))));
        }
        if this.hold {
            this.trace.stalled.fetch_add(1, Ordering::SeqCst);
            return Poll::Pending;
        }
        Poll::Ready(None)
    }
}
impl Drop for ReceiveBody {
    fn drop(&mut self) {
        self.trace.drops.fetch_add(1, Ordering::SeqCst);
    }
}

type ReceiveDriver = Pin<Box<dyn Future<Output = Result<(), hyper::Error>> + Send>>;
struct ReceiveRun {
    io: FakeIo,
    trace: Arc<Mutex<ReceiveTrace>>,
    outcomes: Outcomes,
    calls: Arc<AtomicUsize>,
    markers: Arc<Mutex<Vec<hyper::ext::NodeHttpResponseTiming>>>,
    bodies: Vec<Arc<ReceiveBodyTrace>>,
    registry: CompletionRegistry,
    scans: crate::http_receive::ScanRegistry,
    request_parent: tokio_util::sync::CancellationToken,
    driver: Option<ReceiveDriver>,
    terminal: bool,
    release: Option<tokio::sync::oneshot::Sender<()>>,
    origin: Instant,
}
impl ReceiveRun {
    fn new(mode: ReceiveWriteMode, headed: bool) -> Self {
        let io = FakeIo::request(0);
        let trace = Arc::new(Mutex::new(ReceiveTrace::new(mode)));
        let outcomes = Outcomes::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let markers = Arc::new(Mutex::new(Vec::new()));
        let bodies = (0..3)
            .map(|_| Arc::new(ReceiveBodyTrace::default()))
            .collect::<Vec<_>>();
        let registry = CompletionRegistry::new(8);
        let scans = crate::http_receive::ScanRegistry::new();
        let request_parent = tokio_util::sync::CancellationToken::new();
        let registration = scans.register(registry.clone(), request_parent.clone());
        let control = registration.control();
        let (release, wait) = tokio::sync::oneshot::channel();
        let wait = Arc::new(Mutex::new(Some(wait)));
        let observed = outcomes.clone();
        let counted = calls.clone();
        let prepared = markers.clone();
        let tracked = registry.clone();
        let body_traces = bodies.clone();
        let service = service_fn(move |request: Request<Incoming>| {
            let index = counted.fetch_add(1, Ordering::SeqCst);
            assert!(index < 3);
            let marker = request
                .extensions()
                .get::<hyper::ext::NodeHttpResponseTiming>()
                .expect("actual parser response timing extension")
                .clone();
            prepared.lock().unwrap().push(marker.clone());
            let current_body = headed && index == 1;
            let response = Response::builder()
                .status(200)
                .header("x-receive-case", ["old", "current", "queued"][index])
                .body(ReceiveBody {
                    data: current_body.then(|| Bytes::from_static(b"PROVIDER_PREFIX")),
                    hold: current_body,
                    trace: body_traces[index].clone(),
                })
                .unwrap();
            let callback = observed.clone();
            let response = if request.method() == hyper::Method::HEAD {
                tracked
                    .track_empty_response(response, move |value| {
                        callback.lock().unwrap().push((index, value))
                    })
                    .unwrap()
            } else {
                tracked
                    .track(response, move |value| {
                        callback.lock().unwrap().push((index, value))
                    })
                    .unwrap()
            };
            let wait = if !headed && index == 1 {
                wait.lock().unwrap().take()
            } else {
                None
            };
            if index != 1 {
                // This actual handler performs the equivalent of local HEAD
                // res.end. A queued prepared response must not replace current.
                marker.mark_prepared();
            }
            async move {
                if let Some(wait) = wait {
                    wait.await.expect("owned late-service release");
                    marker.mark_prepared(); // Deliberately ready AFTER expiry.
                }
                Ok::<_, Infallible>(response)
            }
        });
        let stream = ReceiveIo {
            inner: io.clone(),
            trace: trace.clone(),
            outcomes: outcomes.clone(),
        };
        let run_registry = registry.clone();
        let driver = Box::pin(async move {
            let result = serve_http1_scanned(stream, run_registry, service, control).await;
            drop(registration);
            result
        });
        Self {
            io,
            trace,
            outcomes,
            calls,
            markers,
            bodies,
            registry,
            scans,
            request_parent,
            driver: Some(driver),
            terminal: false,
            release: Some(release),
            origin: Instant::now(),
        }
    }
    fn poll(&mut self) {
        if self.terminal {
            return;
        }
        let mut cx = Context::from_waker(Waker::noop());
        for _ in 0..64 {
            if self
                .driver
                .as_mut()
                .unwrap()
                .as_mut()
                .poll(&mut cx)
                .is_ready()
            {
                self.terminal = true;
                break;
            }
        }
    }
    fn send(&mut self, bytes: &[u8]) {
        let wake = {
            let mut s = self.io.0.lock().unwrap();
            assert!(s.input.is_empty());
            s.input.extend(bytes.iter().copied());
            s.read_waker.take()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
        self.poll();
        assert!(self.io.0.lock().unwrap().input.is_empty());
    }
    async fn at(&mut self, millis: u64) {
        let target = self.origin + Duration::from_millis(millis);
        assert!(Instant::now() <= target);
        tokio::time::advance(target - Instant::now()).await;
        assert_eq!(Instant::now(), target);
        self.poll();
    }
    fn wire(&self) -> Vec<u8> {
        self.io.0.lock().unwrap().output.clone()
    }
    fn sorted_outcomes(&self) -> Vec<(usize, Delivery)> {
        let mut rows = self.outcomes.lock().unwrap().clone();
        rows.sort_by_key(|x| x.0);
        rows
    }
    fn establish_prior_flush(&mut self) -> Vec<u8> {
        self.send(b"HEAD /old HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(!self.terminal);
        assert_eq!(self.calls.load(Ordering::SeqCst), 1);
        assert_eq!(self.registry.pending(), 0);
        assert_eq!(self.sorted_outcomes(), [(0, Delivery::Flushed)]);
        assert!(self.markers.lock().unwrap()[0].is_prepared());
        let wire = self.wire();
        assert!(wire.starts_with(b"HTTP/1.1 200 "));
        assert!(
            wire.windows(b"x-receive-case: old".len())
                .any(|x| x == b"x-receive-case: old")
        );
        assert!(wire.ends_with(b"\r\n\r\n"));
        wire
    }
    fn assert_two_pending(&self, headed: bool) {
        assert_eq!(
            self.calls.load(Ordering::SeqCst),
            3,
            "partial fourth head must not be admitted"
        );
        assert!(!self.terminal);
        assert_eq!(self.registry.pending(), 2);
        assert_eq!(self.sorted_outcomes(), [(0, Delivery::Flushed)]);
        let markers = self.markers.lock().unwrap();
        assert_eq!(markers.len(), 3);
        assert_eq!(markers[1].is_prepared(), headed);
        assert!(
            markers[2].is_prepared(),
            "queued handler already ended its HEAD response"
        );
        let state = self.registry.lock();
        assert_eq!(state.records.len(), 2);
        assert!(state.records[1].eof && !state.records[1].submitted);
        if !headed {
            assert!(state.records[0].eof && !state.records[0].submitted);
        } else {
            assert!(!state.records[0].eof && state.records[0].submitted);
        }
    }
    fn finish(mut self) {
        assert!(
            self.terminal,
            "timeout must not await permanently blocked write/flush/shutdown"
        );
        drop(self.driver.take());
        assert_eq!(self.scans.len(), 0);
        assert_eq!(self.registry.pending(), 0);
        assert!(self.request_parent.is_cancelled());
        assert_eq!(self.trace.lock().unwrap().drops, 1);
        for body in &self.bodies {
            assert_eq!(body.drops.load(Ordering::SeqCst), 1);
        }
        assert_eq!(self.sorted_outcomes(), receive_expected_outcomes());
        assert_eq!(self.trace.lock().unwrap().flushes, 0);
        assert_eq!(self.trace.lock().unwrap().shutdowns, 0);
    }
}
impl Drop for ReceiveRun {
    fn drop(&mut self) {
        self.request_parent.cancel();
        drop(self.driver.take());
    }
}
fn receive_expected_outcomes() -> Vec<(usize, Delivery)> {
    vec![
        (0, Delivery::Flushed),
        (1, Delivery::Failed(Failure::Deadline)),
        (2, Delivery::Failed(Failure::Deadline)),
    ]
}
const RECEIVE_RAW408: &[u8] = b"HTTP/1.1 408 Request Timeout\r\nConnection: close\r\n\r\n";
const RECEIVE_UNSUBMITTED:&[u8]=b"HEAD /current HTTP/1.1\r\nHost: localhost\r\n\r\nHEAD /queued HTTP/1.1\r\nHost: localhost\r\n\r\nGET /partial HTTP/1.1\r\nHost:";
const RECEIVE_HEADED:&[u8]=b"GET /current HTTP/1.1\r\nHost: localhost\r\n\r\nHEAD /queued HTTP/1.1\r\nHost: localhost\r\n\r\nGET /partial HTTP/1.1\r\nHost:";

#[tokio::test(start_paused = true)]
async fn receive_timeout_fails_queued_eof_records_before_one_best_effort_raw_write() {
    for mode in [
        ReceiveWriteMode::Healthy,
        ReceiveWriteMode::Pending,
        ReceiveWriteMode::PartialSeven,
        ReceiveWriteMode::Error,
    ] {
        let mut run = ReceiveRun::new(mode, false);
        let old = run.establish_prior_flush();
        run.at(1000).await;
        run.send(RECEIVE_UNSUBMITTED);
        run.assert_two_pending(false);
        assert_eq!(run.wire(), old);
        run.at(30000).await;
        run.trace.lock().unwrap().armed = true;
        run.scans.scan(Instant::now());
        // Callback failure linearizes BEFORE the actual raw write and before
        // a now-ready service can prepare/submit the held response.
        assert_eq!(run.sorted_outcomes(), receive_expected_outcomes());
        run.release
            .take()
            .unwrap()
            .send(())
            .expect("held actual service future still owned before driver resumes");
        run.poll();
        assert!(run.terminal);
        let trace = run.trace.lock().unwrap();
        assert_eq!(trace.writes, vec![RECEIVE_RAW408.to_vec()]);
        assert_eq!(trace.callbacks_at_write.len(), 1);
        let mut observed = trace.callbacks_at_write[0].clone();
        observed.sort_by_key(|x| x.0);
        assert_eq!(observed, receive_expected_outcomes());
        drop(trace);
        let suffix = match mode {
            ReceiveWriteMode::Healthy => RECEIVE_RAW408,
            ReceiveWriteMode::PartialSeven => &RECEIVE_RAW408[..7],
            _ => &[],
        };
        assert_eq!(run.wire(), [old, suffix.to_vec()].concat());
        run.finish();
    }
}

#[tokio::test(start_paused = true)]
async fn receive_timeout_does_not_append_raw408_to_a_prepared_current_provider_response() {
    let mut run = ReceiveRun::new(ReceiveWriteMode::Pending, true);
    run.establish_prior_flush();
    run.at(1000).await;
    run.send(RECEIVE_HEADED);
    run.assert_two_pending(true);
    assert!(run.bodies[1].polls.load(Ordering::SeqCst) >= 2);
    assert!(run.bodies[1].stalled.load(Ordering::SeqCst) > 0);
    let wire = run.wire();
    assert!(
        wire.windows(b"PROVIDER_PREFIX".len())
            .any(|x| x == b"PROVIDER_PREFIX")
    );
    run.at(30000).await;
    run.trace.lock().unwrap().armed = true;
    run.scans.scan(Instant::now());
    assert_eq!(run.sorted_outcomes(), receive_expected_outcomes());
    run.poll();
    assert!(run.terminal);
    assert_eq!(run.wire(), wire);
    assert!(
        run.trace.lock().unwrap().writes.is_empty(),
        "prepared current response suppresses raw408 injection"
    );
    run.finish();
}
