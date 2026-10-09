use super::gateway_intent::{Cause, Disconnect, Event, IntentIo, Probe, Registry};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll, Waker},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[derive(Clone, Copy)]
enum Fault {
    Eof,
    Read,
    Write,
    Flush,
    None,
}
struct FakeIo(Fault);
impl AsyncRead for FakeIo {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(if matches!(self.0, Fault::Read) {
            Err(io::ErrorKind::ConnectionReset.into())
        } else {
            Ok(())
        })
    }
}
impl AsyncWrite for FakeIo {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(if matches!(self.0, Fault::Write) {
            Err(io::ErrorKind::BrokenPipe.into())
        } else {
            Ok(bytes.len())
        })
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(if matches!(self.0, Fault::Flush) {
            Err(io::ErrorKind::BrokenPipe.into())
        } else {
            Ok(())
        })
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
#[test]
fn io_cause_is_observed_before_result_returns_and_ordinary_operations_do_not_cancel() {
    for (fault, cause) in [
        (Fault::Eof, Disconnect::ReadEof),
        (Fault::Read, Disconnect::ReadError),
        (Fault::Write, Disconnect::WriteError),
        (Fault::Flush, Disconnect::FlushError),
    ] {
        let probe = Probe::default();
        let registry = Registry::new(probe.clone(), 1);
        let intent = registry.register().unwrap();
        let mut io = IntentIo::new(FakeIo(fault), Some(registry));
        let mut cx = Context::from_waker(Waker::noop());
        match fault {
            Fault::Eof | Fault::Read => {
                let mut bytes = [0];
                let _ = Pin::new(&mut io).poll_read(&mut cx, &mut ReadBuf::new(&mut bytes));
            }
            Fault::Write => {
                let _ = Pin::new(&mut io).poll_write(&mut cx, b"x");
            }
            Fault::Flush => {
                let _ = Pin::new(&mut io).poll_flush(&mut cx);
            }
            Fault::None => unreachable!(),
        }
        assert!(
            probe
                .rows()
                .iter()
                .any(|row| row.event == Event::Terminal(Cause::Downstream(cause)))
        );
        assert_eq!(probe.live(), 1);
        drop(io);
        drop(intent);
        assert_eq!(probe.live(), 0);
    }
    let probe = Probe::default();
    let registry = Registry::new(probe.clone(), 1);
    let intent = registry.register().unwrap();
    let mut io = IntentIo::new(FakeIo(Fault::None), Some(registry));
    let mut cx = Context::from_waker(Waker::noop());
    let _ = Pin::new(&mut io).poll_read(&mut cx, &mut ReadBuf::new(&mut []));
    let _ = Pin::new(&mut io).poll_write(&mut cx, b"");
    let _ = Pin::new(&mut io).poll_write_vectored(&mut cx, &[]);
    let _ = Pin::new(&mut io).poll_flush(&mut cx);
    let _ = Pin::new(&mut io).poll_shutdown(&mut cx);
    drop(io);
    drop(intent);
    assert!(
        !probe
            .rows()
            .iter()
            .any(|row| matches!(row.event, Event::Terminal(_)))
    );
    assert_eq!(probe.live(), 0);
}
#[test]
fn weak_registry_bounds_release_and_first_terminal_cause_are_independent() {
    let probe = Probe::default();
    let registry = Registry::new(probe.clone(), 1);
    let first = registry.register().unwrap();
    assert!(registry.register().is_err());
    drop(first);
    assert_eq!(probe.live(), 0);
    let second = registry.register().unwrap();
    second.finish(Cause::UpstreamFailure);
    second.finish(Cause::Downstream(Disconnect::ReadEof));
    assert_eq!(
        probe
            .rows()
            .iter()
            .filter(|r| r.id == 1 && matches!(r.event, Event::Terminal(_)))
            .count(),
        1
    );
    assert!(
        !probe
            .rows()
            .iter()
            .any(|r| matches!(r.event, Event::Action(_)))
    );
    drop(second);
    let third = registry.register().unwrap();
    third.finish(Cause::Downstream(Disconnect::ReadError));
    third.finish(Cause::UpstreamFailure);
    assert!(
        probe
            .rows()
            .iter()
            .any(|r| r.id == 2
                && r.event == Event::Terminal(Cause::Downstream(Disconnect::ReadError)))
    );
    drop(third);
    let delivered = registry.register().unwrap();
    delivered.observe(Event::UpstreamEof);
    assert!(
        !probe
            .rows()
            .iter()
            .any(|r| r.id == 3 && matches!(r.event, Event::Terminal(_)))
    );
    delivered.finish(Cause::Delivered);
    delivered.finish(Cause::Downstream(Disconnect::WriteError));
    assert!(
        !probe
            .rows()
            .iter()
            .any(|r| r.id == 3 && matches!(r.event, Event::Action(_)))
    );
    drop(delivered);
    assert_eq!(probe.live(), 0);
}

use super::gateway_intent::{Action, Transport};
use super::{client::SpikeHttpClient, lifecycle_tests::isolated_tls};
use crate::{server::Gateway, server_events::EventSinks};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinSet,
};
const LIMIT: Duration = Duration::from_secs(4);
const TOKEN: &str = "synthetic-gateway-intent-token";
#[derive(Clone, Copy, Debug)]
enum Scenario {
    CompleteFin,
    NoBodyFin,
    DeadlineHead,
    DeadlineBody,
    FinHead,
    FinBody,
    ResetHead,
    ResetBody,
    WriteReset,
    ShortBody,
    InvalidHead,
    UpstreamReset,
    FailureThenReset,
    CancelThenFailure,
    DeferredEof,
    Reassigned,
    Sibling,
    ResumeChain,
}
async fn prefix(stream: &mut (impl AsyncWrite + Unpin)) {
    stream
        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 10000\r\n\r\npartial")
        .await
        .unwrap();
}
async fn through(stream: &mut (impl AsyncRead + Unpin), suffix: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    while !bytes.windows(suffix.len()).any(|part| part == suffix) {
        let mut chunk = [0; 1024];
        let count = stream.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0, "fixture response ended before expected marker");
        assert!(bytes.len() + count <= 65536, "fixture wire bound");
        bytes.extend_from_slice(&chunk[..count]);
    }
    bytes
}
async fn eof(stream: &mut (impl AsyncRead + Unpin)) {
    let mut byte = [0];
    match stream.read(&mut byte).await {
        Ok(0) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionReset | io::ErrorKind::UnexpectedEof
            ) => {}
        result => panic!("expected physical upstream close, got {result:?}"),
    }
}
fn reset(stream: TcpStream) {
    stream.set_zero_linger().unwrap();
    drop(stream);
}
async fn actual(scenario: Scenario) {
    actual_version(scenario, None).await;
}
async fn actual_version(scenario: Scenario, version: Option<openssl::ssl::SslVersion>) {
    let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!(
        "{}://{}",
        if version.is_some() { "https" } else { "http" },
        provider.local_addr().unwrap()
    );
    let mut config = autorouter_core::config::read_config(
        &serde_json::json!({
            "AUTOROUTER_EVALUATOR":"jev", "TYPESAFE_API_KEY":"synthetic-evaluator",
            "ANTHROPIC_API_KEY":"synthetic-provider", "AUTOROUTER_TOKEN":TOKEN,
            "AUTOROUTER_UPSTREAM_URL":origin,"AUTOROUTER_JEV_URL":format!("{origin}/v1/systemone"),
        }),
        true,
        std::path::Path::new("/synthetic"),
    )
    .unwrap();
    if matches!(scenario, Scenario::DeadlineHead | Scenario::DeadlineBody) {
        config.upstream_timeout_ms = 1000;
    }
    let (trust, tls) = isolated_tls(version);
    let client = Arc::new(SpikeHttpClient::with_snapshot_aborts(true, &trust, true).unwrap());
    let counts = client.raw_counts.clone();
    let probe = Probe::default();
    if matches!(scenario, Scenario::DeferredEof | Scenario::Reassigned) {
        probe.hold_bodies();
    }
    let gateway = Gateway::with_test_intent(
        Gateway::new(config, client.clone(), EventSinks::default()).unwrap(),
        probe.clone(),
    );
    let running = gateway.listen(0).await.unwrap();
    let address = running.address;
    let observed = probe.clone();
    let counted = counts.clone();
    let cache = client.raw_sessions.as_ref().unwrap().clone();
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        let mut downstream = TcpStream::connect(address).await.unwrap();
        downstream.write_all(format!("GET /v1/models/session-0 HTTP/1.1\r\nHost: synthetic\r\nX-Api-Key: {TOKEN}\r\n\r\n").as_bytes()).await.unwrap();
        let mut upstream = accept_peer(&provider, tls.as_ref()).await;
        assert!(!upstream.resumed());
        let head = through(&mut upstream, b"\r\n\r\n").await;
        assert!(head.starts_with(b"GET /v1/models/session-0 HTTP/1.1\r\n"));
        if matches!(scenario, Scenario::DeferredEof) {
            prefix(&mut upstream).await;
            observed.wait(|rows| rows.iter().any(|r| r.event == Event::BodyHeld)).await;
            let intent = observed.intent(0).unwrap();
            upstream.shutdown().await.unwrap();
            // This is an observed stage boundary, not a claim that a sent FIN
            // has been read. The original failed 1s hypothesis is retained.
            assert_eq!(intent.transport(), Transport::Live);
            assert!(!observed.rows().iter().any(|r| matches!(r.event, Event::Terminal(_))), "body failure was polled through hold gate");
            observed.release_bodies();
            observed.wait(|rows| rows.iter().any(|r| r.event == Event::Terminal(Cause::UpstreamFailure))).await;
            assert!(matches!(intent.transport(), Transport::Ordinary | Transport::Missing));
            reset(downstream);
            eof(&mut upstream).await;
            assert_eq!(counted.shutdown_calls.load(Ordering::SeqCst), 0);
            drop(intent);
        } else if matches!(scenario, Scenario::Reassigned) {
            upstream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\n\r\nDONE").await.unwrap();
            observed.wait(|rows| rows.iter().any(|r| r.id == 0 && r.event == Event::BodyHeld)).await;
            let mut second = TcpStream::connect(address).await.unwrap();
            second.write_all(format!("GET /v1/models/session-1 HTTP/1.1\r\nHost: synthetic\r\nX-Api-Key: {TOKEN}\r\n\r\n").as_bytes()).await.unwrap();
            let head = through(&mut upstream, b"\r\n\r\n").await;
            assert!(head.starts_with(b"GET /v1/models/session-1 HTTP/1.1\r\n"), "B did not acquire A's actual connection");
            reset(downstream);
            observed.wait(|rows| rows.iter().any(|r| r.id == 0 && r.event == Event::Action(Action::Retired))).await;
            assert_eq!(counted.shutdown_calls.load(Ordering::SeqCst), 0);
            upstream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\n\r\nDONE").await.unwrap();
            observed.release_bodies(); through(&mut second, b"DONE").await;
        } else if matches!(scenario, Scenario::Sibling) {
            prefix(&mut upstream).await; through(&mut downstream, b"partial").await;
            let mut second = TcpStream::connect(address).await.unwrap();
            second.write_all(format!("GET /v1/models/session-1 HTTP/1.1\r\nHost: synthetic\r\nX-Api-Key: {TOKEN}\r\n\r\n").as_bytes()).await.unwrap();
            let mut sibling = accept_peer(&provider, tls.as_ref()).await;
            through(&mut sibling, b"\r\n\r\n").await;
            reset(downstream);
            observed.wait(|rows| rows.iter().any(|r| r.id == 0 && r.event == Event::Action(Action::Requested))).await;
            eof(&mut upstream).await;
            sibling.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\n\r\nDONE").await.unwrap();
            through(&mut second, b"DONE").await;
            assert_eq!(counted.shutdown_calls.load(Ordering::SeqCst), 1);
        } else if matches!(scenario, Scenario::CompleteFin | Scenario::NoBodyFin) {
            if matches!(scenario, Scenario::NoBodyFin) {
                upstream.write_all(b"HTTP/1.1 204 No Content\r\n\r\n").await.unwrap();
                through(&mut downstream, b"\r\n\r\n").await;
            } else {
                upstream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\n\r\nDONE").await.unwrap();
                through(&mut downstream, b"DONE").await;
            }
            observed.wait(|rows| rows.iter().any(|r| r.event == Event::Terminal(Cause::Delivered))).await;
            downstream.shutdown().await.unwrap();
        } else if matches!(scenario, Scenario::InvalidHead | Scenario::UpstreamReset) {
            if matches!(scenario, Scenario::InvalidHead) {
                upstream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 1\r\ncontent-length: 1\r\n\r\nx").await.unwrap();
                upstream.shutdown().await.unwrap();
            } else { upstream.reset(); }
            let response = through(&mut downstream, b"\r\n\r\n").await;
            assert!(response.starts_with(b"HTTP/1.1 502"));
            observed.wait(|rows| rows.iter().any(|r| r.event == Event::Terminal(Cause::UpstreamFailure))).await;
            downstream.shutdown().await.unwrap();
        } else if matches!(scenario, Scenario::ShortBody | Scenario::FailureThenReset) {
            prefix(&mut upstream).await; through(&mut downstream, b"partial").await;
            upstream.shutdown().await.unwrap();
            observed.wait(|rows| rows.iter().any(|r| r.event == Event::Terminal(Cause::UpstreamFailure))).await;
            if matches!(scenario, Scenario::FailureThenReset) { reset(downstream); }
            eof(&mut upstream).await;
            assert!(observed.rows().iter().any(|r| matches!(r.event, Event::Transport(Transport::Ordinary | Transport::Missing))), "upstream-first must carry positive closed-transport evidence");
        } else {
            if matches!(scenario, Scenario::FinBody | Scenario::ResetBody | Scenario::CancelThenFailure | Scenario::ResumeChain | Scenario::DeadlineBody) {
                prefix(&mut upstream).await; through(&mut downstream, b"partial").await;
            }
            if matches!(scenario, Scenario::WriteReset) { observed.hold_reads(); }
            if matches!(scenario, Scenario::DeadlineHead | Scenario::DeadlineBody) {
                tokio::time::pause();
                tokio::time::advance(Duration::from_millis(1001)).await;
                tokio::time::resume();
            } else if matches!(scenario, Scenario::FinHead | Scenario::FinBody) { downstream.shutdown().await.unwrap(); }
            else { reset(downstream); }
            if matches!(scenario, Scenario::WriteReset) { prefix(&mut upstream).await; }
            observed.wait(|rows| rows.iter().any(|r| r.event == Event::Action(Action::Requested))).await;
            if matches!(scenario, Scenario::CancelThenFailure) { upstream.shutdown().await.unwrap(); }
            tokio::time::timeout(Duration::from_secs(1), eof(&mut upstream)).await.expect("gateway intent did not physically close selected peer");
            assert_eq!(counted.shutdown_calls.load(Ordering::SeqCst), 1);
            let expected = if matches!(scenario, Scenario::FinHead | Scenario::FinBody) { Disconnect::ReadEof }
                else if matches!(scenario, Scenario::WriteReset) { Disconnect::WriteError } else { Disconnect::ReadError };
            let cause = if matches!(scenario, Scenario::DeadlineHead | Scenario::DeadlineBody) { Cause::Deadline } else { Cause::Downstream(expected) };
            assert!(observed.rows().iter().any(|r| r.event == Event::Terminal(cause)), "actual IO/deadline cause differs from stimulus");
            if matches!(scenario, Scenario::ResumeChain) {
                assert_eq!(cache.lock().unwrap().snapshot().0, 0, "cancelled TLS ticket remained cached");
                for (id, resumed) in [(1, false), (2, true)] {
                    let mut next = TcpStream::connect(address).await.unwrap();
                    next.write_all(format!("GET /v1/models/session-{id} HTTP/1.1\r\nHost: synthetic\r\nX-Api-Key: {TOKEN}\r\n\r\n").as_bytes()).await.unwrap();
                    let mut peer = accept_peer(&provider, tls.as_ref()).await;
                    assert_eq!(peer.resumed(), resumed, "server-counted full/full/resumed chain");
                    through(&mut peer, b"\r\n\r\n").await;
                    peer.write_all(b"HTTP/1.1 200 OK\r\nconnection: close\r\ncontent-length: 4\r\n\r\nDONE").await.unwrap();
                    through(&mut next, b"DONE").await;
                    observed.wait(|rows| rows.iter().any(|r| r.id == id && r.event == Event::Terminal(Cause::Delivered))).await;
                    eof(&mut peer).await;
                    drop(peer); drop(next);
                    while counted.active.load(Ordering::SeqCst) != 0 { tokio::task::yield_now().await; }
                    assert_eq!(cache.lock().unwrap().snapshot().0, 1);
                }
                assert_eq!(counted.shutdown_calls.load(Ordering::SeqCst), 1);
            }

        }
        observed.release_reads();
        observed.release_bodies();
    });
    let result = tokio::time::timeout(LIMIT, tasks.join_next()).await;
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    probe.release_reads();
    probe.release_bodies();
    tokio::time::timeout(LIMIT, running.close())
        .await
        .expect("gateway task cleanup");
    drop(gateway);
    drop(client);
    tokio::time::timeout(LIMIT, async {
        while counts.active.load(Ordering::SeqCst) != 0
            || counts.tasks.load(Ordering::SeqCst) != 0
            || counts.shutdown_handles.load(Ordering::SeqCst) != 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("upstream task/socket cleanup");
    assert_eq!(probe.live(), 0, "intent registry ownership leak");
    eprintln!("intent scenario {scenario:?}: {:?}", probe.rows());
    result.expect("intent scenario deadline").unwrap().unwrap();
    if matches!(
        scenario,
        Scenario::CompleteFin
            | Scenario::NoBodyFin
            | Scenario::ShortBody
            | Scenario::InvalidHead
            | Scenario::UpstreamReset
            | Scenario::FailureThenReset
    ) {
        assert_eq!(counts.shutdown_calls.load(Ordering::SeqCst), 0);
    }
}
macro_rules! actual_case {
    ($name:ident, $scenario:ident) => {
        #[tokio::test]
        async fn $name() {
            actual(Scenario::$scenario).await;
        }
    };
}
actual_case!(get_complete_then_fin, CompleteFin);
actual_case!(get_fin_before_headers, FinHead);
actual_case!(get_fin_mid_body, FinBody);
actual_case!(get_reset_before_headers, ResetHead);
actual_case!(get_reset_mid_body, ResetBody);
actual_case!(get_paused_read_reset_write, WriteReset);
actual_case!(get_upstream_short_body, ShortBody);
actual_case!(get_upstream_invalid_head, InvalidHead);
actual_case!(get_upstream_reset_before_headers, UpstreamReset);
actual_case!(get_upstream_failure_then_reset, FailureThenReset);
actual_case!(get_cancel_then_upstream_failure, CancelThenFailure);

actual_case!(get_unpolled_body_defers_eof_until_released, DeferredEof);
actual_case!(
    get_old_buffered_body_cannot_abort_reassigned_socket,
    Reassigned
);
actual_case!(get_cancellation_preserves_same_origin_sibling, Sibling);

#[tokio::test]
async fn tls12_gateway_cancel_full_full_resumed() {
    actual_version(
        Scenario::ResumeChain,
        Some(openssl::ssl::SslVersion::TLS1_2),
    )
    .await;
}
#[tokio::test]
async fn tls13_gateway_cancel_full_full_resumed() {
    actual_version(
        Scenario::ResumeChain,
        Some(openssl::ssl::SslVersion::TLS1_3),
    )
    .await;
}
#[tokio::test]
async fn tls12_gateway_old_body_cannot_abort_reassigned_socket() {
    actual_version(Scenario::Reassigned, Some(openssl::ssl::SslVersion::TLS1_2)).await;
}
#[tokio::test]
async fn tls13_gateway_old_body_cannot_abort_reassigned_socket() {
    actual_version(Scenario::Reassigned, Some(openssl::ssl::SslVersion::TLS1_3)).await;
}
#[tokio::test]
async fn tls12_gateway_abort_preserves_sibling() {
    actual_version(Scenario::Sibling, Some(openssl::ssl::SslVersion::TLS1_2)).await;
}
#[tokio::test]
async fn tls13_gateway_abort_preserves_sibling() {
    actual_version(Scenario::Sibling, Some(openssl::ssl::SslVersion::TLS1_3)).await;
}

enum Peer {
    Tcp(TcpStream),
    Tls(hyper_util::rt::TokioIo<hyper_openssl::SslStream<hyper_util::rt::TokioIo<TcpStream>>>),
}
impl Peer {
    fn resumed(&self) -> bool {
        match self {
            Self::Tcp(_) => false,
            Self::Tls(stream) => stream.inner().ssl().session_reused(),
        }
    }
    fn reset(self) {
        match &self {
            Self::Tcp(stream) => stream.set_zero_linger().unwrap(),
            Self::Tls(stream) => stream.inner().get_ref().inner().set_zero_linger().unwrap(),
        }
        drop(self);
    }
}
async fn accept_peer(listener: &TcpListener, context: Option<&openssl::ssl::SslContext>) -> Peer {
    let (stream, _) = listener.accept().await.unwrap();
    if let Some(context) = context {
        let mut stream = hyper_openssl::SslStream::new(
            openssl::ssl::Ssl::new(context).unwrap(),
            hyper_util::rt::TokioIo::new(stream),
        )
        .unwrap();
        Pin::new(&mut stream).accept().await.unwrap();
        Peer::Tls(hyper_util::rt::TokioIo::new(stream))
    } else {
        Peer::Tcp(stream)
    }
}
impl AsyncRead for Peer {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}
impl AsyncWrite for Peer {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

actual_case!(get_no_body_delivery_then_fin, NoBodyFin);
actual_case!(get_deadline_before_headers, DeadlineHead);
actual_case!(get_deadline_after_headers, DeadlineBody);
