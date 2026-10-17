//! Actual gateway controls preserve the original held-body stimuli. These are
//! bounded local synthetic schedules, not complete transport qualification.
use super::super::super::{gateway_intent, gateway_terminal, lifecycle_tests::isolated_tls};
use super::*;
use crate::{server::Gateway, server_events::EventSinks};
use openssl::ssl::SslVersion;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
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

use super::super::super::loopback_peer::accept_peer;

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
    for version in [None, Some(SslVersion::TLS1_2), Some(SslVersion::TLS1_3)] {
        actual_empty_handoff(version).await;
    }
}

async fn actual_empty_handoff(version: Option<SslVersion>) {
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
        let (trust, tls) = isolated_tls(version);
        let client = Arc::new(BufferedRawPoolClient::new(&trust).unwrap());
        let buffers = client.probe.clone();
        let counts = client.inner.raw_counts.clone();
        let cache = Arc::downgrade(&client.inner.raw_sessions);
        let request = Request::builder()
            .method(method)
            .uri(format!(
                "{}://{}/v1/models/empty",
                if version.is_some() { "https" } else { "http" },
                peer.local_addr().unwrap()
            ))
            .body(Full::new(Bytes::new()))
            .unwrap();
        let worker = client.clone();
        let mut tasks = JoinSet::new();
        tasks.spawn(async move { worker.request_raw(request).await.unwrap() });
        let result = tokio::time::timeout(Duration::from_secs(8), async {
            let mut socket = accept_peer(&peer, tls.as_ref()).await;
            match &socket {
                super::super::super::loopback_peer::Peer::Tcp(_) => assert!(version.is_none()),
                super::super::super::loopback_peer::Peer::Tls(stream) => assert_eq!(
                    stream.inner().ssl().version_str(),
                    if version == Some(SslVersion::TLS1_2) {
                        "TLSv1.2"
                    } else {
                        "TLSv1.3"
                    }
                ),
            }
            assert!(
                head(&mut socket)
                    .await
                    .starts_with(format!("{method} /v1/models/empty HTTP/1.1\r\n").as_bytes())
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.flush().await.unwrap();
            let received = tasks.join_next().await.unwrap().unwrap();
            let expected_status: u16 = response.split_whitespace().nth(1).unwrap().parse().unwrap();
            assert_eq!(received.status().as_u16(), expected_status);
            let length = response
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length: "));
            assert_eq!(
                received
                    .headers()
                    .get("content-length")
                    .map(|value| value.to_str().unwrap()),
                length
            );
            assert!(
                received.body().is_end_stream(),
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
            assert_eq!(buffers.snapshot().clean_sources, 1);
            assert_eq!(buffers.snapshot().failed_sources, 0);
            assert_eq!(
                counts.active.load(Ordering::SeqCst),
                1,
                "empty body EOF must precede held peer socket closure"
            );
            drop(received);
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

fn consumer_free_ids(snapshot: &serde_json::Value) -> Vec<u64> {
    snapshot["free"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group.as_array().unwrap())
        .map(|id| id.as_u64().unwrap())
        .collect()
}

fn consumer_assignments(snapshot: &serde_json::Value) -> Vec<(u64, u64, bool)> {
    snapshot["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["event"] == "assigned")
        .map(|row| {
            (
                row["request"].as_u64().unwrap(),
                row["connection"].as_u64().unwrap(),
                row["reused"].as_bool().unwrap(),
            )
        })
        .collect()
}

fn consumer_peer_version(
    peer: &super::super::super::loopback_peer::Peer,
    version: Option<SslVersion>,
) {
    match peer {
        super::super::super::loopback_peer::Peer::Tcp(_) => assert!(version.is_none()),
        super::super::super::loopback_peer::Peer::Tls(stream) => assert_eq!(
            stream.inner().ssl().version_str(),
            if version == Some(SslVersion::TLS1_2) {
                "TLSv1.2"
            } else {
                "TLSv1.3"
            }
        ),
    }
}

async fn actual_consumer_pool_handoff(version: Option<SslVersion>, consume: bool) {
    use http_body_util::BodyExt;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!(
        "{}://{}",
        if version.is_some() { "https" } else { "http" },
        listener.local_addr().unwrap()
    );
    let (trust, tls) = isolated_tls(version);
    let client = Arc::new(BufferedRawPoolClient::new(&trust).unwrap());
    let buffers = client.probe.clone();
    let counts = client.inner.raw_counts.clone();
    let fetch = client.inner.fetch_counts.clone();
    let cache = Arc::downgrade(&client.inner.raw_sessions);
    let worker = client.clone();
    let mut scenarios = JoinSet::new();
    // The outer owner still performs cleanup if assertions panic or time out.
    scenarios.spawn(async move {
        let mut requests = JoinSet::new();
        let requester = worker.clone();
        let request = Request::get(format!("{origin}/v1/models/pool-0"))
            .header("x-synthetic-request", "a")
            .body(Full::new(Bytes::new()))
            .unwrap();
        requests.spawn(async move { requester.request_raw(request).await.unwrap() });
        let mut peer = Some(accept_peer(&listener, tls.as_ref()).await);
        consumer_peer_version(peer.as_ref().unwrap(), version);
        let a_head = String::from_utf8(head(peer.as_mut().unwrap()).await).unwrap();
        assert!(a_head.starts_with("GET /v1/models/pool-0 HTTP/1.1\r\n"));
        assert!(a_head.to_ascii_lowercase().lines().any(|line| line == "x-synthetic-request: a"));
        // A single byte makes the last data frame deterministic without
        // depending on TCP/TLS segmentation or closing the provider socket.
        peer.as_mut().unwrap().write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: keep-alive\r\nKeep-Alive: timeout=60\r\n\r\nA").await.unwrap();
        peer.as_mut().unwrap().flush().await.unwrap();
        let mut a = Some(requests.join_next().await.unwrap().unwrap());
        assert_eq!(a.as_ref().unwrap().status().as_u16(), 200);
        assert_eq!(a.as_ref().unwrap().headers()["content-length"], "1");
        until(|| {
            let state = worker.probe.snapshot();
            state.responses_ready == 1 && state.clean_sources == 1
                && state.producer_tasks == 0 && state.queued_bytes == 1
        }).await;
        assert_eq!(worker.probe.snapshot().consumer_polls, 0);
        assert!(!a.as_ref().unwrap().body().is_end_stream());
        let before = worker.inner.snapshot();
        let assigned = consumer_assignments(&before);
        assert_eq!(assigned.len(), 1);
        let first_id = assigned[0].1;
        assert_eq!(assigned, [(0, first_id, false)]);
        assert!(consumer_free_ids(&before).is_empty(), "producer EOF cannot return the reservation");
        assert_eq!(before["connections"], 1);
        assert_eq!(worker.inner.raw_counts.active.load(Ordering::SeqCst), 1);

        let retained = if consume {
            let bytes = a.as_mut().unwrap().body_mut().frame().await.unwrap().unwrap().into_data().unwrap();
            assert_eq!(&bytes[..], b"A");
            let retained = bytes.clone();
            drop(bytes);
            assert!(a.as_ref().unwrap().body().is_end_stream());
            assert_eq!(worker.probe.snapshot().consumer_polls, 1, "no extra None poll");
            until(|| consumer_free_ids(&worker.inner.snapshot()) == [first_id]).await;
            let state = worker.probe.snapshot();
            assert_eq!(state.queued_bytes, 0);
            assert_eq!(state.allocated_blocks, 1);
            assert_eq!(state.outstanding_blocks, 1);
            Some(retained)
        } else {
            drop(a.take());
            until(|| {
                worker.inner.snapshot()["live"] == 0
                    && worker.inner.raw_counts.active.load(Ordering::SeqCst) == 0
                    && worker.probe.snapshot().allocated_blocks == 0
            }).await;
            assert_eq!(worker.probe.snapshot().consumer_polls, 0);
            assert!(consumer_free_ids(&worker.inner.snapshot()).is_empty());
            // EOF is observed only after abandonment retired A, never as the
            // stimulus establishing A's earlier clean source-body completion.
            assert!(closed(peer.as_mut().unwrap()).await.is_empty());
            drop(peer.take());
            None
        };

        let requester = worker.clone();
        let request = Request::get(format!("{origin}/v1/models/pool-1"))
            .header("x-synthetic-request", "b")
            .body(Full::new(Bytes::new()))
            .unwrap();
        requests.spawn(async move { requester.request_raw(request).await.unwrap() });
        if !consume {
            peer = Some(accept_peer(&listener, tls.as_ref()).await);
            consumer_peer_version(peer.as_ref().unwrap(), version);
        }
        // On success this is the same accepted Peer used for A: a new
        // connection cannot satisfy this complete second request-head read.
        let b_head = String::from_utf8(head(peer.as_mut().unwrap()).await).unwrap();
        assert!(b_head.starts_with("GET /v1/models/pool-1 HTTP/1.1\r\n"));
        assert!(b_head.to_ascii_lowercase().lines().any(|line| line == "x-synthetic-request: b"));
        let selected = worker.inner.snapshot();
        let assigned = consumer_assignments(&selected);
        assert_eq!(assigned.len(), 2);
        let second_id = assigned[1].1;
        assert_eq!(assigned, [(0, first_id, false), (1, second_id, consume)]);
        assert_eq!(first_id == second_id, consume);
        assert_eq!(selected["connections"].as_u64().unwrap(), if consume { 1 } else { 2 });
        assert_eq!(selected["requests"], 2);
        assert!(consumer_free_ids(&selected).is_empty());
        drop(a.take());
        if let Some(bytes) = &retained {
            assert_eq!(&bytes[..], b"A");
            assert_eq!(worker.probe.snapshot().outstanding_blocks, 1);
        }
        peer.as_mut().unwrap().write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: keep-alive\r\nKeep-Alive: timeout=60\r\n\r\nB").await.unwrap();
        peer.as_mut().unwrap().flush().await.unwrap();
        let mut b = requests.join_next().await.unwrap().unwrap();
        assert_eq!(b.status().as_u16(), 200);
        assert_eq!(b.headers()["content-length"], "1");
        until(|| {
            let state = worker.probe.snapshot();
            state.clean_sources == 2 && state.producer_tasks == 0 && state.queued_bytes == 1
        }).await;
        let bytes = b.body_mut().frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(&bytes[..], b"B");
        assert!(b.body().is_end_stream());
        assert_eq!(worker.probe.snapshot().consumer_polls, if consume { 2 } else { 1 });
        until(|| consumer_free_ids(&worker.inner.snapshot()) == [second_id]).await;
        drop((bytes, b));
        until(|| worker.probe.snapshot().allocated_blocks == usize::from(consume)).await;
        let state = worker.probe.snapshot();
        assert_eq!(state.clean_sources, 2);
        assert_eq!(state.failed_sources, 0);
        assert_eq!(state.cancelled_sources, 0);
        assert_eq!(state.outstanding_blocks, usize::from(consume));
        assert_eq!(worker.inner.fetch_counts.attempts.load(Ordering::SeqCst), 0);
        assert!(requests.is_empty());
        (peer.take().unwrap(), retained)
    });
    let result = tokio::time::timeout(Duration::from_secs(8), scenarios.join_next()).await;
    scenarios.abort_all();
    while scenarios.join_next().await.is_some() {}
    drop(client);
    let (passed, peer, retained) = match result {
        Ok(Some(Ok((peer, retained)))) => (true, Some(peer), retained),
        _ => (false, None, None),
    };
    drop(peer);
    until(|| {
        counts.active.load(Ordering::SeqCst) == 0
            && counts.tasks.load(Ordering::SeqCst) == 0
            && counts.shutdown_handles.load(Ordering::SeqCst) == 0
            && buffers.snapshot().producer_tasks == 0
    })
    .await;
    assert!(cache.upgrade().is_none());
    assert_eq!(fetch.attempts.load(Ordering::SeqCst), 0);
    assert_eq!(fetch.active.load(Ordering::SeqCst), 0);
    assert_eq!(fetch.tasks.load(Ordering::SeqCst), 0);
    assert_eq!(fetch.shutdown_handles.load(Ordering::SeqCst), 0);
    if let Some(bytes) = &retained {
        assert_eq!(&bytes[..], b"A");
        assert_eq!(buffers.snapshot().allocated_blocks, 1);
        assert_eq!(buffers.snapshot().outstanding_blocks, 1);
    }
    drop(retained);
    until(|| {
        let state = buffers.snapshot();
        state.live_pools == 0
            && state.allocated_blocks == 0
            && state.outstanding_blocks == 0
            && state.queued_bytes == 0
            && state.producer_tasks == 0
    })
    .await;
    assert!(
        passed,
        "actual consumer pool handoff scenario failed after cleanup"
    );
}

#[tokio::test]
async fn actual_constructor_final_data_returns_pool_and_reuses_peer_with_retained_bytes() {
    for version in [None, Some(SslVersion::TLS1_2), Some(SslVersion::TLS1_3)] {
        actual_consumer_pool_handoff(version, true).await;
    }
}

#[tokio::test]
async fn actual_constructor_abandoned_prefetch_retires_peer_before_replacement() {
    for version in [None, Some(SslVersion::TLS1_2), Some(SslVersion::TLS1_3)] {
        actual_consumer_pool_handoff(version, false).await;
    }
}
