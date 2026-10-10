//! Separate test-only raw Agent. Unique senders follow consumer body ownership.
//! Idle retirement never implies delivery, explicit abort, or a TLS cache error.
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicUsize, Ordering},
};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::client::conn::http1::SendRequest;
use hyper::{Request, Response, Uri};
use hyper_util::client::legacy::connect::{
    Connected, Connection, Http1Reservation, bind_http1_reservation, capture_http1_assignment,
};
use tokio::sync::oneshot;
use tokio::task::AbortHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tower_service::Service;

use super::client::{Connector, Counts, SpikeHttpClient, raw_pool_connector};
use super::gateway_intent::Intent;
use super::policy::TrustSnapshot;
use super::session::{Cache, RawCache};
use crate::http_client::{HttpError, HttpTransport};

const FREE_LIMIT: usize = 256;
const DEFAULT_TIMEOUT_MS: u64 = 5000;

type Trace = serde_json::Value;
#[derive(Default)]
struct ProbeState {
    rows: Vec<Trace>,
    requested_holds: std::collections::BTreeSet<usize>,
    gates: BTreeMap<usize, Weak<Gate>>,
}
#[derive(Clone)]
pub(super) struct Probe {
    state: Arc<Mutex<ProbeState>>,
    start: Instant,
}
impl Default for Probe {
    fn default() -> Self {
        Self {
            state: Arc::default(),
            start: Instant::now(),
        }
    }
}
impl Probe {
    fn record(&self, event: &str, mut fields: Trace) {
        fields["event"] = event.into();
        fields["ms"] = u64::try_from(self.start.elapsed().as_millis())
            .unwrap()
            .into();
        let mut state = self.state.lock().unwrap();
        assert!(state.rows.len() < 1024, "raw pool trace bound");
        state.rows.push(fields);
    }
    pub(super) fn hold(&self, request: usize) {
        assert!(self.state.lock().unwrap().requested_holds.insert(request));
    }
    pub(super) fn release(&self, request: usize) {
        let gate = self
            .state
            .lock()
            .unwrap()
            .gates
            .get(&request)
            .and_then(Weak::upgrade)
            .expect("owned response gate");
        gate.release();
    }
    pub(super) fn release_all(&self) {
        let gates: Vec<_> = self
            .state
            .lock()
            .unwrap()
            .gates
            .values()
            .filter_map(Weak::upgrade)
            .collect();
        for gate in gates {
            gate.release();
        }
    }
    fn gate(&self, request: usize) -> Arc<Gate> {
        let mut state = self.state.lock().unwrap();
        let gate = Arc::new(Gate(Mutex::new(GateState {
            held: state.requested_holds.contains(&request),
            waker: None,
        })));
        state.gates.insert(request, Arc::downgrade(&gate));
        gate
    }
    fn snapshot(&self) -> (Vec<Trace>, Vec<usize>) {
        let (rows, gates) = {
            let state = self.state.lock().unwrap();
            (
                state.rows.clone(),
                state
                    .gates
                    .iter()
                    .map(|(id, gate)| (*id, gate.clone()))
                    .collect::<Vec<_>>(),
            )
        };
        let held = gates
            .into_iter()
            .filter_map(|(id, weak)| {
                weak.upgrade()
                    .filter(|gate| gate.0.lock().unwrap().held)
                    .map(|_| id)
            })
            .collect();
        (rows, held)
    }
}
struct GateState {
    held: bool,
    waker: Option<Waker>,
}
struct Gate(Mutex<GateState>);
impl Gate {
    fn poll(&self, cx: &mut Context<'_>) -> bool {
        let waker = cx.waker().clone();
        let (held, old) = {
            let mut state = self.0.lock().unwrap();
            if state.held {
                (true, state.waker.replace(waker))
            } else {
                (false, Some(waker))
            }
        };
        drop(old);
        !held
    }
    fn release(&self) {
        let old = {
            let mut state = self.0.lock().unwrap();
            state.held = false;
            state.waker.take()
        };
        if let Some(waker) = old {
            waker.wake();
        }
    }
}

/// Node's leading decimal hint uses parseInt, not a strict whole-string number.
/// Values >= 6 seconds, including unrepresentably large inputs, use the default.
fn idle_timeout(headers: &hyper::HeaderMap) -> Option<Duration> {
    let mut hint = Vec::new();
    for (index, value) in headers.get_all("keep-alive").iter().enumerate() {
        if index != 0 {
            hint.extend_from_slice(b", ");
        }
        hint.extend_from_slice(value.as_bytes());
    }
    let Some(mut bytes) = hint.strip_prefix(b"timeout=") else {
        return Some(Duration::from_millis(DEFAULT_TIMEOUT_MS));
    };
    if !bytes.first().is_some_and(u8::is_ascii_digit) {
        return Some(Duration::from_millis(DEFAULT_TIMEOUT_MS));
    }
    let mut seconds = 0_u64;
    while let Some(byte) = bytes.first().filter(|byte| byte.is_ascii_digit()) {
        seconds = (seconds * 10 + u64::from(*byte - b'0')).min(6);
        bytes = &bytes[1..];
    }
    if seconds <= 1 {
        None
    } else {
        Some(Duration::from_secs(seconds.min(6) - 1))
    }
}

struct OwnedTask(Option<AbortHandle>);
impl OwnedTask {
    /// The task registry retains the other cancellation handle until task exit.
    fn disarm(mut self) {
        drop(self.0.take());
    }
}
impl Drop for OwnedTask {
    fn drop(&mut self) {
        if let Some(handle) = &self.0 {
            handle.abort();
        }
    }
}
struct ActivityState {
    last: Instant,
    generation: usize,
    busy: bool,
    fired: bool,
    timeout: Duration,
}
struct Activity {
    state: Mutex<ActivityState>,
    changed: tokio::sync::Notify,
}
impl Activity {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ActivityState {
                last: Instant::now(),
                generation: 0,
                busy: true,
                fired: false,
                timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
            }),
            changed: tokio::sync::Notify::new(),
        })
    }
    fn touch(&self) {
        {
            let mut state = self.state.lock().unwrap();
            if !state.busy {
                return;
            }
            state.last = Instant::now();
            state.generation += 1;
            state.fired = false;
        }
        self.changed.notify_one();
    }
    fn idle_state(&self, timeout: Duration) {
        {
            let mut state = self.state.lock().unwrap();
            state.timeout = timeout;
            state.busy = false;
            state.last = Instant::now();
            state.generation += 1;
            state.fired = false;
        }
    }
    fn idle(&self, timeout: Duration) {
        self.idle_state(timeout);
        self.changed.notify_one();
    }
    fn timeout_ms(&self) -> u64 {
        u64::try_from(self.state.lock().unwrap().timeout.as_millis()).unwrap()
    }
    fn busy(&self, busy: bool) {
        {
            let mut state = self.state.lock().unwrap();
            state.busy = busy;
            state.last = Instant::now();
            state.generation += 1;
            state.fired = false;
        }
        self.changed.notify_one();
    }
    async fn watch(self: Arc<Self>, id: usize, probe: Probe) {
        loop {
            let observed = {
                let state = self.state.lock().unwrap();
                (state.busy && !state.fired)
                    .then_some((state.last + state.timeout, state.generation))
            };
            let Some((deadline, generation)) = observed else {
                self.changed.notified().await;
                continue;
            };
            tokio::select! {
                _ = self.changed.notified() => continue,
                _ = tokio::time::sleep_until(deadline) => {}
            }
            let fired = {
                let mut state = self.state.lock().unwrap();
                if state.busy && !state.fired && state.generation == generation {
                    state.fired = true;
                    true
                } else {
                    false
                }
            };
            if fired {
                probe.record(
                    "timeout",
                    serde_json::json!({"connection":id,"free":false,"destroyed":false}),
                );
            }
        }
    }
}
struct ActivityIo<T> {
    io: T,
    activity: Arc<Activity>,
}
impl<T: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for ActivityIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.io).poll_read(cx, buf);
        if buf.filled().len() > before {
            self.activity.touch();
        }
        result
    }
}
impl<T: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for ActivityIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let result = Pin::new(&mut self.io).poll_write(cx, buf);
        if matches!(result, Poll::Ready(Ok(n)) if n > 0) {
            self.activity.touch();
        }
        result
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let result = Pin::new(&mut self.io).poll_write_vectored(cx, bufs);
        if matches!(result, Poll::Ready(Ok(n)) if n > 0) {
            self.activity.touch();
        }
        result
    }
    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

struct Entry {
    id: usize,
    sender: SendRequest<Full<Bytes>>,
    connected: Connected,
    _driver: OwnedTask,
    activity: Arc<Activity>,
    _inactivity: OwnedTask,
}
struct Idle {
    entry: Entry,
    generation: usize,
    deadline: Instant,
    timeout_ms: u64,
    _timer: OwnedTask,
}
#[derive(Default)]
struct State {
    idle: BTreeMap<String, Vec<Idle>>,
    live: BTreeMap<usize, Weak<Activity>>,
}
struct Shared {
    state: Mutex<State>,
    tasks: Mutex<BTreeMap<usize, AbortHandle>>,
    sequence: AtomicUsize,
    connections: AtomicUsize,
    requests: AtomicUsize,
    connector: Connector,
    counts: Arc<Counts>,
    stop: CancellationToken,
    probe: Probe,
}
impl Drop for Shared {
    fn drop(&mut self) {
        self.stop.cancel();
        let handles = std::mem::take(self.tasks.get_mut().unwrap_or_else(|e| e.into_inner()));
        let idle =
            std::mem::take(&mut self.state.get_mut().unwrap_or_else(|e| e.into_inner()).idle);
        for handle in handles.into_values() {
            handle.abort();
        }
        drop(idle);
    }
}
struct TaskLease {
    shared: Weak<Shared>,
    counts: Arc<Counts>,
    id: usize,
}
impl Drop for TaskLease {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            let removed = shared
                .tasks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&self.id);
            drop(removed);
        }
        self.counts.tasks.fetch_sub(1, Ordering::SeqCst);
    }
}
fn spawn_owned(shared: &Arc<Shared>, work: impl Future<Output = ()> + Send + 'static) -> OwnedTask {
    let id = shared.sequence.fetch_add(1, Ordering::SeqCst);
    let (start, started) = oneshot::channel();
    shared.counts.tasks.fetch_add(1, Ordering::SeqCst);
    let lease = TaskLease {
        shared: Arc::downgrade(shared),
        counts: shared.counts.clone(),
        id,
    };
    let task = tokio::spawn(async move {
        let _lease = lease;
        if started.await.is_ok() {
            work.await;
        }
    });
    let handle = task.abort_handle();
    let old = shared.tasks.lock().unwrap().insert(id, handle.clone());
    assert!(old.is_none());
    let _ = start.send(());
    OwnedTask(Some(handle))
}
struct ConnectionLease {
    shared: Weak<Shared>,
    id: usize,
}
impl Drop for ConnectionLease {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            shared.closed(self.id);
        }
    }
}
impl Shared {
    fn closed(&self, id: usize) {
        let removed = {
            let mut state = self.state.lock().unwrap();
            state.live.remove(&id);
            let key = state.idle.iter().find_map(|(key, entries)| {
                entries
                    .iter()
                    .any(|idle| idle.entry.id == id)
                    .then(|| key.clone())
            });
            key.map(|key| {
                let list = state.idle.get_mut(&key).unwrap();
                let position = list.iter().position(|idle| idle.entry.id == id).unwrap();
                let removed = list.remove(position);
                if list.is_empty() {
                    state.idle.remove(&key);
                }
                removed
            })
        };
        drop(removed);
        self.probe
            .record("close", serde_json::json!({"connection":id}));
    }
    async fn acquire(self: &Arc<Self>, uri: &Uri, key: &str) -> Result<(Entry, bool), HttpError> {
        loop {
            let idle = {
                let mut state = self.state.lock().unwrap();
                let idle = state.idle.get_mut(key).and_then(Vec::pop);
                if state.idle.get(key).is_some_and(Vec::is_empty) {
                    state.idle.remove(key);
                }
                idle
            };
            let Some(idle) = idle else {
                break;
            };
            let Idle {
                entry,
                deadline,
                _timer,
                ..
            } = idle;
            drop(_timer);
            if deadline > Instant::now() && entry.sender.is_ready() {
                entry.activity.busy(true);
                return Ok((entry, true));
            }
            drop(entry);
        }
        let mut connector = self.connector.clone();
        let stream = connector
            .call(uri.clone())
            .await
            .map_err(|_| HttpError::Network)?;
        let connected = stream.inner().connected();
        let activity = Activity::new();
        let stream = hyper_util::rt::TokioIo::new(ActivityIo {
            io: stream.into_inner(),
            activity: activity.clone(),
        });
        let (sender, connection) = hyper::client::conn::http1::Builder::new()
            .handshake(stream)
            .await
            .map_err(|_| HttpError::Network)?;
        let id = self.connections.fetch_add(1, Ordering::SeqCst) + 1;
        assert!(
            self.state
                .lock()
                .unwrap()
                .live
                .insert(id, Arc::downgrade(&activity))
                .is_none()
        );
        let lease = ConnectionLease {
            shared: Arc::downgrade(self),
            id,
        };
        let driver = spawn_owned(self, async move {
            // Guard also runs when the driver is aborted before its first poll.
            let _lease = lease;
            let _ = connection.await;
        });
        let inactivity = spawn_owned(self, activity.clone().watch(id, self.probe.clone()));
        Ok((
            Entry {
                id,
                sender,
                connected,
                _driver: driver,
                activity,
                _inactivity: inactivity,
            },
            false,
        ))
    }
    fn give_back(self: &Arc<Self>, key: String, entry: Entry, timeout: Option<Duration>) {
        // responseKeepAlive clears the completed response's timer before free
        // admission. Active/held consumers never take this transition.
        entry.activity.idle(Duration::ZERO);
        let Some(timeout) = timeout else {
            // The cap guard precedes keepSocketAlive's hint policy. In the
            // uncapped veto path it applies the default before rejecting reuse.
            let capped = self
                .state
                .lock()
                .unwrap()
                .idle
                .get(&key)
                .is_some_and(|entries| entries.len() >= FREE_LIMIT);
            let timeout_ms = if capped { 0 } else { DEFAULT_TIMEOUT_MS };
            entry.activity.idle(Duration::from_millis(timeout_ms));
            self.probe.record(
                "free",
                serde_json::json!({"connection":entry.id,"kept":false,"timeout":timeout_ms}),
            );
            drop(entry);
            return;
        };
        if self.stop.is_cancelled() || !entry.sender.is_ready() {
            drop(entry);
            return;
        }
        let generation = self.sequence.fetch_add(1, Ordering::SeqCst);
        let deadline = Instant::now() + timeout;
        let id = entry.id;
        let (inserted, ready) = oneshot::channel();
        let weak = Arc::downgrade(self);
        let expired_key = key.clone();
        let timer = spawn_owned(self, async move {
            if ready.await.is_err() {
                return;
            }
            tokio::time::sleep_until(deadline).await;
            if let Some(shared) = weak.upgrade() {
                let removed = {
                    let mut state = shared.state.lock().unwrap();
                    let removed = state.idle.get_mut(&expired_key).and_then(|entries| {
                        entries
                            .iter()
                            .position(|idle| idle.entry.id == id && idle.generation == generation)
                            .map(|position| entries.remove(position))
                    });
                    if state.idle.get(&expired_key).is_some_and(Vec::is_empty) {
                        state.idle.remove(&expired_key);
                    }
                    removed
                };
                if removed.is_some() {
                    shared
                        .probe
                        .record("timeout", serde_json::json!({"connection":id,"free":true}));
                }
                drop(removed);
            }
        });
        let timeout_ms = u64::try_from(timeout.as_millis()).unwrap();
        let idle = Idle {
            entry,
            generation,
            deadline,
            timeout_ms,
            _timer: timer,
        };
        let rejected = self.insert_idle(key, idle);
        let kept = rejected.is_none();
        // A cap rejection never arms an idle socket timer. Its response has
        // finished and the owned connection is disposed immediately afterward.
        if let Some(rejected) = &rejected {
            rejected.entry.activity.idle(Duration::ZERO);
        }
        self.probe.record(
            "free",
            serde_json::json!({"connection":id,"kept":kept,"timeout":if kept { timeout_ms } else { 0 }}),
        );
        if kept {
            let _ = inserted.send(());
        }
        drop(rejected);
    }
    fn insert_idle(&self, key: String, idle: Idle) -> Option<Idle> {
        let mut state = self.state.lock().unwrap();
        // Driver retirement and insertion share this lock: a close after the
        // initial readiness check cannot strand a dead entry in the free map.
        if !state.live.contains_key(&idle.entry.id)
            || !idle.entry.sender.is_ready()
            || state
                .idle
                .get(&key)
                .is_some_and(|entries| entries.len() >= FREE_LIMIT)
        {
            return Some(idle);
        }
        // No waker/callback here: the observer is already inactive. The owned
        // idle timer starts only after insertion releases this lock.
        idle.entry
            .activity
            .idle_state(Duration::from_millis(idle.timeout_ms));
        state.idle.entry(key).or_default().push(idle);
        None
    }

    fn complete(
        self: &Arc<Self>,
        key: String,
        mut entry: Entry,
        reservation: Option<Http1Reservation>,
        timeout: Option<Duration>,
        clean: bool,
    ) {
        let unpoisoned = reservation.is_none_or(Http1Reservation::retire);
        if !clean || !unpoisoned || entry.sender.is_closed() {
            drop(entry);
            return;
        }
        // Consumer end clears the busy timer even when dispatch readiness
        // needs one more driver turn before the entry can join the free list.
        entry.activity.idle(Duration::ZERO);
        if entry.sender.is_ready() {
            self.give_back(key, entry, timeout);
            return;
        }
        let weak = Arc::downgrade(self);
        let stop = self.stop.clone();
        // Registry owns this short readiness task until it completes or shutdown.
        let task = spawn_owned(self, async move {
            let ready = tokio::select! { biased; _ = stop.cancelled() => false, result = entry.sender.ready() => result.is_ok() };
            if ready && let Some(shared) = weak.upgrade() {
                shared.give_back(key, entry, timeout);
            }
        });
        // The registry retains cancellation ownership; this local handle must not abort it.
        task.disarm();
    }
}

type Completion = Box<dyn FnOnce(bool) + Send>;
pub(super) struct OwnedBody<B: Body> {
    inner: Pin<Box<B>>,
    completion: Option<Completion>,
    gate: Option<Arc<Gate>>,
}
impl<B: Body> OwnedBody<B> {
    fn new(body: B, completion: Option<Completion>, gate: Option<Arc<Gate>>) -> Self {
        let empty = body.is_end_stream();
        let held = gate
            .as_ref()
            .is_some_and(|gate| gate.0.lock().unwrap().held);
        let mut owned = Self {
            inner: Box::pin(body),
            completion,
            gate,
        };
        // Explicit empty-response handoff: no body frame exists to consume.
        // Held consumers cannot take this path, even if the decoder knows EOF.
        if empty && !held {
            owned.complete(true);
        }
        owned
    }
    fn complete(&mut self, clean: bool) {
        if let Some(callback) = self.completion.take() {
            callback(clean);
        }
    }
}
impl<B: Body> Body for OwnedBody<B> {
    type Data = B::Data;
    type Error = B::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if this.gate.as_ref().is_some_and(|gate| !gate.poll(cx)) {
            return Poll::Pending;
        }
        let result = this.inner.as_mut().poll_frame(cx);
        match &result {
            Poll::Ready(None) => this.complete(true),
            Poll::Ready(Some(Err(_))) => this.complete(false),
            Poll::Ready(Some(Ok(_))) if this.inner.is_end_stream() => this.complete(true),
            _ => {}
        }
        result
    }
    fn is_end_stream(&self) -> bool {
        self.completion.is_none()
            && !self
                .gate
                .as_ref()
                .is_some_and(|gate| gate.0.lock().unwrap().held)
            && self.inner.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
impl<B: Body> Drop for OwnedBody<B> {
    fn drop(&mut self) {
        // Drop is cancellation, never consumer completion or forwarding proof.
        self.complete(false);
    }
}

pub(super) struct RawPoolClient {
    shared: Arc<Shared>,
    fallback: SpikeHttpClient,
    pub(super) raw_counts: Arc<Counts>,
    pub(super) fetch_counts: Arc<Counts>,
    pub(super) raw_sessions: RawCache,
    pub(super) probe: Probe,
}
impl RawPoolClient {
    pub(super) fn new(snapshot: &TrustSnapshot) -> Result<Self, HttpError> {
        Self::with_gate(snapshot, None)
    }
    fn with_gate(
        snapshot: &TrustSnapshot,
        gate: Option<Arc<super::abort::DialGate>>,
    ) -> Result<Self, HttpError> {
        let raw_counts = Arc::new(Counts::default());
        let raw_sessions = Arc::new(Mutex::new(Cache::new(100)));
        let connector =
            raw_pool_connector(snapshot, raw_counts.clone(), raw_sessions.clone(), gate)?;
        let fallback = SpikeHttpClient::with_snapshot(false, snapshot)?;
        let fetch_counts = fallback.fetch_counts.clone();
        let probe = Probe::default();
        let shared = Arc::new(Shared {
            state: Mutex::default(),
            tasks: Mutex::default(),
            sequence: AtomicUsize::new(1),
            connections: AtomicUsize::default(),
            requests: AtomicUsize::default(),
            connector,
            counts: raw_counts.clone(),
            stop: CancellationToken::new(),
            probe: probe.clone(),
        });
        Ok(Self {
            shared,
            fallback,
            raw_counts,
            fetch_counts,
            raw_sessions,
            probe,
        })
    }
    pub(super) fn snapshot(&self) -> Trace {
        let (rows, held) = self.probe.snapshot();
        let state = self.shared.state.lock().unwrap();
        let free: Vec<Vec<_>> = state
            .idle
            .values()
            .map(|entries| entries.iter().map(|idle| idle.entry.id).collect())
            .collect();
        let sockets: Vec<_> = state.live.iter().map(|(id, activity)| {
            let idle = state.idle.values().flatten().find(|idle| idle.entry.id == *id);
            serde_json::json!({"id":id,"timeout":idle.map_or_else(|| activity.upgrade().map_or(DEFAULT_TIMEOUT_MS, |activity| activity.timeout_ms()), |idle| idle.timeout_ms),"free":idle.is_some(),"destroyed":false})
        }).collect();
        serde_json::json!({"rows":rows,"connections":self.shared.connections.load(Ordering::SeqCst),"live":state.live.len(),"requests":self.shared.requests.load(Ordering::SeqCst),"free":free,"sockets":sockets,"held":held,"queued":0})
    }
}
// Node's URL-based gateway boundary canonicalizes host spelling and numeric
// ports before the global Agent key is formed. Credentials do not join this key.
fn pool_key(uri: &Uri) -> Result<String, HttpError> {
    Ok(format!(
        "{}://{}:{}",
        uri.scheme_str().ok_or(HttpError::InvalidRequest)?,
        uri.host()
            .ok_or(HttpError::InvalidRequest)?
            .to_ascii_lowercase(),
        uri.port_u16()
            .unwrap_or(if uri.scheme_str() == Some("https") {
                443
            } else {
                80
            })
    ))
}
impl RawPoolClient {
    async fn request_parts(
        &self,
        mut request: Request<Full<Bytes>>,
    ) -> Result<(Response<Incoming>, Completion, Arc<Gate>), HttpError> {
        if !matches!(request.uri().scheme_str(), Some("http" | "https"))
            || request.method() == hyper::Method::CONNECT
            || request.headers().contains_key("upgrade")
            || request.headers().get_all("connection").iter().any(|value| {
                value
                    .as_bytes()
                    .split(|byte| *byte == b',')
                    .any(|token| token.trim_ascii().eq_ignore_ascii_case(b"upgrade"))
            })
            || !matches!(
                request.version(),
                hyper::Version::HTTP_10 | hyper::Version::HTTP_11
            )
        {
            return Err(HttpError::InvalidRequest);
        }
        if let Some(intent) = request.extensions().get::<Arc<Intent>>().cloned() {
            intent.install(capture_http1_assignment(&mut request));
        }
        request
            .extensions_mut()
            .insert(hyper::ext::NodeHttpResponsePolicy);
        let uri = request.uri().clone();
        let key = pool_key(&uri)?;
        let ordinal = self.shared.requests.fetch_add(1, Ordering::SeqCst);
        let label = uri
            .path()
            .strip_prefix("/v1/models/pool-")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(ordinal);
        let (mut entry, reused) = self.shared.acquire(&uri, &key).await?;
        let reservation = bind_http1_reservation(&mut request, &entry.connected)
            .map_err(|()| HttpError::InvalidRequest)?;
        self.probe.record(
            "assigned",
            serde_json::json!({"request":label,"connection":entry.id,"reused":reused}),
        );
        if !request.headers().contains_key("host") {
            let host = uri.host().ok_or(HttpError::InvalidRequest)?;
            let port = uri.port().filter(|port| {
                !matches!(
                    (uri.scheme_str(), port.as_u16()),
                    (Some("http"), 80) | (Some("https"), 443)
                )
            });
            let host = port.map_or_else(|| host.to_owned(), |port| format!("{host}:{port}"));
            request.headers_mut().insert(
                "host",
                hyper::header::HeaderValue::from_str(&host)
                    .map_err(|_| HttpError::InvalidRequest)?,
            );
        }
        *request.uri_mut() = uri
            .path_and_query()
            .map_or("/", |value| value.as_str())
            .parse()
            .map_err(|_| HttpError::InvalidRequest)?;
        let response = entry
            .sender
            .send_request(request)
            .await
            .map_err(|_| HttpError::Network)?;
        let timeout = idle_timeout(response.headers());
        let shared = self.shared.clone();
        let gate = self.probe.gate(label);
        Ok((
            response,
            Box::new(move |clean| shared.complete(key, entry, reservation, timeout, clean)),
            gate,
        ))
    }
}
impl HttpTransport for RawPoolClient {
    type ResponseBody = OwnedBody<Incoming>;
    async fn request_raw(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        let (response, completion, gate) = self.request_parts(request).await?;
        Ok(response.map(|body| OwnedBody::new(body, Some(completion), Some(gate))))
    }
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        self.fallback
            .request(request)
            .await
            .map(|response| response.map(|body| OwnedBody::new(body, None, None)))
    }
}

#[path = "raw_pool_tests.rs"]
mod tests;

#[path = "raw_pool_child.rs"]
mod child;

#[path = "raw_pool_adversarial.rs"]
mod adversarial;

#[path = "raw_pool_buffered.rs"]
pub(super) mod buffered;
