//! Immediate bounded HTTP/1 IO; gateway/client codecs remain the real ones.
use super::super::{TOKEN, service};
use bytes::Bytes;
use hyper::Uri;
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::rt::TokioIo;
use std::collections::{BTreeMap, VecDeque};
use std::future::{Ready, ready};
use std::io::{self, IoSlice};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio_util::sync::CancellationToken;
use tower_service::Service;

pub(super) const MAX_REQUEST: usize = 1024 * 1024;
pub(super) const MAX_RESPONSE: usize = 128 * 1024;
const CHUNK: usize = 8192;
#[derive(Clone)]
pub(super) struct Message {
    pub first: String,
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Bytes,
    pub consumed: usize,
}
fn invalid() -> io::Error {
    io::Error::other("Invalid bounded synthetic HTTP wire")
}
pub(super) fn parse(bytes: &[u8], request: bool) -> io::Result<Option<Message>> {
    if bytes.len() > if request { MAX_REQUEST } else { MAX_RESPONSE } {
        return Err(invalid());
    }
    let Some(offset) = bytes.windows(4).position(|value| value == b"\r\n\r\n") else {
        return Ok(None);
    };
    if offset > 16 * 1024 {
        return Err(invalid());
    }
    let text = std::str::from_utf8(&bytes[..offset]).map_err(|_| invalid())?;
    let mut lines = text.split("\r\n");
    let first = lines.next().ok_or_else(invalid)?.to_owned();
    let mut headers = BTreeMap::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or_else(invalid)?;
        if name.is_empty()
            || headers
                .insert(name.to_ascii_lowercase(), value.trim().to_owned())
                .is_some()
        {
            return Err(invalid());
        }
    }
    let start = offset + 4;
    let (body, consumed) = if headers
        .get("transfer-encoding")
        .is_some_and(|value| value == "chunked")
    {
        let mut body = Vec::new();
        let mut cursor = start;
        loop {
            let Some(size_end) = bytes[cursor..]
                .windows(2)
                .position(|value| value == b"\r\n")
            else {
                return Ok(None);
            };
            let size_text =
                std::str::from_utf8(&bytes[cursor..cursor + size_end]).map_err(|_| invalid())?;
            if size_text.is_empty() || !size_text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(invalid());
            }
            let size = usize::from_str_radix(size_text, 16).map_err(|_| invalid())?;
            cursor += size_end + 2;
            let end = cursor.checked_add(size).ok_or_else(invalid)?;
            let consumed = end.checked_add(2).ok_or_else(invalid)?;
            if bytes.len() < consumed {
                return Ok(None);
            }
            if &bytes[end..consumed] != b"\r\n" {
                return Err(invalid());
            }
            if size == 0 {
                break (Bytes::from(body), consumed);
            }
            body.extend_from_slice(&bytes[cursor..end]);
            cursor = consumed;
        }
    } else {
        let length = headers
            .get("content-length")
            .ok_or_else(invalid)?
            .parse::<usize>()
            .map_err(|_| invalid())?;
        let consumed = start.checked_add(length).ok_or_else(invalid)?;
        if consumed > if request { MAX_REQUEST } else { MAX_RESPONSE } {
            return Err(invalid());
        }
        if bytes.len() < consumed {
            return Ok(None);
        }
        (Bytes::copy_from_slice(&bytes[start..consumed]), consumed)
    };
    let status = if request {
        0
    } else {
        first
            .split(' ')
            .nth(1)
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?
    };
    for key in [
        "date",
        "host",
        "connection",
        "keep-alive",
        "content-length",
        "transfer-encoding",
    ] {
        headers.remove(key);
    }
    Ok(Some(Message {
        first,
        status,
        headers,
        body,
        consumed,
    }))
}
pub(super) fn provider_wire() -> Bytes {
    let mut bytes = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-Probe: synthetic\r\nConnection: keep-alive, x-hop\r\nX-Hop: removed\r\n\r\n", service::RESPONSE.len()).into_bytes();
    bytes.extend_from_slice(service::RESPONSE);
    Bytes::from(bytes)
}
pub(super) fn request_wire(bytes: &[u8], index: usize) -> Bytes {
    let mut wire = format!("POST /v1/messages?probe=whole HTTP/1.1\r\nHost: localhost\r\nX-Api-Key: {TOKEN}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-Claude-Code-Session-Id: synthetic-benchmark-session\r\nX-Claude-Code-Agent-Id: agent-0\r\nX-Claude-Code-Prompt-Id: synthetic-prompt-{index}\r\nX-Claude-Code-Request-Class: main\r\nX-Synthetic-Permission: preserve\r\nCookie: remove-synthetic-cookie\r\nConnection: keep-alive\r\n\r\n", bytes.len()).into_bytes();
    wire.extend_from_slice(bytes);
    Bytes::from(wire)
}
#[derive(Default)]
pub(super) struct ProviderState {
    pub records: Mutex<Vec<Message>>,
    pub live: AtomicUsize,
}
impl ProviderState {
    fn record(&self, request: Message) -> io::Result<()> {
        if request.first != "POST /v1/messages?probe=whole HTTP/1.1" {
            return Err(invalid());
        }
        let mut records = self.records.lock().unwrap();
        if !records.is_empty() {
            return Err(invalid());
        }
        records.push(request);
        Ok(())
    }
}
struct UpstreamIo {
    provider: Arc<ProviderState>,
    received: Vec<u8>,
    reply: Bytes,
    read_waker: Option<Waker>,
}
impl AsyncRead for UpstreamIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.reply.is_empty() {
            self.read_waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let count = output.remaining().min(CHUNK).min(self.reply.len());
        output.put_slice(&self.reply[..count]);
        self.reply = self.reply.slice(count..);
        Poll::Ready(Ok(()))
    }
}
impl AsyncWrite for UpstreamIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        if bytes.len() > MAX_REQUEST.saturating_sub(self.received.len()) {
            return Poll::Ready(Err(invalid()));
        }
        self.received.extend_from_slice(bytes);
        match parse(&self.received, true) {
            Err(error) => return Poll::Ready(Err(error)),
            Ok(None) => {}
            Ok(Some(request)) => {
                if request.consumed != self.received.len() || !self.reply.is_empty() {
                    return Poll::Ready(Err(invalid()));
                }
                if let Err(error) = self.provider.record(request) {
                    return Poll::Ready(Err(error));
                }
                self.received.clear();
                self.reply = provider_wire();
                if let Some(waker) = self.read_waker.take() {
                    waker.wake();
                }
            }
        }
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
impl Drop for UpstreamIo {
    fn drop(&mut self) {
        self.provider.live.fetch_sub(1, Ordering::SeqCst);
    }
}
pub(super) struct ConnectorIo(TokioIo<UpstreamIo>);
impl Connection for ConnectorIo {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}
impl hyper::rt::Read for ConnectorIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buffer)
    }
}
impl hyper::rt::Write for ConnectorIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
    fn is_write_vectored(&self) -> bool {
        false
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffers: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let bytes = buffers
            .iter()
            .find(|bytes| !bytes.is_empty())
            .map_or(&[][..], |bytes| &bytes[..]);
        Pin::new(&mut self.0).poll_write(cx, bytes)
    }
}
#[derive(Clone)]
pub(super) struct Connector(pub Arc<ProviderState>);
impl Service<Uri> for Connector {
    type Response = ConnectorIo;
    type Error = io::Error;
    type Future = Ready<io::Result<ConnectorIo>>;
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, uri: Uri) -> Self::Future {
        if uri.scheme_str() != Some("http")
            || uri.authority().map(|value| value.as_str()) != Some("127.0.0.1:1")
        {
            return ready(Err(invalid()));
        }
        self.0.live.fetch_add(1, Ordering::SeqCst);
        ready(Ok(ConnectorIo(TokioIo::new(UpstreamIo {
            provider: self.0.clone(),
            received: Vec::new(),
            reply: Bytes::new(),
            read_waker: None,
        }))))
    }
}

#[derive(Default)]
struct DownstreamState {
    incoming: VecDeque<Bytes>,
    incoming_bytes: usize,
    output: Vec<u8>,
    reader: Option<Waker>,
    closed: bool,
    flushes: usize,
    hold_flush: bool,
    writer: Option<Waker>,
}
#[derive(Clone, Default)]
pub(super) struct Downstream(Arc<Mutex<DownstreamState>>);
impl Downstream {
    pub fn feed(&self, bytes: Bytes) -> io::Result<()> {
        let mut state = self.0.lock().unwrap();
        if state.closed || bytes.len() > MAX_REQUEST.saturating_sub(state.incoming_bytes) {
            return Err(invalid());
        }
        state.incoming_bytes += bytes.len();
        state.incoming.push_back(bytes);
        let reader = state.reader.take();
        drop(state);
        if let Some(reader) = reader {
            reader.wake();
        }
        Ok(())
    }
    pub fn take_response(&self) -> io::Result<Message> {
        let bytes = std::mem::take(&mut self.0.lock().unwrap().output);
        let response = parse(&bytes, false)?.ok_or_else(invalid)?;
        if response.consumed != bytes.len() {
            return Err(invalid());
        }
        Ok(response)
    }
    pub fn flushes(&self) -> usize {
        self.0.lock().unwrap().flushes
    }
    pub fn hold_flush(&self) {
        self.0.lock().unwrap().hold_flush = true;
    }
    pub fn release_flush(&self) {
        let mut state = self.0.lock().unwrap();
        state.hold_flush = false;
        let writer = state.writer.take();
        drop(state);
        if let Some(writer) = writer {
            writer.wake();
        }
    }
}
impl AsyncRead for Downstream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut state = self.0.lock().unwrap();
        let Some(bytes) = state.incoming.front_mut() else {
            if state.closed {
                return Poll::Ready(Ok(()));
            }
            state.reader = Some(cx.waker().clone());
            return Poll::Pending;
        };
        let count = output.remaining().min(CHUNK).min(bytes.len());
        output.put_slice(&bytes[..count]);
        *bytes = bytes.slice(count..);
        if bytes.is_empty() {
            state.incoming.pop_front();
        }
        state.incoming_bytes -= count;
        Poll::Ready(Ok(()))
    }
}
impl AsyncWrite for Downstream {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut state = self.0.lock().unwrap();
        if state.closed || bytes.len() > MAX_RESPONSE.saturating_sub(state.output.len()) {
            return Poll::Ready(Err(invalid()));
        }
        state.output.extend_from_slice(bytes);
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut state = self.0.lock().unwrap();
        if state.hold_flush {
            state.writer = Some(cx.waker().clone());
            return Poll::Pending;
        }
        state.flushes += 1;
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.0.lock().unwrap().closed = true;
        Poll::Ready(Ok(()))
    }
}

pub(super) struct LoopbackProvider {
    pub address: std::net::SocketAddr,
    cancellation: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl LoopbackProvider {
    pub async fn start(provider: Arc<ProviderState>) -> io::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let cancellation = CancellationToken::new();
        let stopping = cancellation.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let accepted = tokio::select! { biased;
                    _ = stopping.cancelled() => break,
                    _ = connections.join_next(), if !connections.is_empty() => continue,
                    result = listener.accept() => result,
                };
                let Ok((mut stream, _)) = accepted else {
                    break;
                };
                let provider = provider.clone();
                let token = stopping.clone();
                connections.spawn(async move {
                    let mut received = Vec::new();
                    let mut chunk = [0; CHUNK];
                    loop {
                        let count = tokio::select! { biased; _ = token.cancelled() => return, count = stream.read(&mut chunk) => count };
                        let Ok(count) = count else { return; };
                        if count == 0 || count > MAX_REQUEST.saturating_sub(received.len()) { return; }
                        received.extend_from_slice(&chunk[..count]);
                        let request = match parse(&received, true) { Ok(Some(request)) => request, Ok(None) => continue, Err(_) => return };
                        if request.consumed != received.len() || provider.record(request).is_err() { return; }
                        received.clear();
                        if stream.write_all(&provider_wire()).await.is_err() { return; }
                    }
                });
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        Ok(Self {
            address,
            cancellation,
            task: Some(task),
        })
    }
    pub async fn close(mut self) {
        self.cancellation.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
impl Drop for LoopbackProvider {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn codecs_require_complete_bounded_wire_and_retain_exact_bytes_and_headers() {
        let wire = provider_wire();
        for length in 0..wire.len() {
            assert!(parse(&wire[..length], false).unwrap().is_none());
        }
        let parsed = parse(&wire, false).unwrap().unwrap();
        assert_eq!(parsed.status, 200);
        assert_eq!(parsed.body.as_ref(), service::RESPONSE);
        assert_eq!(parsed.headers["x-probe"], "synthetic");
        assert_eq!(parsed.headers["x-hop"], "removed");
        assert!(parse(&vec![b'x'; MAX_RESPONSE + 1], false).is_err());
        assert!(
            parse(
                b"HTTP/1.1 200 OK\r\ncontent-length: 1\r\ncontent-length: 1\r\n\r\nx",
                false
            )
            .is_err()
        );
        assert!(
            parse(
                b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\nffffffffffffffff\r\n",
                false
            )
            .is_err()
        );
    }
    #[tokio::test]
    async fn synthetic_connector_cannot_open_arbitrary_origins_or_accept_other_endpoints() {
        let mut connector = Connector(Arc::new(ProviderState::default()));
        for uri in [
            "http://example.invalid:1",
            "https://127.0.0.1:1",
            "http://127.0.0.1:2",
        ] {
            assert!(connector.call(uri.parse().unwrap()).await.is_err());
        }
        let mut socket = connector
            .call("http://127.0.0.1:1".parse().unwrap())
            .await
            .unwrap();
        let socket = &mut socket.0;
        // TokioIo delegates the same codec; no forbidden request is recorded.
        assert!(
            socket
                .inner_mut()
                .write_all(b"POST /forbidden HTTP/1.1\r\ncontent-length: 0\r\n\r\n")
                .await
                .is_err()
        );
        assert!(connector.0.records.lock().unwrap().is_empty());
    }
}
