//! Bounded actual HTTP peers for complete original gateway callbacks.
//! A body-poll witness is distinct from downstream receipt of those bytes.
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::{
    HeaderMap, Request, Response,
    body::{Body, Frame, Incoming, SizeHint},
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{
    collections::VecDeque,
    convert::Infallible,
    future::Future,
    io,
    net::SocketAddr,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const BODY_LIMIT: usize = 2 * 1024 * 1024;
const CALL_LIMIT: usize = 8;
const CONNECTION_LIMIT: usize = 8;
const BOUND: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct Observation {
    pub method: String,
    pub path: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

#[derive(Clone)]
pub struct StreamGate {
    first: CancellationToken,
    release: CancellationToken,
    last: CancellationToken,
}
impl StreamGate {
    pub async fn wait_first_polled(&self) -> Result<(), String> {
        tokio::time::timeout(BOUND, self.first.cancelled())
            .await
            .map_err(|_| "mock first-poll witness timed out".into())
    }
    pub fn first_was_polled(&self) -> bool {
        self.first.is_cancelled()
    }
    pub fn final_was_polled(&self) -> bool {
        self.last.is_cancelled()
    }
    pub fn released(&self) -> bool {
        self.release.is_cancelled()
    }
    pub fn release(&self) {
        self.release.cancel();
    }
}

pub struct ResponsePlan {
    status: u16,
    headers: HeaderMap,
    body: PeerBody,
}
impl ResponsePlan {
    pub fn bytes(status: u16, headers: HeaderMap, body: impl Into<Bytes>) -> Self {
        Self {
            status,
            headers,
            body: PeerBody {
                first: Some(body.into()),
                tail: VecDeque::new(),
                gate: None,
                released: None,
            },
        }
    }
    pub fn held(status: u16, headers: HeaderMap, first: Bytes, last: Bytes) -> (Self, StreamGate) {
        let gate = StreamGate {
            first: CancellationToken::new(),
            release: CancellationToken::new(),
            last: CancellationToken::new(),
        };
        let body = PeerBody {
            first: Some(first),
            tail: VecDeque::from([last]),
            gate: Some(gate.clone()),
            released: Some(Box::pin(gate.release.clone().cancelled_owned())),
        };
        (
            Self {
                status,
                headers,
                body,
            },
            gate,
        )
    }
    // This helper is also compiled by the earlier integration-test consumer.
    #[allow(dead_code)]
    pub fn chunks(status: u16, headers: HeaderMap, chunks: Vec<Bytes>) -> Self {
        let mut tail: VecDeque<_> = chunks.into();
        let first = tail.pop_front();
        Self {
            status,
            headers,
            body: PeerBody {
                first,
                tail,
                gate: None,
                released: None,
            },
        }
    }
    #[allow(dead_code)]
    pub fn held_tail(
        status: u16,
        headers: HeaderMap,
        first: Bytes,
        tail: Vec<Bytes>,
    ) -> (Self, StreamGate) {
        let (mut plan, gate) = Self::held(status, headers, first, Bytes::new());
        plan.body.tail = tail.into();
        (plan, gate)
    }
    fn into_response(self) -> Result<Response<PeerBody>, String> {
        validate_headers(&self.headers)?;
        if self.body.gate.is_some() && self.body.tail.is_empty() {
            return Err("held mock response requires at least one tail chunk".into());
        }
        let size = self.body.first.as_ref().map_or(0, Bytes::len)
            + self.body.tail.iter().map(Bytes::len).sum::<usize>();
        if size > BODY_LIMIT || usize::from(self.body.first.is_some()) + self.body.tail.len() > 16 {
            return Err("mock response exceeds its byte bound".into());
        }
        let mut response = Response::builder()
            .status(self.status)
            .body(self.body)
            .map_err(|_| "mock response has invalid status")?;
        *response.headers_mut() = self.headers;
        Ok(response)
    }
}

struct PeerBody {
    first: Option<Bytes>,
    tail: VecDeque<Bytes>,
    gate: Option<StreamGate>,
    released: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
}
impl Body for PeerBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let body = self.get_mut();
        if let Some(first) = body.first.take() {
            if let Some(gate) = &body.gate {
                gate.first.cancel();
            }
            return Poll::Ready(Some(Ok(Frame::data(first))));
        }
        if let Some(released) = &mut body.released {
            if released.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
            body.released = None;
        }
        if let Some(last) = body.tail.pop_front() {
            if body.tail.is_empty()
                && let Some(gate) = &body.gate
            {
                gate.last.cancel();
            }
            return Poll::Ready(Some(Ok(Frame::data(last))));
        }
        Poll::Ready(None)
    }
    fn is_end_stream(&self) -> bool {
        self.first.is_none() && self.tail.is_empty()
    }
    fn size_hint(&self) -> SizeHint {
        if self.gate.is_some() {
            SizeHint::default()
        } else {
            SizeHint::with_exact(
                (self.first.as_ref().map_or(0, Bytes::len)
                    + self.tail.iter().map(Bytes::len).sum::<usize>()) as u64,
            )
        }
    }
}

#[derive(Default)]
struct State {
    calls: Mutex<Vec<Observation>>,
    errors: Mutex<Vec<String>>,
}
impl State {
    fn fail(&self, message: impl Into<String>) {
        let mut errors = self.errors.lock().unwrap();
        if errors.len() < 16 {
            let mut message = message.into();
            let mut end = message.len().min(1024);
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
            errors.push(message);
        }
    }
}

pub struct Peer {
    pub address: SocketAddr,
    state: Arc<State>,
    stop: CancellationToken,
    owner: Option<JoinHandle<()>>,
    tasks: TaskTracker,
}
impl Peer {
    pub async fn start(
        responder: impl Fn(&Observation, usize) -> Result<ResponsePlan, String> + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let state = Arc::new(State::default());
        let stop = CancellationToken::new();
        let tasks = TaskTracker::new();
        let (shared, signal, tracked) = (state.clone(), stop.clone(), tasks.clone());
        let responder = Arc::new(responder);
        let owner = tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            let mut accepted = 0;
            loop {
                tokio::select! {
                    biased;
                    _ = signal.cancelled() => break,
                    result = children.join_next(), if !children.is_empty() => {
                        if result.unwrap().is_err() {
                            shared.fail("mock connection task failed");
                        }
                    }
                    result = listener.accept() => {
                        let (socket, _) = match result {
                            Ok(value) => value,
                            Err(_) => { shared.fail("mock listener accept failed"); break; }
                        };
                        accepted += 1;
                        if accepted > CONNECTION_LIMIT {
                            shared.fail("mock connection limit exceeded");
                            drop(socket);
                            break;
                        }
                        let (state, responder) = (shared.clone(), responder.clone());
                        children.spawn(tracked.track_future(async move {
                            let errors = state.clone();
                            let service = service_fn(move |request: Request<Incoming>| {
                                let (state, responder) = (state.clone(), responder.clone());
                                async move {
                                    let result = tokio::time::timeout(BOUND, observe(request)).await;
                                    let response = match result {
                                        Ok(Ok(observation)) => {
                                            let index = {
                                                let mut calls = state.calls.lock().unwrap();
                                                if calls.len() == CALL_LIMIT {
                                                    None
                                                } else {
                                                    let index = calls.len();
                                                    calls.push(observation.clone());
                                                    Some(index)
                                                }
                                            };
                                            match index {
                                                Some(index) => match catch_unwind(AssertUnwindSafe(|| responder(&observation, index))) {
                                                    Ok(result) => result.and_then(ResponsePlan::into_response),
                                                    Err(_) => Err("mock response selector panicked".into()),
                                                },
                                                None => Err("mock request limit exceeded".into()),
                                            }
                                        }
                                        Ok(Err(error)) => Err(error),
                                        Err(_) => Err("mock request read timed out".into()),
                                    };
                                    let response = response.unwrap_or_else(|error| {
                                        state.fail(error);
                                        ResponsePlan::bytes(500, HeaderMap::new(), Bytes::from_static(b"mock failure"))
                                            .into_response().unwrap()
                                    });
                                    Ok::<_, Infallible>(response)
                                }
                            });
                            if hyper::server::conn::http1::Builder::new()
                                .max_buf_size(64 * 1024)
                                .timer(TokioTimer::new())
                                .header_read_timeout(BOUND)
                                .serve_connection(TokioIo::new(socket), service).await.is_err()
                            {
                                errors.fail("mock HTTP connection failed");
                            }
                        }));
                    }
                }
            }
            drop(listener);
            children.abort_all();
            while let Some(result) = children.join_next().await {
                if result.is_err_and(|error| !error.is_cancelled()) {
                    shared.fail("mock connection task failed during cleanup");
                }
            }
            tracked.close();
            tracked.wait().await;
        });
        Ok(Self {
            address,
            state,
            stop,
            owner: Some(owner),
            tasks,
        })
    }
    pub fn observations(&self) -> Vec<Observation> {
        self.state.calls.lock().unwrap().clone()
    }
    pub fn errors(&self) -> Vec<String> {
        self.state.errors.lock().unwrap().clone()
    }
    pub async fn close(mut self) -> Result<(), String> {
        self.stop.cancel();
        let mut owner = self.owner.take().unwrap();
        match tokio::time::timeout(BOUND, &mut owner).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => self.state.fail("mock listener owner failed"),
            Err(_) => {
                self.state.fail("mock listener cleanup timed out");
                owner.abort();
                let _ = tokio::time::timeout(BOUND, owner).await;
            }
        }
        self.tasks.close();
        if tokio::time::timeout(BOUND, self.tasks.wait())
            .await
            .is_err()
        {
            self.state.fail("mock connection cleanup timed out");
        }
        let errors = self.errors();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        // The independently owned listener task retains connection cancellation
        // and joining even if the caller drops a pending close future.
        self.stop.cancel();
    }
}

async fn observe(request: Request<Incoming>) -> Result<Observation, String> {
    let (parts, mut body) = request.into_parts();
    validate_headers(&parts.headers)?;
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| "mock request body read failed")?;
        if let Some(data) = frame.data_ref() {
            if data.len() > BODY_LIMIT - bytes.len() {
                return Err("mock request body exceeds its byte bound".into());
            }
            bytes.extend_from_slice(data);
        }
    }
    Ok(Observation {
        method: parts.method.to_string(),
        path: parts.uri.to_string(),
        headers: parts.headers,
        body: bytes,
    })
}

fn validate_headers(headers: &HeaderMap) -> Result<(), String> {
    if headers.len() > 64
        || headers
            .iter()
            .map(|(key, value)| key.as_str().len() + value.as_bytes().len())
            .sum::<usize>()
            > 8192
    {
        return Err("mock headers exceed finite bounds".into());
    }
    Ok(())
}
