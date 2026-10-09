//! Frozen bounded-json inputs with explicit native ownership observations.
//! JavaScript reader locks, arbitrary abort reasons and asynchronous cancel
//! callbacks are separate embedding boundaries, not emulated by Body::drop.
use autorouter_runtime::bounded_json::{
    DECISION_RESPONSE_LIMIT, MODEL_METADATA_LIMIT, ReadError, read_bounded_json,
};
use bytes::Bytes;
use hyper::HeaderMap;
use hyper::body::{Body, Frame};
use serde_json::json;
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Lifetime {
    polls: AtomicUsize,
    drops: AtomicUsize,
    ready: Notify,
}
struct ResponseBody {
    chunks: VecDeque<Bytes>,
    stalled: bool,
    lifetime: Arc<Lifetime>,
}
impl Body for ResponseBody {
    type Data = Bytes;
    type Error = std::io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        self.lifetime.polls.fetch_add(1, Ordering::SeqCst);
        self.lifetime.ready.notify_one();
        match self.chunks.pop_front() {
            Some(chunk) => Poll::Ready(Some(Ok(Frame::data(chunk)))),
            None if self.stalled => Poll::Pending,
            None => Poll::Ready(None),
        }
    }
}
impl Drop for ResponseBody {
    fn drop(&mut self) {
        self.lifetime.drops.fetch_add(1, Ordering::SeqCst);
    }
}
fn body(bytes: &[u8], width: usize, stalled: bool) -> (ResponseBody, Arc<Lifetime>) {
    let lifetime = Arc::new(Lifetime::default());
    (
        ResponseBody {
            chunks: bytes.chunks(width).map(Bytes::copy_from_slice).collect(),
            stalled,
            lifetime: lifetime.clone(),
        },
        lifetime,
    )
}
fn released(lifetime: &Arc<Lifetime>) {
    assert_eq!(lifetime.drops.load(Ordering::SeqCst), 1);
    assert_eq!(Arc::strong_count(lifetime), 1);
}
async fn bounded(schedule: impl std::future::Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(1), schedule)
        .await
        .expect("complete bounded-reader schedule exceeded independent fixture deadline");
}

#[tokio::test]
async fn exact_frozen_utf8_boundary_releases_body_and_preserves_public_limits() {
    bounded(async {
        let bytes = "{\"text\":\"🦊é\"}".as_bytes();
        let (response, lifetime) = body(bytes, 1, false);
        assert_eq!(
            read_bounded_json(
                response,
                &HeaderMap::new(),
                bytes.len(),
                &CancellationToken::new(),
            )
            .await
            .unwrap(),
            json!({"text":"🦊é"})
        );
        assert_eq!(lifetime.polls.load(Ordering::SeqCst), bytes.len() + 1);
        released(&lifetime);
        assert_eq!(DECISION_RESPONSE_LIMIT, 65_536);
        assert_eq!(MODEL_METADATA_LIMIT, 1_048_576);
    })
    .await;
}

#[tokio::test]
async fn exact_65537_byte_overflow_releases_body_under_all_three_frozen_headers() {
    bounded(async {
        for length in [None, Some("1"), Some("65537")] {
            let (response, lifetime) = body(&vec![0; 65_537], 65_537, true);
            let mut headers = HeaderMap::new();
            if let Some(length) = length {
                headers.insert("content-length", length.parse().unwrap());
            }
            assert_eq!(
                read_bounded_json(
                    response,
                    &headers,
                    DECISION_RESPONSE_LIMIT,
                    &CancellationToken::new(),
                )
                .await,
                Err(ReadError::Oversized)
            );
            // The oversized header is only an early rejection. The other two
            // cases must actually consume the authoritative stream bytes.
            assert_eq!(
                lifetime.polls.load(Ordering::SeqCst),
                usize::from(length != Some("65537"))
            );
            released(&lifetime);
        }
    })
    .await;
}

#[tokio::test]
async fn frozen_private_malformed_and_empty_bodies_return_payload_free_typed_errors() {
    bounded(async {
        for bytes in [b"{\"PRIVATE_SECRET\":\"unterminated".as_slice(), b""] {
            let (response, lifetime) = body(bytes, 1, false);
            let error = read_bounded_json(
                response,
                &HeaderMap::new(),
                DECISION_RESPONSE_LIMIT,
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
            assert_eq!(error, ReadError::InvalidJson);
            assert!(!format!("{error:?}").contains("PRIVATE_"));
            released(&lifetime);
        }
    })
    .await;
}

#[tokio::test]
async fn cancellation_after_first_pending_poll_releases_exactly_one_native_body() {
    bounded(async {
        let (response, lifetime) = body(b"", 1, true);
        let cancellation = CancellationToken::new();
        let headers = HeaderMap::new();
        let stop = async {
            lifetime.ready.notified().await;
            assert_eq!(lifetime.polls.load(Ordering::SeqCst), 1);
            cancellation.cancel();
        };
        let (result, ()) = tokio::join!(
            read_bounded_json(response, &headers, DECISION_RESPONSE_LIMIT, &cancellation,),
            stop
        );
        assert_eq!(result, Err(ReadError::Cancelled));
        released(&lifetime);
    })
    .await;
}

#[tokio::test]
async fn already_cancelled_overflow_is_not_polled_and_ordinary_overflow_is_released() {
    bounded(async {
        for cancelled in [false, true] {
            let (response, lifetime) = body(&vec![0; 65_537], 65_537, true);
            let cancellation = CancellationToken::new();
            if cancelled {
                cancellation.cancel();
            }
            assert_eq!(
                read_bounded_json(
                    response,
                    &HeaderMap::new(),
                    DECISION_RESPONSE_LIMIT,
                    &cancellation,
                )
                .await,
                Err(if cancelled {
                    ReadError::Cancelled
                } else {
                    ReadError::Oversized
                })
            );
            assert_eq!(
                lifetime.polls.load(Ordering::SeqCst),
                usize::from(!cancelled)
            );
            released(&lifetime);
        }
    })
    .await;
}
