//! Isolated OpenSSL stream experiment (stage A plus test-only B1 options). Compiled only into the runtime
//! library test executable; there is no shipping CLI switch or transport hook.
mod abort;
mod abort_tests;
mod client;
pub(crate) mod gateway_intent;
mod gateway_intent_tests;
mod idle_close;
mod idle_close_tests;
mod lifecycle;
mod lifecycle_tests;
mod options;
mod policy;
mod raw_pool;
mod request_lease_tests;
mod session;

use crate::server::Gateway;
use crate::server_events::EventSinks;
use client::SpikeHttpClient;
use std::sync::Arc;

/// Invoked by the differential driver as the sole selected test in a fresh
/// child process. Ordinary cargo test never starts this synthetic gateway.
#[tokio::test]
async fn gateway_child() {
    run_gateway_child(false).await;
}

/// Separate test executable entrypoint; the original stage A/B2 mode is unchanged.
#[tokio::test]
async fn gateway_intent_child() {
    run_gateway_child(true).await;
}

async fn run_gateway_child(intents: bool) {
    let mode = std::env::var("AUTOROUTER_SYNTHETIC_OPENSSL_SPIKE").unwrap_or_default();
    if !matches!(mode.as_str(), "stage-a" | "stage-b2") {
        return;
    }
    let options = std::env::var("NODE_OPTIONS").unwrap_or_default();
    if let Err(diagnostic) = options::Options::parse(&options) {
        let executable = std::env::args().next().unwrap_or_default();
        for line in diagnostic.lines() {
            eprintln!("{executable}: {line}");
        }
        std::process::exit(9);
    }
    let candidate = if intents {
        policy::trust_snapshot().and_then(|snapshot| {
            SpikeHttpClient::with_snapshot_aborts(mode == "stage-b2", &snapshot, true)
        })
    } else {
        SpikeHttpClient::with_sessions(mode == "stage-b2")
    };
    let transport = match candidate {
        Ok(transport) => Arc::new(transport),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let environment: serde_json::Map<String, serde_json::Value> = std::env::vars()
        .map(|(key, value)| (key, value.into()))
        .collect();
    let config = autorouter_core::config::read_config(
        &environment.into(),
        true,
        &std::env::current_dir().unwrap(),
    )
    .unwrap();
    let gateway = Gateway::new(config, transport.clone(), EventSinks::default()).unwrap();
    let probe = gateway_intent::Probe::default();
    let gateway = if intents {
        Gateway::with_test_intent(gateway, probe.clone())
    } else {
        gateway
    };
    let server = gateway.listen(0).await.unwrap();
    eprintln!("AutoRouter listening on http://{}", server.address);
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).unwrap();
        tokio::select! { _ = terminate.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await.unwrap();
    server.close().await;
    drop(gateway);
    if intents {
        assert_eq!(probe.live(), 0, "gateway intent cleanup");
        eprintln!("OpenSSL gateway intent fixture: {:?}", probe.rows());
    }
    if mode == "stage-b2" {
        use std::sync::atomic::Ordering;
        let raw = transport.raw_counts.clone();
        let fetch = transport.fetch_counts.clone();
        let cache = transport.raw_sessions.as_ref().unwrap();
        let (entries, bytes, peak_bytes) = cache.lock().unwrap().snapshot();
        let weak_cache = Arc::downgrade(cache);
        drop(transport);
        let cleaned = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while raw.active.load(Ordering::SeqCst) != 0 || fetch.active.load(Ordering::SeqCst) != 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok();
        eprintln!(
            "OpenSSL session fixture: {}",
            serde_json::json!({
                "raw_attempts": raw.attempts.load(Ordering::SeqCst),
                "raw_completed": raw.completed.load(Ordering::SeqCst),
                "raw_active": raw.active.load(Ordering::SeqCst),
                "fetch_active": fetch.active.load(Ordering::SeqCst),
                "entries_before_drop": entries, "bytes_before_drop": bytes, "peak_bytes": peak_bytes,
                "cache_released": weak_cache.upgrade().is_none(), "cleaned": cleaned,
            })
        );
    }
}

#[tokio::test]
async fn stalled_handshake_cancellation_releases_owned_socket_and_preserves_sibling() {
    use crate::http_client::HttpTransport;
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::Request;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let stalled = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let healthy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stalled_address = stalled.local_addr().unwrap();
    let healthy_address = healthy.local_addr().unwrap();
    let (hello_tx, hello_rx) = tokio::sync::oneshot::channel();
    let stalled_peer = tokio::spawn(async move {
        let (mut socket, _) = stalled.accept().await.unwrap();
        let mut bytes = [0; 16384];
        let count = socket.read(&mut bytes).await.unwrap();
        assert!(count > 0, "client emitted no handshake flight");
        hello_tx.send(()).unwrap();
        loop {
            match socket.read(&mut bytes).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    });
    let healthy_peer = tokio::spawn(async move {
        let (mut socket, _) = healthy.accept().await.unwrap();
        let mut head = Vec::new();
        let mut bytes = [0; 2048];
        while !head.windows(4).any(|part| part == b"\r\n\r\n") {
            let count = socket.read(&mut bytes).await.unwrap();
            assert_ne!(count, 0);
            head.extend_from_slice(&bytes[..count]);
            assert!(head.len() < 16384);
        }
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-length: 9\r\nconnection: close\r\n\r\nsynthetic",
            )
            .await
            .unwrap();
    });
    let client = Arc::new(SpikeHttpClient::new().unwrap());
    let waiter_client = client.clone();
    let mut request = Request::get(format!("https://{stalled_address}/synthetic"))
        .body(Full::new(Bytes::new()))
        .unwrap();
    let mut capture = hyper_util::client::legacy::connect::capture_connection(&mut request);
    assert!(lifecycle::ConnectionIdentity::captured(&capture).is_none());
    let waiter = tokio::spawn(async move { waiter_client.request_raw(request).await });
    tokio::time::timeout(Duration::from_secs(3), hello_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(lifecycle::ConnectionIdentity::captured(&capture).is_none());
    let response = tokio::time::timeout(
        Duration::from_secs(3),
        client.request_raw(
            Request::get(format!("http://{healthy_address}/synthetic"))
                .body(Full::new(Bytes::new()))
                .unwrap(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "synthetic"
    );
    waiter.abort();
    assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
    assert!(
        tokio::time::timeout(
            Duration::from_secs(3),
            capture.wait_for_connection_metadata()
        )
        .await
        .unwrap()
        .is_none()
    );
    tokio::time::timeout(Duration::from_secs(3), stalled_peer)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), healthy_peer)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while client.raw_counts.active.load(Ordering::SeqCst) != 0
            || client.raw_counts.tasks.load(Ordering::SeqCst) != 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(client.raw_counts.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(client.raw_counts.completed.load(Ordering::SeqCst), 0);
    assert_eq!(client.fetch_counts.attempts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn non_loopback_destinations_are_rejected_before_connection_attempts() {
    use crate::http_client::HttpTransport;
    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::Request;
    use std::sync::atomic::Ordering;
    let client = SpikeHttpClient::new().unwrap();
    for uri in ["https://synthetic.invalid/", "http://192.0.2.1/"] {
        let request = || Request::get(uri).body(Full::new(Bytes::new())).unwrap();
        assert!(client.request_raw(request()).await.is_err());
        assert!(client.request(request()).await.is_err());
    }
    assert_eq!(client.raw_counts.attempts.load(Ordering::SeqCst), 0);
    assert_eq!(client.fetch_counts.attempts.load(Ordering::SeqCst), 0);
}
