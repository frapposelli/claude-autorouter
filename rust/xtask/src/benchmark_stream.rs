//! Deterministic streaming fixtures; delivery timing is a driver observation.
use bytes::Bytes;
use hyper::body::{Body, Frame};
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Burst,
    Paced,
}

#[derive(Clone)]
pub struct Fixture {
    pub frames: Arc<[Bytes]>,
    pub expected: Bytes,
    pub first_delay: Duration,
    pub gap: Duration,
}

fn event(name: &str, value: serde_json::Value) -> Bytes {
    Bytes::from(format!("event: {name}\ndata: {value}\n\n"))
}

impl Fixture {
    pub fn new(kind: Kind) -> Self {
        use serde_json::json;
        let (count, text_bytes, first_delay, gap) = match kind {
            Kind::Burst => (128, 32768, 0, 0),
            Kind::Paced => (32, 2048, 5, 1),
        };
        let mut frames = vec![
            event(
                "message_start",
                json!({"type":"message_start","message":{"id":"synthetic-benchmark-message","type":"message","role":"assistant","model":"claude-sonnet-5","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":32,"output_tokens":0}}}),
            ),
            event(
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            ),
        ];
        let text = "x".repeat(text_bytes);
        for _ in 0..count {
            frames.push(event("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}})));
        }
        frames.extend([
            event("content_block_stop", json!({"type":"content_block_stop","index":0})),
            event("message_delta", json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":8}})),
            event("message_stop", json!({"type":"message_stop"})),
        ]);
        let expected = Bytes::from(
            frames
                .iter()
                .flat_map(|v| v.iter().copied())
                .collect::<Vec<_>>(),
        );
        Self {
            frames: frames.into(),
            expected,
            first_delay: Duration::from_millis(first_delay),
            gap: Duration::from_millis(gap),
        }
    }
}

#[derive(Default)]
pub struct Counts {
    started: AtomicU64,
    completed: AtomicU64,
    abandoned: AtomicU64,
}

impl Counts {
    pub fn snapshot(&self) -> [u64; 3] {
        [&self.started, &self.completed, &self.abandoned].map(|v| v.load(Ordering::SeqCst))
    }
}

pub struct Stream {
    fixture: Fixture,
    index: usize,
    delay: Option<Pin<Box<tokio::time::Sleep>>>,
    counts: Arc<Counts>,
}

impl Stream {
    pub fn new(fixture: Fixture, counts: Arc<Counts>) -> Self {
        counts.started.fetch_add(1, Ordering::SeqCst);
        let delay = (!fixture.first_delay.is_zero())
            .then(|| Box::pin(tokio::time::sleep(fixture.first_delay)));
        Self {
            fixture,
            index: 0,
            delay,
            counts,
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if self.index < self.fixture.frames.len() {
            self.counts.abandoned.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl Body for Stream {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        if let Some(delay) = &mut self.delay
            && delay.as_mut().poll(cx).is_pending()
        {
            return Poll::Pending;
        }
        self.delay = None;
        let Some(bytes) = self.fixture.frames.get(self.index).cloned() else {
            return Poll::Ready(None);
        };
        self.index += 1;
        if self.index == self.fixture.frames.len() {
            self.counts.completed.fetch_add(1, Ordering::SeqCst);
        } else if !self.fixture.gap.is_zero() {
            self.delay = Some(Box::pin(tokio::time::sleep(self.fixture.gap)));
        }
        Poll::Ready(Some(Ok(Frame::data(bytes))))
    }

    fn is_end_stream(&self) -> bool {
        self.index == self.fixture.frames.len()
    }
}

/// Match independent HTTP fragmentation against the complete expected bytes.
/// Only nonempty DATA frames advance first-body/stall observations. These are
/// driver-visible gaps; HTTP can coalesce or split the mock's logical frames.
#[derive(Default)]
pub struct Observation {
    offset: usize,
    pub first_body_ms: Option<f64>,
    pub maximum_data_gap_ms: Option<f64>,
    pub data_frames: u64,
    previous: Option<Instant>,
}

impl Observation {
    pub fn data(
        &mut self,
        bytes: &[u8],
        expected: &[u8],
        started: Instant,
        now: Instant,
    ) -> Result<(), String> {
        let end = self
            .offset
            .checked_add(bytes.len())
            .ok_or("Benchmark response size overflow")?;
        if expected.get(self.offset..end) != Some(bytes) {
            return Err("Benchmark response bytes mismatch".into());
        }
        self.offset = end;
        if bytes.is_empty() {
            return Ok(());
        }
        self.first_body_ms
            .get_or_insert(now.duration_since(started).as_secs_f64() * 1000.0);
        if let Some(previous) = self.previous {
            let gap = now.duration_since(previous).as_secs_f64() * 1000.0;
            self.maximum_data_gap_ms = Some(self.maximum_data_gap_ms.unwrap_or(0.0).max(gap));
        }
        self.previous = Some(now);
        self.data_frames += 1;
        Ok(())
    }

    pub fn finish(&self, expected: &[u8]) -> Result<(), String> {
        if self.offset != expected.len() {
            return Err("Benchmark response was truncated".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[tokio::test(start_paused = true)]
    async fn paced_body_delays_first_data_and_counts_full_or_abandoned_streams() {
        let fixture = Fixture::new(Kind::Paced);
        let counts = Arc::new(Counts::default());
        let mut body = Stream::new(fixture.clone(), counts.clone());
        let started = tokio::time::Instant::now();
        let mut actual = Vec::new();
        while let Some(frame) = body.frame().await {
            actual.extend_from_slice(&frame.unwrap().into_data().unwrap());
        }
        assert_eq!(actual, fixture.expected);
        assert_eq!(
            started.elapsed(),
            fixture.first_delay + fixture.gap * (fixture.frames.len() - 1) as u32
        );
        assert_eq!(counts.snapshot(), [1, 1, 0]);
        drop(Stream::new(fixture, counts.clone()));
        assert_eq!(counts.snapshot(), [2, 1, 1]);
    }

    #[test]
    fn fragmentation_keeps_byte_identity_and_first_nonempty_data_boundary() {
        let started = Instant::now();
        let mut observation = Observation::default();
        observation.data(b"", b"abc", started, started).unwrap();
        assert!(observation.first_body_ms.is_none());
        observation
            .data(b"a", b"abc", started, started + Duration::from_millis(5))
            .unwrap();
        observation
            .data(b"bc", b"abc", started, started + Duration::from_millis(12))
            .unwrap();
        observation.finish(b"abc").unwrap();
        assert_eq!(observation.first_body_ms, Some(5.0));
        assert_eq!(observation.maximum_data_gap_ms, Some(7.0));
        assert_eq!(observation.data_frames, 2);
        assert!(observation.data(b"x", b"abc", started, started).is_err());
        assert!(Observation::default().finish(b"abc").is_err());
        assert!(
            Observation::default()
                .data(b"abd", b"abc", started, started)
                .is_err()
        );
        assert!(
            Observation::default()
                .data(b"abcx", b"abc", started, started)
                .is_err()
        );
    }
}
