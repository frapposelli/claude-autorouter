//! Direct buffered-constructor controls. These do not manufacture a real
//! Gateway disconnect or qualify broader TLS/session behavior.
use super::*;
use crate::tls_roots::openssl_spike::{
    abort,
    gateway_intent::Disconnect,
    gateway_terminal::{ConnectionTerminal, Event, FailureCause, TransportOutcome},
    loopback_peer::Peer,
};

#[derive(Clone, Copy, Debug)]
enum BridgePhase {
    PublishedBeforeAdmission,
    PendingHeadersDeadline,
}
struct PausedDeadline;
impl PausedDeadline {
    fn new() -> Self {
        tokio::time::pause();
        Self
    }
}
impl Drop for PausedDeadline {
    fn drop(&mut self) {
        tokio::time::resume();
    }
}

fn assert_protocol(peer: &Peer, version: Option<SslVersion>) {
    match peer {
        Peer::Tcp(_) => assert!(version.is_none()),
        Peer::Tls(stream) => assert_eq!(
            stream.inner().ssl().version_str(),
            match version {
                Some(SslVersion::TLS1_2) => "TLSv1.2",
                Some(SslVersion::TLS1_3) => "TLSv1.3",
                _ => panic!("unexpected TLS fixture version"),
            }
        ),
    }
}

fn assert_no_buffered_response(buffers: &BufferProbe) {
    let state = buffers.snapshot();
    assert_eq!(state.live_pools, 0);
    assert_eq!(state.allocated_blocks, 0);
    assert_eq!(state.outstanding_blocks, 0);
    assert_eq!(state.queued_bytes, 0);
    assert_eq!(state.producer_tasks, 0);
    assert_eq!(state.consumer_polls, 0);
    assert_eq!(state.source_errors, 0);
    assert_eq!(state.responses_ready, 0);
    assert_eq!(state.clean_sources, 0);
    assert_eq!(state.failed_sources, 0);
    assert_eq!(state.cancelled_sources, 0);
}

async fn buffered_qualified_preheader(phase: BridgePhase, version: Option<SslVersion>) {
    let (listener, origin) = bind_listener(version.is_some()).await;
    let (trust, tls) = isolated_tls(version);
    let client = Arc::new(BufferedRawPoolClient::new(&trust).unwrap());
    let raw = client.inner.raw_counts.clone();
    let fetch = client.inner.fetch_counts.clone();
    let buffers = client.probe.clone();
    let pool_probe = client.inner.probe.clone();
    let cache = Arc::downgrade(&client.inner.raw_sessions);
    let probe = gateway_terminal::Probe::default();
    let connection = ConnectionTerminal::new(probe.clone(), 1);
    let owner = connection.handle().register().unwrap();
    let terminal = owner.publisher();
    let identity = terminal.observation().0;
    let token = CancellationToken::new();
    let causes = Arc::new(Mutex::new(Vec::new()));
    let reports = Arc::new(AtomicUsize::new(0));
    let cause_rows = causes.clone();
    let cause_token = token.clone();
    let cause_raw = raw.clone();
    let report_calls = reports.clone();
    let deadline = Instant::now() + Duration::from_secs(30);
    owner.start(
        deadline,
        token.clone(),
        move |cause| {
            // The actual qualified transport action must precede cancellation
            // publication and the existing request cause callback.
            assert!(!cause_token.is_cancelled());
            assert_eq!(cause_raw.shutdown_calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                cause_raw.session_error_closes.load(Ordering::SeqCst),
                usize::from(version.is_some())
            );
            cause_rows.lock().unwrap().push(cause);
        },
        move |_| {
            report_calls.fetch_add(1, Ordering::SeqCst);
        },
    );
    let mut before_admission = matches!(phase, BridgePhase::PublishedBeforeAdmission)
        .then(|| pool_probe.hold_before_admission());
    let mut admission_release = None;
    let mut request = Request::get(format!("{origin}/v1/models/pool-0"))
        .body(Full::new(Bytes::new()))
        .unwrap();
    request.extensions_mut().insert(terminal.clone());
    let worker = client.clone();
    let mut requests = JoinSet::new();
    requests.spawn(async move { worker.request_raw(request).await });
    let result = tokio::time::timeout(Duration::from_secs(45), sibling_catch(async {
        let mut peer = tokio::time::timeout(BARRIER, accept_peer(&listener, tls.as_ref())).await.unwrap();
        assert_protocol(&peer, version);
        let mut request_head = Vec::new();
        let cause = match phase {
            BridgePhase::PublishedBeforeAdmission => {
                admission_release = Some(tokio::time::timeout(BARRIER,
                    before_admission.take().unwrap()).await.unwrap().unwrap());
                assert_eq!(pool_probe.dispatches(), 0, "request was enqueued before admission gate");
                assert_eq!(raw.active.load(Ordering::SeqCst), 1);
                assert_eq!(raw.shutdown_handles.load(Ordering::SeqCst), 1);
                assert_eq!(raw.shutdown_calls.load(Ordering::SeqCst), 0);
                assert_eq!(terminal.failure_cause(), None);
                assert!(probe.transport_events().is_empty());
                assert_no_buffered_response(&buffers);
                let cause = FailureCause::Downstream(Disconnect::ReadEof);
                assert!(terminal.fail(cause));
                terminal.settle_failure().await;
                assert_eq!(terminal.failure_cause(), Some(cause));
                // Physically observe the socket close while the request future
                // still holds its post-publication, pre-admission gate.
                assert_eq!(tokio::time::timeout(BARRIER, closed(&mut peer)).await.unwrap(), Vec::<u8>::new());
                assert!(requests.try_join_next().is_none(), "held request returned before gate release");
                assert_eq!(pool_probe.dispatches(), 0);
                admission_release.take().unwrap().send(()).unwrap();
                cause
            }
            BridgePhase::PendingHeadersDeadline => {
                request_head = tokio::time::timeout(BARRIER, head(&mut peer)).await.unwrap();
                assert!(request_head.starts_with(b"GET /v1/models/pool-0 HTTP/1.1\r\n"));
                let text = std::str::from_utf8(&request_head).unwrap();
                let expected_host = origin.split_once("://").unwrap().1;
                let hosts: Vec<_> = text.lines().filter_map(|line| line.split_once(':'))
                    .filter(|(name, _)| name.eq_ignore_ascii_case("host"))
                    .map(|(_, value)| value.trim()).collect();
                assert_eq!(hosts, [expected_host]);
                assert_eq!(pool_probe.dispatches(), 1, "actual GET must be invoked exactly once");
                assert_eq!(raw.active.load(Ordering::SeqCst), 1);
                assert_eq!(raw.shutdown_handles.load(Ordering::SeqCst), 1);
                assert_eq!(raw.shutdown_calls.load(Ordering::SeqCst), 0);
                assert_eq!(terminal.failure_cause(), None);
                assert!(probe.transport_events().is_empty());
                assert_no_buffered_response(&buffers);
                assert!(requests.try_join_next().is_none(), "headers were not withheld");
                assert!(deadline > Instant::now(), "fixture exceeded its real handshake/head barrier");
                {
                    let _clock = PausedDeadline::new();
                    tokio::time::advance(deadline.duration_since(Instant::now())).await;
                }
                // Resume before waiting for OS socket notifications. Only the
                // request deadline uses virtual time; no virtual IO timeout.
                until(|| terminal.failure_cause() == Some(FailureCause::Deadline)).await;
                terminal.settle_failure().await;
                assert_eq!(tokio::time::timeout(BARRIER, closed(&mut peer)).await.unwrap(), Vec::<u8>::new());
                FailureCause::Deadline
            }
        };
        let returned = tokio::time::timeout(BARRIER, requests.join_next()).await.unwrap().unwrap().unwrap();
        match returned {
            Err(error) => assert_eq!(error, HttpError::Network),
            Ok(_) => panic!("aborted preheader request acquired a response"),
        }
        assert!(requests.is_empty());
        terminal.settle_failure().await;
        assert_eq!(terminal.failure_cause(), Some(cause));
        assert!(!terminal.fail(FailureCause::Upstream), "late generic error replaced first cause");
        assert!(token.is_cancelled());
        assert_eq!(*causes.lock().unwrap(), [cause]);
        assert_eq!(reports.load(Ordering::SeqCst), 0, "no acquired headers were attached");
        let observations = probe.transport_events();
        assert_eq!(observations.len(), 1, "one attempt is not proof of repeated abort success");
        assert_eq!(observations[0].identity, identity);
        assert_eq!(observations[0].cause, cause);
        assert!(matches!(observations[0].outcome, TransportOutcome::Applied(abort::Outcome::Requested)));
        let events = probe.events();
        assert_eq!(events.iter().filter(|event| matches!(event, Event::Failed(id, found)
            if *id == identity && *found == cause)).count(), 1);
        assert!(!events.iter().any(|event| matches!(event, Event::Attached(_) | Event::DeliveryClaimed(..))));
        assert_eq!(raw.attempts.load(Ordering::SeqCst), 1);
        assert_eq!(raw.completed.load(Ordering::SeqCst), usize::from(version.is_some()));
        assert_eq!(raw.shutdown_calls.load(Ordering::SeqCst), 1);
        assert_eq!(raw.session_error_closes.load(Ordering::SeqCst), usize::from(version.is_some()));
        assert_eq!(fetch.attempts.load(Ordering::SeqCst), 0);
        assert_eq!(client.inner.raw_sessions.lock().unwrap().snapshot().0, 0);
        let state = client.inner.snapshot();
        assert_eq!(state["requests"], 1);
        assert_eq!(state["connections"], 1);
        assert_eq!(state["rows"].as_array().unwrap().iter().filter(|row| row["event"] == "assigned").count(), 1);
        assert!(state["free"].as_array().unwrap().iter().all(|ids| ids.as_array().unwrap().is_empty()));
        let expected_dispatches = usize::from(matches!(phase, BridgePhase::PendingHeadersDeadline));
        assert_eq!(pool_probe.dispatches(), expected_dispatches);
        assert!(std::future::poll_fn(|cx| Poll::Ready(listener.poll_accept(cx).is_pending())).await,
            "private transport retried onto another physical connection");
        assert_no_buffered_response(&buffers);
        assert!(!probe.observer_failed());
        serde_json::json!({
            "phase":format!("{phase:?}"), "version":format!("{version:?}"),
            "identity":{"connection":identity.connection,"request":identity.request},
            "cause":format!("{cause:?}"), "transport_outcome":format!("{:?}", observations[0].outcome),
            "request_head":request_head, "post_head_or_pre_admission_http_bytes":[],
            "dispatch_invocations":expected_dispatches, "raw":state,
            "shutdown_calls":1,"session_error_closes":usize::from(version.is_some()),
            "buffer":buffers.snapshot(),"direct_constructor_not_gateway":true,
        })
    })).await;
    // Release private gates and join every request before client/terminal owners
    // are retired, including assertion/deadline failures. Cleanup is not verdict.
    drop(admission_release);
    requests.abort_all();
    tokio::time::timeout(Duration::from_secs(5), async {
        while requests.join_next().await.is_some() {}
    })
    .await
    .expect("request join cleanup exceeded bound");
    drop(owner);
    tokio::time::timeout(Duration::from_secs(5), connection.close())
        .await
        .unwrap();
    drop(client);
    until(|| {
        raw.active.load(Ordering::SeqCst) == 0
            && raw.tasks.load(Ordering::SeqCst) == 0
            && raw.shutdown_handles.load(Ordering::SeqCst) == 0
            && fetch.active.load(Ordering::SeqCst) == 0
            && fetch.tasks.load(Ordering::SeqCst) == 0
            && fetch.shutdown_handles.load(Ordering::SeqCst) == 0
            && probe.tasks() == 0
    })
    .await;
    assert!(cache.upgrade().is_none());
    assert_no_buffered_response(&buffers);
    let row = result
        .expect("bounded direct constructor scenario")
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    eprintln!("buffered_qualified_preheader={row}");
}

macro_rules! qualified_preheader_case {
    ($name:ident, $phase:ident, $version:expr) => {
        #[tokio::test]
        async fn $name() {
            buffered_qualified_preheader(BridgePhase::$phase, $version).await;
        }
    };
}
qualified_preheader_case!(
    http_published_before_admission_aborts_without_dispatch,
    PublishedBeforeAdmission,
    None
);
qualified_preheader_case!(
    tls12_published_before_admission_aborts_without_dispatch,
    PublishedBeforeAdmission,
    Some(SslVersion::TLS1_2)
);
qualified_preheader_case!(
    tls13_published_before_admission_aborts_without_dispatch,
    PublishedBeforeAdmission,
    Some(SslVersion::TLS1_3)
);
qualified_preheader_case!(
    http_preheader_deadline_claims_the_live_reservation,
    PendingHeadersDeadline,
    None
);
qualified_preheader_case!(
    tls12_preheader_deadline_claims_the_live_reservation,
    PendingHeadersDeadline,
    Some(SslVersion::TLS1_2)
);
qualified_preheader_case!(
    tls13_preheader_deadline_claims_the_live_reservation,
    PendingHeadersDeadline,
    Some(SslVersion::TLS1_3)
);
