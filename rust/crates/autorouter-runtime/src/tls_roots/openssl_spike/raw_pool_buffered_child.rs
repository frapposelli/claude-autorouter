//! Bounded fresh child for externally driven synthetic gateway schedules.
use super::super::super::{gateway_intent, gateway_terminal};
use super::*;
use crate::server::Gateway;
use crate::server_events::EventSinks;
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

async fn reply(stream: &mut TcpStream, value: serde_json::Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    if bytes.len() > 131_072 {
        return Err(io::Error::other("control reply bound"));
    }
    bytes.push(b'\n');
    tokio::time::timeout(Duration::from_secs(2), stream.write_all(&bytes))
        .await
        .map_err(|_| io::Error::other("control write deadline"))?
}
async fn command(stream: &mut TcpStream) -> io::Result<Option<serde_json::Value>> {
    let mut line = Vec::new();
    loop {
        let mut byte = [0];
        if stream.read(&mut byte).await? == 0 {
            return Ok(None);
        }
        if byte[0] == b'\n' {
            break;
        }
        if line.len() == 4096 {
            return Err(io::Error::other("control command bound"));
        }
        line.push(byte[0]);
    }
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(io::Error::other)
}
fn snapshot(
    client: &BufferedRawPoolClient,
    intents: &gateway_intent::Probe,
    terminal: &gateway_terminal::Probe,
    logs: &Mutex<Vec<serde_json::Value>>,
) -> serde_json::Value {
    use gateway_terminal::Event;
    let events: Vec<_> = terminal.events().iter().map(|event| match event {
        Event::Registered(id) => serde_json::json!({"event":"registered","connection":id.connection,"request":id.request}),
        Event::Attached(id) => serde_json::json!({"event":"attached","connection":id.connection,"request":id.request}),
        Event::Failed(id, cause) => serde_json::json!({"event":"failed","connection":id.connection,"request":id.request,"cause":match cause { FailureCause::Upstream=>"upstream",FailureCause::Deadline=>"deadline",FailureCause::Downstream(_)=>"downstream",FailureCause::Cancelled=>"cancelled"}}),
        Event::DeliveryClaimed(id, delivery) => serde_json::json!({"event":"delivery_claimed","connection":id.connection,"request":id.request,"flushed":*delivery == crate::transport_completion::Delivery::Flushed}),
        Event::CallbackFinished(id) => serde_json::json!({"event":"callback_finished","connection":id.connection,"request":id.request}),
        Event::CallbackAbandoned(id) => serde_json::json!({"event":"callback_abandoned","connection":id.connection,"request":id.request}),
        Event::Retired(id) => serde_json::json!({"event":"retired","connection":id.connection,"request":id.request}),
        Event::ConnectionClaimed(id) => serde_json::json!({"event":"connection_claimed","connection":id.connection,"request":id.request}),
        Event::Closed(id) => serde_json::json!({"event":"closed","connection":id}),
    }).collect();
    serde_json::json!({"buffers":client.probe.snapshot(),"body_held":intents.rows().iter().any(|row| row.event == gateway_intent::Event::BodyHeld),"terminal":events,"logs":*logs.lock().unwrap(),"raw":client.inner.snapshot()})
}
#[tokio::test]
async fn controlled_gateway_child() {
    let Ok(address) = std::env::var("AUTOROUTER_SYNTHETIC_BUFFERED_CONTROL") else {
        return;
    };
    let address: std::net::SocketAddr = address.parse().unwrap();
    assert!(address.ip().is_loopback());
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
    let trust = super::super::super::policy::trust_snapshot().unwrap();
    let client = Arc::new(BufferedRawPoolClient::new(&trust).unwrap());
    let buffers = client.probe.clone();
    let raw = client.inner.raw_counts.clone();
    let fetch = client.inner.fetch_counts.clone();
    let cache = Arc::downgrade(&client.inner.raw_sessions);
    let intents = gateway_intent::Probe::default();
    intents.hold_bodies();
    let terminal = gateway_terminal::Probe::default();
    let logs = Arc::new(Mutex::new(Vec::new()));
    let captured = logs.clone();
    let observer_failed = Arc::new(AtomicBool::new(false));
    let observer = observer_failed.clone();
    let sinks = EventSinks {
        log: Some(Arc::new(move |document| {
            let mut logs = captured.lock().unwrap();
            if logs.len() == 128 {
                observer.store(true, Ordering::SeqCst);
                return;
            }
            match serde_json::from_str(&document.stringify()) {
                Ok(value) => logs.push(value),
                Err(_) => {
                    observer.store(true, Ordering::SeqCst);
                }
            }
        })),
        ..EventSinks::default()
    };
    let gateway = Gateway::with_test_terminal(
        Gateway::with_test_intent(
            Gateway::new(config, client.clone(), sinks).unwrap(),
            intents.clone(),
        ),
        terminal.clone(),
    );
    let running = gateway.listen(0).await.unwrap();
    let mut control = TcpStream::connect(address).await.unwrap();
    reply(
        &mut control,
        serde_json::json!({"port":running.address.port()}),
    )
    .await
    .unwrap();
    #[cfg(unix)]
    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
    let signal = async {
        #[cfg(unix)]
        {
            tokio::select! { _ = terminate.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    };
    let work = async {
        for _ in 0..2048 {
            let Some(command) = command(&mut control).await? else {
                return Ok(());
            };
            match command["op"].as_str() {
                Some("snapshot") => {}
                Some("hold_response") => buffers.hold_responses(),
                Some("release_response") => buffers.release_responses(),
                Some("release_body") => intents.release_bodies(),
                Some("stop") => return Ok(()),
                _ => return Err(io::Error::other("unknown buffered operation")),
            }
            reply(&mut control, snapshot(&client, &intents, &terminal, &logs)).await?;
        }
        Err(io::Error::other("control command count bound"))
    };
    let result = tokio::select! { biased; _ = signal => Ok(Ok(())), result = tokio::time::timeout(Duration::from_secs(12), work) => result };
    buffers.release_responses();
    intents.release_bodies();
    tokio::time::timeout(Duration::from_secs(2), running.close())
        .await
        .unwrap();
    let final_state = snapshot(&client, &intents, &terminal, &logs);
    drop(gateway);
    drop(client);
    tokio::time::timeout(Duration::from_secs(2), async {
        while raw.active.load(Ordering::SeqCst) != 0
            || raw.tasks.load(Ordering::SeqCst) != 0
            || fetch.active.load(Ordering::SeqCst) != 0
            || fetch.tasks.load(Ordering::SeqCst) != 0
            || terminal.tasks() != 0
            || buffers.snapshot().producer_tasks != 0
            || buffers.snapshot().allocated_blocks != 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("buffered child cleanup deadline");
    let cleanup = serde_json::json!({"cleanup":true,"observer_failed":observer_failed.load(Ordering::SeqCst),"live":raw.active.load(Ordering::SeqCst),"tasks":raw.tasks.load(Ordering::SeqCst),"fetch_live":fetch.active.load(Ordering::SeqCst),"fetch_tasks":fetch.tasks.load(Ordering::SeqCst),"shutdown_handles":raw.shutdown_handles.load(Ordering::SeqCst),"fetch_shutdown_handles":fetch.shutdown_handles.load(Ordering::SeqCst),"terminal_tasks":terminal.tasks(),"intents":intents.live(),"cache_released":cache.upgrade().is_none(),"buffers":buffers.snapshot(),"final_state":final_state});
    let _ = reply(&mut control, cleanup.clone()).await;
    assert_eq!(raw.shutdown_handles.load(Ordering::SeqCst), 0);
    assert_eq!(fetch.shutdown_handles.load(Ordering::SeqCst), 0);
    assert_eq!(intents.live(), 0);
    assert!(cache.upgrade().is_none());
    eprintln!("buffered child cleanup: {cleanup}");
    assert!(
        !observer_failed.load(Ordering::SeqCst),
        "bounded log observer failed after cleanup"
    );
    result.expect("buffered child deadline").unwrap();
}
