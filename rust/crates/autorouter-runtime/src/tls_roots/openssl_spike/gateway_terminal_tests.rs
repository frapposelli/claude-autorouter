use super::*;
use std::future::poll_fn;
use std::sync::Barrier;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
const LIMIT: Duration = Duration::from_secs(2);
async fn claimed(owner: &ConnectionTerminal) {
    tokio::time::timeout(LIMIT, owner.claim_connection_failure())
        .await
        .unwrap();
}
async fn not_claimed(owner: &ConnectionTerminal) {
    let future = owner.claim_connection_failure();
    tokio::pin!(future);
    assert!(
        poll_fn(|cx| Poll::Ready(std::future::Future::poll(future.as_mut(), cx)))
            .await
            .is_pending()
    );
}
fn start(owner: &RequestOwner, token: CancellationToken) {
    owner.start(
        Instant::now() + Duration::from_secs(60),
        token,
        |_| {},
        |_| {},
    );
}
#[tokio::test]
async fn explicit_admission_owner_retires_every_early_return_and_never_reuses_generation() {
    let probe = Probe::default();
    let connection = ConnectionTerminal::new(probe.clone(), 1);
    let handle = connection.handle();
    let first = handle.register().unwrap();
    let old = first.publisher();
    assert!(handle.register().is_err());
    drop(first);
    let second = handle.register().unwrap();
    assert_ne!(old.identity, second.identity);
    assert!(!old.fail(FailureCause::Upstream));
    drop(second);
    not_claimed(&connection).await;
    connection.close().await;
    assert_eq!(probe.tasks(), 0);
    assert!(handle.register().is_err());
}
#[tokio::test]
async fn preattach_body_failure_stays_latched_until_successful_response_attachment() {
    let probe = Probe::default();
    let connection = ConnectionTerminal::new(probe.clone(), 2);
    let request = connection.handle().register().unwrap();
    start(&request, CancellationToken::new());
    assert!(request.publisher().fail(FailureCause::Upstream));
    not_claimed(&connection).await;
    request.attach();
    claimed(&connection).await;
    let mut claim = request.claim_delivery(Delivery::Flushed);
    assert_eq!(claim.delivery(), Delivery::Failed(Failure::Body));
    claim.callback_finished();
    connection.close().await;
    assert_eq!(probe.tasks(), 0);
}
#[tokio::test]
async fn pending_acquisition_failure_retirement_does_not_force_a_downstream_close() {
    let connection = ConnectionTerminal::new(Probe::default(), 1);
    let request = connection.handle().register().unwrap();
    start(&request, CancellationToken::new());
    request.publisher().fail(FailureCause::Upstream);
    drop(request);
    not_claimed(&connection).await;
    drop(connection.handle().register().unwrap());
    connection.close().await;
}
#[tokio::test]
async fn failed_attached_record_survives_owner_drop_and_notification_coalescing() {
    let connection = ConnectionTerminal::new(Probe::default(), 16);
    let mut owners = Vec::new();
    for _ in 0..16 {
        let owner = connection.handle().register().unwrap();
        owner.attach();
        owners.push(owner);
    }
    for owner in owners {
        owner.publisher().fail(FailureCause::Upstream);
        drop(owner);
    }
    claimed(&connection).await;
    assert!(connection.handle().register().is_err());
    connection.close().await;
}
#[tokio::test]
async fn delivered_a_retires_before_late_signal_and_cannot_close_attached_b() {
    let connection = ConnectionTerminal::new(Probe::default(), 1);
    let a = connection.handle().register().unwrap();
    a.attach();
    let late = a.publisher();
    let mut claim = a.claim_delivery(Delivery::Flushed);
    assert_eq!(claim.delivery(), Delivery::Flushed);
    claim.callback_finished();
    let b = connection.handle().register().unwrap();
    b.attach();
    assert!(!late.fail(FailureCause::Upstream));
    not_claimed(&connection).await;
    let mut claim = b.claim_delivery(Delivery::Flushed);
    assert_eq!(claim.delivery(), Delivery::Flushed);
    claim.callback_finished();
    connection.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failure_and_flush_callback_atomically_claim_one_outcome() {
    for _ in 0..32 {
        let connection = ConnectionTerminal::new(Probe::default(), 1);
        let request = connection.handle().register().unwrap();
        request.attach();
        let publisher = request.publisher();
        let barrier = Arc::new(Barrier::new(2));
        let (failed, claim) = std::thread::scope(|scope| {
            let other = barrier.clone();
            let failure = scope.spawn(move || {
                other.wait();
                publisher.fail(FailureCause::Upstream)
            });
            let delivery = scope.spawn(move || {
                barrier.wait();
                request.claim_delivery(Delivery::Flushed)
            });
            (failure.join().unwrap(), delivery.join().unwrap())
        });
        assert_eq!(
            claim.delivery(),
            if failed {
                Delivery::Failed(Failure::Body)
            } else {
                Delivery::Flushed
            }
        );
        if failed {
            claimed(&connection).await;
        } else {
            not_claimed(&connection).await;
        }
        drop(claim);
        connection.close().await;
    }
}
#[tokio::test]
async fn callback_panic_after_claim_records_abandonment_without_reviving_authority() {
    let probe = Probe::default();
    let connection = ConnectionTerminal::new(probe.clone(), 1);
    let owner = connection.handle().register().unwrap();
    owner.attach();
    let late = owner.publisher();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _claim = owner.claim_delivery(Delivery::Flushed);
        panic!("synthetic delivery callback panic");
    }));
    assert!(
        probe
            .events()
            .iter()
            .any(|event| matches!(event, Event::CallbackAbandoned(_)))
    );
    assert!(!late.fail(FailureCause::Upstream));
    not_claimed(&connection).await;
    connection.close().await;
}
#[tokio::test]
async fn reporting_panics_and_reentrant_observers_cannot_suppress_failure_notification() {
    let probe = Probe::default();
    let connection = ConnectionTerminal::new(probe.clone(), 1);
    let owner = connection.handle().register().unwrap();
    let publisher = owner.publisher();
    let reentrant = publisher.clone();
    owner.start(
        Instant::now() + Duration::from_secs(60),
        CancellationToken::new(),
        move |_| {
            assert!(reentrant.failed());
            panic!("synthetic cause observer panic");
        },
        |_| panic!("synthetic report observer panic"),
    );
    owner.attach();
    assert!(publisher.fail(FailureCause::Upstream));
    claimed(&connection).await;
    drop(owner);
    connection.close().await;
    assert_eq!(probe.tasks(), 0);
}
#[tokio::test]
async fn independent_deadline_needs_no_response_body_poll() {
    tokio::time::pause();
    let probe = Probe::default();
    let connection = ConnectionTerminal::new(probe.clone(), 1);
    let owner = connection.handle().register().unwrap();
    owner.start(
        Instant::now() + Duration::from_millis(10),
        CancellationToken::new(),
        |_| {},
        |_| {},
    );
    owner.attach();
    tokio::time::advance(Duration::from_millis(11)).await;
    claimed(&connection).await;
    assert!(
        probe
            .events()
            .iter()
            .any(|event| matches!(event, Event::Failed(_, FailureCause::Deadline)))
    );
    drop(owner);
    connection.close().await;
    assert_eq!(probe.tasks(), 0);
}
#[tokio::test]
async fn cancellation_wakes_the_single_connection_monitor_before_its_later_deadline() {
    let probe = Probe::default();
    let connection = ConnectionTerminal::new(probe.clone(), 1);
    let owner = connection.handle().register().unwrap();
    let token = CancellationToken::new();
    start(&owner, token.clone());
    owner.attach();
    tokio::task::yield_now().await;
    token.cancel();
    claimed(&connection).await;
    assert!(
        probe
            .events()
            .iter()
            .any(|event| matches!(event, Event::Failed(_, FailureCause::Cancelled)))
    );
    drop(owner);
    connection.close().await;
    assert_eq!(probe.tasks(), 0);
}
#[tokio::test]
async fn physical_downstream_eof_is_latched_before_abort_induced_upstream_failure() {
    let probe = Probe::default();
    let connection = ConnectionTerminal::new(probe.clone(), 1);
    let owner = connection.handle().register().unwrap();
    let publisher = owner.publisher();
    owner.start(
        Instant::now() + Duration::from_secs(60),
        CancellationToken::new(),
        move |cause| {
            assert_eq!(cause, FailureCause::Downstream(Disconnect::ReadEof));
            assert!(!publisher.fail(FailureCause::Upstream));
        },
        |_| {},
    );
    owner.attach();
    let (stream, peer) = tokio::io::duplex(16);
    drop(peer);
    let mut stream = TerminalIo::new(stream, Some(connection.handle()));
    assert_eq!(stream.read(&mut [0; 1]).await.unwrap(), 0);
    claimed(&connection).await;
    let first = probe
        .events()
        .into_iter()
        .find(|event| matches!(event, Event::Failed(_, _)))
        .unwrap();
    assert!(matches!(
        first,
        Event::Failed(_, FailureCause::Downstream(Disconnect::ReadEof))
    ));
    drop(owner);
    connection.close().await;
}
#[tokio::test]
async fn physical_downstream_write_error_preserves_exact_io_failure() {
    let connection = ConnectionTerminal::new(Probe::default(), 1);
    let owner = connection.handle().register().unwrap();
    owner.attach();
    let (stream, peer) = tokio::io::duplex(16);
    drop(peer);
    let mut stream = TerminalIo::new(stream, Some(connection.handle()));
    assert_eq!(
        stream.write_all(b"x").await.unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    claimed(&connection).await;
    drop(owner);
    connection.close().await;
}
#[tokio::test]
async fn failed_b_fences_pending_a_flush_but_an_independent_connection_survives() {
    let connection = ConnectionTerminal::new(Probe::default(), 2);
    let a = connection.handle().register().unwrap();
    let b = connection.handle().register().unwrap();
    a.attach();
    b.attach();
    let sibling = ConnectionTerminal::new(Probe::default(), 1);
    let c = sibling.handle().register().unwrap();
    c.attach();
    b.publisher().fail(FailureCause::Upstream);
    assert_eq!(
        a.claim_delivery(Delivery::Flushed).delivery(),
        Delivery::Failed(Failure::Body)
    );
    assert_eq!(
        c.claim_delivery(Delivery::Flushed).delivery(),
        Delivery::Flushed
    );
    claimed(&connection).await;
    not_claimed(&sibling).await;
    drop(b);
    connection.close().await;
    sibling.close().await;
}

#[tokio::test]
async fn acquired_header_body_error_does_not_cancel_request_until_attachment() {
    let connection = ConnectionTerminal::new(Probe::default(), 1);
    let owner = connection.handle().register().unwrap();
    let token = CancellationToken::new();
    start(&owner, token.clone());
    owner.publisher().fail(FailureCause::Upstream);
    assert!(
        !token.is_cancelled(),
        "do not turn an acquired response into a cancelled request"
    );
    not_claimed(&connection).await;
    owner.attach();
    assert!(token.is_cancelled());
    claimed(&connection).await;
    drop(owner);
    connection.close().await;
}
