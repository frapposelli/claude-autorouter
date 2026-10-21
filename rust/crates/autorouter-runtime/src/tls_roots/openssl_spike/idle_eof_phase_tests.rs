//! Diagnostic phase controls using actual TCP/TLS connections, not parity proof.
//! The deadline here owns the fixture hold, not a Gateway RequestTerminal.
use super::super::idle_close::Hold;
use super::*;

async fn observed_eof(read_hold_first: bool) -> (Fixture, Hold) {
    observed_eof_on(None, read_hold_first).await
}

async fn observed_eof_on(version: Option<SslVersion>, read_hold_first: bool) -> (Fixture, Hold) {
    let mut fixture = Fixture::new(version).await;
    let probe = fixture.probe.clone();
    let mut pending = JoinSet::new();
    request(&mut pending, &fixture.client, &fixture.origin, 0);
    let first = tokio::time::timeout(BARRIER, fixture.requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.connection, 1, "actual accepted peer identity");
    first.reply.send(()).unwrap();
    complete(&mut pending, &probe, 1).await;
    assert!(pending.is_empty());
    assert_eq!(probe.assignment(1).unwrap().0, 1);
    let eof = probe.hold_eof_delivery(1);
    let read = read_hold_first.then(|| probe.hold_read(1));
    close(&fixture.closes, 1);
    wait(&probe, Event::PeerClosed(1)).await;
    if let Some(read) = read {
        wait(&probe, Event::ReadHeld(1)).await;
        assert!(
            !probe.rows().contains(&Event::UnderlyingReadEofObserved(1)),
            "old pre-read hold is not a real EOF observation"
        );
        assert!(!probe.rows().contains(&Event::ReadEof(1)));
        drop(read);
    }
    wait(&probe, Event::UnderlyingReadEofObserved(1)).await;
    let rows = probe.rows();
    assert_eq!(
        rows.iter()
            .filter(|row| **row == Event::UnderlyingReadEofObserved(1))
            .count(),
        1
    );
    assert!(!rows.contains(&Event::ReadEofDelivered(1)));
    assert!(!rows.contains(&Event::ReadEof(1)));
    assert!(!rows.contains(&Event::Released(1)));
    assert_eq!(probe.live(), 1);
    assert_eq!(probe.assignment_history(), vec![[1, 1]]);
    assert_eq!(fixture.client.raw_counts.attempts.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.client.raw_counts.active.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture
            .client
            .raw_counts
            .shutdown_calls
            .load(Ordering::SeqCst),
        0
    );
    assert!(matches!(
        fixture.requests.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    (fixture, eof)
}

async fn delivered_without_request(fixture: Fixture) {
    let probe = fixture.probe.clone();
    wait(&probe, Event::ReadEofDelivered(1)).await;
    wait(&probe, Event::Released(1)).await;
    let rows = probe.rows();
    let observed = rows
        .iter()
        .position(|row| *row == Event::UnderlyingReadEofObserved(1))
        .unwrap();
    let delivered = rows
        .iter()
        .position(|row| *row == Event::ReadEofDelivered(1))
        .unwrap();
    let eof = rows
        .iter()
        .position(|row| *row == Event::ReadEof(1))
        .unwrap();
    let released = rows
        .iter()
        .position(|row| *row == Event::Released(1))
        .unwrap();
    assert!(observed < delivered && delivered < eof && eof < released);
    assert_eq!(
        rows.iter()
            .filter(|row| **row == Event::UnderlyingReadEofObserved(1))
            .count(),
        1
    );
    assert_eq!(
        rows.iter()
            .filter(|row| **row == Event::ReadEofDelivered(1))
            .count(),
        1
    );
    assert_eq!(probe.live(), 0);
    assert_eq!(
        fixture.client.raw_counts.attempts.load(Ordering::SeqCst),
        1,
        "quiescent retirement cannot require a following request"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn actual_tcp_eof_hold_releases_without_a_following_request() {
    let (fixture, hold) = observed_eof(false).await;
    drop(hold); // RAII token drop publishes the single real EOF.
    delivered_without_request(fixture).await;
}

#[tokio::test]
async fn preexisting_read_hold_cannot_satisfy_actual_eof_observation() {
    let (fixture, hold) = observed_eof(true).await;
    drop(hold);
    delivered_without_request(fixture).await;
}

#[tokio::test]
async fn owned_fixture_deadline_drops_the_eof_hold_and_finishes_cleanup() {
    let (fixture, hold) = observed_eof(false).await;
    let expired = tokio::time::timeout(Duration::ZERO, async move {
        let _hold = hold;
        std::future::pending::<()>().await;
    })
    .await;
    assert!(expired.is_err());
    delivered_without_request(fixture).await;
}

#[tokio::test]
async fn client_shutdown_can_drop_io_with_an_eof_token_still_alive() {
    let (fixture, hold) = observed_eof(false).await;
    let probe = fixture.probe.clone();
    // The token has no direct connection/reservation owner, but its waker may
    // retain a pending task. Client shutdown must still win without disarming it.
    fixture.finish().await;
    assert_eq!(probe.live(), 0);
    assert!(probe.rows().contains(&Event::Released(1)));
    assert!(!probe.rows().contains(&Event::ReadEofDelivered(1)));
    drop(hold);
}

async fn tls_eof_controls(version: SslVersion) {
    for read_hold_first in [false, true] {
        let (fixture, hold) = observed_eof_on(Some(version), read_hold_first).await;
        drop(hold);
        delivered_without_request(fixture).await;
    }
    let (fixture, hold) = observed_eof_on(Some(version), false).await;
    let expired = tokio::time::timeout(Duration::ZERO, async move {
        let _hold = hold;
        std::future::pending::<()>().await;
    })
    .await;
    assert!(expired.is_err());
    delivered_without_request(fixture).await;

    let (fixture, hold) = observed_eof_on(Some(version), false).await;
    let probe = fixture.probe.clone();
    fixture.finish().await;
    assert_eq!(probe.live(), 0);
    assert!(probe.rows().contains(&Event::Released(1)));
    assert!(!probe.rows().contains(&Event::ReadEofDelivered(1)));
    drop(hold);
}

#[tokio::test]
async fn tls12_observed_eof_release_and_shutdown_controls() {
    tls_eof_controls(SslVersion::TLS1_2).await;
}

#[tokio::test]
async fn tls13_observed_eof_release_and_shutdown_controls() {
    tls_eof_controls(SslVersion::TLS1_3).await;
}

async fn selected_after_observed_eof(version: Option<SslVersion>) {
    let (mut fixture, hold) = observed_eof_on(version, false).await;
    let cached = fixture
        .client
        .raw_sessions
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .snapshot();
    assert_eq!(cached.0, if version.is_some() { 1 } else { 0 });
    let client = fixture.client.clone();
    let probe = fixture.probe.clone();
    let origin = fixture.origin.clone();
    let mut receives = std::mem::replace(&mut fixture.requests, mpsc::channel(1).1);
    let run = tokio::spawn(async move {
        tokio::time::timeout(SCENARIO, async {
            let write = probe.hold_write(1);
            let mut requests = JoinSet::new();
            request(&mut requests, &client, &origin, 1);
            wait(&probe, Event::WriteHeld(1)).await;
            let (connection, selected) = probe.assignment(2).unwrap();
            assert_eq!(connection, 1, "must select the exact EOF-observed IO");
            assert_eq!(probe.assignment_history(), vec![[1, 1], [2, 1]]);
            assert!(!probe.rows().contains(&Event::ReadEofDelivered(1)));
            assert_eq!(client.raw_counts.attempts.load(Ordering::SeqCst), 1);

            // Writes remain held: only delivery of the already observed EOF can
            // finish this request. No new physical attempt or fabricated error.
            drop(hold);
            wait(&probe, Event::ReadEofDelivered(1)).await;
            let result = requests.join_next().await.unwrap().unwrap();
            assert!(matches!(result, Err(HttpError::Network)));
            assert!(selected.claim_abort().is_none());
            assert_eq!(client.raw_counts.attempts.load(Ordering::SeqCst), 1);
            assert!(matches!(
                receives.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            // The real request error must arrive while writes remain held.
            // Release that fixture hold before requiring driver retirement.
            drop(write);
            wait(&probe, Event::Released(1)).await;
            assert_eq!(client.raw_counts.attempts.load(Ordering::SeqCst), 1);
            assert_eq!(
                client
                    .raw_sessions
                    .as_ref()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .snapshot(),
                cached,
                "cache accounting survives the failed request and IO retirement before any new dial"
            );

            request(&mut requests, &client, &origin, 2);
            let third = receives.recv().await.unwrap();
            assert_eq!(third.connection, 2, "recovery requires one fresh IO");
            third.reply.send(()).unwrap();
            complete(&mut requests, &probe, 3).await;
            assert_eq!(probe.assignment_history(), vec![[1, 1], [2, 1], [3, 2]]);
            assert_eq!(client.raw_counts.attempts.load(Ordering::SeqCst), 2);
            assert!(requests.is_empty());
        })
        .await
        .expect("EOF-observed selected request deadline");
    })
    .await;
    fixture.finish().await;
    run.unwrap();
}

#[tokio::test]
async fn request_selected_after_actual_eof_fails_without_retry_and_recovers() {
    for version in [None, Some(SslVersion::TLS1_2), Some(SslVersion::TLS1_3)] {
        selected_after_observed_eof(version).await;
    }
}
