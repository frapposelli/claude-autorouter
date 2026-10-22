//! Scripted lower-adapter contracts, not real TLS or application parity.
use super::*;
use hyper::rt::ReadBuf;
use std::{
    collections::VecDeque,
    future::Future,
    sync::{
        Weak,
        atomic::{AtomicUsize, Ordering},
    },
};

enum WriteStep {
    Accept(usize),
    Pending,
    Error(io::Error),
    Oversized(usize),
}
enum Step {
    Pending,
    Error(io::Error),
}
#[derive(Default)]
struct Observed {
    output: Vec<u8>,
    incoming: VecDeque<u8>,
    events: Vec<&'static str>,
    writes: VecDeque<WriteStep>,
    flushes: VecDeque<Step>,
    shutdowns: VecDeque<Step>,
    read_waiter: Option<Waker>,
    write_waiter: Option<Waker>,
    drops: usize,
}
struct Script(Arc<Mutex<Observed>>);
impl Drop for Script {
    fn drop(&mut self) {
        self.0.lock().unwrap().drops += 1;
    }
}
impl Read for Script {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buffer: ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        let next = cx.waker().clone();
        let (result, old) = {
            let mut state = self.0.lock().unwrap();
            state.events.push("read");
            if state.incoming.is_empty() {
                (Poll::Pending, state.read_waiter.replace(next))
            } else {
                while buffer.remaining() != 0 {
                    let Some(byte) = state.incoming.pop_front() else {
                        break;
                    };
                    buffer.put_slice(&[byte]);
                }
                (Poll::Ready(Ok(())), Some(next))
            }
        };
        drop(old);
        result
    }
}
impl Write for Script {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let next = cx.waker().clone();
        let (result, old) = {
            let mut state = self.0.lock().unwrap();
            state.events.push("write");
            match state.writes.pop_front() {
                Some(WriteStep::Pending) => (Poll::Pending, state.write_waiter.replace(next)),
                Some(WriteStep::Error(error)) => (Poll::Ready(Err(error)), Some(next)),
                Some(WriteStep::Oversized(count)) => (Poll::Ready(Ok(count)), Some(next)),
                step => {
                    let count = match step {
                        Some(WriteStep::Accept(count)) => count.min(bytes.len()),
                        None => bytes.len(),
                        _ => unreachable!(),
                    };
                    state.output.extend_from_slice(&bytes[..count]);
                    (Poll::Ready(Ok(count)), Some(next))
                }
            }
        };
        drop(old);
        result
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let next = cx.waker().clone();
        let (result, old) = {
            let mut state = self.0.lock().unwrap();
            state.events.push("flush");
            match state.flushes.pop_front() {
                Some(Step::Pending) => (Poll::Pending, state.write_waiter.replace(next)),
                Some(Step::Error(error)) => (Poll::Ready(Err(error)), Some(next)),
                None => (Poll::Ready(Ok(())), Some(next)),
            }
        };
        drop(old);
        result
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let next = cx.waker().clone();
        let (result, old) = {
            let mut state = self.0.lock().unwrap();
            state.events.push("shutdown");
            match state.shutdowns.pop_front() {
                Some(Step::Pending) => (Poll::Pending, state.write_waiter.replace(next)),
                Some(Step::Error(error)) => (Poll::Ready(Err(error)), Some(next)),
                None => (Poll::Ready(Ok(())), Some(next)),
            }
        };
        drop(old);
        result
    }
}

fn fixture() -> (HandshakeIo<Script>, Arc<Mutex<Observed>>) {
    let state = Arc::new(Mutex::new(Observed::default()));
    let mut io = HandshakeIo::new(Script(state.clone()));
    let mut release = Box::pin(io.release());
    assert!(matches!(
        release.as_mut().poll(&mut cx()),
        Poll::Ready(Ok(()))
    ));
    drop(release);
    assert!(!io.handshaking);
    assert_eq!(state.lock().unwrap().events, ["flush"]);
    state.lock().unwrap().events.clear();
    io.enable_raw_egress();
    (io, state)
}

fn cx() -> Context<'static> {
    Context::from_waker(Waker::noop())
}

fn write(io: &mut HandshakeIo<Script>, bytes: &[u8]) {
    assert!(matches!(Pin::new(io).poll_write(&mut cx(), bytes),
        Poll::Ready(Ok(count)) if count == bytes.len()));
}

#[derive(Debug)]
struct Payload(Arc<()>);
impl std::fmt::Display for Payload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("synthetic raw egress payload")
    }
}
impl std::error::Error for Payload {}
fn same_payload(error: &io::Error, expected: &Arc<()>) {
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert!(error.raw_os_error().is_none());
    let actual = error
        .get_ref()
        .and_then(|e| e.downcast_ref::<Payload>())
        .unwrap();
    assert!(Arc::ptr_eq(&actual.0, expected));
}

struct WakeProbe {
    readiness: Weak<Readiness>,
    wakes: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
}
impl WakeProbe {
    fn unlocked(&self) {
        if let Some(readiness) = self.readiness.upgrade() {
            assert!(
                readiness.0.try_lock().is_ok(),
                "waker callback under readiness mutex"
            );
        }
    }
}
impl Wake for WakeProbe {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.unlocked();
        self.wakes.fetch_add(1, Ordering::SeqCst);
    }
}
impl Drop for WakeProbe {
    fn drop(&mut self) {
        self.unlocked();
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}
fn probe(io: &HandshakeIo<Script>) -> (Waker, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let wakes = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let waker = Waker::from(Arc::new(WakeProbe {
        readiness: Arc::downgrade(&io.raw_egress.as_ref().unwrap().readiness),
        wakes: wakes.clone(),
        drops: drops.clone(),
    }));
    (waker, wakes, drops)
}

#[test]
fn raw_queue_preserves_bound_order_partial_writes_and_error_identity() {
    let (mut io, state) = fixture();
    let original: Vec<u8> = (0..MAX_PENDING).map(|n| (n % 251) as u8).collect();
    let tail = vec![252; 16 * 1024];
    write(&mut io, &original);
    assert_eq!(io.egress_queued_bytes(), MAX_PENDING);
    assert_eq!(io.pending.capacity(), MAX_PENDING);
    assert!(
        state.lock().unwrap().events.is_empty(),
        "BIO acceptance is memory only"
    );
    assert!(matches!(
        Pin::new(&mut io).poll_flush(&mut cx()),
        Poll::Ready(Ok(()))
    ));
    assert!(
        state.lock().unwrap().events.is_empty(),
        "BIO flush is not physical delivery"
    );
    let before = io.egress_queued_bytes();
    assert!(matches!(
        Pin::new(&mut io).poll_write(&mut cx(), &vec![0; MAX_PENDING + 1]),
        Poll::Ready(Err(_))
    ));
    assert_eq!(io.egress_queued_bytes(), before);
    assert!(state.lock().unwrap().events.is_empty());
    state.lock().unwrap().writes = [WriteStep::Pending].into();
    assert!(
        Pin::new(&mut io)
            .poll_write(&mut cx(), b"unaccepted")
            .is_pending()
    );
    assert_eq!(io.egress_queued_bytes(), before);
    state.lock().unwrap().writes = [WriteStep::Accept(tail.len()), WriteStep::Pending].into();
    assert!(io.poll_egress_drain(&mut cx()).is_pending());
    assert_eq!(state.lock().unwrap().output, original[..tail.len()]);
    assert_eq!(io.egress_queued_bytes(), MAX_PENDING - tail.len());
    write(&mut io, &tail);
    assert_eq!(io.egress_queued_bytes(), MAX_PENDING);
    assert_eq!(io.pending.capacity(), MAX_PENDING);
    assert!(matches!(
        io.poll_egress_drain(&mut cx()),
        Poll::Ready(Ok(()))
    ));
    let expected = [original.as_slice(), tail.as_slice()].concat();
    assert_eq!(state.lock().unwrap().output, expected);
    assert_eq!(io.egress_queued_bytes(), 0);
    assert_eq!(io.pending.capacity(), MAX_PENDING);

    write(&mut io, b"error-owned");
    let payload = Arc::new(());
    state.lock().unwrap().writes = [
        WriteStep::Accept(2),
        WriteStep::Error(io::Error::other(Payload(payload.clone()))),
    ]
    .into();
    let Poll::Ready(Err(error)) = io.poll_egress_drain(&mut cx()) else {
        panic!("missing raw error")
    };
    same_payload(&error, &payload);
    assert_eq!(io.egress_queued_bytes(), b"error-owned".len() - 2);
    assert_eq!(&state.lock().unwrap().output[expected.len()..], b"er");
}

#[test]
fn blocked_memory_credit_does_not_block_independent_incoming_bytes() {
    let (mut io, state) = fixture();
    write(&mut io, &vec![1; MAX_PENDING]);
    state.lock().unwrap().writes = [WriteStep::Pending, WriteStep::Pending].into();
    assert!(
        io.poll_egress_credit(&mut cx(), RAW_WRITE_CREDIT)
            .is_pending()
    );
    assert!(io.poll_egress_drain(&mut cx()).is_pending());
    state.lock().unwrap().incoming.extend(b"early");
    let mut bytes = [0; 5];
    let mut read = ReadBuf::new(&mut bytes);
    assert!(matches!(
        Pin::new(&mut io).poll_read(&mut cx(), read.unfilled()),
        Poll::Ready(Ok(()))
    ));
    assert_eq!(read.filled(), b"early");
    assert_eq!(io.egress_queued_bytes(), MAX_PENDING);
    assert!(state.lock().unwrap().output.is_empty());
    assert_eq!(state.lock().unwrap().events, ["write", "write", "read"]);
    // No SSL was called in this lower-adapter control. Actual TLS early413
    // and distinguishing precredit from in-flight SSL have separate owners.
}

#[test]
fn stable_aggregate_preserves_roles_and_wakes_on_memory_progress() {
    let (mut io, state) = fixture();
    let (reader, read_wakes, read_drops) = probe(&io);
    let (writer, write_wakes, write_drops) = probe(&io);
    io.register_egress_waiter(EgressRole::Reader, &Context::from_waker(&reader));
    io.register_egress_waiter(EgressRole::Writer, &Context::from_waker(&writer));
    let mut bytes = [0; 1];
    let mut read = ReadBuf::new(&mut bytes);
    assert!(
        Pin::new(&mut io)
            .poll_read(&mut Context::from_waker(&reader), read.unfilled())
            .is_pending()
    );
    write(&mut io, &vec![2; MAX_PENDING]);
    state.lock().unwrap().writes = [WriteStep::Pending].into();
    assert!(
        io.poll_egress_credit(&mut Context::from_waker(&writer), RAW_WRITE_CREDIT)
            .is_pending()
    );
    let (raw_read, raw_write) = {
        let mut state = state.lock().unwrap();
        (
            state.read_waiter.take().unwrap(),
            state.write_waiter.take().unwrap(),
        )
    };
    assert!(raw_read.will_wake(&raw_write));
    assert!(!raw_read.will_wake(&reader) && !raw_write.will_wake(&writer));
    let before = (
        read_wakes.load(Ordering::SeqCst),
        write_wakes.load(Ordering::SeqCst),
    );
    raw_write.wake_by_ref();
    assert_eq!(read_wakes.load(Ordering::SeqCst), before.0 + 1);
    assert_eq!(write_wakes.load(Ordering::SeqCst), before.1 + 1);

    let (next_reader, next_wakes, next_drops) = probe(&io);
    drop(reader);
    io.register_egress_waiter(EgressRole::Reader, &Context::from_waker(&next_reader));
    assert_eq!(read_drops.load(Ordering::SeqCst), 1);
    let old_read_count = read_wakes.load(Ordering::SeqCst);
    io.register_egress_waiter(EgressRole::Reader, &Context::from_waker(&raw_read));
    let stored = io
        .raw_egress
        .as_ref()
        .unwrap()
        .readiness
        .0
        .lock()
        .unwrap()
        .reader
        .clone();
    assert!(
        !stored
            .as_ref()
            .is_some_and(|stored| stored.will_wake(&raw_read))
    );
    drop(stored);
    raw_read.wake_by_ref();
    assert_eq!(read_wakes.load(Ordering::SeqCst), old_read_count);
    assert_eq!(next_wakes.load(Ordering::SeqCst), 1);

    let before = (
        next_wakes.load(Ordering::SeqCst),
        write_wakes.load(Ordering::SeqCst),
    );
    state.lock().unwrap().writes = [WriteStep::Accept(RAW_WRITE_CREDIT), WriteStep::Pending].into();
    assert!(
        io.poll_egress_drain(&mut Context::from_waker(&next_reader))
            .is_pending()
    );
    assert_eq!(io.egress_queued_bytes(), MAX_PENDING - RAW_WRITE_CREDIT);
    assert_eq!(next_wakes.load(Ordering::SeqCst), before.0 + 1);
    assert_eq!(
        write_wakes.load(Ordering::SeqCst),
        before.1 + 1,
        "credit progress wakes writer without another raw event"
    );
    let before = next_wakes.load(Ordering::SeqCst);
    write(&mut io, b"x");
    assert_eq!(
        next_wakes.load(Ordering::SeqCst),
        before + 1,
        "new cipher bytes wake waiting reader"
    );
    drop(writer);
    io.clear_egress_waiter(EgressRole::Writer);
    assert_eq!(write_drops.load(Ordering::SeqCst), 1);
    drop(next_reader);
    drop(io);
    assert_eq!(next_drops.load(Ordering::SeqCst), 1);
    assert_eq!(state.lock().unwrap().drops, 1);
    // Socket-held aggregate clones retain no caller waker after owner drop.
    raw_read.wake_by_ref();
    raw_write.wake_by_ref();
}

#[test]
fn shutdown_phase_precedes_drain_but_tcp_entry_follows_physical_flush() {
    let (mut io, state) = fixture();
    write(&mut io, b"notify");
    state.lock().unwrap().writes = [WriteStep::Pending].into();
    assert!(Pin::new(&mut io).poll_shutdown(&mut cx()).is_pending());
    assert!(io.lower_shutdown_started());
    assert!(!io.shutdown_entered());
    assert_eq!(state.lock().unwrap().events, ["write"]);
    state.lock().unwrap().flushes = [Step::Pending].into();
    assert!(Pin::new(&mut io).poll_shutdown(&mut cx()).is_pending());
    assert_eq!(state.lock().unwrap().output, b"notify");
    assert!(!io.shutdown_entered());
    assert_eq!(state.lock().unwrap().events, ["write", "write", "flush"]);
    state.lock().unwrap().shutdowns = [Step::Pending].into();
    assert!(Pin::new(&mut io).poll_shutdown(&mut cx()).is_pending());
    assert!(io.lower_shutdown_started() && io.shutdown_entered());
    assert_eq!(
        state.lock().unwrap().events,
        ["write", "write", "flush", "flush", "shutdown"]
    );
    assert!(matches!(
        Pin::new(&mut io).poll_shutdown(&mut cx()),
        Poll::Ready(Ok(()))
    ));
    assert_eq!(state.lock().unwrap().output, b"notify");
    assert_eq!(
        state.lock().unwrap().events,
        [
            "write", "write", "flush", "flush", "shutdown", "flush", "shutdown"
        ]
    );
}

#[test]
fn lower_shutdown_errors_preserve_payload_and_physical_stage() {
    for stage in ["write", "flush", "shutdown"] {
        let (mut io, state) = fixture();
        write(&mut io, b"owned");
        let payload = Arc::new(());
        let error = io::Error::other(Payload(payload.clone()));
        match stage {
            "write" => state.lock().unwrap().writes = [WriteStep::Error(error)].into(),
            "flush" => state.lock().unwrap().flushes = [Step::Error(error)].into(),
            "shutdown" => state.lock().unwrap().shutdowns = [Step::Error(error)].into(),
            _ => unreachable!(),
        }
        let Poll::Ready(Err(error)) = Pin::new(&mut io).poll_shutdown(&mut cx()) else {
            panic!("missing lower error")
        };
        same_payload(&error, &payload);
        assert!(io.lower_shutdown_started());
        assert_eq!(io.shutdown_entered(), stage == "shutdown");
        assert_eq!(
            state.lock().unwrap().output,
            if stage == "write" {
                b"".as_slice()
            } else {
                b"owned".as_slice()
            }
        );
        let before = state.lock().unwrap().events.clone();
        drop(io);
        assert_eq!(
            state.lock().unwrap().events,
            before,
            "drop does not retry failed lower IO"
        );
    }
}

#[test]
fn cancellation_and_connection_drop_do_not_flush_owned_ciphertext() {
    let (mut io, state) = fixture();
    // Reject direct aggregate registration before it can recurse on wake.
    // The assertion precedes any wake so a missing guard fails without recursion.
    let readiness = Arc::downgrade(&io.raw_egress.as_ref().unwrap().readiness);
    let aggregate = io.egress_waker().unwrap();
    io.register_egress_waiter(EgressRole::Reader, &Context::from_waker(&aggregate));
    let stored = io
        .raw_egress
        .as_ref()
        .unwrap()
        .readiness
        .0
        .lock()
        .unwrap()
        .reader
        .clone();
    assert!(
        !stored
            .as_ref()
            .is_some_and(|stored| stored.will_wake(&aggregate))
    );
    drop(stored);
    drop(aggregate);
    write(&mut io, b"ciphertext");
    state.lock().unwrap().writes = [WriteStep::Accept(2), WriteStep::Pending].into();
    let mut draining = Box::pin(poll_fn(|cx| io.poll_egress_flush(cx)));
    assert!(draining.as_mut().poll(&mut cx()).is_pending());
    drop(draining);
    assert_eq!(state.lock().unwrap().output, b"ci");
    assert_eq!(io.egress_queued_bytes(), b"ciphertext".len() - 2);
    assert!(!io.lower_shutdown_started() && !io.shutdown_entered());
    let before = state.lock().unwrap().events.clone();
    drop(io);
    assert_eq!(state.lock().unwrap().events, before);
    assert_eq!(state.lock().unwrap().output, b"ci");
    assert_eq!(state.lock().unwrap().drops, 1);
    let last_socket_waiter = state.lock().unwrap().write_waiter.take();
    drop(last_socket_waiter);
    assert!(
        readiness.upgrade().is_none(),
        "aggregate metadata released after IO and socket waiter drop"
    );
}

#[test]
fn terminal_failure_wakes_and_releases_both_logical_roles() {
    let (io, state) = fixture();
    let (reader, read_wakes, read_drops) = probe(&io);
    let (writer, write_wakes, write_drops) = probe(&io);
    io.register_egress_waiter(EgressRole::Reader, &Context::from_waker(&reader));
    io.register_egress_waiter(EgressRole::Writer, &Context::from_waker(&writer));
    let aggregate = io.egress_waker().unwrap();
    drop(reader);
    drop(writer);
    io.fail_egress_waiters();
    assert_eq!(read_wakes.load(Ordering::SeqCst), 1);
    assert_eq!(write_wakes.load(Ordering::SeqCst), 1);
    assert_eq!(read_drops.load(Ordering::SeqCst), 1);
    assert_eq!(write_drops.load(Ordering::SeqCst), 1);
    aggregate.wake_by_ref();
    io.fail_egress_waiters();
    assert_eq!(read_wakes.load(Ordering::SeqCst), 1);
    assert_eq!(write_wakes.load(Ordering::SeqCst), 1);
    assert!(
        state.lock().unwrap().events.is_empty(),
        "terminal wake is not raw IO"
    );
    drop(io);
    assert_eq!(state.lock().unwrap().drops, 1);
}

#[test]
fn invalid_raw_write_counts_fail_before_offset_growth() {
    for count in [0, 5, usize::MAX] {
        let (mut io, state) = fixture();
        write(&mut io, b"four");
        state.lock().unwrap().writes = if count == 0 {
            [WriteStep::Accept(0)].into()
        } else {
            [WriteStep::Oversized(count)].into()
        };
        let Poll::Ready(Err(error)) = io.poll_egress_drain(&mut cx()) else {
            panic!("invalid write count accepted")
        };
        assert_eq!(
            error.kind(),
            if count == 0 {
                io::ErrorKind::WriteZero
            } else {
                io::ErrorKind::InvalidData
            }
        );
        assert_eq!(io.egress_queued_bytes(), 4);
        assert_eq!(io.written, 0);
        assert!(state.lock().unwrap().output.is_empty());
        assert_eq!(io.pending.capacity(), MAX_PENDING);
    }
}

#[cfg(test)]
#[path = "raw_egress_finalization_tests.rs"]
mod finalization_tests;
