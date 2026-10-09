//! Actual gateway controls preserve the original held-body stimuli. These are
//! bounded local synthetic schedules, not complete transport qualification.
use super::super::super::{gateway_intent, gateway_terminal, lifecycle_tests::isolated_tls};
use super::*;
use crate::{server::Gateway, server_events::EventSinks};
use openssl::ssl::SslVersion;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

const TOKEN: &str = "synthetic-buffered-gateway-token";
const BARRIER: Duration = Duration::from_secs(2);
async fn until(predicate: impl Fn() -> bool) {
    tokio::time::timeout(BARRIER, async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bounded buffered gateway observation");
}
async fn head(stream: &mut (impl AsyncRead + Unpin)) -> Vec<u8> {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).await.unwrap(), 1);
        bytes.push(byte[0]);
        assert!(bytes.len() <= 16_384);
    }
    bytes
}
async fn closed(stream: &mut (impl AsyncRead + Unpin)) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut block = [0; 4096];
    loop {
        match stream.read(&mut block).await {
            Ok(0) => return bytes,
            Ok(count) => {
                bytes.extend_from_slice(&block[..count]);
                assert!(bytes.len() <= 262_144);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset
                        | io::ErrorKind::BrokenPipe
                        | io::ErrorKind::UnexpectedEof
                ) =>
            {
                return bytes;
            }
            Err(error) => panic!("unexpected read error: {error}"),
        }
    }
}
#[derive(Clone, Copy, Debug)]
enum Schedule {
    SmallHeld,
    LargeRelease,
    LargeCancel,
    BeforeAttachment,
}
async fn actual(schedule: Schedule, version: Option<SslVersion>) {
    let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!(
        "{}://{}",
        if version.is_some() { "https" } else { "http" },
        provider.local_addr().unwrap()
    );
    let config = autorouter_core::config::read_config(
        &serde_json::json!({
            "AUTOROUTER_EVALUATOR":"jev", "TYPESAFE_API_KEY":"synthetic-evaluator",
            "ANTHROPIC_API_KEY":"synthetic-provider", "AUTOROUTER_TOKEN":TOKEN,
            "AUTOROUTER_UPSTREAM_URL":origin,"AUTOROUTER_JEV_URL":format!("{origin}/v1/systemone"),
        }),
        true,
        std::path::Path::new("/synthetic"),
    )
    .unwrap();
    let (trust, tls) = isolated_tls(version);
    let client = Arc::new(BufferedRawPoolClient::new(&trust).unwrap());
    let buffers = client.probe.clone();
    let counts = client.inner.raw_counts.clone();
    let fetch_counts = client.inner.fetch_counts.clone();
    let cache = Arc::downgrade(&client.inner.raw_sessions);
    let intents = gateway_intent::Probe::default();
    intents.hold_bodies();
    let terminals = gateway_terminal::Probe::default();
    let logs = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let captured = logs.clone();
    let sinks = EventSinks {
        log: Some(Arc::new(move |document| {
            let value = serde_json::from_str(&document.stringify()).unwrap();
            let mut logs = captured.lock().unwrap();
            assert!(logs.len() < 128);
            logs.push(value);
        })),
        ..EventSinks::default()
    };
    let gateway = Gateway::with_test_terminal(
        Gateway::with_test_intent(
            Gateway::new(config, client.clone(), sinks).unwrap(),
            intents.clone(),
        ),
        terminals.clone(),
    );
    let running = gateway.listen(0).await.unwrap();
    let address = running.address;
    let observed = buffers.clone();
    let terminal = terminals.clone();
    let intent = intents.clone();
    let observed_logs = logs.clone();
    if matches!(schedule, Schedule::BeforeAttachment) {
        buffers.hold_responses();
    }
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        let mut downstream = TcpStream::connect(address).await.unwrap();
        downstream.write_all(format!("GET /v1/models/session-0 HTTP/1.1\r\nHost: synthetic\r\nX-Api-Key: {TOKEN}\r\n\r\n").as_bytes()).await.unwrap();
        let mut upstream = accept_peer(&provider, tls.as_ref()).await;
        assert!(head(&mut upstream).await.starts_with(b"GET /v1/models/session-0 HTTP/1.1\r\n"));
        upstream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 1000000\r\n\r\n").await.unwrap();
        let large = matches!(schedule, Schedule::LargeRelease | Schedule::LargeCancel);
        upstream.write_all(&vec![b'x'; if large { 131_072 } else { 7 }]).await.unwrap();
        if matches!(schedule, Schedule::BeforeAttachment) {
            until(|| observed.snapshot().responses_ready == 1).await;
        } else {
            until(|| intent.rows().iter().any(|row| row.event == gateway_intent::Event::BodyHeld)).await;
        }
        if large { until(|| observed.snapshot().queued_bytes >= buffered_body::HIGH_WATER).await; }
        assert_eq!(observed.snapshot().consumer_polls, 0, "hold must precede buffer consumer");
        assert!(!terminal.events().iter().any(|event| matches!(event, gateway_terminal::Event::Failed(..))));
        upstream.shutdown().await.unwrap();
        if large {
            // The real producer is saturated and has not polled the decoder's
            // error. A FIN written by the peer is not an observed body failure.
            assert_eq!(observed.snapshot().source_errors, 0);
            assert!(!terminal.events().iter().any(|event| matches!(event, gateway_terminal::Event::Failed(..))));
        }
        if matches!(schedule, Schedule::LargeCancel) {
            downstream.set_zero_linger().unwrap(); drop(downstream);
            until(|| terminal.events().iter().any(|event| matches!(event, gateway_terminal::Event::Failed(_, FailureCause::Downstream(_))))).await;
            let events = terminal.events();
            assert_eq!(events.iter().filter(|event| matches!(event, gateway_terminal::Event::Failed(..))).count(), 1);
        } else {
            if matches!(schedule, Schedule::LargeRelease) { intent.release_bodies(); }
            until(|| terminal.events().iter().any(|event| matches!(event, gateway_terminal::Event::Failed(_, FailureCause::Upstream)))).await;
            if matches!(schedule, Schedule::BeforeAttachment) {
                assert!(!terminal.events().iter().any(|event| matches!(event, gateway_terminal::Event::Attached(_))));
                assert!(!observed_logs.lock().unwrap().iter().any(|row| row["event"] == "upstream_response"));
                observed.release_responses();
            }
            let bytes = closed(&mut downstream).await;
            assert!(!bytes.windows(12).any(|value| value == b"HTTP/1.1 502"));
            if matches!(schedule, Schedule::SmallHeld) { assert_eq!(observed.snapshot().consumer_polls, 0); }
        }
        let _ = closed(&mut upstream).await;
        let events = terminal.events();
        assert!(!events.iter().any(|event| matches!(event, gateway_terminal::Event::DeliveryClaimed(_, crate::transport_completion::Delivery::Flushed))));
        let logs = observed_logs.lock().unwrap();
        assert!(logs.iter().any(|row| row["event"] == "upstream_response" && row["status"] == 200));
        assert!(!logs.iter().any(|row| row["event"] == "proxy_error"));
        eprintln!("buffered gateway {schedule:?} {version:?}: terminal={events:?}; log={logs:?}; buffer={:?}", observed.snapshot());
    });
    let result = tokio::time::timeout(Duration::from_secs(8), tasks.join_next()).await;
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    buffers.release_responses();
    intents.release_bodies();
    tokio::time::timeout(BARRIER, running.close())
        .await
        .unwrap();
    drop(gateway);
    drop(client);
    until(|| {
        counts.tasks.load(Ordering::SeqCst) == 0
            && counts.active.load(Ordering::SeqCst) == 0
            && fetch_counts.tasks.load(Ordering::SeqCst) == 0
            && fetch_counts.active.load(Ordering::SeqCst) == 0
            && terminals.tasks() == 0
            && buffers.snapshot().producer_tasks == 0
            && buffers.snapshot().allocated_blocks == 0
    })
    .await;
    assert_eq!(counts.shutdown_handles.load(Ordering::SeqCst), 0);
    assert_eq!(fetch_counts.shutdown_handles.load(Ordering::SeqCst), 0);
    assert_eq!(intents.live(), 0);
    assert!(cache.upgrade().is_none());
    eprintln!(
        "buffered cleanup {schedule:?} {version:?}: buffers={:?}, raw_tasks=0 raw_live=0 fetch_tasks=0 fetch_live=0 terminal_tasks=0 shutdown_handles=0 cache_released=true",
        buffers.snapshot()
    );
    result
        .expect("8 second outer scenario bound")
        .unwrap()
        .unwrap();
}
macro_rules! actual_case {
    ($name:ident, $schedule:ident, $version:expr) => {
        #[tokio::test]
        async fn $name() {
            actual(Schedule::$schedule, $version).await;
        }
    };
}
actual_case!(original_http_small_held_error, SmallHeld, None);
actual_case!(original_http_large_release_error, LargeRelease, None);
actual_case!(original_http_large_cancel, LargeCancel, None);
actual_case!(
    original_tls12_small_held_error,
    SmallHeld,
    Some(SslVersion::TLS1_2)
);
actual_case!(
    original_tls12_large_release_error,
    LargeRelease,
    Some(SslVersion::TLS1_2)
);
actual_case!(
    original_tls12_large_cancel,
    LargeCancel,
    Some(SslVersion::TLS1_2)
);
actual_case!(
    original_tls13_small_held_error,
    SmallHeld,
    Some(SslVersion::TLS1_3)
);
actual_case!(
    original_tls13_large_release_error,
    LargeRelease,
    Some(SslVersion::TLS1_3)
);
actual_case!(
    original_tls13_large_cancel,
    LargeCancel,
    Some(SslVersion::TLS1_3)
);
actual_case!(
    http_body_error_before_response_attachment,
    BeforeAttachment,
    None
);
actual_case!(
    tls12_body_error_before_response_attachment,
    BeforeAttachment,
    Some(SslVersion::TLS1_2)
);
actual_case!(
    tls13_body_error_before_response_attachment,
    BeforeAttachment,
    Some(SslVersion::TLS1_3)
);

enum Peer {
    Tcp(TcpStream),
    Tls(hyper_util::rt::TokioIo<hyper_openssl::SslStream<hyper_util::rt::TokioIo<TcpStream>>>),
}
async fn accept_peer(listener: &TcpListener, context: Option<&openssl::ssl::SslContext>) -> Peer {
    let (stream, _) = listener.accept().await.unwrap();
    if let Some(context) = context {
        let mut stream = hyper_openssl::SslStream::new(
            openssl::ssl::Ssl::new(context).unwrap(),
            hyper_util::rt::TokioIo::new(stream),
        )
        .unwrap();
        Pin::new(&mut stream).accept().await.unwrap();
        Peer::Tls(hyper_util::rt::TokioIo::new(stream))
    } else {
        Peer::Tcp(stream)
    }
}
impl AsyncRead for Peer {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}
impl AsyncWrite for Peer {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

#[tokio::test]
async fn producer_eof_does_not_return_pool_before_consumer_final_data() {
    use http_body_util::BodyExt;
    let results = Arc::new(Mutex::new(Vec::new()));
    let completed = results.clone();
    let (buffer, producer, probe) =
        buffered_body::start(Full::new(Bytes::from_static(b"accepted")), |_| {});
    let mut body = OwnedBody::new(
        buffer,
        Some(Box::new(move |clean| completed.lock().unwrap().push(clean))),
        None,
    );
    producer.join().await.unwrap();
    assert!(
        results.lock().unwrap().is_empty(),
        "producer has no pool-return authority"
    );
    assert!(!body.is_end_stream());
    let frame = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert_eq!(frame, "accepted");
    assert!(body.is_end_stream());
    assert_eq!(*results.lock().unwrap(), vec![true]);
    drop((frame, body));
    until(|| probe.snapshot().allocated_blocks == 0).await;
    assert_eq!(*results.lock().unwrap(), vec![true]);
}

#[tokio::test]
async fn dropping_prefetched_body_does_not_report_consumer_completion() {
    let results = Arc::new(Mutex::new(Vec::new()));
    let completed = results.clone();
    let (buffer, producer, probe) =
        buffered_body::start(Full::new(Bytes::from_static(b"accepted")), |_| {});
    let body = OwnedBody::new(
        buffer,
        Some(Box::new(move |clean| completed.lock().unwrap().push(clean))),
        None,
    );
    producer.join().await.unwrap();
    drop(body);
    assert_eq!(*results.lock().unwrap(), vec![false]);
    until(|| probe.snapshot().allocated_blocks == 0).await;
}

#[tokio::test]
async fn aggregate_accounting_retains_detached_pools_until_final_bytes_clone_drop() {
    use http_body_util::BodyExt;
    let aggregate = BufferProbe::default();
    let mut retained = Vec::new();
    for _ in 0..8 {
        let (mut body, producer, probe) =
            buffered_body::start(Full::new(Bytes::from_static(b"x")), |_| {});
        aggregate.insert(probe);
        producer.join().await.unwrap();
        let bytes = body.frame().await.unwrap().unwrap().into_data().unwrap();
        retained.push(bytes.clone());
        drop((bytes, body));
    }
    let snapshot = aggregate.snapshot();
    assert_eq!(snapshot.live_pools, 8);
    assert_eq!(snapshot.allocated_blocks, 8);
    assert_eq!(snapshot.outstanding_blocks, 8);
    assert_eq!(snapshot.producer_tasks, 0);
    assert_eq!(snapshot.queued_bytes, 0);
    drop(retained);
    assert_eq!(aggregate.snapshot().allocated_blocks, 0);
    assert_eq!(aggregate.snapshot().live_pools, 0);
}

#[tokio::test]
async fn header_only_handoff_retires_after_explicit_empty_consumer_handoff() {
    let results = Arc::new(Mutex::new(Vec::new()));
    let completed = results.clone();
    let (buffer, producer, probe) = buffered_body::start(Full::new(Bytes::new()), |_| {});
    producer.join().await.unwrap();
    let body = OwnedBody::new(
        buffer,
        Some(Box::new(move |clean| completed.lock().unwrap().push(clean))),
        None,
    );
    assert!(body.is_end_stream());
    assert_eq!(*results.lock().unwrap(), vec![true]);
    assert_eq!(probe.snapshot().consumer_polls, 0);
    drop(body);
    assert_eq!(*results.lock().unwrap(), vec![true]);
}

#[tokio::test]
async fn registry_abort_cancels_and_releases_a_saturated_producer_join() {
    let (trust, _) = isolated_tls(None);
    let client = BufferedRawPoolClient::new(&trust).unwrap();
    let counts = client.inner.raw_counts.clone();
    let (body, producer, probe) =
        buffered_body::start(Full::new(Bytes::from(vec![0; 131_072])), |_| {});
    spawn_owned(&client.inner.shared, async move {
        let _ = producer.join().await;
    })
    .disarm();
    until(|| probe.snapshot().queued_bytes >= buffered_body::HIGH_WATER).await;
    drop(client);
    until(|| counts.tasks.load(Ordering::SeqCst) == 0 && probe.snapshot().producer_tasks == 0)
        .await;
    assert_eq!(probe.snapshot().end, Some(End::Cancelled));
    drop(body);
    until(|| probe.snapshot().allocated_blocks == 0).await;
}

#[tokio::test]
async fn actual_constructor_empty_handoff_needs_no_consumer_body_poll() {
    for (method, response) in [
        ("GET", "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"),
        ("HEAD", "HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n"),
        ("GET", "HTTP/1.1 204 No Content\r\n\r\n"),
        (
            "GET",
            "HTTP/1.1 304 Not Modified\r\nContent-Length: 1000000\r\n\r\n",
        ),
    ] {
        let peer = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (trust, _) = isolated_tls(None);
        let client = Arc::new(BufferedRawPoolClient::new(&trust).unwrap());
        let buffers = client.probe.clone();
        let counts = client.inner.raw_counts.clone();
        let cache = Arc::downgrade(&client.inner.raw_sessions);
        let request = Request::builder()
            .method(method)
            .uri(format!(
                "http://{}/v1/models/empty",
                peer.local_addr().unwrap()
            ))
            .body(Full::new(Bytes::new()))
            .unwrap();
        let worker = client.clone();
        let mut tasks = JoinSet::new();
        tasks.spawn(async move { worker.request_raw(request).await.unwrap() });
        let result = tokio::time::timeout(Duration::from_secs(8), async {
            let (mut socket, _) = peer.accept().await.unwrap();
            let _ = head(&mut socket).await;
            socket.write_all(response.as_bytes()).await.unwrap();
            let response = tasks.join_next().await.unwrap().unwrap();
            assert!(
                response.body().is_end_stream(),
                "{method} header-only handoff was hidden"
            );
            assert_eq!(buffers.snapshot().consumer_polls, 0);
            until(|| {
                client.inner.snapshot()["free"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|group| !group.as_array().unwrap().is_empty())
            })
            .await;
            assert_eq!(buffers.snapshot().consumer_polls, 0);
            drop(response);
            socket
        })
        .await;
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        drop(client);
        drop(result.expect("empty response outer bound"));
        until(|| {
            counts.active.load(Ordering::SeqCst) == 0
                && counts.tasks.load(Ordering::SeqCst) == 0
                && buffers.snapshot().producer_tasks == 0
                && buffers.snapshot().allocated_blocks == 0
        })
        .await;
        assert!(cache.upgrade().is_none());
        assert_eq!(counts.shutdown_handles.load(Ordering::SeqCst), 0);
    }
}
