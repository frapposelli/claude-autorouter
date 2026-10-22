//! Causal EOF/consumer ordering on the buffered Gateway, not parity qualification.
//! Test-only holds intervene at existing boundaries; no production delay/retry.
use super::super::super::super::{abort, lifecycle_tests::isolated_tls, loopback_peer};
use super::*;
use http_body_util::BodyExt;
use openssl::ssl::SslVersion;
use tokio::io::AsyncRead;
use tokio::net::TcpListener;

const TOKEN: &str = "synthetic-buffered-eof-token";
const ENTITY: &[u8] = b"x";
const RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-length: 1\r\nconnection: keep-alive\r\n\r\nx";
const CLOSE_RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-length: 1\r\nconnection: close\r\n\r\nx";
const PHASE: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug)]
enum OrderingCase {
    ConsumerFirst,
    EofFirst,
    CancelHeld,
}

async fn phase<T>(label: &str, work: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(PHASE, work)
        .await
        .unwrap_or_else(|_| panic!("buffered EOF phase deadline: {label}"))
}
async fn until(label: &str, check: impl Fn() -> bool) {
    phase(label, async {
        while !check() {
            tokio::task::yield_now().await;
        }
    })
    .await;
}
fn stages(
    client: &BufferedRawPoolClient,
    event: &str,
    connection: usize,
) -> Vec<serde_json::Value> {
    client
        .inner
        .probe
        .snapshot()
        .0
        .into_iter()
        .filter(|row| row["event"] == event && row["connection"] == connection)
        .collect()
}
fn free(client: &BufferedRawPoolClient, connection: usize) -> bool {
    stages(client, "free", connection)
        .iter()
        .any(|row| row["kept"] == true)
}
fn require_no_free(client: &BufferedRawPoolClient) {
    assert!(!free(client, 1), "A has not entered the free pool");
}
fn require_pending_eof(idle: &idle_close::Probe) {
    use idle_close::Event;
    let rows = idle.rows();
    assert_eq!(
        rows.iter()
            .filter(|row| **row == Event::UnderlyingReadEofObserved(1))
            .count(),
        1
    );
    assert!(!rows.contains(&Event::ReadEofDelivered(1)));
    assert!(!rows.contains(&Event::ReadEof(1)));
    assert!(!rows.contains(&Event::Released(1)));
}
async fn submit(address: std::net::SocketAddr, ordinal: usize) -> TcpStream {
    let mut io = TcpStream::connect(address).await.unwrap();
    io.write_all(format!("GET /v1/models/session-{ordinal} HTTP/1.1\r\nHost: synthetic\r\nX-Api-Key: {TOKEN}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    io
}
async fn response(mut io: TcpStream, expected_status: u16) -> Vec<u8> {
    let head = loopback_peer::read_head(&mut io)
        .await
        .unwrap()
        .expect("actual response head");
    let text = std::str::from_utf8(&head).unwrap();
    let mut lines = text.strip_suffix("\r\n\r\n").unwrap().split("\r\n");
    let status: u16 = lines
        .next()
        .unwrap()
        .split_ascii_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(status, expected_status);
    let mut headers = hyper::HeaderMap::new();
    for line in lines {
        let (name, value) = line.split_once(':').expect("downstream header separator");
        let name = hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap();
        assert!(!headers.contains_key(&name), "duplicate downstream header");
        headers.insert(
            name,
            hyper::header::HeaderValue::from_str(value.trim()).unwrap(),
        );
    }
    // Gateway owns downstream framing independently of provider Content-Length.
    let mut wire = Vec::new();
    let mut bytes = [0; 1024];
    loop {
        let count = io.read(&mut bytes).await.unwrap();
        if count == 0 {
            break;
        }
        assert!(wire.len() + count <= 16_384, "downstream wire bound");
        wire.extend_from_slice(&bytes[..count]);
    }
    if let Some(length) = headers.get("content-length") {
        assert!(!headers.contains_key("transfer-encoding"));
        assert_eq!(
            wire.len(),
            length.to_str().unwrap().parse::<usize>().unwrap()
        );
        return wire;
    }
    assert_eq!(headers.get("transfer-encoding").unwrap(), "chunked");
    let mut tail = wire.as_slice();
    let mut entity = Vec::new();
    for _ in 0..32 {
        let end = tail
            .windows(2)
            .position(|value| value == b"\r\n")
            .expect("complete chunk size");
        assert!(end > 0 && end <= 16, "finite native chunk size");
        let size = usize::from_str_radix(std::str::from_utf8(&tail[..end]).unwrap(), 16).unwrap();
        tail = &tail[end + 2..];
        if size == 0 {
            assert_eq!(
                tail, b"\r\n",
                "complete final chunk and no unowned trailers/bytes"
            );
            return entity;
        }
        assert!(size <= 16_384 && entity.len() + size <= 16_384);
        assert!(tail.len() >= size + 2, "complete chunk payload");
        assert_eq!(&tail[size..size + 2], b"\r\n");
        entity.extend_from_slice(&tail[..size]);
        tail = &tail[size + 2..];
    }
    panic!("downstream chunk count bound");
}
async fn expected_head(peer: &mut (impl AsyncRead + Unpin), ordinal: usize) {
    let head = loopback_peer::read_head(peer)
        .await
        .unwrap()
        .expect("actual provider request");
    assert!(head.starts_with(format!("GET /v1/models/session-{ordinal} HTTP/1.1\r\n").as_bytes()));
}
fn tls_state(peer: &loopback_peer::Peer, version: SslVersion) -> bool {
    match peer {
        loopback_peer::Peer::Tls(tls) => {
            assert_eq!(tls.inner().ssl().version2(), Some(version));
            tls.inner().ssl().session_reused()
        }
        loopback_peer::Peer::Tcp(_) => panic!("actual negotiated TLS required"),
    }
}
fn terminal_id(terminal: &gateway_terminal::Probe) -> gateway_terminal::Identity {
    terminal
        .events()
        .into_iter()
        .find_map(|event| match event {
            gateway_terminal::Event::Registered(id) => Some(id),
            _ => None,
        })
        .expect("actual Gateway request A terminal")
}
fn require_one_success(terminal: &gateway_terminal::Probe, id: gateway_terminal::Identity) {
    use gateway_terminal::Event;
    let events = terminal.events();
    assert_eq!(events.iter().filter(|event| matches!(event,
        Event::DeliveryClaimed(found, crate::transport_completion::Delivery::Flushed) if *found == id)).count(), 1);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::CallbackFinished(found) if *found == id))
            .count(),
        1
    );
    assert!(!events.iter().any(|event| matches!(event,
        Event::Failed(found, _) | Event::CallbackAbandoned(found) if *found == id)));
}
async fn independent_origin(
    client: &BufferedRawPoolClient,
    tls: &openssl::ssl::SslContext,
    version: SslVersion,
) {
    let (listener, origin) = loopback_peer::bind_listener(true).await;
    let request = Request::get(format!("{origin}/v1/models/session-9"))
        .body(Full::new(Bytes::new()))
        .unwrap();
    let consume = async {
        let mut response = client.request_raw(request).await.unwrap();
        assert_eq!(response.status(), hyper::StatusCode::OK);
        until("independent source completion", || {
            let state = client.probe.snapshot();
            state.clean_sources == 2 && state.producer_tasks == 0
        })
        .await;
        let frame = response
            .body_mut()
            .frame()
            .await
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap();
        assert_eq!(frame, ENTITY);
        assert!(
            response.body().is_end_stream(),
            "independent final data releases its own reservation"
        );
    };
    let serve = async {
        let mut peer = loopback_peer::accept_peer(&listener, Some(tls)).await;
        assert!(
            !tls_state(&peer, version),
            "second origin performs its own full handshake"
        );
        expected_head(&mut peer, 9).await;
        peer.write_all(CLOSE_RESPONSE).await.unwrap();
        peer.flush().await.unwrap();
        drop(peer);
    };
    tokio::join!(consume, serve);
}
async fn fresh_request(
    address: std::net::SocketAddr,
    ordinal: usize,
    listener: &TcpListener,
    tls: &openssl::ssl::SslContext,
    version: SslVersion,
) {
    let downstream = submit(address, ordinal).await;
    let mut peer = loopback_peer::accept_peer(listener, Some(tls)).await;
    assert!(
        tls_state(&peer, version),
        "ordinary EOF retains a real usable ticket for this same origin"
    );
    expected_head(&mut peer, ordinal).await;
    peer.write_all(CLOSE_RESPONSE).await.unwrap();
    peer.flush().await.unwrap();
    drop(peer);
    assert_eq!(response(downstream, 200).await, ENTITY);
}
async fn exercise(
    case: OrderingCase,
    version: SslVersion,
    client: Arc<BufferedRawPoolClient>,
    terminal: gateway_terminal::Probe,
    idle: idle_close::Probe,
    address: std::net::SocketAddr,
    provider: (TcpListener, openssl::ssl::SslContext),
) -> Option<idle_close::Hold> {
    use gateway_terminal::Event as Terminal;
    use idle_close::Event;
    let (listener, tls) = provider;
    let downstream = submit(address, 0).await;
    let mut peer = loopback_peer::accept_peer(&listener, Some(&tls)).await;
    assert!(!tls_state(&peer, version));
    expected_head(&mut peer, 0).await;
    let eof = idle.hold_eof_delivery(1);
    peer.write_all(RESPONSE).await.unwrap();
    peer.flush().await.unwrap();
    // Actual peer Drop is merely the stimulus; the next barrier is the real IO EOF.
    drop(peer);
    phase(
        "A underlying EOF",
        idle.wait(Event::UnderlyingReadEofObserved(1)),
    )
    .await;
    until("A clean producer and attached consumer", || {
        let buffer = client.probe.snapshot();
        buffer.clean_sources == 1
            && buffer.producer_tasks == 0
            && terminal
                .events()
                .iter()
                .any(|event| matches!(event, Terminal::Attached(_)))
    })
    .await;
    let a = terminal_id(&terminal);
    let buffer = client.probe.snapshot();
    assert_eq!(buffer.consumer_polls, 0);
    assert!(buffer.queued_bytes > 0);
    assert_eq!(buffer.failed_sources, 0);
    assert_eq!(buffer.cancelled_sources, 0);
    assert!(stages(&client, "consumer_complete", 1).is_empty());
    assert!(stages(&client, "driver_finished", 1).is_empty());
    require_pending_eof(&idle);
    require_no_free(&client);
    assert_eq!(idle.assignment_history(), vec![[1, 1]]);
    let cached = client.inner.raw_sessions.lock().unwrap().snapshot();
    assert_eq!(cached.0, 1);
    assert!(cached.1 > 0, "positive actual accepted ticket bytes");
    assert_eq!(client.inner.raw_counts.attempts.load(Ordering::SeqCst), 1);
    assert!(!terminal.events().iter().any(|event| matches!(event,
        Terminal::DeliveryClaimed(found, _) | Terminal::Failed(found, _) if *found == a)));

    // Same client, distinct owned origin, direct real transport request: no second
    // Gateway, fake reservation or completion task that can mask shared progress.
    phase(
        "independent origin progress",
        independent_origin(&client, &tls, version),
    )
    .await;
    require_pending_eof(&idle);
    require_no_free(&client);
    assert_eq!(
        client.probe.snapshot().consumer_polls,
        1,
        "only independent origin consumed its single data frame"
    );
    assert_eq!(client.inner.raw_counts.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(idle.assignment_history(), vec![[1, 1], [2, 2]]);
    let cached = client.inner.raw_sessions.lock().unwrap().snapshot();
    assert_eq!(
        cached.0, 2,
        "independent origin owns a distinct accepted ticket"
    );
    if matches!(case, OrderingCase::CancelHeld) {
        downstream.set_zero_linger().unwrap();
        drop(downstream);
        until("A downstream cancellation", || {
            terminal.events().iter().any(|event| {
                matches!(event,
            Terminal::Failed(found, gateway_terminal::FailureCause::Downstream(_)) if *found == a)
            })
        })
        .await;
        // Downstream failure may already have dropped the held body/gate.
        client.inner.probe.release_all();
        // The held first frame has not reached the completion registry. Its
        // cancelled owner retires directly, without creating a delivery claim.
        until("A cancelled ownership retirement", || {
            terminal.events().iter().any(|event| {
                matches!(event,
            Terminal::Retired(found) if *found == a)
            }) && terminal.events().contains(&Terminal::Closed(a.connection))
                && stages(&client, "close", 1).len() == 1
                && stages(&client, "consumer_complete", 1).len() == 1
                && terminal
                    .transport_events()
                    .iter()
                    .any(|row| row.identity == a)
        })
        .await;
        let events = terminal.events();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Terminal::Failed(found, _) if *found == a))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Terminal::Retired(found) if *found == a))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == Terminal::Closed(a.connection))
                .count(),
            1
        );
        assert!(!events.iter().any(|event| matches!(event,
            Terminal::DeliveryClaimed(found, _)
                | Terminal::CallbackFinished(found)
                | Terminal::CallbackAbandoned(found) if *found == a)));
        let consumer = stages(&client, "consumer_complete", 1);
        assert_eq!(consumer[0]["clean"], false);
        assert_eq!(consumer[0]["unpoisoned"], false);
        let actions: Vec<_> = terminal
            .transport_events()
            .into_iter()
            .filter(|row| row.identity == a)
            .collect();
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            actions[0].cause,
            gateway_terminal::FailureCause::Downstream(_)
        ));
        let failed_cause = events
            .iter()
            .find_map(|event| match event {
                Terminal::Failed(found, cause) if *found == a => Some(*cause),
                _ => None,
            })
            .unwrap();
        assert_eq!(actions[0].cause, failed_cause);
        assert!(matches!(
            actions[0].outcome,
            gateway_terminal::TransportOutcome::Applied(abort::Outcome::AlreadyClosed(
                abort::Terminal::Ordinary
            ))
        ));
        require_no_free(&client);
        assert_eq!(
            client
                .inner
                .raw_counts
                .shutdown_calls
                .load(Ordering::SeqCst),
            0
        );
        assert_eq!(
            client
                .inner
                .raw_counts
                .session_error_closes
                .load(Ordering::SeqCst),
            0
        );
        assert_eq!(client.inner.raw_sessions.lock().unwrap().snapshot(), cached);
        assert_eq!(client.inner.raw_counts.attempts.load(Ordering::SeqCst), 2);
        return Some(eof); // Owner cleanup below must work with this observer still alive.
    }
    if matches!(case, OrderingCase::EofFirst) {
        drop(eof);
        phase(
            "A EOF delivered first",
            idle.wait(Event::ReadEofDelivered(1)),
        )
        .await;
        until("A driver finished before consumer", || {
            !stages(&client, "driver_finished", 1).is_empty()
        })
        .await;
        phase(
            "A physical release before consumer",
            idle.wait(Event::Released(1)),
        )
        .await;
        assert!(stages(&client, "consumer_complete", 1).is_empty());
        require_no_free(&client);
        client.inner.probe.release(0);
        assert_eq!(response(downstream, 200).await, ENTITY);
        until("A consumer completion after EOF", || {
            !stages(&client, "consumer_complete", 1).is_empty()
        })
        .await;
        let rows = stages(&client, "consumer_complete", 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["clean"], true);
        assert_eq!(rows[0]["sender_closed"], true);
        require_no_free(&client);
    } else {
        client.inner.probe.release(0);
        assert_eq!(response(downstream, 200).await, ENTITY);
        until("A free admission before EOF", || free(&client, 1)).await;
        let rows = stages(&client, "consumer_complete", 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["clean"], true);
        assert_eq!(rows[0]["unpoisoned"], true);
        assert_eq!(rows[0]["sender_closed"], false);
        require_pending_eof(&idle);
        let write = idle.hold_write(1);
        let pending = submit(address, 1).await;
        phase("B actual write held", idle.wait(Event::WriteHeld(1))).await;
        let (connection, selected) = idle
            .assignment(3)
            .expect("actual selected stale reservation");
        assert_eq!(connection, 1);
        assert_eq!(client.inner.raw_counts.attempts.load(Ordering::SeqCst), 2);
        assert!(stages(&client, "driver_finished", 1).is_empty());
        drop(eof);
        phase(
            "B selected before A EOF delivery",
            idle.wait(Event::ReadEofDelivered(1)),
        )
        .await;
        // Delivered EOF disables HTTP progress but does not promise request
        // completion while Hyper's real serialized-write flush remains held.
        drop(write);
        let _ = response(pending, 502).await; // Gateway's actual transport failure; no injected response.
        assert!(
            selected.claim_abort().is_none(),
            "retired selection gains no new abort authority"
        );
        assert_eq!(
            client.inner.raw_counts.attempts.load(Ordering::SeqCst),
            2,
            "no replay/fallback attempt"
        );
        assert_eq!(client.inner.raw_sessions.lock().unwrap().snapshot(), cached);
        // Failed request disposal can abort connection.await. Its RAII lease
        // still retires; the normal-return-only driver marker is inapplicable.
        until("B failed driver retirement", || {
            !stages(&client, "close", 1).is_empty()
        })
        .await;
        phase("B failed physical release", idle.wait(Event::Released(1))).await;
        assert_eq!(stages(&client, "close", 1).len(), 1);
    }
    until("A successful callback completion", || {
        terminal.events().iter().any(|event| {
            matches!(event,
        Terminal::CallbackFinished(found) if *found == a)
        })
    })
    .await;
    require_one_success(&terminal, a);
    assert_eq!(client.inner.raw_sessions.lock().unwrap().snapshot(), cached);
    fresh_request(address, 2, &listener, &tls, version).await;
    assert_eq!(
        client.inner.raw_counts.attempts.load(Ordering::SeqCst),
        3,
        "one independent origin and exactly one fresh same-origin recovery"
    );
    assert_eq!(idle.assignment_history().last().unwrap()[1], 3);
    let accepted =
        std::future::poll_fn(|cx| std::task::Poll::Ready(listener.poll_accept(cx))).await;
    assert!(accepted.is_pending(), "no unseen provider request/retry");
    None
}
async fn actual(case: OrderingCase, version: SslVersion) {
    let (trust, tls) = isolated_tls(Some(version));
    let tls = tls.unwrap();
    let (listener, origin) = phase("bind provider", loopback_peer::bind_listener(true)).await;
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
    let idle = idle_close::Probe::default();
    let client = Arc::new(BufferedRawPoolClient::with_idle(&trust, idle.clone()).unwrap());
    client.inner.probe.hold(0);
    let fixture = phase("start Gateway", Fixture::with_client(config, client)).await;
    let mut task = tokio::spawn(exercise(
        case,
        version,
        fixture.client.clone(),
        fixture.terminal.clone(),
        idle.clone(),
        fixture.running.address,
        (listener, tls),
    ));
    let result = tokio::time::timeout(Duration::from_secs(8), &mut task).await;
    if result.is_err() {
        task.abort();
        let _ = phase("join aborted assertion task", &mut task).await;
    }
    if !matches!(&result, Ok(Ok(_))) {
        eprintln!(
            "buffered EOF failed {case:?} {version:?}: idle={:?}; pool={:?}; buffers={:?}; terminal={:?}; transport={:?}",
            idle.rows(),
            fixture.client.inner.probe.snapshot(),
            fixture.client.probe.snapshot(),
            fixture.terminal.events(),
            fixture.terminal.transport_events()
        );
    }
    fixture.client.inner.probe.release_all();
    let cleanup = fixture.finish().await;
    assert_eq!(cleanup["cleanup"], true);
    assert_eq!(idle.live(), 0);
    let held = result
        .expect("8 second buffered EOF owner")
        .expect("buffered EOF assertion task");
    if held.is_some() {
        assert!(
            !idle
                .rows()
                .contains(&idle_close::Event::ReadEofDelivered(1)),
            "cleanup does not require publishing the held EOF or a following request"
        );
    }
    drop(held);
}
#[tokio::test]
async fn buffered_eof_held_then_consumer_release_uses_actual_stale_assignment() {
    for version in [SslVersion::TLS1_2, SslVersion::TLS1_3] {
        actual(OrderingCase::ConsumerFirst, version).await;
    }
}
#[tokio::test]
async fn buffered_eof_delivered_before_consumer_release_retires_without_free() {
    for version in [SslVersion::TLS1_2, SslVersion::TLS1_3] {
        actual(OrderingCase::EofFirst, version).await;
    }
}
#[tokio::test]
async fn buffered_observed_eof_cancellation_cleans_with_live_observer() {
    for version in [SslVersion::TLS1_2, SslVersion::TLS1_3] {
        actual(OrderingCase::CancelHeld, version).await;
    }
}
