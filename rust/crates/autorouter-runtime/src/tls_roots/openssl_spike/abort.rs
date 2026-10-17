//! An opt-in fixture capability for one actual socket. Never a gateway signal.
//!
//! Metadata contains only a Weak. The only externally callable destructive
//! operation consumes a qualified, once-only HTTP/1 reservation claim. Neither
//! a body Drop nor a connection observation manufactures cancellation intent.
use std::io;
use std::net::{Shutdown, TcpStream};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};

use hyper::http::Extensions;
use hyper_util::client::legacy::connect::AbortClaim;

use super::client::Counts;
use super::session::TicketState;

// A one-shot fixture gate before TLS/connection publication. The release sender
// is owned by the scenario: dropping it also releases the dial during unwind.
type GateEntry = tokio::sync::oneshot::Sender<tokio::sync::oneshot::Sender<()>>;
#[derive(Default)]
pub(super) struct DialGate(Mutex<Option<GateEntry>>);
impl DialGate {
    pub(super) fn hold_next(
        &self,
    ) -> tokio::sync::oneshot::Receiver<tokio::sync::oneshot::Sender<()>> {
        let (send, receive) = tokio::sync::oneshot::channel();
        assert!(self.0.lock().unwrap().replace(send).is_none());
        receive
    }
    pub(super) async fn pause(&self) {
        let send = self.0.lock().unwrap().take();
        if let Some(send) = send {
            let (release, wait) = tokio::sync::oneshot::channel();
            let _ = send.send(release);
            let _ = wait.await;
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Cause {
    ExplicitFixtureCancellation,
    ExplicitFixtureDeadline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Terminal {
    Ordinary,
    IoError,
    Abort(Cause),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Live,
    Closed(Terminal),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Outcome {
    Requested,
    AlreadyClosed(Terminal),
    MissingTransport,
    ShutdownFailed(io::ErrorKind),
}

// Not Clone or Debug: the extra descriptor is owned by the actual IO lifetime.
// A temporary upgrade during the synchronous operation can prolong that life,
// but cannot retarget this owned socket through descriptor number reuse.
pub(super) struct AbortControl {
    phase: Mutex<Phase>,
    error_closed: AtomicBool,
    socket: TcpStream,
    session: Option<Arc<TicketState>>,
    counts: Arc<Counts>,
}

#[derive(Clone)]
pub(super) struct Handle(Weak<AbortControl>);

impl AbortControl {
    pub(super) fn new(
        socket: TcpStream,
        session: Option<Arc<TicketState>>,
        counts: Arc<Counts>,
    ) -> Arc<Self> {
        counts.shutdown_handles.fetch_add(1, Ordering::SeqCst);
        Arc::new(Self {
            phase: Mutex::new(Phase::Live),
            error_closed: AtomicBool::new(false),
            socket,
            session,
            counts,
        })
    }

    pub(super) fn handle(self: &Arc<Self>) -> Handle {
        Handle(Arc::downgrade(self))
    }

    // First terminal cause gates explicit abort only. A later genuine physical
    // IO error still has Node's independent close(hadError) meaning. All cache
    // calls and OS shutdown occur after releasing this phase lock.
    fn first(&self, terminal: Terminal) -> Result<(), Terminal> {
        let mut phase = self.phase.lock().unwrap_or_else(|e| e.into_inner());
        match *phase {
            Phase::Live => {
                *phase = Phase::Closed(terminal);
                Ok(())
            }
            Phase::Closed(previous) => Err(previous),
        }
    }

    fn evict(&self) {
        if self.error_closed.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(session) = &self.session {
            session.close_error();
            self.counts
                .session_error_closes
                .fetch_add(1, Ordering::SeqCst);
        }
    }

    pub(super) fn ordinary_close(&self) {
        let _ = self.first(Terminal::Ordinary);
    }

    pub(super) fn io_error(&self) {
        let _ = self.first(Terminal::IoError);
        self.evict();
    }

    fn abort(&self, cause: Cause) -> Outcome {
        if let Err(previous) = self.first(Terminal::Abort(cause)) {
            return Outcome::AlreadyClosed(previous);
        }
        self.evict();
        self.counts.shutdown_calls.fetch_add(1, Ordering::SeqCst);
        // This is deliberately not a wake-only implementation. Hyper can stop
        // polling its transport while Incoming is backpressured; shutdown must
        // reach the physical socket without a subsequent body or IO poll.
        match self.socket.shutdown(Shutdown::Both) {
            Ok(()) => Outcome::Requested,
            Err(error) => Outcome::ShutdownFailed(error.kind()),
        }
    }
}

impl Drop for AbortControl {
    fn drop(&mut self) {
        self.counts.shutdown_handles.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Handle {
    // Observation only; there is no destructive method accepting a Handle.
    pub(super) fn terminal(&self) -> Option<Terminal> {
        let control = self.0.upgrade()?;
        let phase = *control.phase.lock().unwrap_or_else(|e| e.into_inner());
        match phase {
            Phase::Live => None,
            Phase::Closed(cause) => Some(cause),
        }
    }

    pub(super) fn is_alive(&self) -> bool {
        self.0.strong_count() != 0
    }

    pub(super) fn unread_socket_bytes(&self) -> bool {
        let Some(control) = self.0.upgrade() else {
            return false;
        };
        matches!(control.socket.peek(&mut [0]), Ok(count) if count != 0)
    }
}

pub(super) fn consume(claim: AbortClaim, cause: Cause) -> Outcome {
    let mut extras = Extensions::new();
    claim.get_extras(&mut extras);
    let Some(handle) = extras.remove::<Handle>() else {
        return Outcome::MissingTransport;
    };
    let Some(control) = handle.0.upgrade() else {
        return Outcome::MissingTransport;
    };
    control.abort(cause)
}

#[cfg(test)]
mod tests {
    use super::super::session::{Cache, Key, PolicyId, Verification};
    use super::*;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;

    // Exercise the private cause/once state independently from reservation
    // retirement, which is proved by the actual Hyper schedules. Calls to
    // io_error below inject a physical IO observation; they are not body errors.
    async fn ordered(first: Terminal) {
        tokio::time::timeout(Duration::from_secs(2), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap();
            let (mut peer, _) = listener.accept().await.unwrap();
            let cache = Arc::new(Mutex::new(Cache::new(100)));
            let session = Arc::new(TicketState::new(
                &cache,
                Key {
                    policy: PolicyId(7),
                    origin: "synthetic:1".into(),
                    server_name: None,
                    verification: Verification::Required,
                },
            ));
            session.accept();
            let counts = Arc::new(Counts::default());
            let control =
                AbortControl::new(socket.into_std().unwrap(), Some(session), counts.clone());
            let weak = control.handle();
            assert_eq!(Arc::strong_count(&control), 1);
            assert_eq!(counts.shutdown_handles.load(Ordering::SeqCst), 1);
            match first {
                Terminal::Ordinary => control.ordinary_close(),
                Terminal::IoError => control.io_error(),
                Terminal::Abort(cause) => assert_eq!(control.abort(cause), Outcome::Requested),
            }
            assert_eq!(weak.terminal(), Some(first));
            assert_eq!(
                control.abort(Cause::ExplicitFixtureDeadline),
                Outcome::AlreadyClosed(first)
            );
            assert_eq!(
                counts.shutdown_calls.load(Ordering::SeqCst),
                usize::from(matches!(first, Terminal::Abort(_)))
            );
            assert_eq!(
                counts.session_error_closes.load(Ordering::SeqCst),
                usize::from(first != Terminal::Ordinary)
            );
            // A later physical write/flush/read error retains the old first
            // cause for abort gating, while independently closing sessions once.
            control.io_error();
            control.io_error();
            control.ordinary_close();
            assert_eq!(weak.terminal(), Some(first));
            assert_eq!(counts.session_error_closes.load(Ordering::SeqCst), 1);
            assert!(control.phase.try_lock().is_ok());
            drop(control);
            assert_eq!(counts.shutdown_handles.load(Ordering::SeqCst), 0);
            assert!(!weak.is_alive());
            assert_eq!(peer.read(&mut [0]).await.unwrap(), 0);
        })
        .await
        .expect("ordered terminal fixture deadline");
    }

    #[tokio::test]
    async fn ordinary_close_rejects_late_explicit_abort_but_preserves_late_io_error() {
        ordered(Terminal::Ordinary).await;
    }
    #[tokio::test]
    async fn physical_error_precedes_explicit_intent_and_only_evicts_once() {
        ordered(Terminal::IoError).await;
    }
    #[tokio::test]
    async fn explicit_abort_precedes_error_drop_and_repeated_intent_without_double_eviction() {
        ordered(Terminal::Abort(Cause::ExplicitFixtureCancellation)).await;
    }
}
