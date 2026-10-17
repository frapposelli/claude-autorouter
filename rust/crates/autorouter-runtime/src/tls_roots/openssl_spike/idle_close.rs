//! Test-only idle transport observations. No pool-membership or abort authority.
use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker};

use bytes::Bytes;
use http_body_util::Full;
use hyper::Request;
use hyper::http::Extensions;
use hyper_util::client::legacy::connect::{
    Assignment, CaptureAssignment, capture_http1_assignment,
};
use tokio::sync::Notify;

const CONNECTION_LIMIT: usize = 8;
const REQUEST_LIMIT: usize = 16;
const TRACE_LIMIT: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Event {
    Connected(usize),
    Submitted(usize),
    Selected { request: usize, connection: usize },
    ResponseHead(usize),
    ResponseError(usize),
    BodyComplete(usize),
    ReadHeld(usize),
    WriteHeld(usize),
    ReadEof(usize),
    ReadError(usize, io::ErrorKind),
    WriteError(usize, io::ErrorKind),
    FlushError(usize, io::ErrorKind),
    Released(usize),
    PeerClosed(usize),
    Cleanup,
}

#[derive(Default)]
struct State {
    trace: Vec<Event>,
    connections: Vec<Weak<Connection>>,
    requests: BTreeMap<usize, CaptureAssignment>,
    overflow: bool,
}
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Notify,
}
#[derive(Clone, Default)]
pub(super) struct Probe(Arc<Shared>);

#[derive(Default)]
struct GateState {
    held: bool,
    reported: bool,
    waker: Option<Waker>,
}
#[derive(Default)]
struct Gate(Mutex<GateState>);
pub(super) struct Hold(Arc<Gate>);
impl Drop for Hold {
    fn drop(&mut self) {
        let waker = {
            let mut state = self.0.0.lock().unwrap_or_else(|e| e.into_inner());
            state.held = false;
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}
impl Gate {
    fn hold(self: &Arc<Self>) -> Hold {
        let old = {
            let mut state = self.0.lock().unwrap();
            assert!(!state.held, "idle gate already held");
            state.held = true;
            state.reported = false;
            state.waker.take()
        };
        drop(old);
        Hold(self.clone())
    }
    fn poll(&self, cx: &mut Context<'_>) -> (bool, bool) {
        // Arbitrary Waker Clone/Drop/Wake never executes under a fixture lock.
        let next = cx.waker().clone();
        let (held, first, old) = {
            let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            if state.held {
                let first = !state.reported;
                state.reported = true;
                (true, first, state.waker.replace(next))
            } else {
                (false, false, Some(next))
            }
        };
        drop(old);
        (held, first)
    }
}

pub(super) struct Connection {
    ordinal: usize,
    probe: Weak<Shared>,
    read: Arc<Gate>,
    write: Arc<Gate>,
}
#[derive(Clone)]
pub(super) struct Identity(Weak<Connection>);
impl Connection {
    pub(super) fn identity(self: &Arc<Self>) -> Identity {
        Identity(Arc::downgrade(self))
    }
    pub(super) fn event(&self, make: impl FnOnce(usize) -> Event) {
        if let Some(probe) = self.probe.upgrade() {
            Probe(probe).record(make(self.ordinal));
        }
    }
    pub(super) fn read_ready(&self, cx: &mut Context<'_>) -> Poll<()> {
        let (held, first) = self.read.poll(cx);
        if first {
            self.event(Event::ReadHeld);
        }
        if held { Poll::Pending } else { Poll::Ready(()) }
    }
    pub(super) fn write_ready(&self, cx: &mut Context<'_>) -> Poll<()> {
        let (held, first) = self.write.poll(cx);
        if first {
            self.event(Event::WriteHeld);
        }
        if held { Poll::Pending } else { Poll::Ready(()) }
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.event(Event::Released);
    }
}
impl Probe {
    pub(super) fn record(&self, event: Event) {
        {
            let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.trace.len() < TRACE_LIMIT {
                state.trace.push(event);
            } else {
                state.overflow = true;
            }
        }
        self.0.changed.notify_waiters();
    }
    pub(super) fn connected(&self) -> Arc<Connection> {
        let connection = {
            let mut state = self.0.state.lock().unwrap();
            assert!(
                state.connections.len() < CONNECTION_LIMIT,
                "idle connection bound"
            );
            let connection = Arc::new(Connection {
                ordinal: state.connections.len() + 1,
                probe: Arc::downgrade(&self.0),
                read: Arc::default(),
                write: Arc::default(),
            });
            state.connections.push(Arc::downgrade(&connection));
            connection
        };
        self.record(Event::Connected(connection.ordinal));
        connection
    }
    pub(super) fn attach(&self, request: &mut Request<Full<Bytes>>) -> usize {
        let capture = capture_http1_assignment(request);
        let ordinal = {
            let mut state = self.0.state.lock().unwrap();
            assert!(state.requests.len() < REQUEST_LIMIT, "idle request bound");
            let ordinal = state.requests.len() + 1;
            assert!(state.requests.insert(ordinal, capture).is_none());
            ordinal
        };
        self.record(Event::Submitted(ordinal));
        ordinal
    }
    pub(super) fn assignment(&self, request: usize) -> Option<(usize, Assignment)> {
        let assignment = {
            let state = self.0.state.lock().unwrap();
            state.requests.get(&request)?.assignment()?
        };
        let mut extensions = Extensions::new();
        assignment.get_extras(&mut extensions);
        let identity = extensions.remove::<Identity>()?;
        let connection = identity.0.upgrade()?;
        Some((connection.ordinal, assignment))
    }
    pub(super) fn hold_read(&self, connection: usize) -> Hold {
        self.connection(connection).read.hold()
    }
    pub(super) fn hold_write(&self, connection: usize) -> Hold {
        self.connection(connection).write.hold()
    }
    fn connection(&self, ordinal: usize) -> Arc<Connection> {
        self.0.state.lock().unwrap().connections[ordinal - 1]
            .upgrade()
            .expect("idle connection already released")
    }
    pub(super) fn rows(&self) -> Vec<Event> {
        let state = self.0.state.lock().unwrap();
        assert!(!state.overflow, "idle trace bound");
        state.trace.clone()
    }
    pub(super) fn live(&self) -> usize {
        self.0
            .state
            .lock()
            .unwrap()
            .connections
            .iter()
            .filter(|connection| connection.strong_count() != 0)
            .count()
    }
    pub(super) async fn wait(&self, event: Event) {
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.rows().contains(&event) {
                return;
            }
            changed.await;
        }
    }
}
