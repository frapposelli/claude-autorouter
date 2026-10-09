//! Separate constructor: the producer owns Incoming, while the consumer owns
//! the exact pool reservation. Prefetch EOF therefore cannot return the sender.
use super::super::buffered_body::{self, BufferedBody, End};
use super::super::gateway_terminal::{FailureCause, RequestTerminal};
use super::*;
use std::sync::atomic::AtomicBool;
use tokio::sync::Notify;

#[derive(Clone, Default)]
pub(in super::super) struct BufferProbe {
    pools: Arc<Mutex<Vec<buffered_body::Probe>>>,
    hold_response: Arc<AtomicBool>,
    response_ready: Arc<AtomicUsize>,
    response_changed: Arc<Notify>,
    clean_sources: Arc<AtomicUsize>,
    failed_sources: Arc<AtomicUsize>,
    cancelled_sources: Arc<AtomicUsize>,
}
#[derive(Debug, Default, serde::Serialize)]
pub(in super::super) struct BufferSnapshot {
    pub live_pools: usize,
    pub allocated_blocks: usize,
    pub outstanding_blocks: usize,
    pub queued_bytes: usize,
    pub producer_tasks: usize,
    pub consumer_polls: usize,
    pub source_errors: usize,
    pub responses_ready: usize,
    pub clean_sources: usize,
    pub failed_sources: usize,
    pub cancelled_sources: usize,
}
impl BufferProbe {
    fn insert(&self, probe: buffered_body::Probe) {
        let mut pools = self.pools.lock().unwrap();
        // Detached pools held by final downstream Bytes clones remain charged.
        pools.retain(|probe| {
            let current = probe.snapshot();
            current.live_pools != 0 || current.producer_tasks != 0
        });
        pools.push(probe);
    }
    pub(in super::super) fn snapshot(&self) -> BufferSnapshot {
        let pools = self.pools.lock().unwrap();
        let mut result = BufferSnapshot {
            responses_ready: self.response_ready.load(Ordering::SeqCst),
            clean_sources: self.clean_sources.load(Ordering::SeqCst),
            failed_sources: self.failed_sources.load(Ordering::SeqCst),
            cancelled_sources: self.cancelled_sources.load(Ordering::SeqCst),
            ..BufferSnapshot::default()
        };
        for probe in pools.iter() {
            let value = probe.snapshot();
            result.live_pools += value.live_pools;
            result.allocated_blocks += value.allocated_blocks;
            result.outstanding_blocks += value.outstanding_blocks;
            result.queued_bytes += value.queued_bytes;
            result.producer_tasks += value.producer_tasks;
            result.consumer_polls += value.consumer_polls;
            result.source_errors += usize::from(value.end == Some(End::SourceError));
        }
        result
    }
    pub(in super::super) fn hold_responses(&self) {
        self.hold_response.store(true, Ordering::SeqCst);
    }
    pub(in super::super) fn release_responses(&self) {
        self.hold_response.store(false, Ordering::SeqCst);
        self.response_changed.notify_waiters();
    }
    async fn response_gate(&self) {
        self.response_ready.fetch_add(1, Ordering::SeqCst);
        loop {
            let changed = self.response_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !self.hold_response.load(Ordering::SeqCst) {
                return;
            }
            changed.await;
        }
    }
}
pub(in super::super) struct BufferedRawPoolClient {
    pub(in super::super) inner: RawPoolClient,
    pub(in super::super) probe: BufferProbe,
}
impl BufferedRawPoolClient {
    pub(in super::super) fn new(snapshot: &TrustSnapshot) -> Result<Self, HttpError> {
        Ok(Self {
            inner: RawPoolClient::new(snapshot)?,
            probe: BufferProbe::default(),
        })
    }
}
pub(in super::super) enum ResponseBody {
    Raw(OwnedBody<BufferedBody>),
    Fetch(OwnedBody<Incoming>),
}
impl Body for ResponseBody {
    type Data = Bytes;
    type Error = std::io::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        match self.get_mut() {
            Self::Raw(body) => Pin::new(body).poll_frame(cx).map(|frame| {
                frame.map(|result| {
                    result.map_err(|_| std::io::Error::other("buffered upstream body failed"))
                })
            }),
            Self::Fetch(body) => Pin::new(body).poll_frame(cx).map(|frame| {
                frame.map(|result| {
                    result.map_err(|_| std::io::Error::other("fetch upstream body failed"))
                })
            }),
        }
    }
    fn is_end_stream(&self) -> bool {
        match self {
            Self::Raw(body) => body.is_end_stream(),
            Self::Fetch(body) => body.is_end_stream(),
        }
    }
    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Raw(body) => body.size_hint(),
            Self::Fetch(body) => body.size_hint(),
        }
    }
}
impl HttpTransport for BufferedRawPoolClient {
    type ResponseBody = ResponseBody;
    async fn request_raw(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<ResponseBody>, HttpError> {
        let terminal = request.extensions().get::<RequestTerminal>().cloned();
        let (response, completion, gate) = self.inner.request_parts(request).await?;
        let (parts, incoming) = response.into_parts();
        // Headers are acquired before this producer can publish a body failure.
        // The server owns attachment and preserves that acquired response.
        let observations = self.probe.clone();
        let (body, producer, probe) = buffered_body::start(incoming, move |end| {
            match end {
                End::Clean => &observations.clean_sources,
                End::SourceError | End::InvalidMetadata => &observations.failed_sources,
                End::Cancelled => &observations.cancelled_sources,
            }
            .fetch_add(1, Ordering::SeqCst);
            if let Some(terminal) = terminal {
                match end {
                    End::SourceError | End::InvalidMetadata => {
                        terminal.fail(FailureCause::Upstream);
                    }
                    End::Cancelled => {
                        terminal.fail(FailureCause::Cancelled);
                    }
                    End::Clean => {}
                }
            }
        });
        self.probe.insert(probe);
        // Registry cancellation drops this future and its Producer owner, which
        // aborts the child even while Producer::join is pending.
        spawn_owned(&self.inner.shared, async move {
            let _ = producer.join().await;
        })
        .disarm();
        let body = ResponseBody::Raw(OwnedBody::new(body, Some(completion), Some(gate)));
        self.probe.response_gate().await;
        Ok(Response::from_parts(parts, body))
    }
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<ResponseBody>, HttpError> {
        self.inner
            .request(request)
            .await
            .map(|response| response.map(ResponseBody::Fetch))
    }
}

#[path = "raw_pool_buffered_tests.rs"]
mod tests;

#[path = "raw_pool_buffered_child.rs"]
mod child;
