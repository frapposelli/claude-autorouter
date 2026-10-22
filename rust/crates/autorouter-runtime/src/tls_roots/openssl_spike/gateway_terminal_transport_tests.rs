use super::*;
use bytes::Bytes;
use http_body_util::Full;
use hyper::Request;
use hyper_util::client::legacy::connect::{
    CaptureAssignment, Connected, bind_http1_reservation, capture_http1_assignment,
};
use std::sync::Barrier;
use std::time::Duration;

struct CapturedDrop {
    terminal: RequestTerminal,
    drops: Arc<AtomicUsize>,
}
impl Drop for CapturedDrop {
    fn drop(&mut self) {
        if let Some(shared) = self.terminal.shared.upgrade() {
            assert!(
                shared.state.try_lock().is_ok(),
                "capture dropped under registry lock"
            );
        }
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}
fn action(
    terminal: &RequestTerminal,
    calls: &Arc<AtomicUsize>,
    drops: &Arc<AtomicUsize>,
) -> impl FnOnce(FailureCause) + Send + 'static {
    let calls = calls.clone();
    let captured = CapturedDrop {
        terminal: terminal.clone(),
        drops: drops.clone(),
    };
    move |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        drop(captured);
    }
}
fn capture() -> (Request<Full<Bytes>>, Arc<CaptureAssignment>) {
    let mut request = Request::get("http://127.0.0.1:1/v1/models/owned")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let capture = Arc::new(capture_http1_assignment(&mut request));
    (request, capture)
}
#[tokio::test]
async fn binding_rejects_duplicates_failed_retired_and_closed_without_actions() {
    let connection = ConnectionTerminal::new(Probe::default(), 2);
    let handle = connection.handle();
    let owner = handle.register().unwrap();
    let terminal = owner.publisher();
    let calls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    assert!(
        terminal.admit_transport().is_err(),
        "unbound request admitted"
    );
    terminal
        .bind_transport_action(action(&terminal, &calls, &drops))
        .unwrap();
    assert!(
        terminal
            .bind_transport_action(action(&terminal, &calls, &drops))
            .is_err()
    );
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    terminal.admit_transport().unwrap();
    assert!(
        terminal.admit_transport().is_err(),
        "request admitted twice"
    );
    assert!(
        terminal
            .bind_transport_action(action(&terminal, &calls, &drops))
            .is_err()
    );
    drop(owner);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(drops.load(Ordering::SeqCst), 3);
    assert!(terminal.admit_transport().is_err());
    let failed = handle.register().unwrap();
    let publisher = failed.publisher();
    assert!(publisher.fail(FailureCause::Downstream(Disconnect::ReadEof)));
    assert!(
        publisher
            .bind_transport_action(action(&publisher, &calls, &drops))
            .is_err()
    );
    assert!(publisher.admit_transport().is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(drops.load(Ordering::SeqCst), 4);
    connection.close().await;
    assert!(
        publisher
            .bind_transport_action(action(&publisher, &calls, &drops))
            .is_err()
    );
    assert!(publisher.admit_transport().is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn admitted_action_precedes_cause_report_and_cancellation_outside_locks() {
    let probe = Probe::default();
    let connection = ConnectionTerminal::new(probe.clone(), 1);
    let owner = connection.handle().register().unwrap();
    let terminal = owner.publisher();
    let token = CancellationToken::new();
    let order = Arc::new(Mutex::new(Vec::new()));
    let cause_order = order.clone();
    let report_order = order.clone();
    let cause_token = token.clone();
    let report_token = token.clone();
    owner.start(
        Instant::now() + Duration::from_secs(60),
        token.clone(),
        move |_| {
            assert!(!cause_token.is_cancelled());
            cause_order.lock().unwrap().push("cause");
        },
        move |_| {
            assert!(!report_token.is_cancelled());
            report_order.lock().unwrap().push("report");
        },
    );
    owner.attach();
    let transport_order = order.clone();
    let transport_token = token.clone();
    let reentrant = terminal.clone();
    terminal
        .bind_transport_action(move |cause| {
            assert_eq!(cause, FailureCause::Downstream(Disconnect::WriteError));
            assert!(
                reentrant.failed(),
                "first cause was not published before callout"
            );
            assert!(!transport_token.is_cancelled());
            assert!(reentrant.shared.upgrade().unwrap().state.try_lock().is_ok());
            transport_order.lock().unwrap().push("transport");
        })
        .unwrap();
    terminal.admit_transport().unwrap();
    assert!(terminal.fail(FailureCause::Downstream(Disconnect::WriteError)));
    assert!(!terminal.fail(FailureCause::Deadline));
    assert_eq!(*order.lock().unwrap(), ["transport", "cause", "report"]);
    assert!(token.is_cancelled());
    assert!(!probe.observer_failed());
    assert_eq!(
        probe
            .events()
            .iter()
            .filter(|event| matches!(
                event,
                Event::Failed(_, FailureCause::Downstream(Disconnect::WriteError))
            ))
            .count(),
        1
    );
    drop(owner);
    connection.close().await;
    assert_eq!(probe.tasks(), 0);
}
#[tokio::test]
async fn failure_before_admission_claims_only_an_already_published_reservation() {
    for assigned_first in [false, true] {
        let connection = ConnectionTerminal::new(Probe::default(), 1);
        let owner = connection.handle().register().unwrap();
        let terminal = owner.publisher();
        let (mut request, capture) = capture();
        let taken = Arc::new(AtomicUsize::new(0));
        let recorded = taken.clone();
        let observed = capture.clone();
        terminal
            .bind_transport_action(move |_| {
                let claim = observed.assignment().and_then(|a| a.claim_abort());
                recorded.fetch_add(usize::from(claim.is_some()), Ordering::SeqCst);
            })
            .unwrap();
        let connected = Connected::new();
        let mut reservation = None;
        if assigned_first {
            reservation = bind_http1_reservation(&mut request, &connected).unwrap();
        }
        assert!(terminal.fail(FailureCause::Downstream(Disconnect::ReadEof)));
        if !assigned_first {
            reservation = bind_http1_reservation(&mut request, &connected).unwrap();
        }
        assert!(terminal.admit_transport().is_err());
        assert_eq!(taken.load(Ordering::SeqCst), usize::from(assigned_first));
        // The caller must retire this reservation before dropping its sender;
        // no send_request is invoked by this forced rejection schedule.
        drop(reservation);
        assert!(capture.assignment().unwrap().claim_abort().is_none());
        assert!(
            terminal
                .bind_transport_action(|_| panic!("late action ran"))
                .is_err()
        );
        drop(owner);
        connection.close().await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_and_first_failure_serialize_dispatch_without_replaying_actions() {
    for _ in 0..32 {
        let connection = ConnectionTerminal::new(Probe::default(), 1);
        let owner = connection.handle().register().unwrap();
        let terminal = owner.publisher();
        let calls = Arc::new(AtomicUsize::new(0));
        let record = calls.clone();
        terminal
            .bind_transport_action(move |cause| {
                assert_eq!(cause, FailureCause::Deadline);
                record.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let admit = terminal.clone();
        let fail = terminal.clone();
        let (admitted, failed) = std::thread::scope(|scope| {
            let other = barrier.clone();
            let a = scope.spawn(move || {
                other.wait();
                admit.admit_transport().is_ok()
            });
            let f = scope.spawn(move || {
                barrier.wait();
                fail.fail(FailureCause::Deadline)
            });
            (a.join().unwrap(), f.join().unwrap())
        });
        assert!(failed);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            terminal
                .shared
                .upgrade()
                .unwrap()
                .state
                .lock()
                .unwrap()
                .records
                .get(&terminal.identity.request)
                .unwrap()
                .transport_admitted,
            admitted
        );
        assert!(terminal.admit_transport().is_err());
        assert!(!terminal.fail(FailureCause::Downstream(Disconnect::ReadError)));
        drop(owner);
        connection.close().await;
    }
}
#[tokio::test]
async fn retirement_discards_a_action_and_stale_a_cannot_retarget_b() {
    let probe = Probe::default();
    let connection = ConnectionTerminal::new(probe.clone(), 1);
    let a = connection.handle().register().unwrap();
    let late = a.publisher();
    let a_calls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    late.bind_transport_action(action(&late, &a_calls, &drops))
        .unwrap();
    late.admit_transport().unwrap();
    a.attach();
    let mut completed = a.claim_delivery(Delivery::Flushed);
    completed.callback_finished();
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    let b = connection.handle().register().unwrap();
    let live = b.publisher();
    let b_calls = Arc::new(AtomicUsize::new(0));
    live.bind_transport_action(action(&live, &b_calls, &drops))
        .unwrap();
    live.admit_transport().unwrap();
    assert!(!late.fail(FailureCause::Deadline));
    assert!(late.admit_transport().is_err());
    assert_eq!(a_calls.load(Ordering::SeqCst), 0);
    assert_eq!(b_calls.load(Ordering::SeqCst), 0);
    assert!(live.fail(FailureCause::Deadline));
    assert_eq!(b_calls.load(Ordering::SeqCst), 1);
    drop(b);
    connection.close().await;
    assert_eq!(probe.tasks(), 0);
}
#[tokio::test]
async fn shared_idle_capture_observes_the_same_once_only_reservation() {
    let idle = super::super::idle_close::Probe::default();
    let io = idle.connected();
    let connection = ConnectionTerminal::new(Probe::default(), 1);
    let owner = connection.handle().register().unwrap();
    let terminal = owner.publisher();
    let (mut request, capture) = capture();
    let ordinal = idle.register_capture(capture.clone());
    let connection_metadata = Connected::new().extra(io.identity());
    let reservation = bind_http1_reservation(&mut request, &connection_metadata).unwrap();
    let (peer, observed) = idle.assignment(ordinal).unwrap();
    assert_eq!((ordinal, peer), (1, 1));
    let claims = Arc::new(AtomicUsize::new(0));
    let record = claims.clone();
    terminal
        .bind_transport_action(move |_| {
            if capture.assignment().unwrap().claim_abort().is_some() {
                record.fetch_add(1, Ordering::SeqCst);
            }
        })
        .unwrap();
    terminal.admit_transport().unwrap();
    assert!(terminal.fail(FailureCause::Downstream(Disconnect::ReadEof)));
    assert_eq!(claims.load(Ordering::SeqCst), 1);
    assert!(
        observed.claim_abort().is_none(),
        "observer got a replacement attachment"
    );
    assert!(
        !reservation.unwrap().retire(),
        "claimed reservation returned to pool"
    );
    assert_eq!(
        idle.rows(),
        [
            super::super::idle_close::Event::Connected(1),
            super::super::idle_close::Event::Submitted(1)
        ]
    );
    drop(owner);
    drop(io);
    assert_eq!(idle.live(), 0);
    connection.close().await;
}
#[tokio::test]
async fn panicking_action_is_diagnostic_and_cannot_skip_other_failure_callouts() {
    let probe = Probe::default();
    let connection = ConnectionTerminal::new(probe.clone(), 1);
    let owner = connection.handle().register().unwrap();
    let terminal = owner.publisher();
    let token = CancellationToken::new();
    let callbacks = Arc::new(AtomicUsize::new(0));
    let cause = callbacks.clone();
    let report = callbacks.clone();
    owner.start(
        Instant::now() + Duration::from_secs(60),
        token.clone(),
        move |_| {
            cause.fetch_add(1, Ordering::SeqCst);
        },
        move |_| {
            report.fetch_add(1, Ordering::SeqCst);
        },
    );
    owner.attach();
    terminal
        .bind_transport_action(|_| panic!("synthetic transport action panic"))
        .unwrap();
    terminal.admit_transport().unwrap();
    assert!(terminal.fail(FailureCause::Deadline));
    assert!(
        probe.observer_failed(),
        "transport panic was silently successful"
    );
    assert_eq!(callbacks.load(Ordering::SeqCst), 2);
    assert!(token.is_cancelled());
    drop(owner);
    connection.close().await;
    assert_eq!(probe.tasks(), 0);
}

// A worker always has a finite gate, and the test guard releases and joins it
// even if a pending/ownership assertion unwinds before normal release.
struct HeldSettlementAction {
    release: Arc<(Mutex<bool>, std::sync::Condvar)>,
    worker: Option<std::thread::JoinHandle<bool>>,
}
impl HeldSettlementAction {
    fn bind(terminal: &RequestTerminal) -> (Self, tokio::sync::oneshot::Receiver<()>) {
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let held = release.clone();
        let (entered, receiver) = tokio::sync::oneshot::channel();
        terminal
            .bind_transport_action(move |_| {
                let _ = entered.send(());
                let (mutex, changed) = &*held;
                let released = mutex.lock().unwrap();
                let (released, _) = changed
                    .wait_timeout_while(released, Duration::from_secs(5), |released| !*released)
                    .unwrap();
                assert!(*released, "held action exceeded its finite fixture bound");
            })
            .unwrap();
        terminal.admit_transport().unwrap();
        (
            Self {
                release,
                worker: None,
            },
            receiver,
        )
    }
    fn start(&mut self, terminal: &RequestTerminal, cause: FailureCause) {
        assert!(self.worker.is_none());
        let terminal = terminal.clone();
        self.worker = Some(std::thread::spawn(move || terminal.fail(cause)));
    }
    fn release(&self) {
        let (mutex, changed) = &*self.release;
        *mutex.lock().unwrap_or_else(|error| error.into_inner()) = true;
        changed.notify_all();
    }
    fn finish(mut self) {
        self.release();
        assert!(self.worker.take().unwrap().join().unwrap());
    }
}
impl Drop for HeldSettlementAction {
    fn drop(&mut self) {
        self.release();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[tokio::test]
async fn connection_failure_waits_for_every_dispatch_after_other_owner_retirement() {
    let probe = Probe::default();
    let connection = ConnectionTerminal::new(probe.clone(), 2);
    let a = connection.handle().register().unwrap();
    let b = connection.handle().register().unwrap();
    let first = a.publisher();
    let second = b.publisher();
    a.attach();
    b.attach();
    first.bind_transport_action(|_| {}).unwrap();
    first.admit_transport().unwrap();
    let (mut held, entered) = HeldSettlementAction::bind(&second);
    assert!(first.fail(FailureCause::Downstream(Disconnect::WriteError)));
    first.settle_failure().await;
    assert_eq!(
        connection.shared.state.lock().unwrap().close_latch,
        Some(first.identity)
    );
    held.start(&second, FailureCause::Deadline);
    tokio::time::timeout(Duration::from_secs(1), entered)
        .await
        .unwrap()
        .unwrap();
    let mut claim = Box::pin(connection.claim_connection_failure());
    assert!(
        std::future::poll_fn(|cx| Poll::Ready(std::future::Future::poll(claim.as_mut(), cx),))
            .await
            .is_pending(),
        "settled A bypassed held B"
    );
    drop(b);
    {
        let state = connection.shared.state.lock().unwrap();
        assert!(!state.records.contains_key(&second.identity.request));
        assert!(
            !state
                .inflight
                .get(&second.identity.request)
                .unwrap()
                .complete()
        );
        let retiring = state
            .inflight
            .keys()
            .filter(|request| !state.records.contains_key(request))
            .count();
        assert_eq!(state.records.len() + retiring, 2);
    }
    assert!(
        std::future::poll_fn(|cx| Poll::Ready(std::future::Future::poll(claim.as_mut(), cx),))
            .await
            .is_pending(),
        "retiring B bypassed its unfinished action"
    );
    held.finish();
    tokio::time::timeout(Duration::from_secs(1), claim.as_mut())
        .await
        .unwrap();
    drop(claim);
    assert_eq!(
        probe
            .events()
            .iter()
            .filter(|event| matches!(
                event, Event::ConnectionClaimed(identity) if *identity == first.identity,
            ))
            .count(),
        1
    );
    assert!(!probe.observer_failed());
    drop(a);
    connection.close().await;
    assert_eq!(probe.tasks(), 0);
}

#[tokio::test]
async fn failure_settlement_survives_retirement_close_and_wakes_all_waiters() {
    let probe = Probe::default();
    let connection = ConnectionTerminal::new(probe.clone(), 1);
    let handle = connection.handle();
    let owner = handle.register().unwrap();
    let terminal = owner.publisher();
    let (mut held, entered) = HeldSettlementAction::bind(&terminal);
    held.start(&terminal, FailureCause::Downstream(Disconnect::ReadEof));
    tokio::time::timeout(Duration::from_secs(1), entered)
        .await
        .unwrap()
        .unwrap();
    let mut waiters = tokio::task::JoinSet::new();
    let mut registered = Vec::new();
    for id in 0..2 {
        let terminal = terminal.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        registered.push(receiver);
        waiters.spawn(async move {
            let mut wait = Box::pin(terminal.settle_failure());
            let mut sender = Some(sender);
            std::future::poll_fn(|cx| {
                let result = std::future::Future::poll(wait.as_mut(), cx);
                if let Some(sender) = sender.take() {
                    assert!(result.is_pending(), "waiter missed the held first failure");
                    let _ = sender.send(());
                }
                result
            })
            .await;
            id
        });
    }
    for receiver in registered {
        tokio::time::timeout(Duration::from_secs(1), receiver)
            .await
            .unwrap()
            .unwrap();
    }
    drop(owner);
    {
        let state = connection.shared.state.lock().unwrap();
        assert!(state.records.is_empty());
        assert!(
            state.close_latch.is_none(),
            "capacity check must not rely on a close latch"
        );
        assert_eq!(state.inflight.len(), 1);
    }
    assert!(
        handle.register().is_err(),
        "retired in-flight request lost its capacity charge"
    );
    assert!(waiters.try_join_next().is_none());
    connection.close().await;
    assert_eq!(probe.tasks(), 0);
    let mut after_close = Box::pin(terminal.settle_failure());
    assert!(
        std::future::poll_fn(
            |cx| Poll::Ready(std::future::Future::poll(after_close.as_mut(), cx),)
        )
        .await
        .is_pending(),
        "closed/removed record was mistaken for settled action"
    );
    assert!(waiters.try_join_next().is_none());
    held.finish();
    let mut completed = Vec::new();
    for _ in 0..2 {
        completed.push(
            tokio::time::timeout(Duration::from_secs(1), waiters.join_next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
        );
    }
    completed.sort_unstable();
    assert_eq!(completed, [0, 1]);
    tokio::time::timeout(Duration::from_secs(1), after_close.as_mut())
        .await
        .unwrap();
    assert!(!probe.observer_failed());
}

#[tokio::test]
async fn transport_retirement_and_failure_keep_one_ordered_capability() {
    let probe = Probe::default();
    let connection = ConnectionTerminal::new(probe.clone(), 1);
    let owner = connection.handle().register().unwrap();
    let terminal = owner.publisher();
    let calls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    terminal
        .bind_transport_action(action(&terminal, &calls, &drops))
        .unwrap();
    terminal.admit_transport().unwrap();
    assert!(terminal.retire_transport_action());
    assert_eq!(
        drops.load(Ordering::SeqCst),
        1,
        "retirement must discard captures outside the lock"
    );
    assert!(
        terminal
            .bind_transport_action(action(&terminal, &calls, &drops))
            .is_err()
    );
    assert!(terminal.admit_transport().is_err());
    assert!(terminal.fail(FailureCause::Deadline));
    terminal.settle_failure().await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "failure retargeted retired authority"
    );
    assert_eq!(drops.load(Ordering::SeqCst), 2);
    assert!(terminal.retire_transport_action());
    drop(owner);
    connection.close().await;
    assert!(!probe.observer_failed());

    let probe = Probe::default();
    let connection = ConnectionTerminal::new(probe.clone(), 1);
    let owner = connection.handle().register().unwrap();
    let terminal = owner.publisher();
    let (mut held, entered) = HeldSettlementAction::bind(&terminal);
    held.start(&terminal, FailureCause::Downstream(Disconnect::ReadEof));
    tokio::time::timeout(Duration::from_secs(1), entered)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !terminal.retire_transport_action(),
        "retirement bypassed a first failure"
    );
    drop(owner);
    assert!(
        !terminal.retire_transport_action(),
        "record removal lost the pending action"
    );
    held.finish();
    terminal.settle_failure().await;
    assert!(terminal.retire_transport_action());
    connection.close().await;
    assert!(!probe.observer_failed());
    assert_eq!(probe.tasks(), 0);
}

#[tokio::test]
async fn late_start_and_attach_wait_for_the_first_transport_action() {
    for start_late in [false, true] {
        let probe = Probe::default();
        let connection = ConnectionTerminal::new(probe.clone(), 1);
        let owner = connection.handle().register().unwrap();
        let terminal = owner.publisher();
        let token = CancellationToken::new();
        let causes = Arc::new(AtomicUsize::new(0));
        let reports = Arc::new(AtomicUsize::new(0));
        let start = || {
            let causes = causes.clone();
            let reports = reports.clone();
            owner.start(
                Instant::now() + Duration::from_secs(60),
                token.clone(),
                move |_| {
                    causes.fetch_add(1, Ordering::SeqCst);
                },
                move |_| {
                    reports.fetch_add(1, Ordering::SeqCst);
                },
            );
        };
        if !start_late {
            start();
        } else {
            owner.attach();
        }
        let (mut held, entered) = HeldSettlementAction::bind(&terminal);
        held.start(&terminal, FailureCause::Deadline);
        tokio::time::timeout(Duration::from_secs(1), entered)
            .await
            .unwrap()
            .unwrap();
        if start_late {
            start();
        } else {
            owner.attach();
        }
        assert_eq!(causes.load(Ordering::SeqCst), 0);
        assert_eq!(reports.load(Ordering::SeqCst), 0);
        assert!(
            !token.is_cancelled(),
            "late callback bypassed the first action"
        );
        held.finish();
        terminal.settle_failure().await;
        assert_eq!(causes.load(Ordering::SeqCst), 1);
        assert_eq!(reports.load(Ordering::SeqCst), 1);
        assert!(token.is_cancelled());
        assert!(!probe.observer_failed());
        drop(owner);
        connection.close().await;
        assert_eq!(probe.tasks(), 0);
    }
}
