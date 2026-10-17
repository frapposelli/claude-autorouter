//! Synchronous, bounded controls for encoder-input and flush-poll observations.
//! No timer, socket, executor task, or application completion authority is used.
use super::*;
use crate::body::Frame;
use crate::ext::{
    NodeHttpBodyHandoff, NodeHttpBodyHandoffEvent as Event, NodeHttpFlushPoll as Flush,
};
use crate::proto::h1::ServerTransaction;
use crate::rt::ReadBufCursor;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::io;
use std::sync::{Arc, Mutex};
use std::task::Waker;

#[derive(Clone, Debug, PartialEq)]
enum Observation {
    Event(&'static str, Event),
    Write(usize),
    Flush,
}
type Trace = Arc<Mutex<Vec<Observation>>>;
#[derive(Clone, Copy, Debug)]
enum Mode {
    Ready,
    WritePending,
    FlushPending,
    WriteError,
    FlushError,
}
struct IoState {
    input: VecDeque<u8>,
    output: Vec<u8>,
    mode: Mode,
}
struct TestIo {
    state: Arc<Mutex<IoState>>,
    trace: Trace,
}
impl Read for TestIo {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        mut buf: ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        let mut state = self.state.lock().unwrap();
        if state.input.is_empty() {
            return Poll::Pending;
        }
        let len = buf.remaining().min(state.input.len());
        let bytes: Vec<_> = state.input.drain(..len).collect();
        buf.put_slice(&bytes);
        Poll::Ready(Ok(()))
    }
}
impl Write for TestIo {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut state = self.state.lock().unwrap();
        self.trace
            .lock()
            .unwrap()
            .push(Observation::Write(bytes.len()));
        match state.mode {
            Mode::WritePending => Poll::Pending,
            Mode::WriteError => Poll::Ready(Err(io::ErrorKind::BrokenPipe.into())),
            _ => {
                state.output.extend_from_slice(bytes);
                Poll::Ready(Ok(bytes.len()))
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.trace.lock().unwrap().push(Observation::Flush);
        match self.state.lock().unwrap().mode {
            Mode::FlushPending => Poll::Pending,
            Mode::FlushError => Poll::Ready(Err(io::ErrorKind::BrokenPipe.into())),
            _ => Poll::Ready(Ok(())),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
#[derive(Default)]
struct BodyState {
    frames: VecDeque<Frame<Bytes>>,
    eof: bool,
    polls: usize,
    dropped: bool,
}
struct TestBody(Arc<Mutex<BodyState>>);
impl Body for TestBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let mut state = self.0.lock().unwrap();
        state.polls += 1;
        if let Some(frame) = state.frames.pop_front() {
            Poll::Ready(Some(Ok(frame)))
        } else if state.eof {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }
    fn is_end_stream(&self) -> bool {
        let state = self.0.lock().unwrap();
        state.eof && state.frames.is_empty()
    }
}
impl Drop for TestBody {
    fn drop(&mut self) {
        self.0.lock().unwrap().dropped = true;
    }
}
type ResponseSlot = Arc<Mutex<Option<http::Response<TestBody>>>>;
struct ResponseFuture(ResponseSlot);
impl Future for ResponseFuture {
    type Output = Result<http::Response<TestBody>, Infallible>;
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        self.0
            .lock()
            .unwrap()
            .take()
            .map_or(Poll::Pending, |response| Poll::Ready(Ok(response)))
    }
}
struct TestService(Mutex<VecDeque<ResponseSlot>>);
impl crate::service::Service<Request<IncomingBody>> for TestService {
    type Response = http::Response<TestBody>;
    type Error = Infallible;
    type Future = ResponseFuture;
    fn call(&self, _: Request<IncomingBody>) -> Self::Future {
        ResponseFuture(self.0.lock().unwrap().pop_front().unwrap())
    }
}
type Driver = Dispatcher<Server<TestService, IncomingBody>, TestBody, TestIo, ServerTransaction>;
struct Harness {
    driver: Driver,
    io: Arc<Mutex<IoState>>,
    trace: Trace,
}
impl Harness {
    fn new(requests: &[u8], slots: Vec<ResponseSlot>, trace: Trace) -> Self {
        let io = Arc::new(Mutex::new(IoState {
            input: requests.iter().copied().collect(),
            output: Vec::new(),
            mode: Mode::Ready,
        }));
        let mut conn = Conn::new(TestIo {
            state: io.clone(),
            trace: trace.clone(),
        });
        conn.disable_date_header();
        conn.set_flush_pipeline(false);
        Self {
            driver: Dispatcher::new(Server::new(TestService(Mutex::new(slots.into()))), conn),
            io,
            trace,
        }
    }
    fn prepare(&mut self) {
        let mut cx = Context::from_waker(Waker::noop());
        // At most two complete requests are used by these controls.
        for _ in 0..4 {
            assert!(!matches!(self.driver.poll_read(&mut cx), Poll::Ready(Err(_))));
        }
        self.driver.dispatch.poll_pending(&mut cx).unwrap();
        self.observe();
    }
    fn observe(&self) {
        self.driver
            .dispatch
            .observe_pending(self.driver.body_rx.is_none() && self.driver.conn.can_write_head());
    }
    fn write(&mut self) -> Poll<crate::Result<()>> {
        self.driver
            .poll_write(&mut Context::from_waker(Waker::noop()))
    }
    fn flush(&mut self) -> Poll<crate::Result<()>> {
        self.driver
            .poll_flush(&mut Context::from_waker(Waker::noop()))
    }
    fn events(&self, name: &str) -> Vec<Event> {
        self.trace
            .lock()
            .unwrap()
            .iter()
            .filter_map(|row| match row {
                Observation::Event(label, event) if *label == name => Some(*event),
                _ => None,
            })
            .collect()
    }
}
fn response(
    name: Option<&'static str>,
    trace: &Trace,
    chunks: &[&'static [u8]],
    eof: bool,
) -> (http::Response<TestBody>, Arc<Mutex<BodyState>>) {
    let body = Arc::new(Mutex::new(BodyState {
        frames: chunks
            .iter()
            .map(|b| Frame::data(Bytes::from_static(b)))
            .collect(),
        eof,
        ..BodyState::default()
    }));
    let mut response = http::Response::new(TestBody(body.clone()));
    if let Some(name) = name {
        let trace = trace.clone();
        response
            .extensions_mut()
            .insert(NodeHttpBodyHandoff::new(move |event| {
                trace.lock().unwrap().push(Observation::Event(name, event))
            }));
    }
    (response, body)
}
fn slot(response: http::Response<TestBody>) -> ResponseSlot {
    Arc::new(Mutex::new(Some(response)))
}
fn single(
    name: Option<&'static str>,
    chunks: &[&'static [u8]],
    eof: bool,
) -> (Harness, Arc<Mutex<BodyState>>) {
    let trace = Trace::default();
    let (response, body) = response(name, &trace, chunks, eof);
    (
        Harness::new(
            b"GET / HTTP/1.1\r\nhost: synthetic.invalid\r\n\r\n",
            vec![slot(response)],
            trace,
        ),
        body,
    )
}
fn ready_ok(result: Poll<crate::Result<()>>) {
    assert!(matches!(result, Poll::Ready(Ok(()))), "{result:?}");
}

#[test]
fn encoder_submission_precedes_io_and_only_flush_observes_the_attempt() {
    let (mut h, body) = single(Some("A"), &[b"abc"], false);
    h.prepare();
    assert!(h.write().is_pending());
    assert_eq!(
        h.events("A"),
        vec![
            Event::Active,
            Event::DataSubmitted {
                input_bytes: 3,
                total_input_bytes: 3
            }
        ]
    );
    assert!(h.io.lock().unwrap().output.is_empty());
    assert_eq!(body.lock().unwrap().polls, 2);
    ready_ok(h.flush());
    let trace = h.trace.lock().unwrap();
    let submitted = trace
        .iter()
        .position(|r| matches!(r, Observation::Event(_, Event::DataSubmitted { .. })))
        .unwrap();
    let write = trace
        .iter()
        .position(|r| matches!(r, Observation::Write(_)))
        .unwrap();
    let flushed = trace
        .iter()
        .position(|r| {
            matches!(
                r,
                Observation::Event(
                    _,
                    Event::FlushPolled {
                        total_input_bytes: 3,
                        outcome: Flush::Ready
                    }
                )
            )
        })
        .unwrap();
    assert!(submitted < write && write < flushed);
}

#[test]
fn actual_write_pending_and_underlying_flush_pending_are_distinct() {
    for mode in [Mode::WritePending, Mode::FlushPending] {
        let (mut h, _) = single(Some("A"), &[b"abc"], false);
        h.prepare();
        assert!(h.write().is_pending());
        h.io.lock().unwrap().mode = mode;
        assert!(h.flush().is_pending());
        assert_eq!(
            h.events("A").last(),
            Some(&Event::FlushPolled {
                total_input_bytes: 3,
                outcome: Flush::Pending
            })
        );
        assert_eq!(
            h.io.lock().unwrap().output.is_empty(),
            matches!(mode, Mode::WritePending)
        );
        assert_eq!(
            h.trace.lock().unwrap().contains(&Observation::Flush),
            matches!(mode, Mode::FlushPending)
        );
        h.io.lock().unwrap().mode = Mode::Ready;
        ready_ok(h.flush());
        assert_eq!(
            h.events("A").last(),
            Some(&Event::FlushPolled {
                total_input_bytes: 3,
                outcome: Flush::Ready
            })
        );
    }
}

#[test]
fn write_and_flush_errors_are_observed_as_failure() {
    for mode in [Mode::WriteError, Mode::FlushError] {
        let (mut h, _) = single(Some("A"), &[b"abc"], false);
        h.prepare();
        assert!(h.write().is_pending());
        h.io.lock().unwrap().mode = mode;
        assert!(matches!(h.flush(), Poll::Ready(Err(_))));
        assert_eq!(
            h.events("A").last(),
            Some(&Event::FlushPolled {
                total_input_bytes: 3,
                outcome: Flush::Failed
            })
        );
    }
}

#[test]
fn empty_data_does_not_count_and_subsequent_frames_accumulate() {
    let (mut h, body) = single(Some("A"), &[b"", b"ab", b"", b"c"], false);
    h.prepare();
    assert!(h.write().is_pending());
    ready_ok(h.flush());
    assert_eq!(
        h.events("A"),
        vec![
            Event::Active,
            Event::DataSubmitted {
                input_bytes: 2,
                total_input_bytes: 2
            },
            Event::DataSubmitted {
                input_bytes: 1,
                total_input_bytes: 3
            },
            Event::FlushPolled {
                total_input_bytes: 3,
                outcome: Flush::Ready
            }
        ]
    );
    body.lock()
        .unwrap()
        .frames
        .push_back(Frame::data(Bytes::from_static(b"de")));
    assert!(h.write().is_pending());
    ready_ok(h.flush());
    assert_eq!(
        h.events("A").last(),
        Some(&Event::FlushPolled {
            total_input_bytes: 5,
            outcome: Flush::Ready
        })
    );
}

#[test]
fn no_extension_has_identical_wire_output_and_no_counter() {
    let (mut observed, _) = single(Some("A"), &[b"abc"], true);
    let (mut ordinary, _) = single(None, &[b"abc"], true);
    for h in [&mut observed, &mut ordinary] {
        h.prepare();
        assert!(h.write().is_pending());
        ready_ok(h.flush());
    }
    assert_eq!(
        observed.io.lock().unwrap().output,
        ordinary.io.lock().unwrap().output
    );
    assert!(ordinary.driver.body_handoff.is_none());
    assert!(ordinary.events("A").is_empty());
}

#[test]
fn header_only_head_and_bodyless_statuses_never_submit_data() {
    for (method, status, empty) in [
        ("GET", 200, true),
        ("HEAD", 200, false),
        ("GET", 204, false),
        ("GET", 304, false),
    ] {
        let trace = Trace::default();
        let (mut response, body) = response(
            Some("A"),
            &trace,
            if empty { &[] } else { &[b"abc"] },
            empty,
        );
        *response.status_mut() = http::StatusCode::from_u16(status).unwrap();
        let mut h = Harness::new(
            format!("{method} / HTTP/1.1\r\nhost: synthetic.invalid\r\n\r\n").as_bytes(),
            vec![slot(response)],
            trace,
        );
        h.prepare();
        assert!(h.write().is_pending());
        ready_ok(h.flush());
        ready_ok(h.flush());
        assert_eq!(body.lock().unwrap().polls, 0);
        assert_eq!(
            h.events("A"),
            vec![
                Event::Active,
                Event::FlushPolled {
                    total_input_bytes: 0,
                    outcome: Flush::Ready
                },
                Event::FlushPolled {
                    total_input_bytes: 0,
                    outcome: Flush::Ready
                }
            ]
        );
    }
}

#[test]
fn final_data_and_trailers_do_not_invent_additional_input() {
    for trailers in [false, true] {
        let (mut h, body) = single(Some("A"), &[b"abc"], !trailers);
        if trailers {
            body.lock()
                .unwrap()
                .frames
                .push_back(Frame::trailers(http::HeaderMap::new()));
        }
        h.prepare();
        assert!(h.write().is_pending());
        ready_ok(h.flush());
        assert!(body.lock().unwrap().dropped);
        assert_eq!(
            h.events("A"),
            vec![
                Event::Active,
                Event::DataSubmitted {
                    input_bytes: 3,
                    total_input_bytes: 3
                },
                Event::FlushPolled {
                    total_input_bytes: 3,
                    outcome: Flush::Ready
                }
            ]
        );
    }
}

#[test]
fn fixed_length_clipping_is_truthfully_reported_as_input_not_wire_bytes() {
    let trace = Trace::default();
    let (mut response, _) = response(Some("A"), &trace, &[b"abcde"], false);
    response.headers_mut().insert(
        http::header::CONTENT_LENGTH,
        http::HeaderValue::from_static("3"),
    );
    let mut h = Harness::new(
        b"GET / HTTP/1.1\r\nhost: synthetic.invalid\r\n\r\n",
        vec![slot(response)],
        trace,
    );
    h.prepare();
    assert!(h.write().is_pending());
    ready_ok(h.flush());
    assert!(h.events("A").contains(&Event::DataSubmitted {
        input_bytes: 5,
        total_input_bytes: 5
    }));
    let output = h.io.lock().unwrap().output.clone();
    assert!(output.ends_with(b"\r\n\r\nabc"));
    assert!(!output.ends_with(b"abcde"));
}

#[test]
fn pipeline_flush_ready_can_skip_the_physical_writer() {
    let trace = Trace::default();
    let (response, _) = response(Some("A"), &trace, &[b"abc"], false);
    let mut h = Harness::new(
        b"GET / HTTP/1.1\r\nhost: synthetic.invalid\r\n\r\nG",
        vec![slot(response)],
        trace,
    );
    h.driver.conn.set_flush_pipeline(true);
    h.prepare();
    assert!(h.write().is_pending());
    ready_ok(h.flush());
    assert_eq!(
        h.events("A").last(),
        Some(&Event::FlushPolled {
            total_input_bytes: 3,
            outcome: Flush::Ready
        })
    );
    assert!(h.io.lock().unwrap().output.is_empty());
    assert!(!h
        .trace
        .lock()
        .unwrap()
        .iter()
        .any(|r| matches!(r, Observation::Write(_) | Observation::Flush)));
}

#[test]
fn queued_b_at_front_never_inherits_active_a_input_or_flush() {
    let trace = Trace::default();
    let (a, a_body) = response(Some("A"), &trace, &[b"abc"], false);
    let (b, _) = response(Some("B"), &trace, &[b"de"], true);
    let mut h=Harness::new(b"GET /a HTTP/1.1\r\nhost: synthetic.invalid\r\n\r\nGET /b HTTP/1.1\r\nhost: synthetic.invalid\r\n\r\n",vec![slot(a),slot(b)],trace);
    h.prepare();
    assert!(h.events("A").is_empty());
    assert_eq!(h.events("B"), vec![Event::Queued]);
    assert!(h.write().is_pending());
    assert_eq!(h.driver.dispatch.in_flight.len(), 1);
    h.observe();
    ready_ok(h.flush());
    assert_eq!(h.events("B"), vec![Event::Queued, Event::Queued]);
    a_body.lock().unwrap().eof = true;
    assert!(h.write().is_pending());
    ready_ok(h.flush());
    h.observe();
    assert!(h.write().is_pending());
    ready_ok(h.flush());
    let b_events = h.events("B");
    assert!(b_events.contains(&Event::Active));
    assert!(b_events.contains(&Event::DataSubmitted {
        input_bytes: 2,
        total_input_bytes: 2
    }));
    assert!(b_events.contains(&Event::FlushPolled {
        total_input_bytes: 2,
        outcome: Flush::Ready
    }));
    assert!(!b_events.contains(&Event::FlushPolled {
        total_input_bytes: 3,
        outcome: Flush::Ready
    }));
    assert_eq!(
        h.events("A")
            .iter()
            .filter(|e| matches!(e, Event::Active))
            .count(),
        1
    );
}

#[test]
fn completed_b_behind_unfinished_service_is_queued_but_not_active() {
    let trace = Trace::default();
    let pending = Arc::new(Mutex::new(None));
    let (b, _) = response(Some("B"), &trace, &[b"b"], true);
    let mut h=Harness::new(b"GET /a HTTP/1.1\r\nhost: synthetic.invalid\r\n\r\nGET /b HTTP/1.1\r\nhost: synthetic.invalid\r\n\r\n",vec![pending,slot(b)],trace);
    h.prepare();
    assert!(h.write().is_pending());
    ready_ok(h.flush());
    assert_eq!(h.events("B"), vec![Event::Queued]);
    assert!(h.driver.body_handoff.is_none());
}

#[test]
fn observer_lingers_at_idle_but_next_unobserved_head_clears_it() {
    let trace = Trace::default();
    let (a, _) = response(Some("A"), &trace, &[b"a"], true);
    let (b, _) = response(None, &trace, &[b"b"], true);
    let mut h = Harness::new(
        b"GET /a HTTP/1.1\r\nhost: synthetic.invalid\r\n\r\n",
        vec![slot(a), slot(b)],
        trace,
    );
    h.prepare();
    assert!(h.write().is_pending());
    ready_ok(h.flush());
    ready_ok(h.flush());
    let old = h.events("A");
    assert_eq!(
        old.iter()
            .filter(|e| matches!(
                e,
                Event::FlushPolled {
                    total_input_bytes: 1,
                    ..
                }
            ))
            .count(),
        2
    );
    h.io.lock()
        .unwrap()
        .input
        .extend(b"GET /b HTTP/1.1\r\nhost: synthetic.invalid\r\n\r\n");
    h.prepare();
    assert!(h.write().is_pending());
    ready_ok(h.flush());
    assert!(h.driver.body_handoff.is_none());
    assert_eq!(h.events("A"), old);
}

#[test]
fn checked_overflow_terminates_serving_without_a_flush_event() {
    let (mut h, body) = single(Some("A"), &[], false);
    h.prepare();
    assert!(h.write().is_pending());
    h.driver.body_handoff.as_mut().unwrap().1 = u64::MAX;
    body.lock()
        .unwrap()
        .frames
        .push_back(Frame::data(Bytes::from_static(b"x")));
    let result = Pin::new(&mut h.driver).poll(&mut Context::from_waker(Waker::noop()));
    assert!(matches!(result, Poll::Ready(Err(_))));
    assert_eq!(h.events("A"), vec![Event::Active]);
    assert!(h.io.lock().unwrap().output.is_empty());
    drop(h);
    assert!(body.lock().unwrap().dropped);
}
