//! Ownership regressions use bodies that forbid a convenient extra EOF poll.
use super::*;
use http_body_util::BodyExt;
use std::convert::Infallible;

struct LastData {
    ended: bool,
}
impl Body for LastData {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        assert!(!self.ended, "consumer must not need an extra EOF poll");
        self.ended = true;
        Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"x")))))
    }
    fn is_end_stream(&self) -> bool {
        self.ended
    }
}
fn completion(rows: &Arc<Mutex<Vec<bool>>>) -> Option<Completion> {
    let rows = rows.clone();
    Some(Box::new(move |clean| rows.lock().unwrap().push(clean)))
}
#[tokio::test]
async fn final_data_retires_without_further_none_poll() {
    let rows = Arc::new(Mutex::new(Vec::new()));
    let mut body = OwnedBody::new(LastData { ended: false }, completion(&rows), None);
    assert!(rows.lock().unwrap().is_empty());
    assert_eq!(
        body.frame().await.unwrap().unwrap().into_data().unwrap(),
        Bytes::from_static(b"x")
    );
    assert_eq!(*rows.lock().unwrap(), vec![true]);
    drop(body);
    assert_eq!(*rows.lock().unwrap(), vec![true], "one retirement only");
}
#[test]
fn header_only_handoff_retires_without_any_body_poll() {
    let rows = Arc::new(Mutex::new(Vec::new()));
    let body = OwnedBody::new(LastData { ended: true }, completion(&rows), None);
    assert!(body.is_end_stream());
    assert_eq!(*rows.lock().unwrap(), vec![true]);
    drop(body);
    assert_eq!(*rows.lock().unwrap(), vec![true]);
}
#[test]
fn held_decoder_eof_drop_is_not_consumer_completion() {
    let rows = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new(Gate(Mutex::new(GateState {
        held: true,
        waker: None,
    })));
    let body = OwnedBody::new(LastData { ended: true }, completion(&rows), Some(gate));
    assert!(!body.is_end_stream());
    assert!(rows.lock().unwrap().is_empty());
    drop(body);
    assert_eq!(*rows.lock().unwrap(), vec![false]);
}
#[test]
fn unconsumed_body_drop_is_not_consumer_completion() {
    let rows = Arc::new(Mutex::new(Vec::new()));
    drop(OwnedBody::new(
        LastData { ended: false },
        completion(&rows),
        None,
    ));
    assert_eq!(*rows.lock().unwrap(), vec![false]);
}
#[test]
fn joined_leading_decimal_hint_policy() {
    for (hint, expected) in [
        ("timeout=0", None),
        ("timeout=1", None),
        ("timeout=1.5", None),
        ("timeout=2", Some(1000)),
        ("timeout=2, max=7", Some(1000)),
        ("timeout=6", Some(5000)),
        ("Timeout=2", Some(5000)),
        ("max=7, timeout=2", Some(5000)),
        ("timeout=-2", Some(5000)),
    ] {
        let mut headers = hyper::HeaderMap::new();
        headers.insert("keep-alive", hint.parse().unwrap());
        assert_eq!(
            idle_timeout(&headers).map(|value| value.as_millis()),
            expected,
            "{hint}"
        );
    }
    let mut headers = hyper::HeaderMap::new();
    headers.append("keep-alive", "timeout=2".parse().unwrap());
    headers.append("keep-alive", "max=7".parse().unwrap());
    assert_eq!(idle_timeout(&headers), Some(Duration::from_secs(1)));
    headers.clear();
    headers.insert(
        "keep-alive",
        format!("timeout={}", "9".repeat(100)).parse().unwrap(),
    );
    assert_eq!(idle_timeout(&headers), Some(Duration::from_secs(5)));
}

#[test]
fn empty_first_keep_alive_field_preserves_join_delimiter() {
    let mut headers = hyper::HeaderMap::new();
    headers.append("keep-alive", "".parse().unwrap());
    headers.append("keep-alive", "timeout=2".parse().unwrap());
    assert_eq!(idle_timeout(&headers), Some(Duration::from_secs(5)));
}

#[test]
fn node_url_pool_identity_normalizes_host_and_effective_port() {
    for (a, b) in [
        ("http://LOCALHOST/path", "http://localhost:80/other"),
        ("https://LOCALHOST/path", "https://localhost:00443/other"),
        ("http://LOCALHOST:01234/path", "http://localhost:1234/other"),
    ] {
        assert_eq!(
            pool_key(&a.parse().unwrap()).unwrap(),
            pool_key(&b.parse().unwrap()).unwrap()
        );
    }
    assert_ne!(
        pool_key(&"http://localhost:1234/".parse().unwrap()).unwrap(),
        pool_key(&"https://localhost:1234/".parse().unwrap()).unwrap()
    );
}

#[tokio::test]
async fn upgrade_connection_token_without_attachment_cannot_dial() {
    let (trust, _) = super::super::lifecycle_tests::isolated_tls(None);
    let client = RawPoolClient::new(&trust).unwrap();
    for attached in [false, true] {
        let mut request = Request::get("http://127.0.0.1:9/synthetic")
            .body(Full::new(Bytes::new()))
            .unwrap();
        request
            .headers_mut()
            .append("connection", "keep-alive".parse().unwrap());
        request
            .headers_mut()
            .append("connection", "other, UpGrAdE".parse().unwrap());
        let capture = attached.then(|| capture_http1_assignment(&mut request));
        assert!(matches!(
            client.request_raw(request).await,
            Err(HttpError::InvalidRequest)
        ));
        assert!(capture.is_none_or(|capture| capture.assignment().is_none()));
    }
    assert_eq!(client.raw_counts.attempts.load(Ordering::SeqCst), 0);
    assert_eq!(client.raw_counts.tasks.load(Ordering::SeqCst), 0);
}

struct CompletionFailureGuard {
    release: Arc<(Mutex<bool>, std::sync::Condvar)>,
    worker: Arc<Mutex<Option<std::thread::JoinHandle<bool>>>>,
}
impl CompletionFailureGuard {
    fn release_and_join(&mut self) {
        let (mutex, changed) = &*self.release;
        *mutex.lock().unwrap_or_else(|error| error.into_inner()) = true;
        changed.notify_all();
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(worker) = worker {
            assert!(worker.join().unwrap());
        }
    }
}
impl Drop for CompletionFailureGuard {
    fn drop(&mut self) {
        let (mutex, changed) = &*self.release;
        *mutex.lock().unwrap_or_else(|error| error.into_inner()) = true;
        changed.notify_all();
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}
struct FailureOnFinalData {
    fire: Option<Box<dyn FnOnce() + Send>>,
    ended: bool,
}
impl Body for FailureOnFinalData {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let this = self.get_mut();
        assert!(
            !this.ended,
            "the final-frame control must not need another EOF poll"
        );
        this.fire.take().unwrap()();
        this.ended = true;
        Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"x")))))
    }
    fn is_end_stream(&self) -> bool {
        self.ended
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn final_body_poll_keeps_the_real_entry_and_reservation_until_failure_settles() {
    use super::super::gateway_terminal::{
        ConnectionTerminal, FailureCause, Probe as TerminalProbe,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri: Uri = format!(
        "http://{}/v1/models/owned-completion",
        listener.local_addr().unwrap()
    )
    .parse()
    .unwrap();
    let key = pool_key(&uri).unwrap();
    let (trust, _) = super::super::lifecycle_tests::isolated_tls(None);
    let client = RawPoolClient::new(&trust).unwrap();
    let (entry, reused) = crate::bounded(client.shared.acquire(&uri, &key))
        .await
        .unwrap();
    assert!(!reused);
    let (peer, _) = crate::bounded(listener.accept()).await.unwrap();
    let id = entry.id;
    let activity = Arc::downgrade(&entry.activity);
    let probe = TerminalProbe::default();
    let connection = ConnectionTerminal::new(probe.clone(), 1);
    let owner = connection.handle().register().unwrap();
    let terminal = owner.publisher();
    let mut request = Request::get(uri).body(Full::new(Bytes::new())).unwrap();
    let capture = Arc::new(capture_http1_assignment(&mut request));
    let reservation = bind_http1_reservation(&mut request, &entry.connected).unwrap();
    assert!(reservation.is_some());
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let worker = Arc::new(Mutex::new(None));
    let mut guard = CompletionFailureGuard {
        release: release.clone(),
        worker: worker.clone(),
    };
    let observed = capture.clone();
    let claims = Arc::new(AtomicUsize::new(0));
    let recorded = claims.clone();
    let (entered, receiver) = std::sync::mpsc::sync_channel(1);
    terminal
        .bind_transport_action(move |cause| {
            assert_eq!(cause, FailureCause::Deadline);
            entered.send(()).unwrap();
            let (mutex, changed) = &*release;
            let released = mutex.lock().unwrap();
            let (released, _) = changed
                .wait_timeout_while(released, Duration::from_secs(5), |released| !*released)
                .unwrap();
            assert!(*released, "completion control exceeded its finite gate");
            drop(released);
            assert!(
                observed.assignment().unwrap().claim_abort().is_some(),
                "body completion retired the selected reservation before the first action"
            );
            recorded.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
    terminal.admit_transport().unwrap();
    let fail = terminal.clone();
    let fire = Box::new(move || {
        assert!(!fail.failed());
        let thread = std::thread::spawn(move || fail.fail(FailureCause::Deadline));
        assert!(worker.lock().unwrap().replace(thread).is_none());
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
    });
    let shared = client.shared.clone();
    let completion_terminal = terminal.clone();
    let completed = Arc::new(AtomicUsize::new(0));
    let observed_completion = completed.clone();
    let completion = Box::new(move |clean: bool| {
        assert!(clean, "final-data fixture did not use clean completion");
        observed_completion.fetch_add(1, Ordering::SeqCst);
        shared.complete_owned(
            key,
            entry,
            reservation,
            None,
            Some(completion_terminal),
            clean,
        );
    });
    let mut body = OwnedBody::new(
        FailureOnFinalData {
            fire: Some(fire),
            ended: false,
        },
        Some(completion),
        None,
    );
    assert_eq!(
        body.frame().await.unwrap().unwrap().into_data().unwrap(),
        Bytes::from_static(b"x")
    );
    assert_eq!(completed.load(Ordering::SeqCst), 1);
    assert_eq!(claims.load(Ordering::SeqCst), 0);
    assert!(
        activity.upgrade().is_some(),
        "real Entry was dropped while the action was held"
    );
    {
        let state = client.shared.state.lock().unwrap();
        assert!(state.live.contains_key(&id));
        assert!(
            state.idle.is_empty(),
            "held reservation reached the free pool"
        );
    }
    assert!(!terminal.retire_transport_action());
    drop(body);
    assert_eq!(
        completed.load(Ordering::SeqCst),
        1,
        "drop repeated final-frame completion"
    );
    guard.release_and_join();
    crate::bounded(async {
        while activity.upgrade().is_some() || !client.shared.tasks.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert_eq!(claims.load(Ordering::SeqCst), 1);
    assert!(capture.assignment().unwrap().claim_abort().is_none());
    assert!(client.shared.state.lock().unwrap().idle.is_empty());
    assert_eq!(completed.load(Ordering::SeqCst), 1);
    assert!(!probe.observer_failed());
    drop(owner);
    connection.close().await;
    assert_eq!(probe.tasks(), 0);
    drop(peer);
    drop(listener);
    drop(client);
}
