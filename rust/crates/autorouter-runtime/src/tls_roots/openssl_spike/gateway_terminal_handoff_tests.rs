use super::*;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;
const LIMIT: Duration = Duration::from_secs(2);
fn setup() -> (
    ConnectionTerminal,
    RequestOwner,
    DataProducerOwner,
    CancellationToken,
) {
    let connection = ConnectionTerminal::new(Probe::default(), 16);
    let request = connection.handle().register().unwrap();
    request.attach();
    let stop = CancellationToken::new();
    let producer = request.publisher().producer(stop.clone()).unwrap();
    (connection, request, producer, stop)
}
fn observe(producer: &DataProducerOwner, event: Observation) {
    producer
        .terminal
        .observe_handoff(Some(producer.epoch), event);
}
fn submitted(producer: &DataProducerOwner, bytes: usize, total: u64) {
    observe(
        producer,
        Observation::DataSubmitted {
            input_bytes: bytes,
            total_input_bytes: total,
        },
    );
}
fn flush(producer: &DataProducerOwner, total: u64, outcome: NodeHttpFlushPoll) {
    observe(
        producer,
        Observation::FlushPolled {
            total_input_bytes: total,
            outcome,
        },
    );
}
async fn pending<F: Future>(mut future: Pin<&mut F>) {
    assert!(
        poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx)))
            .await
            .is_pending()
    );
}
async fn finish<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(LIMIT, future)
        .await
        .expect("bounded handoff control")
}

#[tokio::test]
async fn submission_alone_cannot_ack_and_cumulative_bytes_survive_block_splitting() {
    let (connection, request, producer, _) = setup();
    let ticket = producer.ticket(32769).unwrap();
    let mut wait = Box::pin(ticket.wait());
    pending(wait.as_mut()).await;
    submitted(&producer, 16384, 16384);
    submitted(&producer, 16384, 32768);
    flush(&producer, 32768, NodeHttpFlushPoll::Ready);
    pending(wait.as_mut()).await;
    submitted(&producer, 1, 32769);
    pending(wait.as_mut()).await;
    flush(&producer, 32769, NodeHttpFlushPoll::Ready);
    assert!(finish(wait).await.is_ok());
    drop((producer, request));
    connection.close().await;
}
#[tokio::test]
async fn registration_after_observed_target_does_not_lose_ack() {
    let (connection, request, producer, _) = setup();
    submitted(&producer, 3, 3);
    flush(&producer, 3, NodeHttpFlushPoll::Ready);
    assert!(finish(producer.ticket(3).unwrap().wait()).await.is_ok());
    drop((producer, request));
    connection.close().await;
}
#[tokio::test]
async fn duplicate_registration_preserves_first_slot_and_drop_is_sequence_specific() {
    let (connection, request, producer, _) = setup();
    let first = producer.ticket(3).unwrap();
    assert!(producer.ticket(1).is_err());
    submitted(&producer, 3, 3);
    flush(&producer, 3, NodeHttpFlushPoll::Ready);
    let second = producer.ticket(2).unwrap();
    drop(first);
    let mut wait = Box::pin(second.wait());
    pending(wait.as_mut()).await;
    submitted(&producer, 2, 5);
    flush(&producer, 5, NodeHttpFlushPoll::Ready);
    assert!(finish(wait).await.is_ok());
    drop((producer, request));
    connection.close().await;
}
#[tokio::test]
async fn unpolled_ticket_drop_removes_only_its_owned_slot() {
    let (connection, request, producer, _) = setup();
    drop(producer.ticket(3).unwrap());
    let next = producer.ticket(2).unwrap();
    submitted(&producer, 5, 5);
    flush(&producer, 5, NodeHttpFlushPoll::Ready);
    assert!(finish(next.wait()).await.is_ok());
    drop((producer, request));
    connection.close().await;
}
#[tokio::test]
async fn cancellation_before_registration_pending_and_after_ack_always_wins_next_poll() {
    for stage in 0..3 {
        let (connection, request, producer, stop) = setup();
        if stage == 0 {
            stop.cancel();
            assert!(producer.ticket(1).is_err());
        } else {
            let ticket = producer.ticket(1).unwrap();
            if stage == 2 {
                submitted(&producer, 1, 1);
                flush(&producer, 1, NodeHttpFlushPoll::Ready);
            }
            stop.cancel();
            assert!(finish(ticket.wait()).await.is_err());
            assert!(!producer.valid());
        }
        drop((producer, request));
        connection.close().await;
    }
}
#[tokio::test]
async fn queued_epoch_revalidation_preserves_same_target_without_double_counting() {
    let (connection, request, producer, _) = setup();
    let first = producer.ticket(1).unwrap();
    observe(&producer, Observation::Queued);
    observe(&producer, Observation::Active);
    let mut first = Box::pin(first.wait());
    pending(first.as_mut()).await;
    submitted(&producer, 1, 1);
    flush(&producer, 1, NodeHttpFlushPoll::Ready);
    assert!(finish(first).await.is_ok());
    let mut next = Box::pin(producer.ticket(1).unwrap().wait());
    pending(next.as_mut()).await;
    submitted(&producer, 1, 2);
    flush(&producer, 2, NodeHttpFlushPoll::Ready);
    assert!(finish(next).await.is_ok());
    drop((producer, request));
    connection.close().await;
}
#[tokio::test]
async fn writer_pending_is_pause_authority_until_a_later_ready_poll() {
    let (connection, request, producer, _) = setup();
    flush(&producer, 0, NodeHttpFlushPoll::Pending);
    assert!(finish(producer.ticket(1).unwrap().wait()).await.is_ok());
    flush(&producer, 0, NodeHttpFlushPoll::Ready);
    let mut next = Box::pin(producer.ticket(1).unwrap().wait());
    pending(next.as_mut()).await;
    submitted(&producer, 2, 2);
    flush(&producer, 2, NodeHttpFlushPoll::Ready);
    assert!(finish(next).await.is_ok());
    drop((producer, request));
    connection.close().await;
}
#[tokio::test]
async fn explicit_gate_release_and_rehold_retire_exact_pause_epochs() {
    for kind in [PauseKind::Body, PauseKind::Response] {
        let (connection, request, producer, _) = setup();
        let gate = PauseGate::default();
        let registration = gate.bind(request.publisher(), kind).unwrap();
        gate.hold();
        assert!(gate.held());
        assert!(finish(producer.ticket(1).unwrap().wait()).await.is_ok());
        gate.release();
        assert!(!gate.held());
        let mut next = Box::pin(producer.ticket(1).unwrap().wait());
        pending(next.as_mut()).await;
        gate.hold();
        assert!(finish(next).await.is_ok());
        gate.release();
        let third = producer.ticket(1).unwrap();
        drop(registration);
        submitted(&producer, 3, 3);
        flush(&producer, 3, NodeHttpFlushPoll::Ready);
        assert!(finish(third.wait()).await.is_ok());
        drop((producer, request));
        connection.close().await;
    }
}
#[tokio::test]
async fn producer_seal_and_old_a_events_cannot_touch_new_b() {
    let (connection, a, mut producer, _) = setup();
    producer.seal();
    assert!(producer.ticket(1).is_err());
    assert!(
        producer
            .terminal
            .producer(CancellationToken::new())
            .is_err()
    );
    for event in [
        Observation::Active,
        Observation::Queued,
        Observation::FlushPolled {
            total_input_bytes: 0,
            outcome: NodeHttpFlushPoll::Failed,
        },
    ] {
        observe(&producer, event);
    }
    assert!(!a.publisher().failed());
    let mut delivery = a.claim_delivery(Delivery::Flushed);
    assert_eq!(delivery.delivery(), Delivery::Flushed);
    delivery.callback_finished();
    let b = connection.handle().register().unwrap();
    b.attach();
    let next = b.publisher().producer(CancellationToken::new()).unwrap();
    let mut wait = Box::pin(next.ticket(1).unwrap().wait());
    for event in [
        Observation::Active,
        Observation::Queued,
        Observation::DataSubmitted {
            input_bytes: 1,
            total_input_bytes: 1,
        },
        Observation::FlushPolled {
            total_input_bytes: 1,
            outcome: NodeHttpFlushPoll::Ready,
        },
        Observation::FlushPolled {
            total_input_bytes: 1,
            outcome: NodeHttpFlushPoll::Failed,
        },
    ] {
        observe(&producer, event);
    }
    pending(wait.as_mut()).await;
    assert!(!b.publisher().failed());
    submitted(&next, 1, 1);
    flush(&next, 1, NodeHttpFlushPoll::Ready);
    assert!(finish(wait).await.is_ok());
    drop((producer, next, b));
    connection.close().await;
}
#[tokio::test]
async fn failure_while_writer_pending_cannot_fall_through_to_ack() {
    let (connection, request, producer, _) = setup();
    flush(&producer, 0, NodeHttpFlushPoll::Pending);
    flush(&producer, 0, NodeHttpFlushPoll::Failed);
    assert!(request.publisher().failed());
    assert!(producer.ticket(1).is_err());
    let claim = request.claim_delivery(Delivery::Flushed);
    assert_eq!(claim.delivery(), Delivery::Failed(Failure::Body));
    drop((producer, claim));
    connection.close().await;
}
#[tokio::test]
async fn a_flush_cannot_ack_b_even_when_b_is_queued_at_index_zero() {
    let (connection, a, producer, _) = setup();
    let b = connection.handle().register().unwrap();
    b.attach();
    let next = b.publisher().producer(CancellationToken::new()).unwrap();
    let mut wait = Box::pin(next.ticket(1).unwrap().wait());
    submitted(&producer, 1, 1);
    flush(&producer, 1, NodeHttpFlushPoll::Ready);
    pending(wait.as_mut()).await;
    observe(&next, Observation::Queued);
    assert!(finish(wait).await.is_ok());
    drop((producer, next, a, b));
    connection.close().await;
}
#[tokio::test(flavor = "current_thread")]
async fn close_cancelled_at_first_pending_and_owner_drop_release_ticket_and_monitor() {
    for explicit in [false, true] {
        let (connection, request, producer, _) = setup();
        let probe = connection.shared.probe.clone();
        let ticket = producer.ticket(1).unwrap();
        if explicit {
            let mut close = Box::pin(connection.close());
            pending(close.as_mut()).await;
            drop(close);
        } else {
            drop(connection);
        }
        assert!(finish(ticket.wait()).await.is_err());
        drop((producer, request));
        finish(async {
            while probe.tasks() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await;
    }
}
#[tokio::test]
async fn sixteen_records_and_failure_notification_saturation_leave_no_pending_tickets() {
    let connection = ConnectionTerminal::new(Probe::default(), 16);
    let probe = connection.shared.probe.clone();
    let mut held = Vec::new();
    for _ in 0..16 {
        let request = connection.handle().register().unwrap();
        request.attach();
        let producer = request
            .publisher()
            .producer(CancellationToken::new())
            .unwrap();
        let ticket = producer.ticket(1).unwrap();
        held.push((request, producer, ticket));
    }
    assert!(connection.handle().register().is_err());
    for (request, _, _) in &held {
        request.publisher().fail(FailureCause::Upstream);
    }
    for (request, producer, ticket) in held {
        assert!(finish(ticket.wait()).await.is_err());
        drop((request, producer));
    }
    connection.close().await;
    assert_eq!(probe.tasks(), 0);
}
struct PanickingWake {
    shared: Weak<Shared>,
    calls: Arc<AtomicUsize>,
}
impl Wake for PanickingWake {
    fn wake(self: Arc<Self>) {
        assert!(
            self.shared.upgrade().unwrap().state.try_lock().is_ok(),
            "waker under outcome lock"
        );
        self.calls.fetch_add(1, Ordering::SeqCst);
        panic!("synthetic handoff waker panic");
    }
}
#[tokio::test]
async fn ack_waker_panic_latches_failure_outside_lock_and_cleanup_survives() {
    let (connection, request, producer, _) = setup();
    let mut wait = Box::pin(producer.ticket(1).unwrap().wait());
    let calls = Arc::new(AtomicUsize::new(0));
    let waker = Waker::from(Arc::new(PanickingWake {
        shared: Arc::downgrade(&connection.shared),
        calls: calls.clone(),
    }));
    assert!(
        wait.as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    submitted(&producer, 1, 1);
    flush(&producer, 1, NodeHttpFlushPoll::Ready);
    // Both the oneshot and cancellation notification may wake the same task.
    assert!(calls.load(Ordering::SeqCst) >= 1);
    assert!(request.publisher().failed());
    assert!(finish(wait).await.is_err());
    drop((producer, request));
    connection.close().await;
}
#[tokio::test]
async fn cancellation_and_connection_stop_waker_panics_cannot_skip_monitor_cleanup() {
    for connection_stop in [false, true] {
        let (connection, request, producer, stop) = setup();
        let probe = connection.shared.probe.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let waker = Waker::from(Arc::new(PanickingWake {
            shared: Arc::downgrade(&connection.shared),
            calls: calls.clone(),
        }));
        let token = if connection_stop {
            connection.shared.stop.clone()
        } else {
            stop
        };
        let mut cancelled = Box::pin(token.cancelled());
        assert!(
            cancelled
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        let ticket = producer.ticket(1).unwrap();
        connection.close().await;
        assert!(finish(ticket.wait()).await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(probe.tasks(), 0);
        drop((cancelled, producer, request));
    }
}
#[tokio::test]
async fn counter_overflow_and_invalid_future_boundary_fail_closed() {
    for phase in [false, true] {
        let (connection, request, producer, _) = setup();
        let ticket = producer.ticket(1).unwrap();
        if phase {
            connection
                .shared
                .state
                .lock()
                .unwrap()
                .records
                .get_mut(&request.identity.request)
                .unwrap()
                .handoff
                .phase_epoch = u64::MAX;
            observe(&producer, Observation::Active);
        } else {
            flush(&producer, 1, NodeHttpFlushPoll::Ready);
        }
        assert!(finish(ticket.wait()).await.is_err());
        assert!(request.publisher().failed());
        drop((producer, request));
        connection.close().await;
    }
}
#[tokio::test]
async fn source_error_first_cause_and_delivery_claim_remain_independent_of_ack() {
    let (connection, request, producer, _) = setup();
    let pause = request.publisher().pause(PauseKind::Body).unwrap();
    assert!(finish(producer.ticket(1).unwrap().wait()).await.is_ok());
    assert!(request.publisher().fail(FailureCause::Upstream));
    flush(&producer, 0, NodeHttpFlushPoll::Failed);
    let claim = request.claim_delivery(Delivery::Flushed);
    assert_eq!(claim.delivery(), Delivery::Failed(Failure::Body));
    assert!(
        connection
            .shared
            .probe
            .events()
            .iter()
            .any(|e| matches!(e, Event::Failed(_, FailureCause::Upstream)))
    );
    drop((pause, claim, producer));
    connection.close().await;
}

async fn stale_pause_ack_needs_current_authority(kind: usize) {
    let (connection, request, producer, _) = setup();
    let ticket = producer.ticket(1).unwrap();
    match kind {
        0 => {
            let pause = request.publisher().pause(PauseKind::Body).unwrap();
            drop(pause);
        }
        1 => {
            flush(&producer, 0, NodeHttpFlushPoll::Pending);
            flush(&producer, 0, NodeHttpFlushPoll::Ready);
        }
        _ => {
            observe(&producer, Observation::Queued);
            observe(&producer, Observation::Active);
        }
    }
    let mut wait = Box::pin(ticket.wait());
    pending(wait.as_mut()).await;
    submitted(&producer, 1, 1);
    flush(&producer, 1, NodeHttpFlushPoll::Ready);
    assert!(finish(wait).await.is_ok());
    drop((producer, request));
    connection.close().await;
}
#[tokio::test]
async fn stale_gate_ack_after_release_cannot_authorize_the_next_source_poll() {
    stale_pause_ack_needs_current_authority(0).await;
}
#[tokio::test]
async fn stale_writer_pending_ack_after_ready_below_target_must_wait() {
    stale_pause_ack_needs_current_authority(1).await;
}
#[tokio::test]
async fn stale_queued_ack_after_active_must_wait_for_its_byte_target() {
    stale_pause_ack_needs_current_authority(2).await;
}

#[tokio::test]
async fn cached_permission_is_revalidated_after_source_pending_and_gate_rehold() {
    let (connection, request, producer, _) = setup();
    let mut ticket = producer.ticket(1).unwrap();
    let pause = request.publisher().pause(PauseKind::Body).unwrap();
    assert!(poll_fn(|cx| ticket.poll_permission(cx)).await.is_ok());
    drop(pause);
    assert!(
        poll_fn(|cx| Poll::Ready(ticket.poll_permission(cx)))
            .await
            .is_pending()
    );
    let pause = request.publisher().pause(PauseKind::Body).unwrap();
    assert!(poll_fn(|cx| ticket.poll_permission(cx)).await.is_ok());
    drop(pause);
    assert!(
        poll_fn(|cx| Poll::Ready(ticket.poll_permission(cx)))
            .await
            .is_pending()
    );
    submitted(&producer, 1, 1);
    flush(&producer, 1, NodeHttpFlushPoll::Ready);
    assert!(poll_fn(|cx| ticket.poll_permission(cx)).await.is_ok());
    // The same byte target and sequence were rearmed; no copied data was added.
    let shared = producer.terminal.shared.upgrade().unwrap();
    {
        let state = shared.state.lock().unwrap();
        let p = state.records[&request.identity.request]
            .handoff
            .producer
            .as_ref()
            .unwrap();
        assert_eq!((p.published, p.next_sequence), (1, 1));
    }
    drop((ticket, producer, request));
    connection.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn real_buffer_producer_revalidates_pause_before_repolling_a_pending_source() {
    use super::super::super::buffered_body;
    use bytes::Bytes;
    use hyper::body::{Body, Frame};
    #[derive(Default)]
    struct SourceState {
        polls: usize,
        ready: bool,
        waker: Option<Waker>,
    }
    struct Source(Arc<Mutex<SourceState>>, Arc<Notify>);
    impl Body for Source {
        type Data = Bytes;
        type Error = ();
        fn poll_frame(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, ()>>> {
            let mut state = self.0.lock().unwrap();
            state.polls += 1;
            if state.polls == 1 {
                return Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"x")))));
            }
            if state.ready {
                return Poll::Ready(Some(Err(())));
            }
            state.waker = Some(cx.waker().clone());
            drop(state);
            self.1.notify_one();
            Poll::Pending
        }
    }
    let connection = ConnectionTerminal::new(Probe::default(), 16);
    let request = connection.handle().register().unwrap();
    request.attach();
    let terminal = request.publisher();
    let gate = PauseGate::default();
    let registration = gate.bind(terminal.clone(), PauseKind::Body).unwrap();
    gate.hold();
    let source = Arc::new(Mutex::new(SourceState::default()));
    let pending_source = Arc::new(Notify::new());
    let (body, producer, probe) = buffered_body::start_with_handoff(
        Source(source.clone(), pending_source.clone()),
        |_| {},
        Some(terminal.clone()),
    );
    finish(pending_source.notified()).await;
    gate.release();
    let waker = {
        let mut state = source.lock().unwrap();
        state.ready = true;
        state.waker.take().unwrap()
    };
    waker.wake();
    // The next producer turn must rearm its existing ticket, without invoking
    // the now-ready source. Observe registration itself, not a scheduling delay.
    finish(async {
        loop {
            let registered = connection.shared.state.lock().unwrap().records
                [&request.identity.request]
                .handoff
                .producer
                .as_ref()
                .unwrap()
                .pending
                .is_some();
            if registered {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert_eq!(source.lock().unwrap().polls, 2);
    let epoch = connection.shared.state.lock().unwrap().records[&request.identity.request]
        .handoff
        .producer
        .as_ref()
        .unwrap()
        .epoch;
    terminal.observe_handoff(
        Some(epoch),
        Observation::DataSubmitted {
            input_bytes: 1,
            total_input_bytes: 1,
        },
    );
    terminal.observe_handoff(
        Some(epoch),
        Observation::FlushPolled {
            total_input_bytes: 1,
            outcome: NodeHttpFlushPoll::Ready,
        },
    );
    producer.join().await.unwrap();
    assert_eq!(source.lock().unwrap().polls, 3);
    assert_eq!(probe.snapshot().end, Some(buffered_body::End::SourceError));
    drop((body, registration, request));
    connection.close().await;
    assert_eq!(probe.snapshot().producer_tasks, 0);
    assert_eq!(probe.snapshot().outstanding_blocks, 0);
}
