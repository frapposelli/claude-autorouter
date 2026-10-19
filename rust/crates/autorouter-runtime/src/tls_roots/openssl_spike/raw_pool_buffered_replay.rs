//! Fresh children for the unchanged session/idle differential vectors.
//! Constructor observation does not promote this test-only backend.
use super::super::super::{gateway_terminal, idle_close, options, policy};
use super::*;
use crate::server::{Gateway, GatewayHandle};
use crate::server_events::EventSinks;
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const CLEANUP: Duration = Duration::from_secs(2);
struct Fixture {
    client: Arc<BufferedRawPoolClient>,
    gateway: Arc<Gateway<BufferedRawPoolClient>>,
    running: GatewayHandle,
    terminal: gateway_terminal::Probe,
}
impl Fixture {
    async fn new(idle: Option<idle_close::Probe>) -> Self {
        let environment: serde_json::Map<String, serde_json::Value> = std::env::vars()
            .map(|(key, value)| (key, value.into()))
            .collect();
        let config = autorouter_core::config::read_config(
            &environment.into(),
            true,
            std::path::Path::new("/synthetic"),
        )
        .unwrap();
        for endpoint in [&config.upstream, &config.jev_endpoint] {
            assert!(
                endpoint
                    .parse::<Uri>()
                    .unwrap()
                    .host()
                    .unwrap()
                    .parse::<std::net::IpAddr>()
                    .unwrap()
                    .is_loopback()
            );
        }
        let trust = policy::trust_snapshot().unwrap();
        let client = Arc::new(match idle {
            Some(probe) => BufferedRawPoolClient::with_idle(&trust, probe).unwrap(),
            None => BufferedRawPoolClient::new(&trust).unwrap(),
        });
        let terminal = gateway_terminal::Probe::default();
        // No legacy Intent is attached: idle observation and request-terminal
        // ownership use distinct channels, with only one assignment capture.
        let gateway = Gateway::with_test_terminal(
            Gateway::new(config, client.clone(), EventSinks::default()).unwrap(),
            terminal.clone(),
        );
        let running = gateway.listen(0).await.unwrap();
        Self {
            client,
            gateway,
            running,
            terminal,
        }
    }
    async fn finish(self) -> serde_json::Value {
        let Self {
            client,
            gateway,
            running,
            terminal,
        } = self;
        let raw = client.inner.raw_counts.clone();
        let fetch = client.inner.fetch_counts.clone();
        let buffers = client.probe.clone();
        let (entries, bytes, peak_bytes) = client.inner.raw_sessions.lock().unwrap().snapshot();
        let cache = Arc::downgrade(&client.inner.raw_sessions);
        let address = running.address;
        let cleaned = tokio::time::timeout(CLEANUP, async {
            running.close().await;
            drop(gateway);
            drop(client);
            loop {
                let buffered = buffers.snapshot();
                if raw.active.load(Ordering::SeqCst) == 0
                    && raw.tasks.load(Ordering::SeqCst) == 0
                    && raw.shutdown_handles.load(Ordering::SeqCst) == 0
                    && fetch.active.load(Ordering::SeqCst) == 0
                    && fetch.tasks.load(Ordering::SeqCst) == 0
                    && fetch.shutdown_handles.load(Ordering::SeqCst) == 0
                    && buffered.live_pools == 0
                    && buffered.allocated_blocks == 0
                    && buffered.outstanding_blocks == 0
                    && buffered.queued_bytes == 0
                    && buffered.producer_tasks == 0
                    && terminal.tasks() == 0
                    && terminal.writer_snapshot().live_io == 0
                    && terminal.writer_snapshot().waiters == 0
                    && cache.upgrade().is_none()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
            drop(tokio::net::TcpListener::bind(address).await.unwrap());
        })
        .await
        .is_ok();
        let transport = terminal.transport_events();
        let attempts = transport
            .iter()
            .filter(|row| {
                matches!(
                    row.outcome,
                    gateway_terminal::TransportOutcome::Applied(
                        super::super::super::abort::Outcome::Requested
                            | super::super::super::abort::Outcome::ShutdownFailed(_)
                    )
                )
            })
            .count();
        let attempts_match = raw.shutdown_calls.load(Ordering::SeqCst) == attempts;
        let clean = cleaned && attempts_match && !terminal.observer_failed();
        serde_json::json!({"cleanup":clean,"cleaned":clean,
            "raw_attempts":raw.attempts.load(Ordering::SeqCst),
            "raw_completed":raw.completed.load(Ordering::SeqCst),
            "raw_active":raw.active.load(Ordering::SeqCst),
            "fetch_active":fetch.active.load(Ordering::SeqCst),
            "tasks":raw.tasks.load(Ordering::SeqCst),
            "fetch_tasks":fetch.tasks.load(Ordering::SeqCst),
            "shutdown_handles":raw.shutdown_handles.load(Ordering::SeqCst),
            "shutdown_calls":raw.shutdown_calls.load(Ordering::SeqCst),
            "fetch_shutdown_handles":fetch.shutdown_handles.load(Ordering::SeqCst),
            "entries_before_drop":entries,"bytes_before_drop":bytes,"peak_bytes":peak_bytes,
            "cache_released":cache.upgrade().is_none(),"buffers":buffers.snapshot(),
            "terminal_tasks":terminal.tasks(),"writer":terminal.writer_snapshot(),
            "shutdown_attempts_match":attempts_match,
            "session_error_closes":raw.session_error_closes.load(Ordering::SeqCst),
            "terminal_events":format!("{:?}",terminal.events()),
            "transport_actions":transport.iter().map(|row| serde_json::json!({
                "identity":format!("{:?}",row.identity),"cause":format!("{:?}",row.cause),
                "outcome":format!("{:?}",row.outcome)
            })).collect::<Vec<_>>()})
    }
}

#[tokio::test]
async fn session_gateway_child() {
    if std::env::var("AUTOROUTER_SYNTHETIC_OPENSSL_SPIKE").as_deref() != Ok("stage-b2") {
        return;
    }
    if let Err(diagnostic) =
        options::Options::parse(&std::env::var("NODE_OPTIONS").unwrap_or_default())
    {
        let executable = std::env::args().next().unwrap_or_default();
        for line in diagnostic.lines() {
            eprintln!("{executable}: {line}");
        }
        std::process::exit(9);
    }
    let fixture = Fixture::new(None).await;
    #[cfg(unix)]
    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
    eprintln!("AutoRouter listening on http://{}", fixture.running.address);
    let signalled = tokio::time::timeout(Duration::from_secs(25), async {
        #[cfg(unix)]
        tokio::select! { _ = terminate.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await.unwrap();
    })
    .await
    .is_ok();
    let mut cleanup = fixture.finish().await;
    if !signalled {
        cleanup["cleaned"] = false.into();
        cleanup["cleanup"] = false.into();
    }
    eprintln!("OpenSSL session fixture: {cleanup}");
    assert!(
        cleanup["cleaned"].as_bool().unwrap(),
        "buffered session cleanup/deadline"
    );
}

async fn reply(control: &mut TcpStream, value: serde_json::Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    assert!(bytes.len() < 32768, "idle control response bound");
    bytes.push(b'\n');
    control.write_all(&bytes).await
}
#[tokio::test]
async fn idle_gateway_child() {
    let Ok(control_address) = std::env::var("AUTOROUTER_SYNTHETIC_IDLE_CONTROL") else {
        return;
    };
    let address: std::net::SocketAddr = control_address.parse().unwrap();
    assert!(address.ip().is_loopback());
    let probe = idle_close::Probe::default();
    let fixture = Fixture::new(Some(probe.clone())).await;
    let mut control = TcpStream::connect(address).await.unwrap();
    reply(
        &mut control,
        serde_json::json!({"port":fixture.running.address.port()}),
    )
    .await
    .unwrap();
    let mut holds = BTreeMap::new();
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let mut line = Vec::new();
            loop {
                let mut byte = [0];
                if control.read(&mut byte).await? == 0 {
                    return Ok::<_, io::Error>(());
                }
                if byte[0] == b'\n' {
                    break;
                }
                assert!(line.len() < 512, "idle control command bound");
                line.push(byte[0]);
            }
            let command: serde_json::Value = serde_json::from_slice(&line).unwrap();
            match command["op"].as_str().unwrap() {
                "hold" => {
                    let id = usize::try_from(command["connection"].as_u64().unwrap()).unwrap();
                    assert!((1..=8).contains(&id));
                    let kind = command["kind"].as_str().unwrap();
                    let hold = match kind {
                        "read" => probe.hold_read(id),
                        "write" => probe.hold_write(id),
                        _ => panic!("idle gate kind"),
                    };
                    assert!(holds.insert((id, kind.to_owned()), hold).is_none());
                }
                "release" => holds.clear(),
                "snapshot" => {}
                "stop" => break,
                _ => panic!("idle control operation"),
            }
            let selected: Vec<_> = (1..=16)
                .filter_map(|id| probe.assignment(id).map(|(connection, _)| [id, connection]))
                .collect();
            reply(
                &mut control,
                serde_json::json!({
                    "rows":probe.rows().iter().map(|row|format!("{row:?}")).collect::<Vec<_>>(),
                    "selected":selected,"live":probe.live()
                }),
            )
            .await?;
        }
        Ok(())
    })
    .await;
    drop(holds);
    let mut cleanup = fixture.finish().await;
    cleanup["active"] = cleanup["raw_active"].clone();
    let cleaned = cleanup["cleanup"].as_bool().unwrap() && probe.live() == 0;
    cleanup["cleanup"] = cleaned.into();
    if cleaned {
        probe.record(idle_close::Event::Cleanup);
    }
    cleanup["rows"] = serde_json::json!(
        probe
            .rows()
            .iter()
            .map(|row| format!("{row:?}"))
            .collect::<Vec<_>>()
    );
    let _ = reply(&mut control, cleanup).await;
    assert!(cleaned, "buffered idle cleanup");
    result.expect("buffered idle child deadline").unwrap();
}

// The replay profile owns one assignment capture. A legacy Intent must not
// silently replace it or reach acquire/dial before this closed-profile guard.
#[tokio::test]
async fn mixed_legacy_intent_and_idle_profile_rejects_before_admission() {
    use super::super::super::gateway_intent;

    let (trust, _) = super::super::super::lifecycle_tests::isolated_tls(None);
    let idle = idle_close::Probe::default();
    let client = BufferedRawPoolClient::with_idle(&trust, idle.clone()).unwrap();
    let intents = gateway_intent::Probe::default();
    let registry = gateway_intent::Registry::new(intents.clone(), 1);
    let intent = registry.register().unwrap();
    let cache = Arc::downgrade(&client.inner.raw_sessions);
    let before_raw = client.inner.snapshot();
    let before_buffers = serde_json::to_value(client.probe.snapshot()).unwrap();
    let mut request = Request::get("http://127.0.0.1:9/v1/models/mixed-profile")
        .body(Full::new(Bytes::new()))
        .unwrap();
    request.extensions_mut().insert(intent.clone());

    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(250), client.request_raw(request))
            .await
            .expect("mixed-profile rejection must not wait for a socket"),
        Err(HttpError::InvalidRequest)
    ));
    assert_eq!(client.inner.snapshot(), before_raw);
    assert_eq!(
        serde_json::to_value(client.probe.snapshot()).unwrap(),
        before_buffers
    );
    assert!(idle.rows().is_empty(), "idle capture must not be attached");
    assert_eq!(idle.live(), 0);
    let rows = intents.rows();
    assert_eq!(rows.len(), 1, "no Intent capture or terminal event");
    assert_eq!(rows[0].event, gateway_intent::Event::Registered);
    assert_eq!(
        client.inner.raw_sessions.lock().unwrap().snapshot(),
        (0, 0, 0)
    );
    for counts in [&client.inner.raw_counts, &client.inner.fetch_counts] {
        assert_eq!(
            [
                counts.attempts.load(Ordering::SeqCst),
                counts.active.load(Ordering::SeqCst),
                counts.completed.load(Ordering::SeqCst),
                counts.tasks.load(Ordering::SeqCst),
                counts.shutdown_handles.load(Ordering::SeqCst),
                counts.shutdown_calls.load(Ordering::SeqCst),
                counts.session_error_closes.load(Ordering::SeqCst),
                counts.read_bytes.load(Ordering::SeqCst),
                counts.published.load(Ordering::SeqCst),
            ],
            [0; 9]
        );
    }
    drop(intent);
    drop(registry);
    drop(client);
    assert_eq!(intents.live(), 0);
    assert_eq!(idle.live(), 0);
    assert!(cache.upgrade().is_none());
}
