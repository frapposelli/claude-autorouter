// Included within classifier::tests to reuse its actual held request transport.
async fn poll_once(
    future: std::pin::Pin<&mut impl std::future::Future<Output = ResultValue>>,
) -> std::task::Poll<ResultValue> {
    let mut future = future;
    poll_fn(|cx| std::task::Poll::Ready(future.as_mut().poll(cx))).await
}
async fn bounded_classification(
    future: impl std::future::Future<Output = ResultValue>,
) -> ResultValue {
    tokio::time::timeout(Duration::from_secs(2), future)
        .await
        .unwrap()
}
#[tokio::test(flavor = "current_thread")]
async fn cancelled_shared_or_distinct_tokens_prune_unpolled_peer_and_release_before_return() {
    for shared_token in [true, false] {
        let mock = Arc::new(Mock::new(true));
        let config = config();
        let classifier = Classifier::new(mock.clone(), &config);
        let body = document("shared-token");
        let token = CancellationToken::new();
        let peer = if shared_token {
            token.clone()
        } else {
            CancellationToken::new()
        };
        let mut first = Box::pin(classifier.classify(&body, &config, &token));
        let mut second = Box::pin(classifier.classify(&body, &config, &peer));
        assert!(poll_once(first.as_mut()).await.is_pending());
        assert!(poll_once(second.as_mut()).await.is_pending());
        wait_calls(&mock, 1).await;
        assert_eq!(classifier.pending_counts(), (1, 2));
        token.cancel();
        peer.cancel();
        assert!(
            poll_once(first.as_mut()).await.is_pending(),
            "must await actual worker teardown"
        );
        assert_eq!(classifier.pending_counts(), (0, 0));
        assert!(matches!(
            bounded_classification(first).await,
            Err(EvaluationError::Cancelled)
        ));
        assert_eq!(mock.active.load(Ordering::SeqCst), 0);
        // The second subscriber has not been polled since cancellation.
        assert!(matches!(
            bounded_classification(second).await,
            Err(EvaluationError::Cancelled)
        ));
        assert_eq!(mock.calls(), 1);
    }
}
#[tokio::test(flavor = "current_thread")]
async fn one_cancelled_peer_returns_without_waiting_for_the_live_shared_request() {
    let mock = Arc::new(Mock::new(true));
    let config = config();
    let classifier = Classifier::new(mock.clone(), &config);
    let body = document("live-peer");
    let token = CancellationToken::new();
    let live = CancellationToken::new();
    let mut first = Box::pin(classifier.classify(&body, &config, &token));
    let mut second = Box::pin(classifier.classify(&body, &config, &live));
    assert!(poll_once(first.as_mut()).await.is_pending());
    assert!(poll_once(second.as_mut()).await.is_pending());
    wait_calls(&mock, 1).await;
    token.cancel();
    assert!(matches!(
        poll_once(first.as_mut()).await,
        std::task::Poll::Ready(Err(EvaluationError::Cancelled))
    ));
    assert_eq!(mock.active.load(Ordering::SeqCst), 1);
    assert_eq!(classifier.pending_counts(), (1, 1));
    mock.release(1);
    assert_eq!(bounded_classification(second).await.unwrap().source, "jev");
    assert_eq!(mock.active.load(Ordering::SeqCst), 0);
    assert_eq!(mock.calls(), 1);
}
#[tokio::test(flavor = "current_thread")]
async fn shutdown_cancelled_result_waits_for_worker_with_uncancelled_caller() {
    let mock = Arc::new(Mock::new(true));
    let config = config();
    let classifier = Classifier::new(mock.clone(), &config);
    let body = document("shutdown-result");
    let token = CancellationToken::new();
    let mut pending = Box::pin(classifier.classify(&body, &config, &token));
    assert!(poll_once(pending.as_mut()).await.is_pending());
    wait_calls(&mock, 1).await;
    classifier.shutdown();
    assert!(!token.is_cancelled());
    // Shutdown publishes Cancelled while the request is still owned. A watch
    // result is not a completion witness, even when caller cancellation is false.
    assert_eq!(mock.active.load(Ordering::SeqCst), 1);
    assert!(poll_once(pending.as_mut()).await.is_pending());
    assert!(matches!(
        bounded_classification(pending).await,
        Err(EvaluationError::Cancelled)
    ));
    assert_eq!(mock.active.load(Ordering::SeqCst), 0);
    assert_eq!(classifier.pending_counts(), (0, 0));
}
#[tokio::test(flavor = "current_thread")]
async fn dropping_cancel_cleanup_waiter_still_releases_the_exact_old_worker() {
    let mock = Arc::new(Mock::new(true));
    let config = config();
    let classifier = Classifier::new(mock.clone(), &config);
    let body = document("drop-cleanup");
    let token = CancellationToken::new();
    let mut pending = Box::pin(classifier.classify(&body, &config, &token));
    assert!(poll_once(pending.as_mut()).await.is_pending());
    wait_calls(&mock, 1).await;
    token.cancel();
    assert!(poll_once(pending.as_mut()).await.is_pending());
    drop(pending);
    tokio::time::timeout(Duration::from_secs(2), async {
        while mock.active.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(classifier.pending_counts(), (0, 0));
}
#[tokio::test(flavor = "current_thread")]
async fn old_cleanup_completion_cannot_cancel_or_remove_same_key_replacement() {
    for prune_before_old_poll in [false, true] {
        let mock = Arc::new(Mock::new(true));
        let config = config();
        let classifier = Classifier::new(mock.clone(), &config);
        let body = document("replacement-during-cleanup");
        let cancelled = CancellationToken::new();
        let live = CancellationToken::new();
        let mut old = Box::pin(classifier.classify(&body, &config, &cancelled));
        assert!(poll_once(old.as_mut()).await.is_pending());
        wait_calls(&mock, 1).await;
        let key = settings_key(&body, &config);
        let old_entry = classifier.shared.state.lock().unwrap().pending[&key]
            .entry
            .clone();
        cancelled.cancel();
        if !prune_before_old_poll {
            assert!(poll_once(old.as_mut()).await.is_pending());
        }
        let mut replacement = Box::pin(classifier.classify(&body, &config, &live));
        assert!(poll_once(replacement.as_mut()).await.is_pending());
        let replacement_entry = classifier.shared.state.lock().unwrap().pending[&key]
            .entry
            .clone();
        assert!(!Arc::ptr_eq(&old_entry, &replacement_entry));
        if prune_before_old_poll {
            // Admission removed the cancelled old generation before its own future
            // runs again: this directly exercises the Retired detach outcome.
            assert!(poll_once(old.as_mut()).await.is_pending());
        }
        wait_calls(&mock, 2).await;
        assert!(matches!(
            bounded_classification(old).await,
            Err(EvaluationError::Cancelled)
        ));
        assert_eq!(classifier.pending_counts(), (1, 1));
        assert_eq!(mock.active.load(Ordering::SeqCst), 1);
        assert!(!replacement_entry.controller.is_cancelled());
        assert!(old_entry.completed.finished.load(Ordering::Acquire));
        mock.release(1);
        assert_eq!(
            bounded_classification(replacement).await.unwrap().source,
            "jev"
        );
        assert_eq!(
            bounded_classification(classifier.classify(&body, &config, &live))
                .await
                .unwrap()
                .source,
            "cache"
        );
        assert_eq!(mock.calls(), 2);
    }
}
#[tokio::test(flavor = "current_thread")]
async fn worker_guard_dropped_before_first_poll_completes_multiple_and_late_waiters() {
    let completed = Arc::new(WorkerCompletion::default());
    let finish = FinishWorker(completed.clone());
    let started = Arc::new(AtomicBool::new(false));
    let flag = started.clone();
    let task = tokio::spawn(async move {
        let _finish = finish;
        flag.store(true, Ordering::SeqCst);
        std::future::pending::<()>().await;
    });
    let first = completed.wait();
    let second = completed.wait();
    tokio::pin!(first, second);
    assert!(
        poll_fn(|cx| std::task::Poll::Ready(first.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    assert!(
        poll_fn(|cx| std::task::Poll::Ready(second.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(!started.load(Ordering::SeqCst));
    tokio::time::timeout(Duration::from_secs(2), async {
        first.await;
        second.await;
        completed.wait().await;
    })
    .await
    .unwrap();
}
#[tokio::test(flavor = "current_thread")]
async fn finished_worker_without_result_retires_entry_and_returns_network_fallback() {
    let mock = Arc::new(Mock::new(false));
    let config = config();
    let classifier = Classifier::new(mock.clone(), &config);
    let body = document("finished-without-result");
    let key = settings_key(&body, &config);
    let (result, _) = watch::channel(None);
    let entry = Arc::new(Entry {
        controller: CancellationToken::new(),
        result,
        completed: Arc::new(WorkerCompletion::default()),
    });
    {
        let mut state = classifier.shared.state.lock().unwrap();
        let live_subscriber = state.next_id();
        state.pending.insert(
            key,
            Pending {
                entry: entry.clone(),
                subscribers: HashMap::from([(live_subscriber, CancellationToken::new())]),
            },
        );
        state.subscribers = 1;
    }
    assert_eq!(classifier.pending_counts(), (1, 1));
    // Models a dropped unpolled worker retaining the same Sender through Entry.
    drop(FinishWorker(entry.completed.clone()));
    let token = CancellationToken::new();
    let result = bounded_classification(classifier.classify(&body, &config, &token))
        .await
        .unwrap();
    assert_eq!(result.classifier_error, Some("network_error"));
    assert_eq!(result.source, "fallback");
    assert_eq!(mock.calls(), 0);
    assert_eq!(classifier.pending_counts(), (0, 0));
    assert_eq!(
        bounded_classification(classifier.classify(&body, &config, &token))
            .await
            .unwrap()
            .source,
        "jev"
    );
    assert_eq!(mock.calls(), 1);
}
struct PanicOnceTransport {
    active: Arc<AtomicUsize>,
    next: AtomicBool,
}
impl HttpTransport for PanicOnceTransport {
    type ResponseBody = Full<Bytes>;
    async fn request(&self, _: Request<Full<Bytes>>) -> Result<Response<Full<Bytes>>, HttpError> {
        self.active.fetch_add(1, Ordering::SeqCst);
        let _active = Active(self.active.clone());
        assert!(
            !self.next.swap(false, Ordering::SeqCst),
            "synthetic evaluator request poll panic"
        );
        Ok(Response::new(Full::new(Bytes::from_static(
            br#"{"answers":{"tier":{"choice":"haiku","confidence":0.9}}}"#,
        ))))
    }
}
#[tokio::test]
async fn evaluator_poll_panic_releases_request_before_fallback_and_retry() {
    let transport = Arc::new(PanicOnceTransport {
        active: Arc::new(AtomicUsize::new(0)),
        next: AtomicBool::new(true),
    });
    let config = config();
    let classifier = Classifier::new(transport.clone(), &config);
    let body = document("poll-panic");
    let token = CancellationToken::new();
    let first = bounded_classification(classifier.classify(&body, &config, &token))
        .await
        .unwrap();
    assert_eq!(first.classifier_error, Some("network_error"));
    assert_eq!(transport.active.load(Ordering::SeqCst), 0);
    let retry = bounded_classification(classifier.classify(&body, &config, &token))
        .await
        .unwrap();
    assert_eq!(retry.source, "jev");
    assert_eq!(transport.active.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_cancellation_wins_over_late_success_publication_and_waits_for_owner() {
    let mock = Arc::new(Mock::new(true));
    let config = config();
    let classifier = Classifier::new(mock.clone(), &config);
    let body = document("shutdown-late-publication");
    let token = CancellationToken::new();
    let mut pending = Box::pin(classifier.classify(&body, &config, &token));
    assert!(poll_once(pending.as_mut()).await.is_pending());
    wait_calls(&mock, 1).await;
    let key = settings_key(&body, &config);
    let entry = classifier.shared.state.lock().unwrap().pending[&key]
        .entry
        .clone();
    classifier.shutdown();
    assert!(!token.is_cancelled());
    assert!(entry.controller.is_cancelled());
    assert!(!entry.completed.finished.load(Ordering::Acquire));
    assert_eq!(mock.active.load(Ordering::SeqCst), 1);
    // Deliberately overwrite the shutdown watch value, modeling late result
    // publication. Keep a real owned request alive to test that neither this
    // success value nor shutdown's earlier Cancelled is cleanup evidence.
    let mut late = unavailable("claude-sonnet-5", config.evaluator, "network_error");
    late.source = "jev";
    late.reason = "classified";
    late.classifier_error = None;
    assert!(entry.result.send(Some(Ok(late))).is_ok());
    let observed = poll_once(pending.as_mut()).await;
    let incorrectly_ready = observed.is_ready();
    let result = match observed {
        std::task::Poll::Ready(result) => result,
        std::task::Poll::Pending => bounded_classification(pending).await,
    };
    // Preserve owned cleanup before the intended old-code failure assertion.
    tokio::time::timeout(Duration::from_secs(2), entry.completed.wait())
        .await
        .unwrap();
    assert_eq!(mock.active.load(Ordering::SeqCst), 0);
    assert_eq!(classifier.pending_counts(), (0, 0));
    assert!(
        !incorrectly_ready,
        "late watch success bypassed shutdown cleanup"
    );
    assert!(matches!(result, Err(EvaluationError::Cancelled)));
}
