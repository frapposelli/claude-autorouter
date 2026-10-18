//! Actual gateway controls preserve the original held-body stimuli. These are
//! bounded local synthetic schedules, not complete transport qualification.
use super::super::super::{
    gateway_intent, gateway_terminal, lifecycle_tests::isolated_tls, loopback_peer::bind_listener,
};
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
    let (provider, origin) = bind_listener(version.is_some()).await;
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
    crate::bounded(running.close()).await;
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

    let (listener, origin) = bind_listener(version.is_some()).await;
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

// Actual buffered raw A/B plus fetch C and another-origin raw D. No inferred
// TLS-session survival: error-close eviction deliberately remains origin-keyed.
struct SiblingRequest {
    origin: usize,
    connection: usize,
    path: String,
    reply: tokio::sync::oneshot::Sender<super::super::super::loopback_peer::Reply>,
}
fn sibling_peers(
    listener: TcpListener,
    tls: Option<openssl::ssl::SslContext>,
    version: Option<SslVersion>,
    origin: usize,
    send: tokio::sync::mpsc::Sender<SiblingRequest>,
) -> super::super::super::loopback_peer::ListenerPeers {
    use super::super::super::loopback_peer::{ListenerPeers, Peer, read_head, write_reply};
    ListenerPeers::spawn(
        listener,
        tls,
        |accepted| {
            let id = accepted.fetch_add(1, Ordering::SeqCst) + 1;
            if id > 8 {
                return Err(io::Error::other("sibling peer admission bound"));
            }
            Ok(id)
        },
        "sibling provider task failed",
        "sibling TLS handshake failed",
        move |mut stream, connection| async move {
            match (&stream, version) {
                (Peer::Tcp(_), None) => {}
                (Peer::Tls(stream), Some(version)) => {
                    let expected = if version == SslVersion::TLS1_2 {
                        "TLSv1.2"
                    } else {
                        "TLSv1.3"
                    };
                    if stream.inner().ssl().version_str() != expected {
                        return Err(io::Error::other("sibling negotiated protocol mismatch"));
                    }
                }
                _ => return Err(io::Error::other("sibling protocol mismatch")),
            }
            for _ in 0..4 {
                let Some(bytes) = read_head(&mut stream).await? else {
                    return Ok(());
                };
                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| io::Error::other("sibling header encoding"))?;
                let mut line = text
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .split_ascii_whitespace();
                if line.next() != Some("GET") {
                    return Err(io::Error::other("sibling method"));
                }
                let path = line.next().unwrap_or_default().to_owned();
                if line.next() != Some("HTTP/1.1")
                    || line.next().is_some()
                    || !matches!(
                        path.as_str(),
                        "/v1/models/pool-0"
                            | "/v1/models/pool-1"
                            | "/v1/models/fetch-2"
                            | "/v1/models/pool-3"
                            | "/v1/models/pool-4"
                            | "/v1/models/pool-5"
                    )
                {
                    return Err(io::Error::other("sibling request target"));
                }
                if text.to_ascii_lowercase().contains("connection: close\r\n") {
                    return Err(io::Error::other("downstream close must not reach provider"));
                }
                let (reply, receive) = tokio::sync::oneshot::channel();
                send.send(SiblingRequest {
                    origin,
                    connection,
                    path,
                    reply,
                })
                .await
                .map_err(|_| io::Error::other("sibling receiver closed"))?;
                if !write_reply(&mut stream, receive).await? {
                    return Ok(());
                }
            }
            Err(io::Error::other("sibling request-per-peer bound"))
        },
    )
}
async fn sibling_downstream(address: std::net::SocketAddr, label: usize) -> TcpStream {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(format!("GET /v1/models/pool-{label} HTTP/1.1\r\nHost: synthetic\r\nX-Api-Key: {TOKEN}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    stream
}
// Decode this finite fixture's downstream HTTP/1 entity, not upstream CL.
// Gateway clean_headers removes CL/TE; the native encoder owns chunk framing.
struct SiblingWire {
    stream: TcpStream,
    expected: usize,
    decoded: usize,
    chunk_remaining: usize,
    chunks: usize,
    wire_bytes: usize,
    entity_ended: bool,
    socket_end: Option<&'static str>,
}
impl SiblingWire {
    async fn new(mut stream: TcpStream, owner: &str, expected: usize) -> Self {
        let bytes = head(&mut stream).await;
        let text = std::str::from_utf8(&bytes).unwrap();
        let mut lines = text.strip_suffix("\r\n\r\n").unwrap().split("\r\n");
        assert!(lines.next().unwrap().starts_with("HTTP/1.1 200 "));
        let mut headers = hyper::HeaderMap::new();
        for line in lines {
            let (name, value) = line.split_once(':').expect("downstream header separator");
            let name = hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap();
            assert!(!headers.contains_key(&name), "duplicate downstream header");
            let value =
                hyper::header::HeaderValue::from_str(value.trim_matches([' ', '\t'])).unwrap();
            headers.insert(name, value);
        }
        assert!(!headers.contains_key("content-length"));
        assert_eq!(headers["transfer-encoding"], "chunked");
        assert_eq!(headers["connection"], "close");
        assert_eq!(headers["x-owner"], owner);
        assert!(matches!(expected, 6 | 11));
        Self {
            stream,
            expected,
            decoded: 0,
            chunk_remaining: 0,
            chunks: 0,
            wire_bytes: bytes.len(),
            entity_ended: false,
            socket_end: None,
        }
    }
    async fn raw_exact(&mut self, count: usize) -> Vec<u8> {
        assert!(
            count <= 65_536 - self.wire_bytes,
            "downstream cumulative wire bound"
        );
        let mut bytes = vec![0; count];
        self.stream.read_exact(&mut bytes).await.unwrap();
        self.wire_bytes += count;
        bytes
    }
    async fn line(&mut self) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            assert!(bytes.len() < 4096, "downstream chunk line bound");
            bytes.extend_from_slice(&self.raw_exact(1).await);
            if bytes.ends_with(b"\r\n") {
                bytes.truncate(bytes.len() - 2);
                return bytes;
            }
        }
    }
    async fn next_chunk(&mut self) {
        assert!(!self.entity_ended && self.chunk_remaining == 0);
        let line = self.line().await;
        assert!(
            (1..=8).contains(&line.len()) && line.iter().all(u8::is_ascii_hexdigit),
            "unexpected downstream chunk size or extension"
        );
        let count = usize::from_str_radix(std::str::from_utf8(&line).unwrap(), 16).unwrap();
        assert!(
            count <= self.expected - self.decoded,
            "excess downstream entity"
        );
        if count == 0 {
            assert!(
                self.line().await.is_empty(),
                "unexpected downstream trailers"
            );
            self.entity_ended = true;
        } else {
            self.chunk_remaining = count;
            self.chunks += 1;
            assert!(self.chunks <= self.expected, "downstream chunk count bound");
        }
    }
    async fn take(&mut self, count: usize) -> Vec<u8> {
        assert!(count <= self.expected - self.decoded);
        let mut bytes = Vec::with_capacity(count);
        while bytes.len() < count {
            if self.chunk_remaining == 0 {
                self.next_chunk().await;
            }
            assert!(!self.entity_ended, "early downstream entity end");
            let read = self.chunk_remaining.min(count - bytes.len());
            bytes.extend_from_slice(&self.raw_exact(read).await);
            self.chunk_remaining -= read;
            self.decoded += read;
            if self.chunk_remaining == 0 {
                assert_eq!(self.raw_exact(2).await, b"\r\n");
            }
        }
        bytes
    }
    async fn rest(&mut self) -> Vec<u8> {
        let mut bytes = Vec::new();
        while !self.entity_ended {
            if self.chunk_remaining == 0 {
                self.next_chunk().await;
            }
            if !self.entity_ended {
                bytes.extend_from_slice(&self.take(self.chunk_remaining).await);
            }
        }
        assert_eq!(self.decoded, self.expected);
        let mut trailing = [0u8; 1];
        // Preserve the old closed() helper's accepted physical-close outcomes,
        // after independently requiring the real terminal chunk and trailers.
        match self.stream.read(&mut trailing).await {
            Ok(0) => self.socket_end = Some("eof"),
            Ok(count) => {
                self.wire_bytes += count;
                assert!(self.wire_bytes <= 65_536);
                panic!("trailing downstream wire bytes");
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset
                        | io::ErrorKind::BrokenPipe
                        | io::ErrorKind::UnexpectedEof
                ) =>
            {
                self.socket_end = Some("reset_after_entity");
            }
            Err(error) => panic!("unexpected downstream close error: {error}"),
        }
        bytes
    }
    fn framing(&self) -> serde_json::Value {
        serde_json::json!({
            "content_length":null,"transfer_encoding":"chunked","connection":"close",
            "expected_entity_bytes":self.expected,"decoded_bytes":self.decoded,
            "chunks":self.chunks,"wire_bytes":self.wire_bytes,
            "clean_entity_end":self.entity_ended,"socket_end":self.socket_end,
        })
    }
}
async fn sibling_prefix(stream: TcpStream, owner: &str) -> (SiblingWire, Vec<u8>) {
    let mut wire = SiblingWire::new(stream, owner, 11).await;
    let prefix = wire.take(7).await;
    assert_eq!(prefix.as_slice(), format!("{owner}-first").as_bytes());
    assert_eq!(
        wire.chunk_remaining, 0,
        "held seven-byte prefix ends at an actual chunk delimiter"
    );
    assert!(!wire.entity_ended);
    (wire, prefix)
}
async fn sibling_catch<F: Future>(future: F) -> Result<F::Output, Box<dyn std::any::Any + Send>> {
    let mut future = Box::pin(future);
    std::future::poll_fn(move |cx| {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(result)) => Poll::Ready(Ok(result)),
            Err(error) => Poll::Ready(Err(error)),
        }
    })
    .await
}
// RequestOwner::claim_delivery removes the record and marks it retired before
// publishing delivery. Retired is the alternative owner-Drop event, not an
// extra event that successful or failed delivery callbacks must also emit.
fn sibling_assert_callbacks(events: &[gateway_terminal::Event], expected: usize, failed: bool) {
    use super::super::super::gateway_terminal::Event;
    use crate::transport_completion::Delivery;
    let registered: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::Registered(identity) => Some(*identity),
            _ => None,
        })
        .collect();
    assert_eq!(registered.len(), expected);
    for (index, identity) in registered.iter().enumerate() {
        assert!(
            !registered[..index].contains(identity),
            "duplicate registered identity"
        );
    }
    for event in events {
        match event {
            Event::DeliveryClaimed(identity, delivery) => {
                assert!(registered.contains(identity), "foreign delivery identity");
                if failed {
                    assert!(
                        matches!(delivery, Delivery::Failed(_)),
                        "A cannot claim successful delivery"
                    );
                } else {
                    assert_eq!(*delivery, Delivery::Flushed);
                }
            }
            Event::CallbackFinished(identity) => {
                assert!(
                    registered.contains(identity),
                    "foreign completed callback identity"
                );
            }
            Event::CallbackAbandoned(_) => panic!("sibling callback abandoned"),
            Event::Failed(identity, _) => {
                assert!(
                    registered.contains(identity),
                    "foreign terminal failure identity"
                );
            }
            _ => {}
        }
    }
    for identity in registered {
        assert_eq!(
            events
                .iter()
                .filter(
                    |event| matches!(event, Event::DeliveryClaimed(found, _) if *found == identity)
                )
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(
                    |event| matches!(event, Event::CallbackFinished(found) if *found == identity)
                )
                .count(),
            1
        );
    }
}
async fn buffered_sibling_isolation(version: Option<SslVersion>) {
    use super::super::super::gateway_terminal::{Event, FailureCause};
    use super::super::super::loopback_peer::Reply;
    use http_body_util::BodyExt;
    let (trust, tls) = isolated_tls(version);
    let client = Arc::new(BufferedRawPoolClient::new(&trust).unwrap());
    let raw = client.inner.raw_counts.clone();
    let fetch = client.inner.fetch_counts.clone();
    let buffers = client.probe.clone();
    let cache = Arc::downgrade(&client.inner.raw_sessions);
    let (listener, origin) = bind_listener(version.is_some()).await;
    let (other_listener, other_origin) = bind_listener(version.is_some()).await;
    let (send, mut receive) = tokio::sync::mpsc::channel(8);
    let mut peers = vec![
        sibling_peers(listener, tls.clone(), version, 0, send.clone()),
        sibling_peers(other_listener, tls, version, 1, send),
    ];
    let mut gateways = Vec::new();
    let mut running = Vec::new();
    let mut terminals = Vec::new();
    let mut intents = Vec::new();
    for upstream in [&origin, &origin, &other_origin] {
        let config = autorouter_core::config::read_config(&serde_json::json!({
            "AUTOROUTER_EVALUATOR":"jev", "TYPESAFE_API_KEY":"synthetic-evaluator",
            "ANTHROPIC_API_KEY":"synthetic-provider", "AUTOROUTER_TOKEN":TOKEN,
            "AUTOROUTER_UPSTREAM_URL":upstream, "AUTOROUTER_JEV_URL":format!("{origin}/v1/systemone"),
        }), true, std::path::Path::new("/synthetic")).unwrap();
        let terminal = gateway_terminal::Probe::default();
        let intent = gateway_intent::Probe::default();
        let gateway = Gateway::with_test_terminal(
            Gateway::with_test_intent(
                Gateway::new(config, client.clone(), EventSinks::default()).unwrap(),
                intent.clone(),
            ),
            terminal.clone(),
        );
        running.push(gateway.listen(0).await.unwrap());
        gateways.push(gateway);
        terminals.push(terminal);
        intents.push(intent);
    }
    let addresses = [running[0].address, running[1].address, running[2].address];
    let stage = std::sync::Mutex::new("initial_held_responses");
    let result = tokio::time::timeout(Duration::from_secs(15), sibling_catch(async {
        let a = sibling_downstream(addresses[0], 0).await;
        let b = sibling_downstream(addresses[1], 1).await;
        let d = sibling_downstream(addresses[2], 3).await;
        let request = Request::get(format!("{origin}/v1/models/fetch-2")).body(Full::new(Bytes::new())).unwrap();
        let fetch_response = client.request(request);
        tokio::pin!(fetch_response);
        let mut fetched = None;
        let mut ownership = std::collections::BTreeMap::new();
        let mut release = std::collections::BTreeMap::new();
        while ownership.len() < 4 {
            tokio::select! {
                row = receive.recv() => {
                    let row = row.unwrap();
                    let (label, expected_origin, wire): (&str, usize, &'static [u8]) = match row.path.as_str() {
                        "/v1/models/pool-0" => ("A", 0, b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: keep-alive\r\nX-Owner: A\r\n\r\nA-first"),
                        "/v1/models/pool-1" => ("B", 0, b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: keep-alive\r\nX-Owner: B\r\n\r\nB-first"),
                        "/v1/models/fetch-2" => ("C", 0, b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: keep-alive\r\nX-Owner: C\r\n\r\nC-first"),
                        "/v1/models/pool-3" => ("D", 1, b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: keep-alive\r\nX-Owner: D\r\n\r\nD-first"),
                        _ => panic!("unexpected initial sibling request"),
                    };
                    assert_eq!(row.origin, expected_origin);
                    assert!(ownership.insert(label, (row.origin, row.connection)).is_none());
                    let (send, remainder) = tokio::sync::oneshot::channel();
                    assert!(row.reply.send(Reply { first: wire, remainder: Some(remainder) }).is_ok());
                    assert!(release.insert(label, send).is_none());
                },
                value = &mut fetch_response, if fetched.is_none() => fetched = Some(value.unwrap()),
            }
        }
        let fetched = match fetched { Some(value) => value, None => fetch_response.await.unwrap() };
        assert_eq!(fetched.status(), 200);
        assert_eq!(fetched.headers()["content-length"], "11");
        assert_eq!(fetched.headers()["x-owner"], "C");
        let (a, a_prefix) = sibling_prefix(a, "A").await;
        let (mut b, mut b_bytes) = sibling_prefix(b, "B").await;
        let (mut d, mut d_bytes) = sibling_prefix(d, "D").await;
        assert_ne!(ownership["A"], ownership["B"]);
        assert_ne!(ownership["A"], ownership["C"]);
        assert_ne!(ownership["B"], ownership["C"]);
        assert_eq!(raw.attempts.load(Ordering::SeqCst), 3);
        assert_eq!(fetch.attempts.load(Ordering::SeqCst), 1);
        assert!(client.inner.snapshot()["free"].as_array().unwrap().is_empty());
        for terminal in &terminals {
            assert_eq!(terminal.events().iter().filter(|event| matches!(event, Event::Registered(_))).count(), 1);
            assert!(!terminal.events().iter().any(|event| matches!(event, Event::Failed(..) | Event::DeliveryClaimed(..))));
        }
        // Only A's real downstream is reset. Its provider remains held, as do
        // B/C/D; no peer closure is used to manufacture client retirement.
        *stage.lock().unwrap() = "A_reset_retirement_raw_active2";
        let a_framing = a.framing();
        a.stream.set_zero_linger().unwrap();
        drop(a);
        until(|| terminals[0].events().iter().filter(|event| matches!(event, Event::CallbackFinished(_))).count() == 1
            && raw.active.load(Ordering::SeqCst) == 2).await;
        let a_events = terminals[0].events();
        sibling_assert_callbacks(&a_events, 1, true);
        assert_eq!(a_events.iter().filter(|event| matches!(event, Event::Failed(_, FailureCause::Downstream(_)))).count(), 1);
        assert!(!a_events.iter().any(|event| matches!(event, Event::DeliveryClaimed(_, crate::transport_completion::Delivery::Flushed))));
        assert_eq!(fetch.active.load(Ordering::SeqCst), 1);
        for terminal in &terminals[1..] {
            assert!(!terminal.events().iter().any(|event| matches!(event, Event::Failed(..) | Event::DeliveryClaimed(..))));
        }
        assert_eq!(raw.shutdown_calls.load(Ordering::SeqCst), 1);
        if version.is_some() { assert_eq!(raw.session_error_closes.load(Ordering::SeqCst), 1); }
        // The deliberately uncompleted A peer can now end its fixture wait.
        drop(release.remove("A").unwrap());
        for label in ["B", "C", "D"] { release.remove(label).unwrap().send(b"-end").unwrap(); }
        assert!(release.is_empty());
        let b_tail = b.rest().await;
        let d_tail = d.rest().await;
        assert_eq!(b_tail, b"-end"); assert_eq!(d_tail, b"-end");
        b_bytes.extend_from_slice(&b_tail); d_bytes.extend_from_slice(&d_tail);
        let mut body = fetched.into_body();
        let mut bytes = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.unwrap();
            assert!(frame.trailers_ref().is_none());
            if let Ok(data) = frame.into_data() { bytes.extend_from_slice(&data); }
            assert!(bytes.len() <= 11);
        }
        assert_eq!(bytes, b"C-first-end");
        drop(body);
        *stage.lock().unwrap() = "B_D_raw_free_after_all_entities";
        until(|| client.inner.snapshot()["free"].as_array().unwrap().iter()
            .map(|ids| ids.as_array().unwrap().len()).sum::<usize>() == 2).await;
        let mut followups = Vec::new();
        for (index, label, path, expected_owner, wire) in [
            (1usize, 4usize, "/v1/models/pool-4", "B", b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: keep-alive\r\nX-Owner: B\r\n\r\nB-next".as_slice()),
            (2, 5, "/v1/models/pool-5", "D", b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: keep-alive\r\nX-Owner: D\r\n\r\nD-next".as_slice()),
        ] {
            let downstream = sibling_downstream(addresses[index], label).await;
            let row = receive.recv().await.unwrap();
            assert_eq!(row.path, path);
            assert_eq!((row.origin, row.connection), ownership[expected_owner]);
            assert!(row.reply.send(Reply { first: wire, remainder: None }).is_ok());
            let mut downstream = SiblingWire::new(downstream, expected_owner, 6).await;
            let next_body = downstream.rest().await;
            assert_eq!(next_body, format!("{expected_owner}-next").as_bytes());
            followups.push(serde_json::json!({"path":path,"status":200,"owner":expected_owner,"body":next_body,"framing":downstream.framing(),"provider_connection":[row.origin,row.connection]}));
        }
        *stage.lock().unwrap() = "B_D_followup_terminal_retirement";
        until(|| terminals[1..].iter().all(|terminal|
            terminal.events().iter().filter(|event| matches!(event, Event::CallbackFinished(_))).count() == 2)).await;
        for terminal in &terminals[1..] {
            let events = terminal.events();
            sibling_assert_callbacks(&events, 2, false);
            assert_eq!(events.iter().filter(|event| matches!(event, Event::Registered(_))).count(), 2);
            assert_eq!(events.iter().filter(|event| matches!(event, Event::DeliveryClaimed(_, crate::transport_completion::Delivery::Flushed))).count(), 2);
            assert!(!events.iter().any(|event| matches!(event, Event::Failed(..))));
        }
        assert_eq!(raw.attempts.load(Ordering::SeqCst), 3);
        assert_eq!(fetch.attempts.load(Ordering::SeqCst), 1);
        assert_eq!(raw.shutdown_calls.load(Ordering::SeqCst), 1);
        let state = client.inner.snapshot();
        assert_eq!(state["requests"], 5);
        let assigned: Vec<_> = state["rows"].as_array().unwrap().iter().filter(|row| row["event"] == "assigned").collect();
        assert_eq!(assigned.len(), 5);
        for (old, new) in [(1usize, 4usize), (3, 5)] {
            let first = assigned.iter().find(|row| row["request"] == old).unwrap();
            let later = assigned.iter().find(|row| row["request"] == new).unwrap();
            assert_eq!(first["connection"], later["connection"]);
            assert_eq!(later["reused"], true);
        }
        assert_eq!(peers[0].accepted.load(Ordering::SeqCst), 3);
        assert_eq!(peers[1].accepted.load(Ordering::SeqCst), 1);
        assert!(receive.try_recv().is_err());
        eprintln!("buffered_sibling_isolation={}", serde_json::json!({
            "version":format!("{version:?}"),"A":{"status":200,"prefix":a_prefix,"framing":a_framing,"deliberately_abandoned":true},
            "B":{"status":200,"body":b_bytes,"framing":b.framing()},"C":{"status":200,"body":bytes},"D":{"status":200,"body":d_bytes,"framing":d.framing()},
            "followups":followups,"provider_ownership":ownership,"raw":state,
            "raw_attempts":raw.attempts.load(Ordering::SeqCst),"fetch_attempts":fetch.attempts.load(Ordering::SeqCst),
            "A_events":format!("{:?}", terminals[0].events()),"B_events":format!("{:?}",terminals[1].events()),
            "D_events":format!("{:?}",terminals[2].events()),"no_resumption_claim":true}));
    })).await;
    if !matches!(&result, Ok(Ok(()))) {
        eprintln!(
            "buffered_sibling_failure={}",
            serde_json::json!({
                "version":format!("{version:?}"), "stage":*stage.lock().unwrap(),
                "raw":client.inner.snapshot(), "raw_active":raw.active.load(Ordering::SeqCst),
                "raw_tasks":raw.tasks.load(Ordering::SeqCst),
                "raw_shutdown_calls":raw.shutdown_calls.load(Ordering::SeqCst),
                "fetch_active":fetch.active.load(Ordering::SeqCst),
                "fetch_tasks":fetch.tasks.load(Ordering::SeqCst),
                "buffer":format!("{:?}", buffers.snapshot()),
                "terminals":terminals.iter().map(|p| format!("{:?}",p.events())).collect::<Vec<_>>(),
                "intents":intents.iter().map(|p| format!("{:?}",p.rows())).collect::<Vec<_>>(),
            })
        );
    }
    // Cleanup precedes verdict, including panic/deadline paths. Existing peer
    // owner joins its nested tasks; no cleanup-success claim if a bound fails.
    let mut cleanup_failed = false;
    for handle in running {
        cleanup_failed |= tokio::time::timeout(Duration::from_secs(5), handle.close())
            .await
            .is_err();
    }
    drop(gateways);
    drop(client);
    let clients_closed = tokio::time::timeout(Duration::from_secs(5), async {
        while raw.active.load(Ordering::SeqCst) != 0
            || raw.tasks.load(Ordering::SeqCst) != 0
            || fetch.active.load(Ordering::SeqCst) != 0
            || fetch.tasks.load(Ordering::SeqCst) != 0
            || buffers.snapshot().producer_tasks != 0
            || buffers.snapshot().allocated_blocks != 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    for peer in &peers {
        peer.stop.cancel();
    }
    for peer in &mut peers {
        let joined = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(value) = peer.tasks.join_next().await {
                if !matches!(value, Ok(Ok(()))) {
                    cleanup_failed = true;
                }
            }
        })
        .await;
        if joined.is_err() {
            cleanup_failed = true;
            peer.tasks.abort_all();
            if tokio::time::timeout(Duration::from_secs(5), async {
                while peer.tasks.join_next().await.is_some() {}
            })
            .await
            .is_err()
            {
                cleanup_failed = true;
            }
        }
        cleanup_failed |= peer.active.load(Ordering::SeqCst) != 0;
    }
    assert!(
        !cleanup_failed && clients_closed,
        "sibling cleanup exceeded declared bounds"
    );
    assert_eq!(raw.shutdown_handles.load(Ordering::SeqCst), 0);
    assert_eq!(fetch.shutdown_handles.load(Ordering::SeqCst), 0);
    assert!(cache.upgrade().is_none());
    for terminal in &terminals {
        assert_eq!(terminal.tasks(), 0);
        assert!(!terminal.observer_failed());
    }
    for intent in &intents {
        assert_eq!(intent.live(), 0);
    }
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => std::panic::resume_unwind(error),
        Err(_) => panic!("15s buffered sibling scenario deadline"),
    }
}
#[tokio::test]
async fn actual_buffered_http_cancel_preserves_same_origin_sibling_fetch_and_other_origin() {
    buffered_sibling_isolation(None).await;
}
#[tokio::test]
async fn actual_buffered_tls12_cancel_preserves_same_origin_sibling_fetch_and_other_origin() {
    buffered_sibling_isolation(Some(SslVersion::TLS1_2)).await;
}
#[tokio::test]
async fn actual_buffered_tls13_cancel_preserves_same_origin_sibling_fetch_and_other_origin() {
    buffered_sibling_isolation(Some(SslVersion::TLS1_3)).await;
}
