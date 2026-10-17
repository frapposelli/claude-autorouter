//! Experimental single-use ownership of a pooled HTTP/1 reservation.
//!
//! Local patch, not an upstream API. Retirement is not response-body completion
//! or successful delivery. No callback runs during retirement, and no session
//! eviction or IO abort is inferred from Drop.

use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, OnceLock};

use http::{Extensions, Request};
use tokio::sync::watch;

use super::Connected;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Pending,
    Active,
    Claimed,
    Retired,
}

struct Record {
    phase: Mutex<Phase>,
    // Installed once, outside the phase lock. Metadata Clone/Drop may call user
    // code; that must never happen while holding the phase or pool lock.
    metadata: OnceLock<Arc<Connected>>,
}

/// Observe the one selected reservation belonging to an attached request.
///
/// The handle starts no background task and retains no request/IO ownership
/// beyond connector-supplied metadata. Connectors should use weak IO metadata;
/// arbitrary strong ownership in Connected extras is retained as supplied.
/// A request cloned after attachment shares a single-use slot: the first
/// submission consumes it, and subsequent submissions fail before IO.
pub struct CaptureAssignment {
    receiver: watch::Receiver<Option<Assignment>>,
}

/// Opaque identity of a selected HTTP/1 reservation, which may already be retired.
///
/// Metadata observation alone never grants permission to abort a connection.
#[derive(Clone)]
pub struct Assignment {
    record: Arc<Record>,
}

/// A once-only claim made while the selected reservation was still exclusive.
///
/// Claiming poisons future pool reuse; it does not close IO or evict a TLS
/// session. The caller must separately perform its authorized IO action. Drop
/// does not claim that action succeeded and never implies session eviction.
pub struct AbortClaim {
    metadata: Arc<Connected>,
}

macro_rules! opaque_debug {
    ($name:ident) => {
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct(stringify!($name)).finish_non_exhaustive()
            }
        }
    };
}
opaque_debug!(CaptureAssignment);
opaque_debug!(Assignment);
opaque_debug!(AbortClaim);

impl CaptureAssignment {
    /// Return the selected reservation if assignment has happened.
    pub fn assignment(&self) -> Option<Assignment> {
        self.receiver.borrow().clone()
    }

    /// Wait for assignment or for an unassigned request to be released.
    ///
    /// A held handshake can remain pending. Race this method with the caller's
    /// deadline/cancellation; this method creates no watcher task of its own.
    pub async fn wait_for_assignment(&mut self) -> Option<Assignment> {
        if let Some(assignment) = self.assignment() {
            return Some(assignment);
        }
        let _ = self.receiver.changed().await;
        self.assignment()
    }
}

impl Assignment {
    /// Copy selected-connection metadata without granting abort authority.
    pub fn get_extras(&self, extensions: &mut Extensions) {
        if let Some(metadata) = self.record.metadata.get() {
            metadata.get_extras(extensions);
        }
    }

    /// Poison this exact connection only if this reservation remains active.
    ///
    /// Checking and poisoning are one operation synchronized with retirement.
    /// Late or repeated claims return None; no callback, tracing, metadata clone
    /// or IO wake occurs under the lifecycle lock. The returned claim's metadata
    /// may be used after that lock is released to request an explicit IO close.
    pub fn claim_abort(&self) -> Option<AbortClaim> {
        {
            let mut phase = self.record.phase.lock().unwrap_or_else(|e| e.into_inner());
            if *phase != Phase::Active {
                return None;
            }
            // Unlike Connected::poison(), this private operation does not trace.
            self.record.metadata.get()?.poisoned.poison();
            *phase = Phase::Claimed;
        }
        Some(AbortClaim {
            metadata: self.record.metadata.get()?.clone(),
        })
    }
}

impl AbortClaim {
    /// Copy metadata after the claim has released all lifecycle locks.
    pub fn get_extras(&self, extensions: &mut Extensions) {
        self.metadata.get_extras(extensions);
    }
}

#[derive(Clone)]
struct Attachment(Arc<Mutex<Option<PendingGuard>>>);

/// Attach a single-use lease capture to a request before submitting it.
///
/// Supports pooled HTTP/1 clients with automatic retries disabled. Attached
/// HTTP2, CONNECT, Upgrade, retry-enabled and pool-disabled requests are rejected
/// before dispatch. Requests without this extension retain upstream behavior.
/// Dropping an unsubmitted request retires its pending state and wakes its
/// capture with None. No abort authority exists before assignment.
pub fn capture_http1_assignment<B>(request: &mut Request<B>) -> CaptureAssignment {
    let record = Arc::new(Record {
        phase: Mutex::new(Phase::Pending),
        metadata: OnceLock::new(),
    });
    let (sender, receiver) = watch::channel(None);
    let guard = PendingGuard { record, sender };
    request
        .extensions_mut()
        .insert(Attachment(Arc::new(Mutex::new(Some(guard)))));
    CaptureAssignment { receiver }
}

pub(in crate::client::legacy) struct PendingGuard {
    record: Arc<Record>,
    // Dropped outside the phase lock; closure may wake an arbitrary task.
    sender: watch::Sender<Option<Assignment>>,
}

pub(in crate::client::legacy) fn take<B>(
    request: &mut Request<B>,
) -> Result<Option<PendingGuard>, ()> {
    let Some(attachment) = request.extensions_mut().remove::<Attachment>() else {
        return Ok(None);
    };
    let guard = attachment
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    guard.map(Some).ok_or(())
}

/// Exclusive lease owner for the private opt-in raw HTTP/1 adapter.
///
/// The caller must retain the uniquely selected HTTP/1 sender alongside this
/// non-Clone reservation until retirement, and retire before returning that
/// sender to any pool. Connected metadata alone is not proof of IO ownership.
/// Drop retires the lease without claiming clean body completion or IO closure.
#[cfg(feature = "node-http1-raw-pool")]
pub struct Http1Reservation {
    guard: PendingGuard,
}

#[cfg(feature = "node-http1-raw-pool")]
opaque_debug!(Http1Reservation);

/// Consume the existing single-use attachment after selecting unique HTTP/1 IO.
///
/// The caller must own the exact selected sender and must not retry this request.
/// No attachment returns `Ok(None)`; a consumed attachment or an attached
/// non-HTTP/1, CONNECT, Upgrade or negotiated-H2 request returns `Err(())` before
/// assignment. The ordinary legacy client path does not use this bridge.
#[cfg(feature = "node-http1-raw-pool")]
#[allow(
    clippy::result_unit_err,
    reason = "Private opt-in bridge preserves take()'s opaque single-use rejection contract"
)]
pub fn bind_http1_reservation<B>(
    request: &mut Request<B>,
    connected: &Connected,
) -> Result<Option<Http1Reservation>, ()> {
    let Some(guard) = take(request)? else {
        return Ok(None);
    };
    if !matches!(
        request.version(),
        http::Version::HTTP_10 | http::Version::HTTP_11
    ) || request.method() == http::Method::CONNECT
        || request.headers().contains_key(http::header::UPGRADE)
        || request
            .headers()
            .get_all(http::header::CONNECTION)
            .iter()
            .any(|value| {
                value
                    .as_bytes()
                    .split(|byte| *byte == b',')
                    .any(|token| token.trim_ascii().eq_ignore_ascii_case(b"upgrade"))
            })
        || connected.is_negotiated_h2()
    {
        return Err(());
    }
    // Arm ownership before metadata Clone or publication can reenter or panic.
    let mut reservation = Http1Reservation { guard };
    reservation.guard.assign(connected);
    Ok(Some(reservation))
}

#[cfg(feature = "node-http1-raw-pool")]
impl Http1Reservation {
    /// Retire before pool return and report absence of this lease's reuse veto.
    ///
    /// Only an Active lease with present, unpoisoned metadata returns true. This
    /// is not a transport-health or successful-forwarding verdict. The caller
    /// separately verifies clean consumer EOF and sender/driver readiness.
    pub fn retire(self) -> bool {
        let reusable = {
            let mut phase = self
                .guard
                .record
                .phase
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let reusable = *phase == Phase::Active
                && self
                    .guard
                    .record
                    .metadata
                    .get()
                    .is_some_and(|metadata| !metadata.poisoned.poisoned());
            *phase = Phase::Retired;
            reusable
        };
        // PendingGuard and arbitrary metadata Drop run after the lock is gone.
        reusable
    }
}

impl PendingGuard {
    pub(in crate::client::legacy) fn assign(&mut self, connected: &Connected) {
        let metadata = Arc::new(connected.clone());
        assert!(
            self.record.metadata.set(metadata).is_ok(),
            "lease assigned twice"
        );
        {
            let mut phase = self.record.phase.lock().unwrap_or_else(|e| e.into_inner());
            assert!(
                *phase == Phase::Pending,
                "lease assignment after retirement"
            );
            *phase = Phase::Active;
        }
        // The owning Guarded wrapper is armed before this can wake/reenter/panic.
        self.sender.send_replace(Some(Assignment {
            record: self.record.clone(),
        }));
    }

    pub(in crate::client::legacy) fn retire(&self) {
        *self.record.phase.lock().unwrap_or_else(|e| e.into_inner()) = Phase::Retired;
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.retire();
    }
}

// This wrapper is only introduced in feature-on client code. Drop's state
// transition precedes field cleanup, including Pooled::drop's waiter delivery.
pub(in crate::client::legacy) struct Guarded<P> {
    pub(in crate::client::legacy) value: P,
    pub(in crate::client::legacy) guard: Option<PendingGuard>,
}

impl<P> Drop for Guarded<P> {
    fn drop(&mut self) {
        if let Some(guard) = &self.guard {
            guard.retire();
        }
    }
}

impl<P> Deref for Guarded<P> {
    type Target = P;
    fn deref(&self) -> &P {
        &self.value
    }
}
impl<P> DerefMut for Guarded<P> {
    fn deref_mut(&mut self) -> &mut P {
        &mut self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll, Wake, Waker};

    fn pending() -> (PendingGuard, CaptureAssignment) {
        let mut request = Request::new(());
        let capture = capture_http1_assignment(&mut request);
        (take(&mut request).unwrap().unwrap(), capture)
    }

    #[test]
    fn request_and_unpolled_capture_drop_close_without_assignment() {
        let mut request = Request::new(());
        let mut capture = capture_http1_assignment(&mut request);
        drop(request);
        let mut wait = Box::pin(capture.wait_for_assignment());
        assert!(matches!(
            wait.as_mut().poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(None)
        ));
    }

    #[test]
    fn cloned_attachment_is_single_use_without_retiring_first() {
        let mut request = Request::new(());
        let capture = capture_http1_assignment(&mut request);
        let mut duplicate = request.clone();
        let mut guard = take(&mut request).unwrap().unwrap();
        assert!(take(&mut duplicate).is_err());
        guard.assign(&Connected::new());
        assert!(capture.assignment().unwrap().claim_abort().is_some());
    }

    #[test]
    fn claim_is_once_only_and_poison_precedes_retirement() {
        let (mut guard, capture) = pending();
        let connected = Connected::new();
        guard.assign(&connected);
        let assignment = capture.assignment().unwrap();
        assert!(!connected.poisoned.poisoned());
        let claim = assignment.claim_abort().unwrap();
        assert!(connected.poisoned.poisoned());
        assert!(assignment.claim_abort().is_none());
        drop(claim);
        drop(guard);
        assert!(assignment.claim_abort().is_none());
    }

    #[test]
    fn retirement_prevents_late_poison() {
        let (mut guard, capture) = pending();
        let connected = Connected::new();
        guard.assign(&connected);
        let assignment = capture.assignment().unwrap();
        guard.retire();
        assert!(assignment.claim_abort().is_none());
        assert!(!connected.poisoned.poisoned());
    }

    struct PoolReturn(Arc<Record>, Arc<AtomicBool>);
    impl Drop for PoolReturn {
        fn drop(&mut self) {
            let phase = self
                .0
                .phase
                .try_lock()
                .expect("pool return under lifecycle lock");
            assert!(*phase == Phase::Retired, "pool return before retirement");
            self.1.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn guarded_drop_retires_before_pool_return_and_unlocks() {
        let (mut guard, _) = pending();
        guard.assign(&Connected::new());
        let returned = Arc::new(AtomicBool::new(false));
        let guarded = Guarded {
            value: PoolReturn(guard.record.clone(), returned.clone()),
            guard: Some(guard),
        };
        drop(guarded);
        assert!(returned.load(Ordering::SeqCst));
    }

    struct LockAwareExtra {
        record: std::sync::Weak<Record>,
        panic_on_clone: bool,
    }
    impl Clone for LockAwareExtra {
        fn clone(&self) -> Self {
            let record = self.record.upgrade().unwrap();
            assert!(
                record.phase.try_lock().is_ok(),
                "metadata Clone under lifecycle lock"
            );
            assert!(!self.panic_on_clone, "synthetic metadata Clone panic");
            Self {
                record: self.record.clone(),
                panic_on_clone: false,
            }
        }
    }
    impl Drop for LockAwareExtra {
        fn drop(&mut self) {
            if let Some(record) = self.record.upgrade() {
                assert!(
                    record.phase.try_lock().is_ok(),
                    "metadata Drop under lifecycle lock"
                );
            }
        }
    }

    #[test]
    fn metadata_clone_drop_and_extras_are_outside_lock() {
        let (mut guard, capture) = pending();
        let connected = Connected::new().extra(LockAwareExtra {
            record: Arc::downgrade(&guard.record),
            panic_on_clone: false,
        });
        guard.assign(&connected);
        let assignment = capture.assignment().unwrap();
        assignment.get_extras(&mut Extensions::new());
        let claim = assignment.claim_abort().unwrap();
        claim.get_extras(&mut Extensions::new());
        drop(guard);
        drop(capture);
        drop(assignment);
        drop(claim);
        drop(connected);
    }

    #[test]
    fn metadata_clone_panic_retires_before_pool_return() {
        let (guard, capture) = pending();
        let record = guard.record.clone();
        let returned = Arc::new(AtomicBool::new(false));
        let connected = Connected::new().extra(LockAwareExtra {
            record: Arc::downgrade(&record),
            panic_on_clone: true,
        });
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut guarded = Guarded {
                value: PoolReturn(record.clone(), returned.clone()),
                guard: Some(guard),
            };
            guarded.guard.as_mut().unwrap().assign(&connected);
        }));
        assert!(result.is_err());
        assert!(returned.load(Ordering::SeqCst));
        assert!(capture.assignment().is_none());
        assert!(*record.phase.lock().unwrap() == Phase::Retired);
    }

    struct LockAwareWake(std::sync::Weak<Record>, bool);
    impl Wake for LockAwareWake {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            assert!(
                self.0.upgrade().unwrap().phase.try_lock().is_ok(),
                "wake under lifecycle lock"
            );
            assert!(!self.1, "synthetic publication wake panic");
        }
    }

    #[test]
    fn publication_wake_panic_unwinds_through_armed_guard() {
        let (guard, mut capture) = pending();
        let record = guard.record.clone();
        let waker = Waker::from(Arc::new(LockAwareWake(Arc::downgrade(&record), true)));
        let mut wait = Box::pin(capture.wait_for_assignment());
        assert!(
            wait.as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        let returned = Arc::new(AtomicBool::new(false));
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut guarded = Guarded {
                value: PoolReturn(record.clone(), returned.clone()),
                guard: Some(guard),
            };
            guarded.guard.as_mut().unwrap().assign(&Connected::new());
        }));
        drop(wait);
        assert!(result.is_err());
        assert!(returned.load(Ordering::SeqCst));
        assert!(capture.assignment().unwrap().claim_abort().is_none());
    }

    #[test]
    fn record_does_not_form_cycle_or_own_transport() {
        let (mut guard, capture) = pending();
        let record = Arc::downgrade(&guard.record);
        let transport = Arc::new(());
        let weak_transport = Arc::downgrade(&transport);
        guard.assign(&Connected::new().extra(weak_transport.clone()));
        let assignment = capture.assignment().unwrap();
        drop(transport);
        assert!(weak_transport.upgrade().is_none());
        drop(guard);
        drop(capture);
        assert!(record.upgrade().is_some());
        drop(assignment);
        assert!(record.upgrade().is_none());
    }

    #[test]
    fn concurrent_claim_and_retirement_have_only_ordered_outcomes() {
        for _ in 0..64 {
            let (mut guard, capture) = pending();
            let connected = Connected::new();
            guard.assign(&connected);
            let assignment = capture.assignment().unwrap();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            std::thread::scope(|scope| {
                let barrier_peer = barrier.clone();
                let retire = scope.spawn(move || {
                    barrier_peer.wait();
                    drop(guard);
                });
                barrier.wait();
                let claimed = assignment.claim_abort().is_some();
                retire.join().unwrap();
                assert_eq!(connected.poisoned.poisoned(), claimed);
                assert!(assignment.claim_abort().is_none());
            });
        }
    }
    #[test]
    fn forced_claim_then_retire_and_retire_then_claim_orders() {
        for claim_first in [false, true] {
            let (mut guard, capture) = pending();
            let connected = Connected::new();
            guard.assign(&connected);
            let assignment = capture.assignment().unwrap();
            let (release, wait) = std::sync::mpsc::sync_channel(1);
            std::thread::scope(|scope| {
                if claim_first {
                    let peer = scope.spawn(move || {
                        wait.recv_timeout(std::time::Duration::from_secs(2))
                            .unwrap();
                        drop(guard);
                    });
                    assert!(assignment.claim_abort().is_some());
                    release.send(()).unwrap();
                    peer.join().unwrap();
                    assert!(connected.poisoned.poisoned());
                } else {
                    let peer = scope.spawn(move || {
                        drop(guard);
                        release.send(()).unwrap();
                    });
                    wait.recv_timeout(std::time::Duration::from_secs(2))
                        .unwrap();
                    assert!(assignment.claim_abort().is_none());
                    peer.join().unwrap();
                    assert!(!connected.poisoned.poisoned());
                }
                assert!(assignment.claim_abort().is_none());
            });
        }
    }

    #[test]
    fn metadata_drop_panic_occurs_after_retirement_without_lock() {
        #[derive(Clone)]
        struct DropPanic(std::sync::Weak<Record>, Arc<AtomicBool>);
        impl Drop for DropPanic {
            fn drop(&mut self) {
                if self.1.swap(false, Ordering::SeqCst) {
                    let record = self.0.upgrade().unwrap();
                    assert!(
                        *record.phase.try_lock().expect("metadata Drop under lock")
                            == Phase::Retired
                    );
                    panic!("synthetic metadata Drop panic");
                }
            }
        }
        let (mut guard, capture) = pending();
        let armed = Arc::new(AtomicBool::new(false));
        let connected =
            Connected::new().extra(DropPanic(Arc::downgrade(&guard.record), armed.clone()));
        guard.assign(&connected);
        let assignment = capture.assignment().unwrap();
        let guarded = Guarded {
            value: connected,
            guard: Some(guard),
        };
        armed.store(true, Ordering::SeqCst);
        let panic = catch_unwind(AssertUnwindSafe(|| drop(guarded))).unwrap_err();
        assert_eq!(
            panic.downcast_ref::<&str>(),
            Some(&"synthetic metadata Drop panic")
        );
        assert!(assignment.claim_abort().is_none());
    }

    #[cfg(feature = "node-http1-raw-pool")]
    mod raw_pool {
        use super::*;

        fn attached() -> (Request<()>, CaptureAssignment, Arc<Record>) {
            let mut request = Request::new(());
            let capture = capture_http1_assignment(&mut request);
            let record = request
                .extensions()
                .get::<Attachment>()
                .unwrap()
                .0
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .record
                .clone();
            (request, capture, record)
        }

        #[test]
        fn binding_is_single_use_and_preserves_exact_selected_metadata() {
            let (mut request, capture, _) = attached();
            assert!(capture.assignment().is_none());
            let mut duplicate = request.clone();
            let connected = Connected::new().extra(73_u64);
            let owner = bind_http1_reservation(&mut request, &connected)
                .unwrap()
                .unwrap();
            assert!(bind_http1_reservation(&mut duplicate, &Connected::new()).is_err());
            let assignment = capture.assignment().unwrap();
            let mut extras = Extensions::new();
            assignment.get_extras(&mut extras);
            assert_eq!(extras.get::<u64>(), Some(&73));
            assert!(owner.retire());
            assert!(assignment.claim_abort().is_none());
            assert!(!connected.poisoned.poisoned());
        }

        #[test]
        fn unattached_request_does_not_create_a_reservation_or_change_legacy_behavior() {
            let mut request = Request::new(());
            *request.version_mut() = http::Version::HTTP_2;
            assert!(
                bind_http1_reservation(&mut request, &Connected::new().negotiated_h2())
                    .unwrap()
                    .is_none()
            );
        }

        #[test]
        fn abort_claim_before_retirement_vetoes_reuse_exactly_once() {
            let (mut request, capture, _) = attached();
            let connected = Connected::new();
            let owner = bind_http1_reservation(&mut request, &connected)
                .unwrap()
                .unwrap();
            let assignment = capture.assignment().unwrap();
            let claim = assignment.claim_abort().unwrap();
            assert!(connected.poisoned.poisoned());
            assert!(!owner.retire());
            assert!(assignment.claim_abort().is_none());
            drop(claim);
        }

        #[test]
        fn dropped_reservation_revokes_late_authority_without_poisoning() {
            let (mut request, capture, record) = attached();
            let connected = Connected::new();
            let owner = bind_http1_reservation(&mut request, &connected)
                .unwrap()
                .unwrap();
            let assignment = capture.assignment().unwrap();
            drop(owner);
            assert!(*record.phase.try_lock().unwrap() == Phase::Retired);
            assert!(assignment.claim_abort().is_none());
            assert!(!connected.poisoned.poisoned());
        }

        #[test]
        fn retirement_requires_active_present_unpoisoned_metadata() {
            for phase in [Phase::Pending, Phase::Active, Phase::Retired] {
                let (guard, _) = pending();
                let record = guard.record.clone();
                *record.phase.lock().unwrap() = phase;
                assert!(!Http1Reservation { guard }.retire());
                assert!(*record.phase.try_lock().unwrap() == Phase::Retired);
            }
            let (mut request, capture, _) = attached();
            let connected = Connected::new();
            connected.poison();
            let owner = bind_http1_reservation(&mut request, &connected)
                .unwrap()
                .unwrap();
            assert!(!owner.retire());
            assert!(capture.assignment().unwrap().claim_abort().is_none());
        }

        #[test]
        fn retirement_precedes_caller_pool_return_and_releases_lock() {
            let (mut request, capture, record) = attached();
            let owner = bind_http1_reservation(&mut request, &Connected::new())
                .unwrap()
                .unwrap();
            let returned = Arc::new(AtomicBool::new(false));
            let pool = PoolReturn(record, returned.clone());
            assert!(owner.retire());
            drop(pool);
            assert!(returned.load(Ordering::SeqCst));
            assert!(capture.assignment().unwrap().claim_abort().is_none());
        }

        #[test]
        fn unsupported_protocols_consume_attachment_before_any_assignment_clone() {
            for variant in 0..7 {
                let (mut request, mut capture, record) = attached();
                let mut connected = Connected::new().extra(LockAwareExtra {
                    record: Arc::downgrade(&record),
                    panic_on_clone: true,
                });
                match variant {
                    0 => *request.version_mut() = http::Version::HTTP_2,
                    1 => *request.version_mut() = http::Version::HTTP_3,
                    2 => *request.method_mut() = http::Method::CONNECT,
                    3 => {
                        request
                            .headers_mut()
                            .insert(http::header::UPGRADE, "websocket".parse().unwrap());
                    }
                    4 => connected = connected.negotiated_h2(),
                    5 => {
                        request.headers_mut().insert(
                            http::header::CONNECTION,
                            "keep-alive, UpGrAdE".parse().unwrap(),
                        );
                    }
                    6 => {
                        request
                            .headers_mut()
                            .append(http::header::CONNECTION, "keep-alive".parse().unwrap());
                        request
                            .headers_mut()
                            .append(http::header::CONNECTION, " Upgrade ".parse().unwrap());
                    }
                    _ => unreachable!(),
                }
                let mut duplicate = request.clone();
                assert!(bind_http1_reservation(&mut request, &connected).is_err());
                assert!(bind_http1_reservation(&mut duplicate, &Connected::new()).is_err());
                assert!(*record.phase.try_lock().unwrap() == Phase::Retired);
                assert!(capture.assignment().is_none());
                let mut wait = Box::pin(capture.wait_for_assignment());
                assert!(matches!(
                    wait.as_mut().poll(&mut Context::from_waker(Waker::noop())),
                    Poll::Ready(None)
                ));
            }
        }

        #[test]
        fn metadata_clone_panic_unwinds_through_armed_reservation() {
            let (mut request, capture, record) = attached();
            let connected = Connected::new().extra(LockAwareExtra {
                record: Arc::downgrade(&record),
                panic_on_clone: true,
            });
            let panic = catch_unwind(AssertUnwindSafe(|| {
                bind_http1_reservation(&mut request, &connected)
            }));
            assert!(panic.is_err());
            assert!(*record.phase.try_lock().unwrap() == Phase::Retired);
            assert!(capture.assignment().is_none());
        }

        #[test]
        fn metadata_clone_drop_and_publication_wake_stay_outside_lock() {
            let (mut request, mut capture, record) = attached();
            let connected = Connected::new().extra(LockAwareExtra {
                record: Arc::downgrade(&record),
                panic_on_clone: false,
            });
            let waker = Waker::from(Arc::new(LockAwareWake(Arc::downgrade(&record), false)));
            let mut wait = Box::pin(capture.wait_for_assignment());
            assert!(
                wait.as_mut()
                    .poll(&mut Context::from_waker(&waker))
                    .is_pending()
            );
            let owner = bind_http1_reservation(&mut request, &connected)
                .unwrap()
                .unwrap();
            drop(wait);
            let assignment = capture.assignment().unwrap();
            assignment.get_extras(&mut Extensions::new());
            assert!(owner.retire());
            drop(assignment);
            drop(capture);
            drop(connected);
        }

        #[test]
        fn publication_panic_retires_reservation_and_revokes_authority() {
            let (mut request, mut capture, record) = attached();
            let waker = Waker::from(Arc::new(LockAwareWake(Arc::downgrade(&record), true)));
            let mut wait = Box::pin(capture.wait_for_assignment());
            assert!(
                wait.as_mut()
                    .poll(&mut Context::from_waker(&waker))
                    .is_pending()
            );
            let panic = catch_unwind(AssertUnwindSafe(|| {
                bind_http1_reservation(&mut request, &Connected::new())
            }));
            drop(wait);
            assert!(panic.is_err());
            assert!(*record.phase.try_lock().unwrap() == Phase::Retired);
            assert!(capture.assignment().unwrap().claim_abort().is_none());
        }

        #[test]
        fn concurrent_claim_and_retire_return_complementary_reuse_outcomes() {
            for _ in 0..64 {
                let (mut request, capture, record) = attached();
                let connected = Connected::new();
                let owner = bind_http1_reservation(&mut request, &connected)
                    .unwrap()
                    .unwrap();
                let assignment = capture.assignment().unwrap();
                let barrier = Arc::new(std::sync::Barrier::new(2));
                std::thread::scope(|scope| {
                    let peer_barrier = barrier.clone();
                    let retire = scope.spawn(move || {
                        peer_barrier.wait();
                        owner.retire()
                    });
                    barrier.wait();
                    let claimed = assignment.claim_abort().is_some();
                    let reusable = retire.join().unwrap();
                    assert_eq!(reusable, !claimed);
                    assert_eq!(connected.poisoned.poisoned(), claimed);
                    assert!(*record.phase.try_lock().unwrap() == Phase::Retired);
                    assert!(assignment.claim_abort().is_none());
                });
            }
        }
    }

    #[cfg(feature = "http2")]
    #[tokio::test]
    async fn negotiated_http2_is_rejected_without_assignment_or_authority() {
        use crate::client::legacy::Client;
        use crate::client::legacy::connect::Connection;
        use crate::rt::{TokioExecutor, TokioIo};
        use bytes::Bytes;
        use http_body_util::Empty;
        use hyper::rt::{Read, ReadBufCursor, Write};
        use std::{io, pin::Pin};
        struct H2Io(TokioIo<tokio::io::DuplexStream>, Arc<AtomicBool>);
        impl Drop for H2Io {
            fn drop(&mut self) {
                self.1.store(true, Ordering::SeqCst);
            }
        }
        impl Connection for H2Io {
            fn connected(&self) -> Connected {
                Connected::new().negotiated_h2()
            }
        }
        impl Read for H2Io {
            fn poll_read(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buffer: ReadBufCursor<'_>,
            ) -> Poll<io::Result<()>> {
                Pin::new(&mut self.0).poll_read(cx, buffer)
            }
        }
        impl Write for H2Io {
            fn poll_write(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                bytes: &[u8],
            ) -> Poll<io::Result<usize>> {
                Pin::new(&mut self.0).poll_write(cx, bytes)
            }
            fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                Pin::new(&mut self.0).poll_flush(cx)
            }
            fn poll_shutdown(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<io::Result<()>> {
                Pin::new(&mut self.0).poll_shutdown(cx)
            }
        }
        #[derive(Clone)]
        struct H2Connector(Arc<Mutex<Option<H2Io>>>);
        impl tower_service::Service<http::Uri> for H2Connector {
            type Response = H2Io;
            type Error = io::Error;
            type Future = std::future::Ready<Result<H2Io, io::Error>>;
            fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
                Poll::Ready(Ok(()))
            }
            fn call(&mut self, _: http::Uri) -> Self::Future {
                std::future::ready(
                    self.0
                        .lock()
                        .unwrap()
                        .take()
                        .ok_or_else(|| io::Error::other("fixture consumed")),
                )
            }
        }
        let (io, peer) = tokio::io::duplex(65536);
        let closed = Arc::new(AtomicBool::new(false));
        let connector = H2Connector(Arc::new(Mutex::new(Some(H2Io(
            TokioIo::new(io),
            closed.clone(),
        )))));
        let client = Client::builder(TokioExecutor::new())
            .retry_canceled_requests(false)
            .build(connector);
        let mut request = Request::get("http://synthetic.invalid/")
            .body(Empty::<Bytes>::new())
            .unwrap();
        let mut capture = capture_http1_assignment(&mut request);
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(2), client.request(request))
                .await
                .unwrap();
        let error = result.unwrap_err();
        assert!(
            format!("{error:?}").contains("UserRequestLease"),
            "did not reach selected HTTP2 rejection"
        );
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                capture.wait_for_assignment()
            )
            .await
            .unwrap()
            .is_none()
        );
        drop(error);
        drop(client);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !closed.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("HTTP2 client retained its IO");
        drop(peer);
    }
}
