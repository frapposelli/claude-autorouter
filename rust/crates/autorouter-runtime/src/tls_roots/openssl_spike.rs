//! Isolated OpenSSL stream experiment (stage A plus test-only B1 options). Compiled only into the runtime
//! library test executable; there is no shipping CLI switch or transport hook.
mod client;
mod options;
mod policy;

use crate::server::Gateway;
use crate::server_events::EventSinks;
use client::SpikeHttpClient;
use std::sync::Arc;

/// Invoked by the differential driver as the sole selected test in a fresh
/// child process. Ordinary cargo test never starts this synthetic gateway.
#[tokio::test]
async fn gateway_child() {
    if std::env::var("AUTOROUTER_SYNTHETIC_OPENSSL_SPIKE").as_deref() != Ok("stage-a") {
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
    let transport = match SpikeHttpClient::new() {
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
    // Stage-B cleanup and pool-race tests will qualify these counters. This
    // runner does not report missing lifecycle assertions as covered.
    let _counts = (&transport.raw_counts, &transport.fetch_counts);
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
    let waiter = tokio::spawn(async move {
        waiter_client
            .request_raw(
                Request::get(format!("https://{stalled_address}/synthetic"))
                    .body(Full::new(Bytes::new()))
                    .unwrap(),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), hello_rx)
        .await
        .unwrap()
        .unwrap();
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
    assert!(waiter.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(3), stalled_peer)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), healthy_peer)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while client.raw_counts.active.load(Ordering::SeqCst) != 0 {
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
