//! Fresh-process loopback control for the separate raw-agent candidate.
use super::*;
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn controlled_gateway_child() {
    let Ok(control_address) = std::env::var("AUTOROUTER_SYNTHETIC_POOL_CONTROL") else {
        return;
    };
    let address: std::net::SocketAddr = control_address.parse().unwrap();
    assert!(address.ip().is_loopback());
    let environment: serde_json::Map<String, serde_json::Value> =
        std::env::vars().map(|(k, v)| (k, v.into())).collect();
    let config = autorouter_core::config::read_config(
        &environment.into(),
        true,
        std::path::Path::new("/synthetic"),
    )
    .unwrap();
    for endpoint in [&config.upstream, &config.jev_endpoint] {
        let url: Uri = endpoint.parse().unwrap();
        assert!(
            url.host()
                .unwrap()
                .parse::<std::net::IpAddr>()
                .unwrap()
                .is_loopback()
        );
    }
    let snapshot = super::super::policy::trust_snapshot().unwrap();
    let client = Arc::new(RawPoolClient::new(&snapshot).unwrap());
    let raw = client.raw_counts.clone();
    let fetch = client.fetch_counts.clone();
    let probe = client.probe.clone();
    let sessions = client.raw_sessions.clone();
    let gateway = crate::server::Gateway::new(
        config,
        client.clone(),
        crate::server_events::EventSinks::default(),
    )
    .unwrap();
    let intents = super::super::gateway_intent::Probe::default();
    let gateway = if std::env::var("AUTOROUTER_SYNTHETIC_POOL_INTENTS").as_deref() == Ok("1") {
        crate::server::Gateway::with_test_intent(gateway, intents.clone())
    } else {
        gateway
    };
    let running = gateway.listen(0).await.unwrap();
    let mut control = tokio::net::TcpStream::connect(address).await.unwrap();
    async fn reply(control: &mut tokio::net::TcpStream, value: Trace) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(&value).unwrap();
        assert!(bytes.len() <= 131072, "raw pool control response bound");
        bytes.push(b'\n');
        control.write_all(&bytes).await
    }
    reply(
        &mut control,
        serde_json::json!({"port":running.address.port()}),
    )
    .await
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(25), async {
        for _ in 0..1024 {
            let mut line = Vec::new();
            loop {
                let mut b = [0];
                if control.read(&mut b).await? == 0 {
                    return Ok::<_, io::Error>(());
                }
                if b[0] == b'\n' {
                    break;
                }
                assert!(line.len() < 4096, "raw pool command byte bound");
                line.push(b[0]);
            }
            let command: Trace = serde_json::from_slice(&line).unwrap();
            match command["op"].as_str().unwrap() {
                "hold" | "release" => {
                    let id = usize::try_from(command["request"].as_u64().unwrap()).unwrap();
                    assert!(id < 520);
                    if command["op"] == "hold" {
                        probe.hold(id);
                    } else {
                        probe.release(id);
                    }
                }
                "snapshot" => {}
                "stop" => return Ok(()),
                _ => panic!("raw pool control operation"),
            }
            reply(&mut control, client.snapshot()).await?;
        }
        Err(io::Error::other("raw pool command count bound"))
    })
    .await;
    probe.release_all();
    running.close().await;
    drop(gateway);
    drop(client);
    tokio::time::timeout(Duration::from_secs(2), async {
        while raw.active.load(Ordering::SeqCst) != 0
            || raw.tasks.load(Ordering::SeqCst) != 0
            || fetch.active.load(Ordering::SeqCst) != 0
            || fetch.tasks.load(Ordering::SeqCst) != 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("raw pool owned cleanup deadline");
    assert_eq!(raw.shutdown_handles.load(Ordering::SeqCst), 0);
    assert_eq!(fetch.shutdown_handles.load(Ordering::SeqCst), 0);
    assert_eq!(intents.live(), 0);
    let (entries, bytes, peak_bytes) = sessions.lock().unwrap().snapshot();
    let (rows, held) = probe.snapshot();
    assert!(held.is_empty());
    let _ = reply(&mut control, serde_json::json!({"cleanup":true,"rows":rows,"live":raw.active.load(Ordering::SeqCst),"tasks":raw.tasks.load(Ordering::SeqCst),"fetch_live":fetch.active.load(Ordering::SeqCst),"fetch_tasks":fetch.tasks.load(Ordering::SeqCst),"shutdown_handles":raw.shutdown_handles.load(Ordering::SeqCst),"shutdown_calls":raw.shutdown_calls.load(Ordering::SeqCst),"session_error_closes":raw.session_error_closes.load(Ordering::SeqCst),"sessions":{"entries":entries,"bytes":bytes,"peak_bytes":peak_bytes}})).await;
    result.expect("raw pool child deadline").unwrap();
}

/// Separate original-session-driver entrypoint; old spike children are unchanged.
#[tokio::test]
async fn gateway_child() {
    if std::env::var("AUTOROUTER_SYNTHETIC_OPENSSL_SPIKE").as_deref() != Ok("stage-b2") {
        return;
    }
    let options = std::env::var("NODE_OPTIONS").unwrap_or_default();
    if let Err(diagnostic) = super::super::options::Options::parse(&options) {
        let executable = std::env::args().next().unwrap_or_default();
        for line in diagnostic.lines() {
            eprintln!("{executable}: {line}");
        }
        std::process::exit(9);
    }
    let client =
        Arc::new(RawPoolClient::new(&super::super::policy::trust_snapshot().unwrap()).unwrap());
    let environment: serde_json::Map<String, Trace> =
        std::env::vars().map(|(k, v)| (k, v.into())).collect();
    let config = autorouter_core::config::read_config(
        &environment.into(),
        true,
        &std::env::current_dir().unwrap(),
    )
    .unwrap();
    let gateway = crate::server::Gateway::new(
        config,
        client.clone(),
        crate::server_events::EventSinks::default(),
    )
    .unwrap();
    let probe = super::super::gateway_intent::Probe::default();
    let gateway = crate::server::Gateway::with_test_intent(gateway, probe.clone());
    let server = gateway.listen(0).await.unwrap();
    eprintln!("AutoRouter listening on http://{}", server.address);
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).unwrap();
        tokio::select! {_ = terminate.recv()=>{},_ = tokio::signal::ctrl_c()=>{}}
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await.unwrap();
    server.close().await;
    drop(gateway);
    assert_eq!(probe.live(), 0);
    eprintln!("OpenSSL gateway intent fixture: {:?}", probe.rows());
    let raw = client.raw_counts.clone();
    let fetch = client.fetch_counts.clone();
    let weak = Arc::downgrade(&client.raw_sessions);
    let (entries, bytes, peak_bytes) = client.raw_sessions.lock().unwrap().snapshot();
    drop(client);
    let cleaned = tokio::time::timeout(Duration::from_secs(3), async {
        while raw.active.load(Ordering::SeqCst) != 0
            || raw.tasks.load(Ordering::SeqCst) != 0
            || fetch.active.load(Ordering::SeqCst) != 0
            || fetch.tasks.load(Ordering::SeqCst) != 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    eprintln!(
        "OpenSSL session fixture: {}",
        serde_json::json!({"raw_attempts":raw.attempts.load(Ordering::SeqCst),"raw_completed":raw.completed.load(Ordering::SeqCst),"raw_active":raw.active.load(Ordering::SeqCst),"fetch_active":fetch.active.load(Ordering::SeqCst),"raw_tasks":raw.tasks.load(Ordering::SeqCst),"fetch_tasks":fetch.tasks.load(Ordering::SeqCst),"entries_before_drop":entries,"bytes_before_drop":bytes,"peak_bytes":peak_bytes,"cache_released":weak.upgrade().is_none(),"cleaned":cleaned})
    );
    assert!(cleaned);
    assert_eq!(raw.shutdown_handles.load(Ordering::SeqCst), 0);
}
