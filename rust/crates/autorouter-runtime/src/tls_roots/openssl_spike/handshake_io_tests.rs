use super::*;
use std::{
    collections::VecDeque,
    future::Future,
    sync::{Arc, Mutex},
    task::Waker,
};

use hyper::rt::ReadBuf;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf as TokioReadBuf};

#[derive(Default)]
struct Observed {
    bytes: Vec<u8>,
    events: Vec<&'static str>,
    writes: VecDeque<Step>,
    flush_pending: usize,
    flush_error: Option<io::ErrorKind>,
}
enum Step {
    Pending,
    Count(usize),
    Error(io::ErrorKind),
}
struct Script(Arc<Mutex<Observed>>);
impl AsyncWrite for Script {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut state = self.0.lock().unwrap();
        state.events.push("write");
        let count = match state.writes.pop_front() {
            Some(Step::Pending) => {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            Some(Step::Error(error)) => return Poll::Ready(Err(error.into())),
            Some(Step::Count(count)) => count.min(bytes.len()),
            None => bytes.len(),
        };
        state.bytes.extend_from_slice(&bytes[..count]);
        Poll::Ready(Ok(count))
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut state = self.0.lock().unwrap();
        state.events.push("flush");
        if state.flush_pending > 0 {
            state.flush_pending -= 1;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        Poll::Ready(state.flush_error.map_or(Ok(()), |error| Err(error.into())))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.0.lock().unwrap().events.push("shutdown");
        Poll::Ready(Ok(()))
    }
}
impl AsyncRead for Script {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buffer: &mut TokioReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.0.lock().unwrap().events.push("read");
        buffer.put_slice(&b"peer"[..buffer.remaining().min(4)]);
        Poll::Ready(Ok(()))
    }
}
fn fixture() -> (HandshakeIo<TokioIo<Script>>, Arc<Mutex<Observed>>) {
    let state = Arc::new(Mutex::new(Observed::default()));
    (HandshakeIo::new(TokioIo::new(Script(state.clone()))), state)
}
fn write(io: &mut HandshakeIo<TokioIo<Script>>, bytes: &[u8]) {
    let mut cx = Context::from_waker(Waker::noop());
    assert!(matches!(Pin::new(io).poll_write(&mut cx, bytes),
        Poll::Ready(Ok(count)) if count == bytes.len()));
}

#[test]
fn final_flight_flush_and_drop_do_not_publish_unaccepted_bytes() {
    let (mut io, state) = fixture();
    write(&mut io, b"finished");
    let mut cx = Context::from_waker(Waker::noop());
    assert!(matches!(
        Pin::new(&mut io).poll_flush(&mut cx),
        Poll::Ready(Ok(()))
    ));
    assert_eq!(io.pending, b"finished");
    assert_eq!(io.pending.capacity(), MAX_PENDING);
    drop(io); // Rejected certificate, failed signature or cancelled attempt.
    let state = state.lock().unwrap();
    assert!(state.bytes.is_empty());
    assert!(state.events.is_empty());
}

#[test]
fn preceding_flight_drains_before_read_with_partial_write_and_backpressure() {
    let (mut io, state) = fixture();
    state.lock().unwrap().writes = [Step::Count(2), Step::Pending].into();
    write(&mut io, b"hello");
    let mut cx = Context::from_waker(Waker::noop());
    let mut bytes = [0; 4];
    let mut buffer = ReadBuf::new(&mut bytes);
    assert!(
        Pin::new(&mut io)
            .poll_read(&mut cx, buffer.unfilled())
            .is_pending()
    );
    assert!(buffer.filled().is_empty());
    assert_eq!(io.written, 2);
    assert_eq!(state.lock().unwrap().bytes, b"he");
    assert!(matches!(
        Pin::new(&mut io).poll_read(&mut cx, buffer.unfilled()),
        Poll::Ready(Ok(()))
    ));
    assert_eq!(buffer.filled(), b"peer");
    assert_eq!(state.lock().unwrap().bytes, b"hello");
    assert_eq!(
        state.lock().unwrap().events,
        ["write", "write", "write", "flush", "read"]
    );
    assert!(io.pending.is_empty());
    write(&mut io, b"finished");
    drop(io);
    assert_eq!(state.lock().unwrap().bytes, b"hello");
}

#[test]
fn accepted_flight_waits_for_flush_then_frees_buffer_and_delegates_io() {
    let (mut io, state) = fixture();
    write(&mut io, b"finished");
    state.lock().unwrap().flush_pending = 1;
    let mut cx = Context::from_waker(Waker::noop());
    let mut release = Box::pin(io.release());
    assert!(release.as_mut().poll(&mut cx).is_pending());
    assert!(matches!(
        release.as_mut().poll(&mut cx),
        Poll::Ready(Ok(()))
    ));
    drop(release);
    assert!(!io.handshaking);
    assert_eq!(io.pending.capacity(), 0);
    write(&mut io, b"HTTP");
    let mut bytes = [0; 4];
    let mut buffer = ReadBuf::new(&mut bytes);
    assert!(matches!(
        Pin::new(&mut io).poll_read(&mut cx, buffer.unfilled()),
        Poll::Ready(Ok(()))
    ));
    assert!(matches!(
        Pin::new(&mut io).poll_flush(&mut cx),
        Poll::Ready(Ok(()))
    ));
    assert_eq!(state.lock().unwrap().bytes, b"finishedHTTP");
    assert_eq!(
        state.lock().unwrap().events,
        ["write", "flush", "flush", "write", "read", "flush"]
    );
}

#[test]
fn zero_write_and_write_or_flush_errors_preserve_failure_and_owned_bytes() {
    for (step, flush, expected) in [
        (Some(Step::Count(0)), None, io::ErrorKind::WriteZero),
        (
            Some(Step::Error(io::ErrorKind::ConnectionReset)),
            None,
            io::ErrorKind::ConnectionReset,
        ),
        (
            None,
            Some(io::ErrorKind::BrokenPipe),
            io::ErrorKind::BrokenPipe,
        ),
    ] {
        let (mut io, state) = fixture();
        state.lock().unwrap().writes = step.into_iter().collect();
        state.lock().unwrap().flush_error = flush;
        write(&mut io, b"finished");
        let mut cx = Context::from_waker(Waker::noop());
        let mut release = Box::pin(io.release());
        let Poll::Ready(Err(error)) = release.as_mut().poll(&mut cx) else {
            panic!("release hid an egress failure");
        };
        assert_eq!(error.kind(), expected);
        drop(release);
        assert!(io.handshaking);
        assert_eq!(io.pending, b"finished");
        let before = state.lock().unwrap().events.clone();
        drop(io);
        assert_eq!(state.lock().unwrap().events, before);
    }
}

#[test]
fn cancellation_during_release_and_shutdown_never_flush_remaining_bytes() {
    let (mut io, state) = fixture();
    write(&mut io, b"finished");
    state.lock().unwrap().writes = [Step::Count(2), Step::Pending].into();
    let mut cx = Context::from_waker(Waker::noop());
    let mut release = Box::pin(io.release());
    assert!(release.as_mut().poll(&mut cx).is_pending());
    drop(release);
    assert_eq!(io.written, 2);
    assert!(io.handshaking);
    assert!(matches!(
        Pin::new(&mut io).poll_shutdown(&mut cx),
        Poll::Ready(Ok(()))
    ));
    drop(io);
    assert_eq!(state.lock().unwrap().bytes, b"fi");
    assert_eq!(state.lock().unwrap().events, ["write", "write", "shutdown"]);
}

#[test]
fn bounded_flight_rejects_overflow_without_partial_append_or_implicit_drain() {
    let (mut io, state) = fixture();
    write(&mut io, &vec![42; MAX_PENDING]);
    write(&mut io, b"");
    let mut cx = Context::from_waker(Waker::noop());
    assert!(matches!(
        Pin::new(&mut io).poll_write(&mut cx, b"x"),
        Poll::Ready(Err(_))
    ));
    assert_eq!(io.pending.len(), MAX_PENDING);
    assert_eq!(io.pending.capacity(), MAX_PENDING);
    assert!(state.lock().unwrap().events.is_empty());
    drop(io);
    assert!(state.lock().unwrap().events.is_empty());
}

#[test]
fn zero_capacity_read_keeps_final_flight_without_io_or_peer_progress() {
    let (mut io, state) = fixture();
    write(&mut io, b"finished");
    let mut cx = Context::from_waker(Waker::noop());
    let mut bytes = [];
    let mut buffer = ReadBuf::new(&mut bytes);
    assert!(matches!(
        Pin::new(&mut io).poll_read(&mut cx, buffer.unfilled()),
        Poll::Ready(Ok(()))
    ));
    assert_eq!(io.pending, b"finished");
    assert_eq!(io.written, 0);
    assert!(state.lock().unwrap().events.is_empty());
    drop(io);
    assert!(state.lock().unwrap().events.is_empty());
}
