// Included within classifier::tests. No live provider or injected result.
async fn poll_owned_close(
    future: std::pin::Pin<&mut impl std::future::Future<Output = ()>>,
) -> std::task::Poll<()> {
    let mut future = future;
    poll_fn(|cx| std::task::Poll::Ready(future.as_mut().poll(cx))).await
}
async fn bounded_owned_close(future: impl std::future::Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(2), future)
        .await
        .expect("owned close exceeded its test deadline");
}

#[tokio::test(flavor = "current_thread")]
async fn close_waits_for_retired_worker_after_first_close_waiter_is_dropped() {
    let mock = Arc::new(Mock::new(true));
    let settings = config();
    let classifier = Classifier::new(mock.clone(), &settings);
    let gate = Arc::new(shutdown_gate::WorkerPollGate::default());
    classifier.test_worker_poll_gate(gate.clone());
    let body = document("retired-owner-close");
    let caller = CancellationToken::new();
    let mut request = Box::pin(classifier.classify(&body, &settings, &caller));
    assert!(poll_once(request.as_mut()).await.is_pending());
    wait_calls(&mock, 1).await;
    let release = gate.hold();
    drop(request);
    bounded_owned_close(gate.wait_blocked()).await;
    assert_eq!(classifier.pending_counts(), (0, 0));
    assert_eq!(classifier.shared.workers.len(), 1);
    let mut first = Box::pin(classifier.close());
    assert!(poll_owned_close(first.as_mut()).await.is_pending());
    assert_eq!(mock.active.load(Ordering::SeqCst), 1);
    drop(first);
    let mut second = Box::pin(classifier.close());
    assert!(poll_owned_close(second.as_mut()).await.is_pending());
    assert_eq!(classifier.shared.workers.len(), 1);
    drop(release);
    bounded_owned_close(second).await;
    bounded_owned_close(gate.wait_finished()).await;
    assert_eq!(mock.active.load(Ordering::SeqCst), 0);
    assert_eq!(classifier.shared.workers.len(), 0);
    let mut late = Box::pin(classifier.close());
    assert!(poll_owned_close(late.as_mut()).await.is_ready());
    assert!(matches!(
        classifier.classify(&body, &settings, &caller).await,
        Err(EvaluationError::Cancelled)
    ));
    assert_eq!(mock.calls(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn terminal_close_owns_reservation_before_spawn_and_drop_before_first_poll() {
    for drop_before_spawn in [false, true] {
        let mock = Arc::new(Mock::new(true));
        let settings = config();
        let classifier = Classifier::new(mock.clone(), &settings);
        let gate = Arc::new(shutdown_gate::WorkerSpawnGate::default());
        *classifier.shared.test_spawn_gate.lock().unwrap() = Some(gate.clone());
        let body = document("reserved-before-spawn");
        let caller = CancellationToken::new();
        let mut request = Box::pin(classifier.classify(&body, &settings, &caller));
        assert!(poll_once(request.as_mut()).await.is_pending());
        assert!(gate.entered());
        assert_eq!(mock.calls(), 0);
        assert_eq!(classifier.shared.workers.len(), 1);
        let old = classifier.shared.state.lock().unwrap().pending[&settings_key(&body, &settings)]
            .entry
            .clone();
        let mut close = Box::pin(classifier.close());
        assert!(poll_owned_close(close.as_mut()).await.is_pending());
        assert!(!old.completed.finished.load(Ordering::Acquire));
        if drop_before_spawn {
            drop(request);
        } else {
            gate.release();
            assert!(matches!(
                bounded_classification(request).await,
                Err(EvaluationError::Cancelled)
            ));
        }
        bounded_owned_close(close).await;
        assert!(old.completed.finished.load(Ordering::Acquire));
        assert_eq!(classifier.shared.workers.len(), 0);
        assert_eq!(classifier.pending_counts(), (0, 0));
        assert_eq!(mock.calls(), 0);
        assert_eq!(mock.active.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn retired_workers_keep_capacity_until_actual_teardown_then_admission_recovers() {
    let mock = Arc::new(Mock::new(true));
    let settings = config();
    let classifier = Classifier::with_limits(
        mock.clone(),
        &settings,
        Limits {
            pending: 2,
            subscribers: 4,
        },
    );
    let mut releases = Vec::new();
    let mut gates = Vec::new();
    for index in 0..2 {
        let gate = Arc::new(shutdown_gate::WorkerPollGate::default());
        classifier.test_worker_poll_gate(gate.clone());
        let body = document(&format!("retired-{index}"));
        let caller = CancellationToken::new();
        let mut request = Box::pin(classifier.classify(&body, &settings, &caller));
        assert!(poll_once(request.as_mut()).await.is_pending());
        wait_calls(&mock, index + 1).await;
        releases.push(gate.hold());
        drop(request);
        bounded_owned_close(gate.wait_blocked()).await;
        gates.push(gate);
    }
    assert_eq!(classifier.pending_counts(), (0, 0));
    assert_eq!(classifier.shared.workers.len(), 2);
    let body = document("next");
    let caller = CancellationToken::new();
    let refused = classifier
        .classify(&body, &settings, &caller)
        .await
        .unwrap();
    assert_eq!(refused.classifier_error, Some("capacity_exhausted"));
    assert_eq!(mock.calls(), 2);
    drop(releases.remove(0));
    bounded_owned_close(gates[0].wait_finished()).await;
    assert_eq!(classifier.shared.workers.len(), 1);
    *classifier.shared.test_worker_gate.lock().unwrap() = None;
    let mut next = Box::pin(classifier.classify(&body, &settings, &caller));
    assert!(poll_once(next.as_mut()).await.is_pending());
    wait_calls(&mock, 3).await;
    assert_eq!(classifier.shared.workers.len(), 2);
    caller.cancel();
    assert!(matches!(
        bounded_classification(next).await,
        Err(EvaluationError::Cancelled)
    ));
    drop(releases);
    bounded_owned_close(classifier.close()).await;
    assert_eq!(classifier.shared.workers.len(), 0);
    assert_eq!(mock.active.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn reusable_shutdown_keeps_new_generation_and_coalescing_at_worker_capacity() {
    let mock = Arc::new(Mock::new(true));
    let settings = config();
    let classifier = Classifier::with_limits(
        mock.clone(),
        &settings,
        Limits {
            pending: 2,
            subscribers: 4,
        },
    );
    let gate = Arc::new(shutdown_gate::WorkerPollGate::default());
    classifier.test_worker_poll_gate(gate.clone());
    let body = document("reusable-shutdown");
    let old_caller = CancellationToken::new();
    let mut old = Box::pin(classifier.classify(&body, &settings, &old_caller));
    assert!(poll_once(old.as_mut()).await.is_pending());
    wait_calls(&mock, 1).await;
    let release = gate.hold();
    classifier.shutdown();
    drop(old);
    bounded_owned_close(gate.wait_blocked()).await;
    *classifier.shared.test_worker_gate.lock().unwrap() = None;
    let first_token = CancellationToken::new();
    let peer_token = CancellationToken::new();
    let mut replacement = Box::pin(classifier.classify(&body, &settings, &first_token));
    let mut peer = Box::pin(classifier.classify(&body, &settings, &peer_token));
    assert!(poll_once(replacement.as_mut()).await.is_pending());
    wait_calls(&mock, 2).await;
    assert_eq!(classifier.shared.workers.len(), 2);
    assert!(poll_once(peer.as_mut()).await.is_pending());
    assert_eq!(classifier.pending_counts(), (1, 2));
    assert_eq!(mock.calls(), 2);
    first_token.cancel();
    assert!(matches!(
        poll_once(replacement.as_mut()).await,
        std::task::Poll::Ready(Err(EvaluationError::Cancelled))
    ));
    // The deliberately held old request remains a fair semaphore waiter.
    // Positively observe both actual waits, then release both reservations;
    // the old worker is still held by its independent scheduling gate.
    assert_eq!(mock.waiting.load(Ordering::SeqCst), 2);
    assert_eq!(mock.active.load(Ordering::SeqCst), 2);
    mock.release(2);
    assert_eq!(bounded_classification(peer).await.unwrap().source, "jev");
    assert_eq!(mock.waiting.load(Ordering::SeqCst), 1);
    assert_eq!(
        classifier
            .classify(&body, &settings, &peer_token)
            .await
            .unwrap()
            .source,
        "cache"
    );
    assert_eq!(mock.active.load(Ordering::SeqCst), 1);
    drop(release);
    bounded_owned_close(classifier.close()).await;
    assert_eq!(mock.active.load(Ordering::SeqCst), 0);
    assert_eq!(classifier.shared.workers.len(), 0);
    assert!(matches!(
        classifier.classify(&body, &settings, &peer_token).await,
        Err(EvaluationError::Cancelled)
    ));
    assert_eq!(mock.calls(), 2);
}
