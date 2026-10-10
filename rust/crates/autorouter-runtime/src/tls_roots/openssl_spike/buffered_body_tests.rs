use super::*;
use http_body_util::{BodyExt, Full};
use std::convert::Infallible;
use std::sync::atomic::AtomicBool;
use std::task::Wake;
use std::time::Duration;
use tokio::sync::oneshot;

const LIMIT: Duration = Duration::from_secs(2);
async fn until(probe: &Probe, predicate: impl Fn(&Snapshot) -> bool) {
    tokio::time::timeout(LIMIT, async {
        while !predicate(&probe.snapshot()) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bounded buffer observation");
}
async fn clean(probe: &Probe) {
    until(probe, |state| {
        state.producer_tasks == 0 && state.outstanding_blocks == 0 && state.allocated_blocks == 0
    })
    .await;
}
struct Generated {
    left: usize,
    chunk: usize,
    final_end: bool,
    polls: Arc<AtomicUsize>,
    dropped: Arc<AtomicBool>,
}
impl Body for Generated {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let this = self.get_mut();
        this.polls.fetch_add(1, Ordering::SeqCst);
        if this.left == 0 {
            assert!(!this.final_end, "polled None after final-data end hint");
            return Poll::Ready(None);
        }
        let count = this.left.min(this.chunk);
        this.left -= count;
        Poll::Ready(Some(Ok(Frame::data(Bytes::from(vec![7; count])))))
    }
    fn is_end_stream(&self) -> bool {
        self.final_end && self.left == 0
    }
}
impl Drop for Generated {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}
fn generated(bytes: usize, chunk: usize, final_end: bool) -> Generated {
    Generated {
        left: bytes,
        chunk,
        final_end,
        polls: Arc::new(AtomicUsize::new(0)),
        dropped: Arc::new(AtomicBool::new(false)),
    }
}
struct Frames(VecDeque<Result<Frame<Bytes>, ()>>);
impl Body for Frames {
    type Data = Bytes;
    type Error = ();
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, ()>>> {
        Poll::Ready(self.0.pop_front())
    }
}
async fn pending(body: &mut BufferedBody) {
    let result = poll_fn(|cx| Poll::Ready(Pin::new(&mut *body).poll_frame(cx))).await;
    assert!(result.is_pending());
}

#[tokio::test]
async fn tiny_ready_frames_coalesce_at_the_actual_logical_threshold() {
    let input = generated(2 * HIGH_WATER, 1, true);
    let polls = input.polls.clone();
    let (body, producer, probe) = start(input, |_| {});
    until(&probe, |state| state.queued_bytes == HIGH_WATER).await;
    let state = probe.snapshot();
    assert_eq!(state.queued_blocks, HIGH_WATER / BLOCK_BYTES);
    assert_eq!(state.outstanding_blocks, HIGH_WATER / BLOCK_BYTES);
    assert_eq!(polls.load(Ordering::SeqCst), HIGH_WATER);
    assert_eq!(state.end, None);
    drop(body);
    tokio::time::timeout(LIMIT, producer.shutdown())
        .await
        .unwrap()
        .unwrap();
    clean(&probe).await;
}

#[tokio::test]
async fn oversized_input_is_copied_incrementally_without_a_total_body_cap() {
    // This fake Full owns an 8 MiB input allocation, outside the application
    // block budget. This is not a total transport-memory proof.
    let bytes = 8 * 1024 * 1024;
    let (mut body, producer, probe) = start(Full::new(Bytes::from(vec![3; bytes])), |_| {});
    until(&probe, |state| state.queued_bytes == HIGH_WATER).await;
    assert_eq!(probe.snapshot().end, None);
    let mut received = 0;
    while let Some(frame) = tokio::time::timeout(LIMIT, body.frame()).await.unwrap() {
        let data = frame.unwrap().into_data().unwrap();
        assert!(data.iter().all(|byte| *byte == 3));
        received += data.len();
    }
    assert_eq!(received, bytes);
    assert!(body.is_end_stream());
    producer.join().await.unwrap();
    let peak = probe.snapshot();
    assert!(peak.peak_outstanding_blocks <= BLOCKS);
    assert!(peak.peak_queued_bytes < HIGH_WATER + BLOCK_BYTES);
    drop(body);
    clean(&probe).await;
}

#[tokio::test]
async fn final_bytes_clone_owns_credit_after_dequeue_and_consumer_drop() {
    let (mut body, producer, probe) = start(generated(2 * 1024 * 1024, BLOCK_BYTES, true), |_| {});
    let mut retained = Vec::new();
    for _ in 0..BLOCKS {
        let data = tokio::time::timeout(LIMIT, body.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap();
        let clone = data.slice(1..2);
        drop(data);
        retained.push(clone);
    }
    until(&probe, |state| {
        state.outstanding_blocks == BLOCKS && state.queued_bytes == 0
    })
    .await;
    pending(&mut body).await;
    assert_eq!(probe.snapshot().allocated_blocks, BLOCKS);
    assert_eq!(probe.snapshot().producer_tasks, 1);
    drop(retained.pop());
    until(&probe, |state| state.queued_bytes != 0).await;
    let replacement = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert!(probe.snapshot().peak_outstanding_blocks <= BLOCKS);
    drop(body);
    producer.shutdown().await.unwrap();
    assert_eq!(probe.snapshot().producer_tasks, 0);
    assert_eq!(probe.snapshot().outstanding_blocks, BLOCKS);
    drop(replacement);
    drop(retained);
    clean(&probe).await;
}

#[tokio::test]
async fn held_consumer_drop_cancels_producer_and_drops_input_without_any_poll() {
    let input = generated(1024 * 1024, 1024, true);
    let dropped = input.dropped.clone();
    let (body, producer, probe) = start(input, |_| {});
    until(&probe, |state| state.queued_bytes == HIGH_WATER).await;
    drop(body);
    producer.shutdown().await.unwrap();
    assert!(dropped.load(Ordering::SeqCst));
    clean(&probe).await;
}

#[tokio::test]
async fn producer_abort_before_first_poll_releases_its_task_guard() {
    let (mut body, producer, probe) = start(generated(99, 9, true), |_| {});
    drop(producer);
    until(&probe, |state| state.producer_tasks == 0).await;
    assert_eq!(body.frame().await.unwrap().unwrap_err(), End::Cancelled);
    drop(body);
    clean(&probe).await;
}

#[tokio::test]
async fn source_failure_is_reported_without_consumer_demand_and_follows_accepted_data() {
    let (signal, observed) = oneshot::channel();
    let (mut body, producer, probe) = start(
        Frames(VecDeque::from([
            Ok(Frame::data(Bytes::from_static(b"accepted"))),
            Err(()),
        ])),
        move |end| {
            signal.send(end).unwrap();
        },
    );
    assert_eq!(
        tokio::time::timeout(LIMIT, observed)
            .await
            .unwrap()
            .unwrap(),
        End::SourceError
    );
    assert!(!body.is_end_stream());
    assert_eq!(
        body.frame().await.unwrap().unwrap().into_data().unwrap(),
        "accepted"
    );
    assert_eq!(body.frame().await.unwrap().unwrap_err(), End::SourceError);
    assert!(body.is_end_stream());
    producer.join().await.unwrap();
    drop(body);
    clean(&probe).await;
}

#[tokio::test]
async fn trailers_preserve_order_and_do_not_turn_a_later_error_into_clean_end() {
    let mut trailers = HeaderMap::new();
    trailers.insert("x-synthetic", "trailer".parse().unwrap());
    let (mut body, producer, probe) = start(
        Frames(VecDeque::from([
            Ok(Frame::data(Bytes::from_static(b"prefix"))),
            Ok(Frame::trailers(trailers.clone())),
            Err(()),
        ])),
        |_| {},
    );
    producer.join().await.unwrap();
    assert_eq!(
        body.frame().await.unwrap().unwrap().into_data().unwrap(),
        "prefix"
    );
    assert_eq!(
        body.frame()
            .await
            .unwrap()
            .unwrap()
            .into_trailers()
            .unwrap(),
        trailers
    );
    assert!(!body.is_end_stream());
    assert_eq!(body.frame().await.unwrap().unwrap_err(), End::SourceError);
    drop(body);
    clean(&probe).await;
}

#[tokio::test]
async fn final_data_end_hint_needs_no_extra_input_poll_none() {
    let input = generated(7, 7, true);
    let polls = input.polls.clone();
    let (mut body, producer, probe) = start(input, |_| {});
    producer.join().await.unwrap();
    let data = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert_eq!(data.len(), 7);
    assert!(body.is_end_stream());
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    drop((body, data));
    clean(&probe).await;
}

#[tokio::test]
async fn header_only_source_never_requires_a_fictitious_input_frame() {
    let input = generated(0, 1, true);
    let polls = input.polls.clone();
    let (body, producer, probe) = start(input, |_| {});
    producer.join().await.unwrap();
    assert!(body.is_end_stream());
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    assert_eq!(probe.snapshot().allocated_blocks, 0);
    drop(body);
    clean(&probe).await;
}

#[tokio::test]
async fn callback_panic_cannot_erase_the_latched_source_failure_or_leak_storage() {
    let (mut body, producer, probe) = start(Frames(VecDeque::from([Err(())])), |_| {
        panic!("synthetic callback panic")
    });
    producer.join().await.unwrap();
    assert_eq!(probe.snapshot().callback_panics, 1);
    assert_eq!(body.frame().await.unwrap().unwrap_err(), End::SourceError);
    drop(body);
    clean(&probe).await;
}

struct InspectWake(Probe);
impl Wake for InspectWake {
    fn wake(self: Arc<Self>) {
        let _ = self.0.snapshot();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        let _ = self.0.snapshot();
    }
}
struct Gated {
    ready: Arc<AtomicBool>,
}
impl Body for Gated {
    type Data = Bytes;
    type Error = ();
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, ()>>> {
        if self.ready.load(Ordering::SeqCst) {
            Poll::Ready(Some(Err(())))
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}
#[tokio::test]
async fn consumer_wake_runs_outside_the_queue_lock() {
    let ready = Arc::new(AtomicBool::new(false));
    let (mut body, producer, probe) = start(
        Gated {
            ready: ready.clone(),
        },
        |_| {},
    );
    let waker = Waker::from(Arc::new(InspectWake(probe.clone())));
    assert!(
        Pin::new(&mut body)
            .poll_frame(&mut Context::from_waker(&waker))
            .is_pending()
    );
    ready.store(true, Ordering::SeqCst);
    tokio::time::timeout(LIMIT, producer.join())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(body.frame().await.unwrap().unwrap_err(), End::SourceError);
    drop(body);
    clean(&probe).await;
}

#[tokio::test]
async fn bounded_trailer_slot_rejects_invalid_fake_metadata() {
    let mut trailers = HeaderMap::new();
    trailers.insert(
        "x-synthetic",
        hyper::header::HeaderValue::from_bytes(&vec![b'x'; 16_385]).unwrap(),
    );
    let (mut body, producer, probe) = start(
        Frames(VecDeque::from([Ok(Frame::trailers(trailers))])),
        |_| {},
    );
    producer.join().await.unwrap();
    assert_eq!(
        body.frame().await.unwrap().unwrap_err(),
        End::InvalidMetadata
    );
    drop(body);
    clean(&probe).await;
}

#[tokio::test]
async fn independent_eight_saturated_bodies_release_all_application_backing() {
    let mut rows = Vec::new();
    for _ in 0..8 {
        let row = start(generated(2 * HIGH_WATER, 31, true), |_| {});
        until(&row.2, |state| state.queued_bytes >= HIGH_WATER).await;
        rows.push(row);
    }
    for (body, producer, probe) in rows {
        assert!(probe.snapshot().outstanding_blocks <= BLOCKS);
        drop(body);
        producer.shutdown().await.unwrap();
        clean(&probe).await;
    }
}

struct PanicWake;
impl Wake for PanicWake {
    fn wake(self: Arc<Self>) {
        panic!("synthetic consumer wake panic");
    }
}
#[tokio::test]
async fn consumer_waker_panic_cannot_suppress_independent_terminal_publication() {
    let ready = Arc::new(AtomicBool::new(false));
    let (send, receive) = oneshot::channel();
    let (mut body, producer, probe) = start(
        Gated {
            ready: ready.clone(),
        },
        move |end| {
            send.send(end).unwrap();
        },
    );
    let waker = Waker::from(Arc::new(PanicWake));
    assert!(
        Pin::new(&mut body)
            .poll_frame(&mut Context::from_waker(&waker))
            .is_pending()
    );
    ready.store(true, Ordering::SeqCst);
    assert_eq!(
        tokio::time::timeout(LIMIT, receive).await.unwrap().unwrap(),
        End::SourceError
    );
    producer.join().await.unwrap();
    assert_eq!(probe.snapshot().waker_panics, 1);
    assert_eq!(body.frame().await.unwrap().unwrap_err(), End::SourceError);
    drop(body);
    clean(&probe).await;
}
#[tokio::test]
async fn abort_before_first_poll_still_publishes_cancellation_once() {
    let (send, receive) = oneshot::channel();
    let (body, producer, probe) = start(generated(99, 1, true), move |end| {
        send.send(end).unwrap();
    });
    drop(producer);
    assert_eq!(
        tokio::time::timeout(LIMIT, receive).await.unwrap().unwrap(),
        End::Cancelled
    );
    drop(body);
    clean(&probe).await;
}
#[tokio::test]
async fn legal_1001_repeated_empty_trailer_values_are_consumed_cleanly() {
    let mut headers = HeaderMap::new();
    for _ in 0..1001 {
        headers.append("x", hyper::header::HeaderValue::from_static(""));
    }
    let (mut body, producer, probe) = start(
        Frames(VecDeque::from([Ok(Frame::trailers(headers))])),
        |_| {},
    );
    producer.join().await.unwrap();
    assert_eq!(
        body.frame()
            .await
            .unwrap()
            .unwrap()
            .into_trailers()
            .unwrap()
            .len(),
        1001
    );
    assert!(body.is_end_stream());
    drop(body);
    clean(&probe).await;
}

#[tokio::test]
async fn cancelling_a_pending_join_aborts_the_owned_producer() {
    let (body, producer, probe) = start(
        Gated {
            ready: Arc::new(AtomicBool::new(false)),
        },
        |_| {},
    );
    let join = tokio::spawn(producer.join());
    tokio::task::yield_now().await;
    assert_eq!(probe.snapshot().producer_tasks, 1);
    join.abort();
    assert!(join.await.unwrap_err().is_cancelled());
    until(&probe, |state| state.producer_tasks == 0).await;
    assert_eq!(probe.snapshot().end, Some(End::Cancelled));
    drop(body);
    clean(&probe).await;
}
