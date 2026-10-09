//! Actual loopback ownership probes. No provider, environment mutation, sleep,
//! request abort implementation, or session-eviction inference is involved.
use std::io;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper::body::Body;
use hyper_openssl::SslStream;
use hyper_util::client::legacy::connect::capture_connection;
use hyper_util::rt::TokioIo;
use openssl::asn1::Asn1Time;
use openssl::ec::{EcGroup, EcKey};
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::PKey;
use openssl::ssl::{Ssl, SslContext, SslContextBuilder, SslMethod, SslVersion};
use openssl::x509::extension::{BasicConstraints, ExtendedKeyUsage, SubjectAlternativeName};
use openssl::x509::store::X509StoreBuilder;
use openssl::x509::{X509, X509NameBuilder};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::client::SpikeHttpClient;
use super::lifecycle::ConnectionIdentity;
use super::policy::TrustSnapshot;
use crate::http_client::HttpTransport;

const LIMIT: Duration = Duration::from_secs(5);

struct Reply {
    first: &'static [u8],
    remainder: Option<oneshot::Receiver<&'static [u8]>>,
}
struct PeerRequest {
    connection: usize,
    path: String,
    reply: oneshot::Sender<Reply>,
}
struct PeerLease(Arc<AtomicUsize>);
impl Drop for PeerLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Fixture {
    client: Arc<SpikeHttpClient>,
    origin: String,
    requests: mpsc::Receiver<PeerRequest>,
    tasks: JoinSet<io::Result<()>>,
    stop: CancellationToken,
    active: Arc<AtomicUsize>,
    accepted: Arc<AtomicUsize>,
}

fn isolated_tls(version: Option<SslVersion>) -> (TrustSnapshot, Option<SslContext>) {
    openssl::init_without_config().unwrap();
    let mut store = X509StoreBuilder::new().unwrap();
    let Some(version) = version else {
        return (TrustSnapshot::isolated(store.build()), None);
    };
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
    let key = PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap();
    let mut name = X509NameBuilder::new().unwrap();
    name.append_entry_by_nid(Nid::COMMONNAME, "synthetic loopback lifecycle")
        .unwrap();
    let name = name.build();
    let mut cert = X509::builder().unwrap();
    cert.set_version(2).unwrap();
    cert.set_subject_name(&name).unwrap();
    cert.set_issuer_name(&name).unwrap();
    cert.set_pubkey(&key).unwrap();
    let serial = openssl::bn::BigNum::from_u32(1)
        .unwrap()
        .to_asn1_integer()
        .unwrap();
    cert.set_serial_number(&serial).unwrap();
    cert.set_not_before(&Asn1Time::from_unix(1).unwrap())
        .unwrap();
    cert.set_not_after(&Asn1Time::days_from_now(1).unwrap())
        .unwrap();
    cert.append_extension(BasicConstraints::new().critical().ca().build().unwrap())
        .unwrap();
    cert.append_extension(ExtendedKeyUsage::new().server_auth().build().unwrap())
        .unwrap();
    let san = SubjectAlternativeName::new()
        .ip("127.0.0.1")
        .build(&cert.x509v3_context(None, None))
        .unwrap();
    cert.append_extension(san).unwrap();
    cert.sign(&key, MessageDigest::sha256()).unwrap();
    let cert = cert.build();
    store.add_cert(cert.clone()).unwrap();
    let mut context = SslContextBuilder::new(SslMethod::tls_server()).unwrap();
    context.set_certificate(&cert).unwrap();
    context.set_private_key(&key).unwrap();
    context.check_private_key().unwrap();
    context.set_min_proto_version(Some(version)).unwrap();
    context.set_max_proto_version(Some(version)).unwrap();
    (
        TrustSnapshot::isolated(store.build()),
        Some(context.build()),
    )
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
        let client = Arc::new(SpikeHttpClient::with_snapshot(true, &trust).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!(
            "{}://{}",
            if version.is_some() { "https" } else { "http" },
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
        }
    }

    async fn finish(mut self) {
        let raw = self.client.raw_counts.clone();
        let fetch = self.client.fetch_counts.clone();
        let cache = Arc::downgrade(self.client.raw_sessions.as_ref().unwrap());
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
    }
}

fn request(origin: &str, path: &str) -> Request<Full<Bytes>> {
    Request::get(format!("{origin}/{path}"))
        .body(Full::new(Bytes::new()))
        .unwrap()
}

// Spawn the assertion-bearing scenario separately: even assertion panic/timeout
// drops its requests and bodies, then the outer fixture performs bounded cleanup.
async fn run_reuse(version: Option<SslVersion>, drop_old_body: bool) {
    let mut fixture = Fixture::new(version).await;
    let client = fixture.client.clone();
    let origin = fixture.origin.clone();
    let (unused, replacement) = mpsc::channel(1);
    drop(unused);
    let mut requests = std::mem::replace(&mut fixture.requests, replacement);
    let mut scenarios = JoinSet::new();
    scenarios.spawn(async move {
        let mut a = request(&origin, "a");
        let capture_a = capture_connection(&mut a);
        assert!(ConnectionIdentity::captured(&capture_a).is_none());
        let response_a = client.request_raw(a);
        tokio::pin!(response_a);
        let observed_a = tokio::select! {
            event = requests.recv() => event.unwrap(),
            _ = &mut response_a => panic!("response arrived before fixture reply"),
        };
        assert_eq!(observed_a.path, "/a");
        observed_a
            .reply
            .send(Reply {
                first: b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\n\r\nAAAA",
                remainder: None,
            })
            .ok()
            .unwrap();
        let response_a = response_a.await.unwrap();
        let identity_a = ConnectionIdentity::captured(&capture_a).unwrap();
        assert!(identity_a.same_connection(response_a.extensions().get().unwrap()));
        let body_a = response_a.into_body();
        assert!(!body_a.is_end_stream());
        assert_eq!(body_a.size_hint().exact(), Some(4));

        let mut b = request(&origin, "b");
        let capture_b = capture_connection(&mut b);
        // Deliberately do not poll a capture watcher while B is dispatched.
        let response_b = client.request_raw(b);
        tokio::pin!(response_b);
        let observed_b = tokio::select! {
            event = requests.recv() => event.unwrap(),
            _ = &mut response_b => panic!("response arrived before fixture reply"),
        };
        assert_eq!(observed_b.path, "/b");
        assert_eq!(observed_a.connection, observed_b.connection);
        let identity_b = ConnectionIdentity::captured(&capture_b).unwrap();
        assert!(identity_a.same_connection(&identity_b));
        assert!(identity_a.is_alive());
        assert!(identity_a.same_connection(&ConnectionIdentity::captured(&capture_a).unwrap()));
        // A's capture is unchanged and A's Incoming still contains all bytes,
        // although the peer has already received B on that same connection.
        assert!(!body_a.is_end_stream());
        assert_eq!(body_a.size_hint().exact(), Some(4));
        if drop_old_body {
            drop(body_a);
        } else {
            assert_eq!(body_a.collect().await.unwrap().to_bytes(), "AAAA");
        }
        drop(capture_a);
        assert!(identity_b.is_alive());
        observed_b
            .reply
            .send(Reply {
                first: b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\n\r\nBBBB",
                remainder: None,
            })
            .ok()
            .unwrap();
        let response_b = response_b.await.unwrap();
        assert!(identity_b.same_connection(response_b.extensions().get().unwrap()));
        assert_eq!(
            response_b.into_body().collect().await.unwrap().to_bytes(),
            "BBBB"
        );
        identity_b
    });
    let result = tokio::time::timeout(LIMIT, scenarios.join_next()).await;
    scenarios.abort_all();
    while scenarios.join_next().await.is_some() {}
    let accepted = fixture.accepted.load(Ordering::SeqCst);
    fixture.finish().await;
    let identity = result.unwrap().unwrap().unwrap();
    assert!(!identity.is_alive(), "capture retained the transport");
    assert_eq!(
        accepted, 1,
        "fixture unexpectedly opened a sibling connection"
    );
}

#[tokio::test]
async fn http_reuse_precedes_old_incoming_and_capture_retirement() {
    run_reuse(None, false).await;
}

#[tokio::test]
async fn tls12_reuse_precedes_old_incoming_and_capture_retirement() {
    run_reuse(Some(SslVersion::TLS1_2), false).await;
}

#[tokio::test]
async fn tls13_reuse_precedes_old_incoming_and_capture_retirement() {
    run_reuse(Some(SslVersion::TLS1_3), false).await;
}

#[tokio::test]
async fn http_dropping_old_buffered_incoming_cannot_close_new_request() {
    run_reuse(None, true).await;
}

#[tokio::test]
async fn tls12_dropping_old_buffered_incoming_cannot_close_new_request() {
    run_reuse(Some(SslVersion::TLS1_2), true).await;
}

#[tokio::test]
async fn tls13_dropping_old_buffered_incoming_cannot_close_new_request() {
    run_reuse(Some(SslVersion::TLS1_3), true).await;
}

async fn run_isolation(version: Option<SslVersion>) {
    let mut fixture = Fixture::new(version).await;
    let client = fixture.client.clone();
    let origin = fixture.origin.clone();
    let (unused, replacement) = mpsc::channel(1);
    drop(unused);
    let mut requests = std::mem::replace(&mut fixture.requests, replacement);
    let mut scenarios = JoinSet::new();
    scenarios.spawn(async move {
        let mut a = request(&origin, "a");
        let capture_a = capture_connection(&mut a);
        let response_a = client.request_raw(a);
        tokio::pin!(response_a);
        let a_peer = tokio::select! {
            event = requests.recv() => event.unwrap(),
            _ = &mut response_a => panic!("response arrived before fixture reply"),
        };
        let (complete_a, remainder_a) = oneshot::channel();
        a_peer
            .reply
            .send(Reply {
                first: b"HTTP/1.1 200 OK\r\ncontent-length: 8\r\n\r\nAAAA",
                remainder: Some(remainder_a),
            })
            .ok()
            .unwrap();
        let response_a = response_a.await.unwrap();
        let identity_a = ConnectionIdentity::captured(&capture_a).unwrap();
        assert!(identity_a.same_connection(response_a.extensions().get().unwrap()));
        let mut b = request(&origin, "b");
        let capture_b = capture_connection(&mut b);
        let response_b = client.request_raw(b);
        tokio::pin!(response_b);
        let b_peer = tokio::select! {
            event = requests.recv() => event.unwrap(),
            _ = &mut response_b => panic!("response arrived before fixture reply"),
        };
        assert_eq!(b_peer.path, "/b");
        assert_ne!(a_peer.connection, b_peer.connection);
        let identity_b = ConnectionIdentity::captured(&capture_b).unwrap();
        assert!(!identity_a.same_connection(&identity_b));
        // Retaining/dropping metadata is not an operation on either connection.
        drop(capture_a);
        assert!(identity_a.is_alive());
        assert!(identity_b.is_alive());
        complete_a.send(b"AAAA").unwrap();
        assert_eq!(
            response_a.into_body().collect().await.unwrap().to_bytes(),
            "AAAAAAAA"
        );
        b_peer
            .reply
            .send(Reply {
                first: b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\n\r\nBBBB",
                remainder: None,
            })
            .ok()
            .unwrap();
        let response_b = response_b.await.unwrap();
        assert!(identity_b.same_connection(response_b.extensions().get().unwrap()));
        assert_eq!(
            response_b.into_body().collect().await.unwrap().to_bytes(),
            "BBBB"
        );
        (identity_a, identity_b)
    });
    let result = tokio::time::timeout(LIMIT, scenarios.join_next()).await;
    scenarios.abort_all();
    while scenarios.join_next().await.is_some() {}
    let accepted = fixture.accepted.load(Ordering::SeqCst);
    fixture.finish().await;
    let (a, b) = result.unwrap().unwrap().unwrap();
    assert!(!a.is_alive());
    assert!(!b.is_alive());
    assert_eq!(accepted, 2);
}

#[tokio::test]
async fn http_same_origin_pending_responses_have_distinct_selected_connections() {
    run_isolation(None).await;
}

#[tokio::test]
async fn tls12_same_origin_pending_responses_have_distinct_selected_connections() {
    run_isolation(Some(SslVersion::TLS1_2)).await;
}

#[tokio::test]
async fn tls13_same_origin_pending_responses_have_distinct_selected_connections() {
    run_isolation(Some(SslVersion::TLS1_3)).await;
}

#[tokio::test]
async fn failed_assertion_still_releases_client_tasks_sockets_and_cache() {
    let mut fixture = Fixture::new(None).await;
    let client = fixture.client.clone();
    let origin = fixture.origin.clone();
    let (unused, replacement) = mpsc::channel(1);
    drop(unused);
    let mut requests = std::mem::replace(&mut fixture.requests, replacement);
    let mut scenarios = JoinSet::new();
    scenarios.spawn(async move {
        let response = client.request_raw(request(&origin, "assertion-failure"));
        tokio::pin!(response);
        let event = tokio::select! {
            event = requests.recv() => event.unwrap(),
            _ = &mut response => panic!("response arrived before fixture reply"),
        };
        assert_eq!(event.path, "/assertion-failure");
        // Keep reply ownership until the deliberate unwind. The fixture peer
        // may then report its expected closed reply channel during cleanup.
        panic!("synthetic lifecycle assertion failure");
    });
    let result = tokio::time::timeout(LIMIT, scenarios.join_next()).await;
    scenarios.abort_all();
    while scenarios.join_next().await.is_some() {}
    // The application assertion may cause the server peer to finish with an
    // error; it is still a completed fixture task, not a leaked ownership chain.
    fixture.stop.cancel();
    fixture.finish().await;
    let error = result.unwrap().unwrap().unwrap_err();
    assert!(error.is_panic());
    let panic = error.into_panic();
    assert_eq!(
        panic.downcast_ref::<&str>(),
        Some(&"synthetic lifecycle assertion failure")
    );
}

#[tokio::test]
async fn unpolled_request_has_no_connection_task_or_capture() {
    let (trust, _) = isolated_tls(None);
    let client = SpikeHttpClient::with_snapshot(true, &trust).unwrap();
    let mut request = request("http://127.0.0.1:1", "never-polled");
    let mut capture = capture_connection(&mut request);
    let future = client.request_raw(request);
    assert!(ConnectionIdentity::captured(&capture).is_none());
    drop(future);
    assert!(ConnectionIdentity::captured(&capture).is_none());
    assert_eq!(client.raw_counts.attempts.load(Ordering::SeqCst), 0);
    assert_eq!(client.raw_counts.tasks.load(Ordering::SeqCst), 0);
    assert_eq!(client.raw_counts.active.load(Ordering::SeqCst), 0);
    assert!(
        tokio::time::timeout(LIMIT, capture.wait_for_connection_metadata())
            .await
            .unwrap()
            .is_none()
    );
}
