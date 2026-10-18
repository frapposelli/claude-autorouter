//! One Gateway and buffered transport own eight real downstream TCP clients.
//! Pending writes are injected by the bounded developer probe, not OS saturation.
//! This distinct default-Queue experiment exhausts twelve retained block owners;
//! it does not claim the historical queued>=64KiB experiment passed.
use super::*;
use crate::tls_roots::openssl_spike::gateway_terminal::{Event, FailureCause, Identity};
use sha2::{Digest, Sha256};

const LANES: usize = 8;
const TOTAL_REQUESTS: usize = 9;
const ENTITY_LIMIT: usize = 262144;
type EightRouter = PipelineRouter<LANES>;
type Clients = tokio::task::JoinSet<(usize, bool)>;

fn names() -> [[String; 4]; LANES] {
    std::array::from_fn(|i| {
        [
            format!("task-{i}"),
            format!("session-{i}"),
            format!("old-{i}"),
            format!("new-{i}"),
        ]
    })
}
fn entity(i: usize) -> Vec<u8> {
    let mut bytes = b": padding\n\n".repeat(23040);
    for event in [
        json!({
            "type":"message_start",
            "message":{"model":"claude-sonnet-5","usage":{"input_tokens":1,"output_tokens":0}}
        }),
        json!({
            "type":"content_block_start",
            "index":0,
            "content_block":{"type":"tool_use","id":names()[i][3],"name":"Read","input":{}}
        }),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}),
        json!({"type":"message_stop"}),
    ] {
        bytes.extend_from_slice(format!("data: {event}\n\n").as_bytes());
    }
    // Exceed all twelve owned blocks without changing the bounded reader.
    assert!(bytes.len() > 12 * 16384);
    assert!(bytes.len() <= ENTITY_LIMIT);
    bytes
}
async fn downstream(address: std::net::SocketAddr, i: usize) -> (usize, bool) {
    let stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
            .await
            .unwrap();
    let names = names();
    let body = json!({
        "model":"claude-opus-5-5",
        "max_tokens":100,
        "messages":[{"role":"user","content":names[i][0]}]
    })
    .to_string();
    let request = Request::post("/v1/messages")
        .header("host", "synthetic")
        .header("x-api-key", LOCAL_TOKEN)
        .header("x-claude-code-session-id", &names[i][1])
        .header("x-claude-code-prompt-id", &names[i][0])
        .header("content-type", "application/json")
        .header("connection", "close")
        .body(Full::new(Bytes::from(body)))
        .unwrap();
    let read = async {
        let response = sender.send_request(request).await?;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        assert_eq!(response.headers()["x-pipeline-marker"], names[i][0]);
        let mut incoming = response.into_body();
        let mut bytes = Vec::new();
        while let Some(frame) = incoming.frame().await {
            let frame = frame?;
            assert!(frame.trailers_ref().is_none());
            if let Ok(data) = frame.into_data() {
                assert!(data.len() <= ENTITY_LIMIT - bytes.len());
                bytes.extend_from_slice(&data);
            }
        }
        assert_eq!(bytes, entity(i));
        Ok::<_, hyper::Error>(())
    };
    tokio::pin!(read, connection);
    let completed = tokio::select! {
        biased;
        value = &mut read => value.is_ok(),
        _ = &mut connection => read.await.is_ok(),
    };
    (i, completed)
}
fn owners(probe: &TerminalProbe) -> Vec<Identity> {
    probe
        .events()
        .iter()
        .filter_map(|event| match event {
            Event::Registered(id) => Some(*id),
            _ => None,
        })
        .collect()
}
fn full_block_owners(state: &Value, count: usize) -> bool {
    let pools = state["pools"].as_array().unwrap();
    pools.len() == count
        && pools.iter().all(|row| {
            row["allocated"] == 12
                && row["outstanding"] == 12
                && row["queued"] == 0
                && row["producer"] == 1
        })
        && state["buffers"]["live_pools"] == count
}
fn pool(state: &Value, id: Identity) -> &Value {
    state["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["connection"] == id.connection && row["request"] == id.request)
        .unwrap()
}
fn assert_active_tokens(router: &EightRouter, cancelled: &[usize]) {
    let state = router.0.lock().unwrap();
    for (i, token) in state.cancellation.iter().enumerate() {
        assert_eq!(
            token.as_ref().unwrap().is_cancelled(),
            cancelled.contains(&i)
        );
    }
}
fn assert_held_owners(
    held: &Value,
    ids: &[Identity; LANES],
    router: &EightRouter,
    probe: &TerminalProbe,
) {
    for field in ["clean_sources", "failed_sources", "cancelled_sources"] {
        assert_eq!(held["buffers"][field], 0);
    }
    let connections = ids
        .iter()
        .map(|id| id.connection)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(connections.len(), LANES);
    assert_eq!(held["raw_attempts"], LANES);
    assert!(held["raw_live"].as_u64().unwrap() <= LANES as u64);
    assert_eq!(held["fetch_attempts"], 0);
    assert_eq!(held["buffers"]["producer_tasks"], LANES);
    for id in ids {
        let row = pool(held, *id);
        assert_eq!(row["producer"], 1);
        assert_eq!(row["allocated"], 12);
        assert_eq!(row["outstanding"], 12);
        assert_eq!(row["queued"], 0);
        for field in ["allocated", "outstanding", "peak_blocks"] {
            assert!(row[field].as_u64().unwrap() <= 12);
        }
        assert!(row["peak_queued"].as_u64().unwrap() < 81920);
        assert_eq!(row["panics"], json!([0, 0]));
    }
    router.check([0; LANES], LANES);
    assert_active_tokens(router, &[]);
    assert!(
        !probe
            .events()
            .iter()
            .any(|event| { matches!(event, Event::Failed(..) | Event::DeliveryClaimed(..)) })
    );
}
async fn start_and_fill_block_owners(
    address: std::net::SocketAddr,
    clients: &mut Clients,
    snapshot: &dyn Fn() -> Value,
    probe: &TerminalProbe,
    received: &mut tokio::sync::mpsc::Receiver<usize>,
) -> ([Identity; LANES], Value) {
    clients.spawn(downstream(address, 0));
    until(|| full_block_owners(&snapshot(), 1)).await;
    {
        let _clock = PausedClock::new();
        tokio::time::advance(Duration::from_secs(5)).await;
    }
    // Sequential registration fixes lane→physical owner attribution while the
    // first client's producer stays saturated. All eight remain concurrent.
    for i in 1..LANES {
        clients.spawn(downstream(address, i));
        until(|| owners(probe).len() == i + 1).await;
    }
    until(|| full_block_owners(&snapshot(), LANES) && probe.writer_snapshot().waiters == LANES)
        .await;
    let mut arrivals = Vec::new();
    for _ in 0..LANES {
        arrivals.push(received.recv().await.unwrap());
    }
    arrivals.sort_unstable();
    assert_eq!(arrivals, (0..LANES).collect::<Vec<_>>());
    let ids: [Identity; LANES] = owners(probe).try_into().unwrap();
    (ids, snapshot())
}
async fn cancel_second_lane(router: &EightRouter) -> [usize; LANES] {
    router.0.lock().unwrap().cancellation[1]
        .as_ref()
        .unwrap()
        .cancel();
    until(|| router.0.lock().unwrap().released[1] == 1).await;
    let mut released = [0; LANES];
    released[1] = 1;
    router.check_with_releases([0; LANES], released, 7);
    assert_active_tokens(router, &[1]);
    released
}
async fn expire_first_lane(
    router: &EightRouter,
    snapshot: &dyn Fn() -> Value,
    probe: &TerminalProbe,
    ids: &[Identity; LANES],
    released: &mut [usize; LANES],
) -> Value {
    let observed = snapshot();
    let first = pool(&observed, ids[0])["deadline_ns"].as_u64().unwrap();
    for row in observed["pools"].as_array().unwrap() {
        if row["connection"] != ids[0].connection {
            assert!(row["deadline_ns"].as_u64().unwrap() >= first + 4_000_000_000);
        }
    }
    {
        let _clock = PausedClock::new();
        let advance = (first + 1_000_000).saturating_sub(observed["clock_ns"].as_u64().unwrap());
        tokio::time::advance(Duration::from_nanos(advance)).await;
    }
    until(|| router.0.lock().unwrap().released[0] == 1).await;
    released[0] = 1;
    router.check_with_releases([0; LANES], *released, 6);
    assert_active_tokens(router, &[0, 1]);
    until(|| full_block_owners(&snapshot(), 6) && probe.writer_snapshot().waiters == 6).await;
    observed
}
fn assert_isolated_failures(probe: &TerminalProbe, ids: &[Identity; LANES]) {
    let events = probe.events();
    for (i, expected) in [(0, FailureCause::Deadline), (1, FailureCause::Cancelled)] {
        let failures = events
            .iter()
            .filter_map(|event| match event {
                Event::Failed(id, cause) if *id == ids[i] => Some(*cause),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(failures, [expected]);
    }
    assert!(!events.iter().any(|event| {
        matches!(event, Event::Failed(id, _) | Event::DeliveryClaimed(id, _) if ids[2..].contains(id))
    }));
}
async fn finish_six_ordinary(
    clients: &mut Clients,
    router: &EightRouter,
    probe: &TerminalProbe,
    released: [usize; LANES],
) -> [usize; LANES] {
    probe.release_writes();
    for _ in 0..LANES {
        let (i, completed) = clients.join_next().await.unwrap().unwrap();
        assert_eq!(completed, i >= 2);
    }
    let mut completed = [1; LANES];
    completed[0] = 0;
    completed[1] = 0;
    router.check_with_releases(completed, released, 0);
    completed
}
async fn recover_first_lane(
    address: std::net::SocketAddr,
    clients: &mut Clients,
    received: &mut tokio::sync::mpsc::Receiver<usize>,
    router: &EightRouter,
    released: [usize; LANES],
    completed: &mut [usize; LANES],
) {
    clients.spawn(downstream(address, 0));
    assert_eq!(clients.join_next().await.unwrap().unwrap(), (0, true));
    assert_eq!(received.recv().await, Some(0));
    completed[0] = 1;
    until(|| router.0.lock().unwrap().completed[0] == 1).await;
    router.check_with_releases(*completed, released, 0);
}
fn assert_final_events(probe: &TerminalProbe, ids: &[Identity; LANES]) -> Vec<Event> {
    let events = probe.events();
    let registered = owners(probe);
    assert_eq!(registered.len(), TOTAL_REQUESTS);
    // The recovery is a ninth physical connection, not an alias of a held one.
    assert_eq!(
        registered
            .iter()
            .map(|id| id.connection)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        TOTAL_REQUESTS
    );
    assert_eq!(
        matching(&events, |event| matches!(
            event,
            Event::DeliveryClaimed(_, Delivery::Flushed)
        )),
        7
    );
    assert_eq!(
        matching(&events, |event| matches!(
            event,
            Event::DeliveryClaimed(_, Delivery::Failed(Failure::ConnectionClosed))
        )),
        2
    );
    for id in &ids[..2] {
        assert_eq!(
            matching(&events, |event| matches!(
                event,
                Event::DeliveryClaimed(owner, Delivery::Failed(Failure::ConnectionClosed)) if owner == id
            )),
            1
        );
    }
    assert_eq!(
        matching(&events, |event| matches!(event, Event::DeliveryClaimed(..))),
        TOTAL_REQUESTS
    );
    assert_eq!(
        matching(&events, |event| matches!(event, Event::CallbackFinished(_))),
        TOTAL_REQUESTS
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::CallbackAbandoned(_)))
    );
    events
}
fn assert_final_records(records: &Mutex<Vec<Value>>) -> Vec<Value> {
    let rows = records.lock().unwrap();
    assert_eq!(rows.len(), 18);
    for (status, count) in [("completed", 7), ("cancelled", 1), ("error", 1)] {
        assert_eq!(
            matching(&rows, |row| row["event"] == "outcome"
                && row["status"] == status),
            count
        );
    }
    assert_eq!(
        matching(&rows, |row| row["event"] == "outcome"
            && row["completion_confirmed"] == true),
        7
    );
    rows.clone()
}
// This observes actual provider writes and shutdown, not request arrival or
// the task owner's stop branch. All pre-action errors remain fatal.
async fn await_provider_exchanges(
    completed: &mut tokio::sync::mpsc::Receiver<usize>,
    expected: &[usize],
) {
    let mut observed = tokio::time::timeout(BARRIER, async {
        let mut observed = Vec::with_capacity(expected.len());
        for _ in expected {
            observed.push(
                completed
                    .recv()
                    .await
                    .expect("provider exchange success signal"),
            );
        }
        observed
    })
    .await
    .expect("provider full write and shutdown exceeded its bound");
    observed.sort_unstable();
    assert_eq!(observed, expected);
    // The sender may close after the ninth and final successful exchange.
    assert!(completed.try_recv().is_err());
}
struct LaneSchedule<'a> {
    address: std::net::SocketAddr,
    clients: &'a mut Clients,
    snapshot: &'a dyn Fn() -> Value,
    probe: &'a TerminalProbe,
    received: &'a mut tokio::sync::mpsc::Receiver<usize>,
    provider_completed: &'a mut tokio::sync::mpsc::Receiver<usize>,
    tail_gate: &'a CancellationToken,
    router: &'a EightRouter,
    records: &'a Mutex<Vec<Value>>,
}
impl LaneSchedule<'_> {
    async fn exercise(&mut self, version: Option<SslVersion>) {
        let (ids, held) = start_and_fill_block_owners(
            self.address,
            self.clients,
            self.snapshot,
            self.probe,
            self.received,
        )
        .await;
        // The withheld final byte keeps all eight real upstream sockets live.
        assert_eq!(held["raw_live"], LANES);
        assert_eq!(self.probe.writer_snapshot().waiters, LANES);
        assert_held_owners(&held, &ids, self.router, self.probe);
        self.tail_gate.cancel();
        await_provider_exchanges(self.provider_completed, &[0, 1, 2, 3, 4, 5, 6, 7]).await;
        // Completion at the provider must not drain or confirm the held clients.
        let held_after_provider_completion = (self.snapshot)();
        assert!(full_block_owners(&held_after_provider_completion, LANES));
        assert_eq!(self.probe.writer_snapshot().waiters, LANES);
        assert_held_owners(
            &held_after_provider_completion,
            &ids,
            self.router,
            self.probe,
        );
        let mut released = cancel_second_lane(self.router).await;
        let deadline_schedule =
            expire_first_lane(self.router, self.snapshot, self.probe, &ids, &mut released).await;
        assert_isolated_failures(self.probe, &ids);
        let after_failures = (self.snapshot)();
        let mut completed =
            finish_six_ordinary(self.clients, self.router, self.probe, released).await;
        recover_first_lane(
            self.address,
            self.clients,
            self.received,
            self.router,
            released,
            &mut completed,
        )
        .await;
        await_provider_exchanges(self.provider_completed, &[0]).await;
        until(|| {
            matching(&self.probe.events(), |event| {
                matches!(event, Event::CallbackFinished(_))
            }) == TOTAL_REQUESTS
        })
        .await;
        let events = assert_final_events(self.probe, &ids);
        let rows = assert_final_records(self.records);
        let final_state = (self.snapshot)();
        assert_eq!(final_state["raw_attempts"], TOTAL_REQUESTS);
        assert_eq!(final_state["fetch_attempts"], 0);
        assert_eq!(
            final_state["raw_tls_handshakes_completed"],
            if version.is_some() { TOTAL_REQUESTS } else { 0 }
        );
        eprintln!(
            "eight_gateway_full_block_owners={}",
            json!({
                "version":format!("{version:?}"),
                "held":held,
                "held_after_provider_completion":held_after_provider_completion,
                "provider_exchanges_completed_before_actions":[0,1,2,3,4,5,6,7],
                "provider_recovery_exchange_completed":0,
                "deadline_schedule":deadline_schedule,
                "after_failures":after_failures,
                "final":final_state,
                "events":format!("{events:?}"),
                "records":rows,
                "entity_bytes":(0..LANES).map(|i|entity(i).len()).collect::<Vec<_>>(),
                "entity_sha256":(0..LANES).map(|i|format!("{:x}",Sha256::digest(entity(i)))).collect::<Vec<_>>()
            })
        );
    }
}
async fn eight(version: Option<SslVersion>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (transport, snapshot, tls) =
        crate::tls_roots::openssl_spike::buffered_delivery_fixture(version).unwrap();
    let transport = Arc::new(transport);
    let config = buffered_config(address, version, Path::new("/synthetic"));
    let router = EightRouter::named(names(), TOTAL_REQUESTS);
    let probe = TerminalProbe::default();
    probe.hold_writes();
    let records = Arc::new(Mutex::new(Vec::new()));
    let saved = records.clone();
    let gateway = Gateway::with_test_terminal(
        Gateway::with_router(
            config,
            transport.clone(),
            router.clone(),
            EventSinks {
                record: Some(Arc::new(move |row| {
                    let mut rows = saved.lock().unwrap();
                    assert!(rows.len() < 24);
                    rows.push(row);
                })),
                ..EventSinks::default()
            },
        )
        .unwrap(),
        probe.clone(),
    );
    let running = gateway.listen(0).await.unwrap();
    let gateway_address = running.address;
    let stop = CancellationToken::new();
    let mut tasks = PipelineTasks {
        stop: stop.clone(),
        tasks: Vec::new(),
    };
    let live = Arc::new(AtomicUsize::new(0));
    let (ready, mut received) = tokio::sync::mpsc::channel(TOTAL_REQUESTS);
    let (completed, mut provider_completed) = tokio::sync::mpsc::channel(TOTAL_REQUESTS);
    let tail_gate = CancellationToken::new();
    tasks.tasks.push(tokio::spawn(pipeline_provider_with(
        listener,
        tls,
        version,
        PipelineSignals {
            ready,
            release_a: CancellationToken::new(),
            completed: Some(completed),
        },
        stop,
        live.clone(),
        Arc::new(PipelinePlan {
            tasks: names().map(|row| row[0].clone()).to_vec(),
            bodies: (0..LANES).map(entity).collect(),
            content_type: "text/event-stream",
            accepts: TOTAL_REQUESTS,
            hold_a: false,
            tail_gate: Some(tail_gate.clone()),
        }),
    )));
    let mut clients = Clients::new();
    let result = {
        let mut schedule = LaneSchedule {
            address: gateway_address,
            clients: &mut clients,
            snapshot: &snapshot,
            probe: &probe,
            received: &mut received,
            provider_completed: &mut provider_completed,
            tail_gate: &tail_gate,
            router: router.as_ref(),
            records: records.as_ref(),
        };
        tokio::time::timeout(
            Duration::from_secs(60),
            pipeline_catch_assertions(schedule.exercise(version)),
        )
        .await
    };

    if !matches!(&result, Ok(Ok(()))) {
        eprintln!(
            "eight_gateway_before_failed_cleanup={}",
            json!({
                "version":format!("{version:?}"),
                "snapshot":snapshot(),
                "writer":probe.writer_snapshot(),
                "events":format!("{:?}",probe.events()),
                "provider_live":live.load(Ordering::SeqCst)
            })
        );
    }

    // All assertion/fixture failures reach the same finite owned cleanup path.
    probe.release_writes();
    clients.abort_all();
    let clients_joined = tokio::time::timeout(BARRIER, async {
        while clients.join_next().await.is_some() {}
    })
    .await
    .is_ok();
    let closed = tokio::time::timeout(BARRIER, running.close()).await.is_ok();
    let errors = tasks.close().await;
    drop(gateway);
    drop(transport);
    until(|| buffered_drained(&snapshot()) && probe.tasks() == 0).await;
    assert!(closed && errors.is_empty(), "{errors:?}");
    assert!(
        clients_joined,
        "owned downstream client abort/join exceeded its bound"
    );
    assert_eq!(live.load(Ordering::SeqCst), 0);
    assert_eq!(
        (
            probe.writer_snapshot().live_io,
            probe.writer_snapshot().waiters
        ),
        (0, 0)
    );
    assert!(!probe.observer_failed());
    drop(tokio::net::TcpListener::bind(address).await.unwrap());
    drop(
        tokio::net::TcpListener::bind(gateway_address)
            .await
            .unwrap(),
    );
    eprintln!(
        "eight_gateway_cleanup={}",
        json!({
            "version":format!("{version:?}"),
            "snapshot":snapshot(),
            "terminal_tasks":probe.tasks(),
            "writer":probe.writer_snapshot(),
            "provider_live":live.load(Ordering::SeqCst),
            "clients_joined":clients_joined
        })
    );
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => std::panic::resume_unwind(error),
        Err(_) => panic!("eight client fixture deadline"),
    }
}
#[tokio::test]
async fn eight_gateway_http_full_block_owners_cancel_expire_and_recover() {
    eight(None).await;
}
#[tokio::test]
async fn eight_gateway_tls12_full_block_owners_cancel_expire_and_recover() {
    eight(Some(SslVersion::TLS1_2)).await;
}
#[tokio::test]
async fn eight_gateway_tls13_full_block_owners_cancel_expire_and_recover() {
    eight(Some(SslVersion::TLS1_3)).await;
}
