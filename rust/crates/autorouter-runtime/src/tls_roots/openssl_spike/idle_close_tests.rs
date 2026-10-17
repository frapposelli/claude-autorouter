//! Explicit client-close and current-selection barriers, not pool-state claims.
use std::collections::BTreeMap;
use std::io;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use hyper_openssl::SslStream;
use hyper_util::rt::TokioIo;
use openssl::ssl::{Ssl, SslVersion};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::client::SpikeHttpClient;
use super::idle_close::{Event, Probe};
use super::lifecycle_tests::isolated_tls;
use crate::http_client::{HttpError, HttpTransport};

const BARRIER: Duration = Duration::from_secs(2);
const SCENARIO: Duration = Duration::from_secs(8);
const RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: keep-alive\r\n\r\nx";

struct PeerRequest {
    connection: usize,
    reply: oneshot::Sender<()>,
}
struct PeerLease {
    active: Arc<AtomicUsize>,
    probe: Probe,
    ordinal: usize,
}
impl Drop for PeerLease {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
        self.probe.record(Event::PeerClosed(self.ordinal));
    }
}
async fn peer<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    ordinal: usize,
    requests: mpsc::Sender<PeerRequest>,
) -> io::Result<()> {
    loop {
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            if stream.read(&mut byte).await? == 0 {
                return Ok(());
            }
            head.push(byte[0]);
            if head.len() > 16384 {
                return Err(io::Error::other("idle header bound"));
            }
        }
        let (reply, wait) = oneshot::channel();
        requests
            .send(PeerRequest {
                connection: ordinal,
                reply,
            })
            .await
            .map_err(|_| io::Error::other("idle request receiver released"))?;
        if wait.await.is_err() {
            return Ok(());
        }
        stream.write_all(RESPONSE).await?;
        stream.flush().await?;
    }
}
struct Fixture {
    client: Arc<SpikeHttpClient>,
    probe: Probe,
    origin: String,
    requests: mpsc::Receiver<PeerRequest>,
    closes: Arc<Mutex<BTreeMap<usize, oneshot::Sender<()>>>>,
    stop: CancellationToken,
    tasks: JoinSet<()>,
    active: Arc<AtomicUsize>,
}
impl Fixture {
    async fn new(version: Option<SslVersion>) -> Self {
        let (trust, tls) = isolated_tls(version);
        let probe = Probe::default();
        let client =
            Arc::new(SpikeHttpClient::with_snapshot_idle(true, &trust, probe.clone()).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!(
            "{}://{}",
            if version.is_some() { "https" } else { "http" },
            listener.local_addr().unwrap()
        );
        let (send, requests) = mpsc::channel(16);
        let closes = Arc::new(Mutex::new(BTreeMap::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let stop = CancellationToken::new();
        let mut tasks = JoinSet::new();
        let (peer_closes, peer_active, peer_probe, stopping) =
            (closes.clone(), active.clone(), probe.clone(), stop.clone());
        tasks.spawn(async move {
            let mut peers = JoinSet::new();
            let mut ordinal = 0;
            loop {
                let stream = tokio::select! {
                    _ = stopping.cancelled() => break,
                    result = listener.accept() => result.unwrap().0,
                };
                ordinal += 1;
                assert!(ordinal <= 8, "idle peer connection bound");
                let (close, mut closed) = oneshot::channel();
                peer_closes.lock().unwrap().insert(ordinal, close);
                peer_active.fetch_add(1, Ordering::SeqCst);
                let lease = PeerLease {
                    active: peer_active.clone(),
                    probe: peer_probe.clone(),
                    ordinal,
                };
                let (tls, send) = (tls.clone(), send.clone());
                peers.spawn(async move {
                    let _lease = lease;
                    let exchange = async {
                        if let Some(context) = tls {
                            let mut stream =
                                SslStream::new(Ssl::new(&context).unwrap(), TokioIo::new(stream))
                                    .unwrap();
                            std::pin::Pin::new(&mut stream)
                                .accept()
                                .await
                                .map_err(io::Error::other)?;
                            peer(TokioIo::new(stream), ordinal, send).await
                        } else {
                            peer(stream, ordinal, send).await
                        }
                    };
                    // The exact peer stream is dropped before PeerClosed is recorded.
                    // No synthetic TLS close_notify is added to abrupt-close fixtures.
                    tokio::select! { _ = &mut closed => {}, _ = exchange => {} }
                });
            }
            peers.abort_all();
            while let Some(result) = peers.join_next().await {
                assert!(result.is_ok() || result.unwrap_err().is_cancelled());
            }
        });
        Self {
            client,
            probe,
            origin,
            requests,
            closes,
            stop,
            tasks,
            active,
        }
    }
    async fn finish(mut self) {
        self.stop.cancel();
        let closes = std::mem::take(&mut *self.closes.lock().unwrap());
        drop(closes);
        let raw = self.client.raw_counts.clone();
        let fetch = self.client.fetch_counts.clone();
        drop(self.client);
        tokio::time::timeout(BARRIER, async {
            while let Some(result) = self.tasks.join_next().await {
                result.unwrap();
            }
            while raw.active.load(Ordering::SeqCst) != 0
                || raw.tasks.load(Ordering::SeqCst) != 0
                || fetch.active.load(Ordering::SeqCst) != 0
                || fetch.tasks.load(Ordering::SeqCst) != 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("idle cleanup deadline");
        assert_eq!(raw.shutdown_handles.load(Ordering::SeqCst), 0);
        assert_eq!(raw.shutdown_calls.load(Ordering::SeqCst), 0);
        assert_eq!(self.active.load(Ordering::SeqCst), 0);
        assert_eq!(self.probe.live(), 0);
        self.probe.record(Event::Cleanup);
        eprintln!("Idle-close fixture: {:?}", self.probe.rows());
    }
}
async fn wait(probe: &Probe, event: Event) {
    tokio::time::timeout(BARRIER, probe.wait(event))
        .await
        .expect("idle event deadline");
}
fn close(closes: &Arc<Mutex<BTreeMap<usize, oneshot::Sender<()>>>>, ordinal: usize) {
    let sender = closes
        .lock()
        .unwrap()
        .remove(&ordinal)
        .expect("owned peer close");
    sender.send(()).unwrap();
}
fn request(
    tasks: &mut JoinSet<Result<(StatusCode, Bytes), HttpError>>,
    client: &Arc<SpikeHttpClient>,
    origin: &str,
    ordinal: usize,
) {
    let client = client.clone();
    let uri = format!("{origin}/v1/models/session-{ordinal}");
    tasks.spawn(async move {
        let response = client
            .request_raw(Request::get(uri).body(Full::new(Bytes::new())).unwrap())
            .await?;
        let status = response.status();
        let mut body = response.into_body();
        let mut bytes = Vec::new();
        while let Some(frame) = body.frame().await {
            if let Ok(data) = frame.map_err(|_| HttpError::Network)?.into_data() {
                assert!(data.len() <= 65536 - bytes.len(), "idle response bound");
                bytes.extend_from_slice(&data);
            }
        }
        Ok((status, Bytes::from(bytes)))
    });
}
async fn complete(
    tasks: &mut JoinSet<Result<(StatusCode, Bytes), HttpError>>,
    probe: &Probe,
    ordinal: usize,
) {
    let result = tokio::time::timeout(BARRIER, tasks.join_next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result, (StatusCode::OK, Bytes::from_static(b"x")));
    probe.record(Event::BodyComplete(ordinal));
}
async fn scenario(version: Option<SslVersion>, selection_first: bool) {
    let mut fixture = Fixture::new(version).await;
    let client = fixture.client.clone();
    let probe = fixture.probe.clone();
    let origin = fixture.origin.clone();
    let closes = fixture.closes.clone();
    let mut receives = std::mem::replace(&mut fixture.requests, mpsc::channel(1).1);
    let run = tokio::spawn(async move {
        tokio::time::timeout(SCENARIO, async {
            let mut requests = JoinSet::new();
            request(&mut requests, &client, &origin, 0);
            let first = receives.recv().await.unwrap();
            first.reply.send(()).unwrap();
            complete(&mut requests, &probe, 1).await;
            let (connection, old_assignment) = probe.assignment(1).unwrap();
            assert_eq!(connection, 1);
            if selection_first {
                // Hold the next socket read and HTTP write, never handshake IO.
                let read = probe.hold_read(connection);
                let write = probe.hold_write(connection);
                request(&mut requests, &client, &origin, 1);
                wait(&probe, Event::WriteHeld(connection)).await;
                let (next_connection, selected) = probe.assignment(2).unwrap();
                assert_eq!(next_connection, connection, "must select the exact prior connection");
                probe.record(Event::Selected { request: 2, connection });
                // This is an existing exclusive-lease negative assertion, not a
                // nonmutating pool/retirement observation. It must not poison B.
                assert!(old_assignment.claim_abort().is_none());
                close(&closes, first.connection);
                wait(&probe, Event::PeerClosed(first.connection)).await;
                drop(read);
                drop(write);
                let result = requests.join_next().await.unwrap().unwrap();
                assert!(result.is_err(), "closed selected request cannot succeed or retry");
                assert!(selected.claim_abort().is_none());
                assert_eq!(client.raw_counts.attempts.load(Ordering::SeqCst), 1);
            } else {
                close(&closes, first.connection);
                wait(&probe, Event::PeerClosed(first.connection)).await;
                wait(&probe, Event::Released(connection)).await;
                assert!(probe.rows().iter().any(|event| matches!(event, Event::ReadEof(id) | Event::ReadError(id, _) if *id == connection)), "transport release must have actual prior read-terminal evidence");
                assert!(old_assignment.claim_abort().is_none());
            }
            request(&mut requests, &client, &origin, 2);
            let next = receives.recv().await.unwrap();
            assert_ne!(next.connection, first.connection);
            next.reply.send(()).unwrap();
            let ordinal = if selection_first { 3 } else { 2 };
            complete(&mut requests, &probe, ordinal).await;
            assert_ne!(probe.assignment(ordinal).unwrap().0, connection);
            assert_eq!(client.raw_counts.attempts.load(Ordering::SeqCst), 2);
            assert!(requests.is_empty());
        }).await.expect("idle scenario deadline");
    }).await;
    fixture.finish().await;
    run.unwrap();
}

#[tokio::test]
async fn observed_http_close_before_next_request() {
    scenario(None, false).await;
}
#[tokio::test]
async fn selected_http_connection_before_close_observation() {
    scenario(None, true).await;
}
#[tokio::test]
async fn observed_tls12_close_before_next_request() {
    scenario(Some(SslVersion::TLS1_2), false).await;
}
#[tokio::test]
async fn selected_tls12_connection_before_close_observation() {
    scenario(Some(SslVersion::TLS1_2), true).await;
}
#[tokio::test]
async fn observed_tls13_close_before_next_request() {
    scenario(Some(SslVersion::TLS1_3), false).await;
}
#[tokio::test]
async fn selected_tls13_connection_before_close_observation() {
    scenario(Some(SslVersion::TLS1_3), true).await;
}

/// Separate synthetic entrypoint. It never runs a gateway during ordinary tests.
#[tokio::test]
async fn controlled_gateway_child() {
    let Ok(control_address) = std::env::var("AUTOROUTER_SYNTHETIC_IDLE_CONTROL") else {
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
        let url: hyper::Uri = endpoint.parse().unwrap();
        assert!(
            url.host()
                .unwrap()
                .parse::<std::net::IpAddr>()
                .unwrap()
                .is_loopback()
        );
    }
    let probe = Probe::default();
    let snapshot = super::policy::trust_snapshot().unwrap();
    let client =
        Arc::new(SpikeHttpClient::with_snapshot_idle(true, &snapshot, probe.clone()).unwrap());
    let raw = client.raw_counts.clone();
    let fetch = client.fetch_counts.clone();
    let gateway = crate::server::Gateway::new(
        config,
        client.clone(),
        crate::server_events::EventSinks::default(),
    )
    .unwrap();
    let running = gateway.listen(0).await.unwrap();
    let mut control = tokio::net::TcpStream::connect(address).await.unwrap();
    async fn reply(
        control: &mut tokio::net::TcpStream,
        value: serde_json::Value,
    ) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(&value).unwrap();
        assert!(bytes.len() < 32768, "idle control response bound");
        bytes.push(b'\n');
        control.write_all(&bytes).await
    }
    reply(
        &mut control,
        serde_json::json!({"port":running.address.port()}),
    )
    .await
    .unwrap();
    let mut holds = BTreeMap::new();
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let mut line = Vec::new();
            loop {
                let mut b=[0];
                if control.read(&mut b).await? == 0 { return Ok::<_,io::Error>(()); }
                if b[0]==b'\n' { break; }
                assert!(line.len()<512, "idle control command bound");
                line.push(b[0]);
            }
            let command: serde_json::Value=serde_json::from_slice(&line).unwrap();
            let op=command["op"].as_str().unwrap();
            match op {
                "hold" => {
                    let id=usize::try_from(command["connection"].as_u64().unwrap()).unwrap();
                    assert!((1..=8).contains(&id));
                    let kind=command["kind"].as_str().unwrap();
                    let hold=match kind {"read"=>probe.hold_read(id),"write"=>probe.hold_write(id),_=>panic!("idle gate kind")};
                    assert!(holds.insert((id,kind.to_owned()),hold).is_none());
                }
                "release" => { holds.clear(); }
                "snapshot" => {}
                "stop" => break,
                _ => panic!("idle control operation"),
            }
            let selected: Vec<_>=(1..=16).filter_map(|id| probe.assignment(id).map(|(connection,_)|[id,connection])).collect();
            reply(&mut control,serde_json::json!({"rows":probe.rows().iter().map(|row|format!("{row:?}")).collect::<Vec<_>>(),"selected":selected,"live":probe.live()})).await?;
        }
        Ok(())
    }).await;
    // Release gates before any teardown await. Control EOF and command errors
    // take this same ownership path; no process-wide or origin-wide abort.
    drop(holds);
    running.close().await;
    drop(gateway);
    drop(client);
    tokio::time::timeout(BARRIER, async {
        while raw.active.load(Ordering::SeqCst) != 0
            || raw.tasks.load(Ordering::SeqCst) != 0
            || fetch.active.load(Ordering::SeqCst) != 0
            || fetch.tasks.load(Ordering::SeqCst) != 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("idle child cleanup deadline");
    assert_eq!(probe.live(), 0);
    assert_eq!(raw.shutdown_handles.load(Ordering::SeqCst), 0);
    assert_eq!(raw.shutdown_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fetch.shutdown_handles.load(Ordering::SeqCst), 0);
    probe.record(Event::Cleanup);
    let _=reply(&mut control,serde_json::json!({"cleanup":true,"rows":probe.rows().iter().map(|row|format!("{row:?}")).collect::<Vec<_>>(),"active":raw.active.load(Ordering::SeqCst),"tasks":raw.tasks.load(Ordering::SeqCst),"shutdown_handles":raw.shutdown_handles.load(Ordering::SeqCst)})).await;
    result.expect("idle child overall deadline").unwrap();
}
