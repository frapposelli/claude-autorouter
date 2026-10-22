//! Fixture-only physical write gate. It grants no producer or delivery authority.
use super::*;
use serde::Serialize;
use std::task::Waker;

#[derive(Clone, Default)]
pub(super) struct WriterProbe(Arc<Mutex<WriterState>>);
#[derive(Default)]
struct WriterState {
    held: bool,
    observer_failed: bool,
    live_io: usize,
    blocked_calls: u64,
    attempted_bytes: u64,
    written_bytes: u64,
    waiters: BTreeMap<u64, Waker>,
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct WriterSnapshot {
    pub held: bool,
    pub observer_failed: bool,
    pub live_io: usize,
    pub waiters: usize,
    pub blocked_calls: u64,
    pub attempted_bytes: u64,
    pub written_bytes: u64,
}
impl WriterProbe {
    pub(super) fn hold(&self) {
        self.0.lock().unwrap().held = true;
    }
    pub(super) fn release(&self) {
        let waiters = {
            let mut state = self.0.lock().unwrap();
            state.held = false;
            std::mem::take(&mut state.waiters)
        };
        for (_, waker) in waiters {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| waker.wake())).is_err() {
                self.0.lock().unwrap().observer_failed = true;
            }
        }
    }
    pub(super) fn snapshot(&self) -> WriterSnapshot {
        let state = self.0.lock().unwrap();
        WriterSnapshot {
            held: state.held,
            observer_failed: state.observer_failed,
            live_io: state.live_io,
            waiters: state.waiters.len(),
            blocked_calls: state.blocked_calls,
            attempted_bytes: state.attempted_bytes,
            written_bytes: state.written_bytes,
        }
    }
    fn blocked(&self, connection: u64, cx: &Context<'_>, bytes: usize) -> (bool, bool) {
        if bytes == 0 {
            return (false, false);
        }
        // Cloning/dropping a user waker happens outside the gate lock.
        let mut waker = Some(cx.waker().clone());
        let (blocked, first, old) = {
            let mut state = self.0.lock().unwrap();
            if !state.held {
                (false, false, None)
            } else {
                let first = !state.waiters.contains_key(&connection);
                if let (Some(calls), Some(total)) = (
                    state.blocked_calls.checked_add(1),
                    state.attempted_bytes.checked_add(bytes as u64),
                ) {
                    state.blocked_calls = calls;
                    state.attempted_bytes = total;
                } else {
                    state.observer_failed = true;
                }
                if first && state.waiters.len() == 128 {
                    state.observer_failed = true;
                    (true, first, None)
                } else {
                    (
                        true,
                        first,
                        state.waiters.insert(connection, waker.take().unwrap()),
                    )
                }
            }
        };
        drop(old);
        (blocked, first)
    }
    fn wrote(&self, bytes: usize) {
        let mut state = self.0.lock().unwrap();
        if let Some(total) = state.written_bytes.checked_add(bytes as u64) {
            state.written_bytes = total;
        } else {
            state.observer_failed = true;
        }
    }
    pub(super) fn register(&self, connection: u64) -> WriterLease {
        self.0.lock().unwrap().live_io += 1;
        WriterLease {
            probe: self.clone(),
            connection,
        }
    }
}
pub(super) struct WriterLease {
    probe: WriterProbe,
    connection: u64,
}
impl WriterLease {
    pub(super) fn blocked(&self, cx: &Context<'_>, bytes: usize) -> (bool, bool) {
        self.probe.blocked(self.connection, cx, bytes)
    }
    pub(super) fn wrote(&self, bytes: usize) {
        self.probe.wrote(bytes);
    }
}
impl Drop for WriterLease {
    fn drop(&mut self) {
        let waker = {
            let mut state = self.probe.0.lock().unwrap();
            state.live_io -= 1;
            state.waiters.remove(&self.connection)
        };
        drop(waker);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::poll_fn;
    use std::task::Wake;
    struct ReadyWriter(Arc<AtomicUsize>);
    impl AsyncWrite for ReadyWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.0.fetch_add(bytes.len(), Ordering::SeqCst);
            Poll::Ready(Ok(bytes.len()))
        }
        fn poll_write_vectored(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[IoSlice<'_>],
        ) -> Poll<io::Result<usize>> {
            self.poll_write(cx, &vec![0; bytes.iter().map(|b| b.len()).sum()])
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
    #[tokio::test]
    async fn held_physical_writer_never_calls_inner_and_drop_releases_waiter_without_opening_gate()
    {
        for vectored in [false, true] {
            let probe = Probe::default();
            let connection = ConnectionTerminal::new(probe.clone(), 1);
            let request = connection.handle().register().unwrap();
            request.attach();
            let writes = Arc::new(AtomicUsize::new(0));
            let mut stream =
                TerminalIo::new(ReadyWriter(writes.clone()), Some(connection.handle()));
            probe.hold_writes();
            let result = poll_fn(|cx| {
                Poll::Ready(if vectored {
                    Pin::new(&mut stream)
                        .poll_write_vectored(cx, &[IoSlice::new(b"ab"), IoSlice::new(b"c")])
                } else {
                    Pin::new(&mut stream).poll_write(cx, b"abc")
                })
            })
            .await;
            assert!(result.is_pending());
            assert_eq!(writes.load(Ordering::SeqCst), 0);
            assert_eq!(probe.writer_snapshot().waiters, 1);
            assert!(request.publisher().fail(FailureCause::Upstream));
            let events = probe.events();
            assert!(
                events
                    .iter()
                    .position(|e| matches!(e, Event::WriteHeld(_)))
                    .unwrap()
                    < events
                        .iter()
                        .position(|e| matches!(e, Event::Failed(_, FailureCause::Upstream)))
                        .unwrap()
            );
            drop(stream);
            assert_eq!(probe.writer_snapshot().waiters, 0);
            assert_eq!(probe.writer_snapshot().live_io, 0);
            assert!(probe.writer_snapshot().held);
            drop(request);
            connection.close().await;
        }
    }
    struct PanicWake(WriterProbe);
    impl Wake for PanicWake {
        fn wake(self: Arc<Self>) {
            assert!(
                self.0.0.try_lock().is_ok(),
                "wake inside physical gate lock"
            );
            panic!("synthetic physical writer waker panic");
        }
    }
    #[tokio::test]
    async fn physical_gate_release_contains_panic_and_ready_writer_preserves_all_bytes() {
        let probe = Probe::default();
        let connection = ConnectionTerminal::new(probe.clone(), 1);
        let writes = Arc::new(AtomicUsize::new(0));
        let mut stream = TerminalIo::new(ReadyWriter(writes.clone()), Some(connection.handle()));
        probe.hold_writes();
        let waker = Waker::from(Arc::new(PanicWake(probe.writer.clone())));
        assert!(
            Pin::new(&mut stream)
                .poll_write(&mut Context::from_waker(&waker), b"abc")
                .is_pending()
        );
        probe.release_writes();
        assert!(probe.observer_failed());
        assert_eq!(probe.writer_snapshot().waiters, 0);
        assert!(!probe.writer_snapshot().held);
        assert_eq!(
            poll_fn(|cx| Pin::new(&mut stream).poll_write(cx, b"abc"))
                .await
                .unwrap(),
            3
        );
        assert_eq!(writes.load(Ordering::SeqCst), 3);
        assert_eq!(probe.writer_snapshot().written_bytes, 3);
        drop(stream);
        connection.close().await;
        assert_eq!(probe.writer_snapshot().live_io, 0);
    }
}
