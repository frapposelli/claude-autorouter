//! Bounded real-loopback schedules for unique pool/body/cancellation ownership.
use super::super::abort::{self, Cause, DialGate, Outcome};
use super::super::lifecycle_tests::isolated_tls;
use super::*;
use http_body_util::BodyExt;
use hyper_openssl::SslStream;
use hyper_util::rt::TokioIo;
use openssl::ssl::{Ssl, SslVersion};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
// Assertion unwind owns every request task; dropping a handle cancels it.
struct TestTask<T>(tokio::task::JoinHandle<T>);
impl<T> TestTask<T> {
    fn abort(&self) {
        self.0.abort();
    }
    fn is_finished(&self) -> bool {
        self.0.is_finished()
    }
}
impl<T> Drop for TestTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl<T> Future for TestTask<T> {
    type Output = Result<T, tokio::task::JoinError>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}
fn owned_spawn<T: Send + 'static>(future: impl Future<Output = T> + Send + 'static) -> TestTask<T> {
    TestTask(tokio::spawn(future))
}
const LIMIT: Duration = Duration::from_secs(8);
const COMPLETE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: keep-alive\r\n\r\nx";
struct Reply {
    first: &'static [u8],
    remainder: Option<oneshot::Receiver<&'static [u8]>>,
}
struct PeerRequest {
    connection: usize,
    path: String,
    head: String,
    reply: oneshot::Sender<Reply>,
}
struct PeerLease(Arc<AtomicUsize>);
impl Drop for PeerLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
struct Fixture {
    client: Arc<RawPoolClient>,
    origin: String,
    requests: mpsc::Receiver<PeerRequest>,
    tasks: JoinSet<io::Result<()>>,
    stop: CancellationToken,
    active: Arc<AtomicUsize>,
    accepted: Arc<AtomicUsize>,
    tls: Option<openssl::ssl::SslContext>,
}
async fn peer<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    connection: usize,
    requests: mpsc::Sender<PeerRequest>,
) -> io::Result<()> {
    loop {
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            let count = match stream.read(&mut byte).await {
                Ok(count) => count,
                // The client owns shutdown; TLS Drop need not send close_notify.
                // Only an empty between-request read may treat this as teardown.
                Err(error)
                    if head.is_empty()
                        && matches!(
                            error.kind(),
                            io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset
                        ) =>
                {
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            if count == 0 {
                return if head.is_empty() {
                    Ok(())
                } else {
                    Err(io::Error::other("fixture incomplete request"))
                };
            }
            head.push(byte[0]);
            if head.len() > 16384 {
                return Err(io::Error::other("fixture header bound"));
            }
        }
        let request_head = String::from_utf8(head.clone()).unwrap();
        let path = std::str::from_utf8(&head)
            .ok()
            .and_then(|head| head.split_ascii_whitespace().nth(1))
            .ok_or_else(|| io::Error::other("fixture request target"))?
            .to_owned();
        let (reply, receive) = oneshot::channel();
        requests
            .send(PeerRequest {
                connection,
                path,
                head: request_head,
                reply,
            })
            .await
            .map_err(|_| io::Error::other("fixture request receiver closed"))?;
        let Ok(reply) = receive.await else {
            return Ok(()); // Scenario unwound before authorizing a reply.
        };
        stream.write_all(reply.first).await?;
        stream.flush().await?;
        if let Some(remainder) = reply.remainder {
            let Ok(remainder) = remainder.await else {
                return Ok(()); // Scenario unwound with a partial response.
            };
            stream.write_all(remainder).await?;
            stream.flush().await?;
        }
    }
}

impl Fixture {
    async fn new(version: Option<SslVersion>) -> Self {
        let (trust, tls) = isolated_tls(version);
        let client = Arc::new(RawPoolClient::new(&trust).unwrap());
        Self::listen(client, tls).await
    }
    async fn listen(client: Arc<RawPoolClient>, tls: Option<openssl::ssl::SslContext>) -> Self {
        let secure = tls.is_some();
        let server_tls = tls.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!(
            "{}://{}",
            if secure { "https" } else { "http" },
            listener.local_addr().unwrap()
        );
        let (send, requests) = mpsc::channel(16);
        let stop = CancellationToken::new();
        let shutdown = stop.clone();
        let active = Arc::new(AtomicUsize::new(0));
        let accepted = Arc::new(AtomicUsize::new(0));
        let active_peer = active.clone();
        let accepted_peer = accepted.clone();
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            let mut peers = JoinSet::new();
            let result = loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => break Ok(()),
                    result = peers.join_next(), if !peers.is_empty() => {
                        match result {
                            Some(Ok(Ok(()))) => {},
                            _ => break Err(io::Error::other("fixture peer task failed")),
                        }
                    },
                    socket = listener.accept() => {
                        let (socket, _) = socket?;
                        let connection = accepted_peer.fetch_add(1, Ordering::SeqCst);
                        assert!(connection < 16, "raw adversarial peer bound");
                        active_peer.fetch_add(1, Ordering::SeqCst);
                        let lease = PeerLease(active_peer.clone());
                        let context = tls.clone();
                        let requests = send.clone();
                        peers.spawn(async move {
                            let _lease = lease;
                            if let Some(context) = context {
                                let ssl = Ssl::new(&context)?;
                                let mut stream = SslStream::new(ssl, TokioIo::new(socket))?;
                                std::pin::Pin::new(&mut stream).accept().await
                                    .map_err(|_| io::Error::other("fixture handshake failed"))?;
                                peer(TokioIo::new(stream), connection, requests).await
                            } else {
                                peer(socket, connection, requests).await
                            }
                        });
                    }
                }
            };
            peers.abort_all();
            while peers.join_next().await.is_some() {}
            result
        });
        Self {
            client,
            origin,
            requests,
            tasks,
            stop,
            active,
            accepted,
            tls: server_tls,
        }
    }

    pub(super) async fn finish(mut self) {
        let raw = self.client.raw_counts.clone();
        let fetch = self.client.fetch_counts.clone();
        let cache = Arc::downgrade(&self.client.raw_sessions);
        // Drop the clients independently of the peer shutdown. This proves that
        // cleanup does not depend on the fixture first closing remote sockets.
        drop(self.client);
        let clients_closed = tokio::time::timeout(LIMIT, async {
            while raw.active.load(Ordering::SeqCst) != 0
                || fetch.active.load(Ordering::SeqCst) != 0
                || raw.tasks.load(Ordering::SeqCst) != 0
                || fetch.tasks.load(Ordering::SeqCst) != 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok();
        self.stop.cancel();
        let peers_closed = tokio::time::timeout(LIMIT, async {
            while let Some(result) = self.tasks.join_next().await {
                assert!(matches!(result, Ok(Ok(()))), "fixture listener task failed");
            }
        })
        .await
        .is_ok();
        self.tasks.abort_all();
        assert!(
            clients_closed,
            "client connections or background tasks leaked"
        );
        assert!(peers_closed, "fixture tasks leaked");
        assert_eq!(self.active.load(Ordering::SeqCst), 0);
        assert!(cache.upgrade().is_none());
        assert_eq!(raw.shutdown_handles.load(Ordering::SeqCst), 0);
        assert_eq!(fetch.shutdown_handles.load(Ordering::SeqCst), 0);
        eprintln!(
            "Raw pool fixture cleanup: {}",
            serde_json::json!({"raw_active":raw.active.load(Ordering::SeqCst),"raw_tasks":raw.tasks.load(Ordering::SeqCst),"raw_shutdown_handles":raw.shutdown_handles.load(Ordering::SeqCst),"fetch_active":fetch.active.load(Ordering::SeqCst),"fetch_tasks":fetch.tasks.load(Ordering::SeqCst),"fetch_shutdown_handles":fetch.shutdown_handles.load(Ordering::SeqCst),"peer_active":self.active.load(Ordering::SeqCst),"cache_released":cache.upgrade().is_none()})
        );
    }
}

fn request(origin: &str, label: usize) -> Request<Full<Bytes>> {
    Request::get(format!("{origin}/v1/models/pool-{label}"))
        .body(Full::new(Bytes::new()))
        .unwrap()
}
async fn wait(predicate: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("raw ownership barrier");
}
async fn consume(response: Response<OwnedBody<Incoming>>) {
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        Bytes::from_static(b"x")
    );
}
type RequestTask = TestTask<Result<Response<OwnedBody<Incoming>>, HttpError>>;
fn spawn(
    client: &Arc<RawPoolClient>,
    origin: &str,
    label: usize,
) -> (
    RequestTask,
    hyper_util::client::legacy::connect::CaptureAssignment,
) {
    let mut req = request(origin, label);
    let capture = capture_http1_assignment(&mut req);
    let client = client.clone();
    (
        owned_spawn(async move { client.request_raw(req).await }),
        capture,
    )
}
async fn complete_peer(peer: PeerRequest) {
    assert!(peer.path.starts_with("/v1/models/pool-"));
    peer.reply
        .send(Reply {
            first: COMPLETE,
            remainder: None,
        })
        .ok()
        .unwrap();
}

async fn held_owner(version: Option<SslVersion>, cancel: bool) {
    let mut fixture = Fixture::new(version).await;
    let client = fixture.client.clone();
    let origin = fixture.origin.clone();
    let mut receives = std::mem::replace(&mut fixture.requests, mpsc::channel(1).1);
    let run = owned_spawn(async move {
        tokio::time::timeout(LIMIT, async {
            let (first, capture) = spawn(&client, &origin, 0);
            let peer = receives.recv().await.unwrap();
            let first_id = peer.connection;
            complete_peer(peer).await;
            let response = first.await.unwrap().unwrap();
            let old = capture.assignment().unwrap();
            eprintln!("Raw pool fixture stage: complete_body_held");
            // Decoder readiness cannot return A while its complete body is held.
            wait(|| client.raw_counts.read_bytes.load(Ordering::SeqCst) > 0).await;
            assert_eq!(client.snapshot()["free"].as_array().unwrap().len(), 0);
            let (second, second_capture) = spawn(&client, &origin, 1);
            let peer = receives.recv().await.unwrap();
            assert_ne!(first_id, peer.connection);
            complete_peer(peer).await;
            consume(second.await.unwrap().unwrap()).await;
            assert!(second_capture.assignment().unwrap().claim_abort().is_none());
            if cancel {
                let claim = old
                    .claim_abort()
                    .expect("held consumer retains exact reservation");
                assert_eq!(
                    abort::consume(claim, Cause::ExplicitFixtureCancellation),
                    Outcome::Requested
                );
                drop(response);
            } else {
                consume(response).await;
            }
            assert!(old.claim_abort().is_none(), "A cannot affect a later owner");
            wait(|| {
                client.snapshot()["free"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_array().unwrap().len())
                    .sum::<usize>()
                    == if cancel { 1 } else { 2 }
            })
            .await;
            let (third, _) = spawn(&client, &origin, 2);
            let peer = receives.recv().await.unwrap();
            if cancel {
                assert_ne!(peer.connection, first_id);
            } else {
                assert_eq!(peer.connection, first_id);
            }
            complete_peer(peer).await;
            consume(third.await.unwrap().unwrap()).await;
            assert_eq!(client.raw_counts.attempts.load(Ordering::SeqCst), 2);
            assert_eq!(
                client.raw_counts.shutdown_calls.load(Ordering::SeqCst),
                usize::from(cancel)
            );
            if version.is_some() {
                assert_eq!(
                    client
                        .raw_counts
                        .session_error_closes
                        .load(Ordering::SeqCst),
                    usize::from(cancel)
                );
            }
        })
        .await
        .unwrap();
    })
    .await;
    fixture.finish().await;
    run.unwrap();
}
#[tokio::test]
async fn http_held_consumer_reserves_exact_connection() {
    held_owner(None, false).await;
}
#[tokio::test]
async fn tls12_held_consumer_reserves_exact_connection() {
    held_owner(Some(SslVersion::TLS1_2), false).await;
}
#[tokio::test]
async fn tls13_held_consumer_reserves_exact_connection() {
    held_owner(Some(SslVersion::TLS1_3), false).await;
}
#[tokio::test]
async fn http_selected_cancel_preserves_sibling_and_vetoes_reuse() {
    held_owner(None, true).await;
}
#[tokio::test]
async fn tls12_selected_cancel_preserves_sibling_and_vetoes_reuse() {
    held_owner(Some(SslVersion::TLS1_2), true).await;
}
#[tokio::test]
async fn tls13_selected_cancel_preserves_sibling_and_vetoes_reuse() {
    held_owner(Some(SslVersion::TLS1_3), true).await;
}

async fn profile_isolation(version: Option<SslVersion>) {
    let mut fixture = Fixture::new(version).await;
    let client = fixture.client.clone();
    let origin = fixture.origin.clone();
    let mut receives = std::mem::replace(&mut fixture.requests, mpsc::channel(1).1);
    let run = owned_spawn(async move {
        tokio::time::timeout(LIMIT, async {
            let mut raw_connection = None;
            for label in 0..4 {
                let mut req = request(&origin, label);
                let credential = if label == 1 {
                    "Bearer synthetic-refreshed"
                } else {
                    "Bearer synthetic-initial"
                };
                req.headers_mut()
                    .insert("authorization", credential.parse().unwrap());
                let candidate = client.clone();
                let task = owned_spawn(async move {
                    if label == 2 {
                        candidate.request(req).await
                    } else {
                        candidate.request_raw(req).await
                    }
                });
                let peer = receives.recv().await.unwrap();
                assert!(peer.head.to_ascii_lowercase().contains(&format!(
                    "authorization: {}\r\n",
                    credential.to_ascii_lowercase()
                )));
                if label == 0 {
                    raw_connection = Some(peer.connection);
                } else if label == 2 {
                    assert_ne!(raw_connection, Some(peer.connection));
                } else {
                    assert_eq!(raw_connection, Some(peer.connection));
                }
                complete_peer(peer).await;
                consume(task.await.unwrap().unwrap()).await;
            }
            assert_eq!(client.raw_counts.attempts.load(Ordering::SeqCst), 1);
            assert_eq!(client.fetch_counts.attempts.load(Ordering::SeqCst), 1);
            assert_eq!(client.snapshot()["free"].as_array().unwrap().len(), 1);
            let rows = client.snapshot().to_string();
            assert!(!rows.contains("synthetic-initial") && !rows.contains("synthetic-refreshed"));
        })
        .await
        .unwrap();
    })
    .await;
    fixture.finish().await;
    run.unwrap();
}
#[tokio::test]
async fn http_raw_fetch_and_refreshed_headers_are_separate() {
    profile_isolation(None).await;
}
#[tokio::test]
async fn tls12_raw_fetch_and_refreshed_headers_are_separate() {
    profile_isolation(Some(SslVersion::TLS1_2)).await;
}
#[tokio::test]
async fn tls13_raw_fetch_and_refreshed_headers_are_separate() {
    profile_isolation(Some(SslVersion::TLS1_3)).await;
}

async fn origin_isolation(version: Option<SslVersion>) {
    let mut first = Fixture::new(version).await;
    let mut second = Fixture::listen(first.client.clone(), first.tls.clone()).await;
    let client = first.client.clone();
    let a = first.origin.clone();
    let b = second.origin.clone();
    let mut ra = std::mem::replace(&mut first.requests, mpsc::channel(1).1);
    let mut rb = std::mem::replace(&mut second.requests, mpsc::channel(1).1);
    let run = owned_spawn(async move {
        tokio::time::timeout(LIMIT, async {
            for label in 0..4 {
                let (task, _) = spawn(&client, if label % 2 == 0 { &a } else { &b }, label);
                let peer = if label % 2 == 0 {
                    ra.recv().await.unwrap()
                } else {
                    rb.recv().await.unwrap()
                };
                assert_eq!(peer.connection, 0);
                complete_peer(peer).await;
                consume(task.await.unwrap().unwrap()).await;
            }
            assert_eq!(client.raw_counts.attempts.load(Ordering::SeqCst), 2);
            assert_eq!(client.snapshot()["free"].as_array().unwrap().len(), 2);
        })
        .await
        .unwrap();
    })
    .await;
    tokio::join!(first.finish(), second.finish());
    run.unwrap();
}
#[tokio::test]
async fn http_two_origin_pool_entries_are_separate() {
    origin_isolation(None).await;
}
#[tokio::test]
async fn tls12_two_origin_pool_entries_are_separate() {
    origin_isolation(Some(SslVersion::TLS1_2)).await;
}
#[tokio::test]
async fn tls13_two_origin_pool_entries_are_separate() {
    origin_isolation(Some(SslVersion::TLS1_3)).await;
}

#[tokio::test]
async fn unpolled_cancel_and_abandoned_dial_cannot_consume_sibling_connection() {
    let (trust, tls) = isolated_tls(None);
    let gate = Arc::new(DialGate::default());
    let client = Arc::new(RawPoolClient::with_gate(&trust, Some(gate.clone())).unwrap());
    let mut fixture = Fixture::listen(client.clone(), tls).await;
    let origin = fixture.origin.clone();
    let accepted = fixture.accepted.clone();
    let mut receives = std::mem::replace(&mut fixture.requests, mpsc::channel(1).1);
    let run = owned_spawn(async move {
        tokio::time::timeout(LIMIT, async {
            let mut req = request(&origin, 0);
            let capture = capture_http1_assignment(&mut req);
            drop(client.request_raw(req));
            assert!(capture.assignment().is_none());
            assert_eq!(accepted.load(Ordering::SeqCst), 0);
            let paused = gate.hold_next();
            let (abandoned, old) = spawn(&client, &origin, 1);
            let release = paused.await.unwrap();
            assert!(old.assignment().is_none());
            eprintln!("Raw pool fixture stage: pending_dial_owned_before_cancel");
            abandoned.abort();
            assert!(matches!(abandoned.await, Err(error) if error.is_cancelled()));
            assert_eq!(
                client.raw_counts.active.load(Ordering::SeqCst),
                0,
                "cancelled dial must release IO before its fixture gate is released"
            );
            drop(release);
            let (survivor, capture) = spawn(&client, &origin, 2);
            let peer = receives.recv().await.unwrap();
            complete_peer(peer).await;
            consume(survivor.await.unwrap().unwrap()).await;
            assert!(capture.assignment().unwrap().claim_abort().is_none());
            assert!(old.assignment().is_none());
            let (next, _) = spawn(&client, &origin, 3);
            let peer = receives.recv().await.unwrap();
            assert_eq!(peer.connection, 1);
            complete_peer(peer).await;
            consume(next.await.unwrap().unwrap()).await;
            assert_eq!(client.raw_counts.attempts.load(Ordering::SeqCst), 2);
        })
        .await
        .unwrap();
    })
    .await;
    fixture.finish().await;
    run.unwrap();
}

#[tokio::test]
async fn cancelled_old_idle_timer_cannot_close_reassigned_request() {
    let mut fixture = Fixture::new(None).await;
    let client = fixture.client.clone();
    let origin = fixture.origin.clone();
    let mut receives = std::mem::replace(&mut fixture.requests, mpsc::channel(1).1);
    let run = owned_spawn(async move {
        tokio::time::timeout(LIMIT, async {
            let (first, old) = spawn(&client, &origin, 0);
            let peer = receives.recv().await.unwrap();
            let connection = peer.connection;
            peer.reply
                .send(Reply {
                    first:
                        b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nKeep-Alive: timeout=2\r\n\r\nx",
                    remainder: None,
                })
                .ok()
                .unwrap();
            consume(first.await.unwrap().unwrap()).await;
            wait(|| !client.snapshot()["free"].as_array().unwrap().is_empty()).await;
            assert!(old.assignment().unwrap().claim_abort().is_none());
            let (second, current) = spawn(&client, &origin, 1);
            let peer = receives.recv().await.unwrap();
            assert_eq!(peer.connection, connection);
            eprintln!("Raw pool fixture stage: reassigned_before_old_deadline");
            tokio::time::sleep(Duration::from_millis(1200)).await;
            assert_eq!(client.snapshot()["live"], 1);
            assert!(current.assignment().is_some());
            assert!(!second.is_finished());
            complete_peer(peer).await;
            consume(second.await.unwrap().unwrap()).await;
            assert_eq!(client.raw_counts.attempts.load(Ordering::SeqCst), 1);
            assert_eq!(client.raw_counts.shutdown_calls.load(Ordering::SeqCst), 0);
        })
        .await
        .unwrap();
    })
    .await;
    fixture.finish().await;
    run.unwrap();
}

#[tokio::test]
async fn actual_driver_close_between_ready_and_idle_insertion_is_rejected() {
    let fixture = Fixture::new(None).await;
    let client = fixture.client.clone();
    let uri: Uri = fixture.origin.parse().unwrap();
    let run = owned_spawn(async move {
        tokio::time::timeout(LIMIT, async {
            let (mut entry, _) = client
                .shared
                .acquire(&uri, "controlled-insertion")
                .await
                .unwrap();
            entry.sender.ready().await.unwrap();
            assert!(entry.sender.is_ready());
            let id = entry.id;
            let idle = Idle {
                entry,
                generation: 1,
                deadline: Instant::now() + Duration::from_secs(5),
                timeout_ms: 5000,
                _timer: spawn_owned(&client.shared, std::future::pending()),
            };
            idle.entry._driver.0.as_ref().unwrap().abort();
            wait(|| !client.shared.state.lock().unwrap().live.contains_key(&id)).await;
            let rejected = client
                .shared
                .insert_idle("controlled-insertion".to_owned(), idle);
            assert!(rejected.is_some());
            assert!(client.shared.state.lock().unwrap().idle.is_empty());
            drop(rejected);
        })
        .await
        .unwrap();
    })
    .await;
    fixture.finish().await;
    run.unwrap();
}

#[tokio::test]
async fn decoder_error_never_returns_its_connection_to_pool() {
    let mut fixture = Fixture::new(None).await;
    let client = fixture.client.clone();
    let origin = fixture.origin.clone();
    let mut receives = std::mem::replace(&mut fixture.requests, mpsc::channel(1).1);
    let run = owned_spawn(async move {
        tokio::time::timeout(LIMIT, async {
            let (first, capture) = spawn(&client, &origin, 0);
            let peer = receives.recv().await.unwrap();
            let old = peer.connection;
            peer.reply
                .send(Reply {
                    first: b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nZ\r\n",
                    remainder: None,
                })
                .ok()
                .unwrap();
            let response = first.await.unwrap().unwrap();
            assert!(response.into_body().collect().await.is_err());
            assert!(capture.assignment().unwrap().claim_abort().is_none());
            assert!(client.snapshot()["free"].as_array().unwrap().is_empty());
            let (next, _) = spawn(&client, &origin, 1);
            let peer = receives.recv().await.unwrap();
            assert_ne!(peer.connection, old);
            complete_peer(peer).await;
            consume(next.await.unwrap().unwrap()).await;
            assert_eq!(client.raw_counts.attempts.load(Ordering::SeqCst), 2);
        })
        .await
        .unwrap();
    })
    .await;
    fixture.finish().await;
    run.unwrap();
}

async fn subscription_refresh_gateway(version: Option<SslVersion>) {
    use autorouter_core::config::{AuthMode, read_config};
    let mut fixture = Fixture::new(version).await;
    // Explicit in-memory synthetic subscription fixture, like existing gateway
    // contract tests. This does not relax the public config origin guard.
    let mut config=read_config(&serde_json::json!({"AUTOROUTER_EVALUATOR":"jev","ANTHROPIC_API_KEY":"synthetic-unused","TYPESAFE_API_KEY":"synthetic-unused","AUTOROUTER_TOKEN":"synthetic-local-token"}),false,std::path::Path::new("/synthetic")).unwrap();
    config.upstream = fixture.origin.clone();
    config.auth_mode = AuthMode::Subscription;
    let client = fixture.client.clone();
    let gateway = crate::server::Gateway::new(
        config,
        client.clone(),
        crate::server_events::EventSinks::default(),
    )
    .unwrap();
    let running = gateway.listen(0).await.unwrap();
    let address = running.address;
    let mut receives = std::mem::replace(&mut fixture.requests, mpsc::channel(1).1);
    let run = owned_spawn(async move {
        tokio::time::timeout(LIMIT, async {
            let downstream =
                hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                    .build_http::<Full<Bytes>>();
            for (label, credential) in ["Bearer synthetic-initial", "Bearer synthetic-refreshed"]
                .iter()
                .enumerate()
            {
                let req = Request::get(format!("http://{address}/v1/models/pool-{label}"))
                    .header("x-autorouter-token", "synthetic-local-token")
                    .header("authorization", *credential)
                    .header("anthropic-beta", "oauth-2025-04-20")
                    .body(Full::new(Bytes::new()))
                    .unwrap();
                let request_client = downstream.clone();
                let task = owned_spawn(async move { request_client.request(req).await });
                let peer = receives.recv().await.unwrap();
                assert_eq!(
                    peer.connection, 0,
                    "refresh must retain the socket but forward the current credential"
                );
                assert!(peer.head.to_ascii_lowercase().contains(&format!(
                    "authorization: {}\r\n",
                    credential.to_ascii_lowercase()
                )));
                assert!(!peer.head.contains("synthetic-local-token"));
                complete_peer(peer).await;
                let response = task.await.unwrap().unwrap();
                assert_eq!(response.status(), 200);
                assert_eq!(
                    response.into_body().collect().await.unwrap().to_bytes(),
                    Bytes::from_static(b"x")
                );
            }
            assert_eq!(client.raw_counts.attempts.load(Ordering::SeqCst), 1);
            assert_eq!(client.fetch_counts.attempts.load(Ordering::SeqCst), 0);
        })
        .await
        .unwrap();
    })
    .await;
    running.close().await;
    drop(gateway);
    fixture.finish().await;
    run.unwrap();
}
#[tokio::test]
async fn http_subscription_gateway_forwards_refreshed_credential_on_reused_socket() {
    subscription_refresh_gateway(None).await;
}
#[tokio::test]
async fn tls12_subscription_gateway_forwards_refreshed_credential_on_reused_socket() {
    subscription_refresh_gateway(Some(SslVersion::TLS1_2)).await;
}
#[tokio::test]
async fn tls13_subscription_gateway_forwards_refreshed_credential_on_reused_socket() {
    subscription_refresh_gateway(Some(SslVersion::TLS1_3)).await;
}

#[tokio::test]
async fn wire_host_normalizes_numeric_port_and_preserves_explicit_override() {
    let mut fixture = Fixture::new(None).await;
    let client = fixture.client.clone();
    let origin = fixture.origin.clone();
    let mut receives = std::mem::replace(&mut fixture.requests, mpsc::channel(1).1);
    let run = owned_spawn(async move {
        tokio::time::timeout(LIMIT, async {
            let (base, port) = origin.rsplit_once(':').unwrap();
            for (label, override_host) in [(0, false), (1, true)] {
                let mut request = Request::get(format!(
                    "{base}:0{port}/v1/models/pool-{label}?synthetic=%2F"
                ))
                .body(Full::new(Bytes::new()))
                .unwrap();
                if override_host {
                    request
                        .headers_mut()
                        .insert("host", "synthetic-override:0123".parse().unwrap());
                }
                let candidate = client.clone();
                let task = owned_spawn(async move { candidate.request_raw(request).await });
                let peer = receives.recv().await.unwrap();
                assert_eq!(peer.connection, 0);
                assert_eq!(peer.path, format!("/v1/models/pool-{label}?synthetic=%2F"));
                let expected = if override_host {
                    "host: synthetic-override:0123\r\n".to_owned()
                } else {
                    format!("host: 127.0.0.1:{port}\r\n")
                };
                assert!(peer.head.to_ascii_lowercase().contains(&expected));
                complete_peer(peer).await;
                consume(task.await.unwrap().unwrap()).await;
            }
        })
        .await
        .unwrap();
    })
    .await;
    fixture.finish().await;
    run.unwrap();
}

#[tokio::test]
async fn hinted_idle_expiry_closes_without_explicit_abort() {
    let mut fixture = Fixture::new(None).await;
    let client = fixture.client.clone();
    let origin = fixture.origin.clone();
    let mut receives = std::mem::replace(&mut fixture.requests, mpsc::channel(1).1);
    let run = owned_spawn(async move {
        tokio::time::timeout(LIMIT, async {
            let (first, _) = spawn(&client, &origin, 0);
            let peer = receives.recv().await.unwrap();
            peer.reply
                .send(Reply {
                    first:
                        b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nKeep-Alive: timeout=2\r\n\r\nx",
                    remainder: None,
                })
                .ok()
                .unwrap();
            consume(first.await.unwrap().unwrap()).await;
            wait(|| !client.snapshot()["free"].as_array().unwrap().is_empty()).await;
            eprintln!("Raw pool fixture stage: hinted_idle_accepted");
            wait(|| client.snapshot()["live"] == 0).await;
            assert!(client.snapshot()["free"].as_array().unwrap().is_empty());
            assert_eq!(client.raw_counts.shutdown_calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                client
                    .raw_counts
                    .session_error_closes
                    .load(Ordering::SeqCst),
                0
            );
            let (next, _) = spawn(&client, &origin, 1);
            let peer = receives.recv().await.unwrap();
            assert_eq!(peer.connection, 1);
            complete_peer(peer).await;
            consume(next.await.unwrap().unwrap()).await;
        })
        .await
        .unwrap();
    })
    .await;
    fixture.finish().await;
    run.unwrap();
}
