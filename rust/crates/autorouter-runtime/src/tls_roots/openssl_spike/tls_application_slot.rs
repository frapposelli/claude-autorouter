//! Bounded plaintext ownership for a future raw TLS write adapter.
//!
//! This slot is not a TLS duplex driver. After a real TLS write is Pending,
//! moving its immutable bytes here does not authorize an intervening SSL_read.
//! Integration needs bounded ciphertext egress or a qualified WANT-aware driver.
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::io::AsyncWrite;

pub(super) const LIMIT: usize = 16 * 1024;

// No Debug: admitted application bytes do not belong in routine diagnostics.
#[derive(Default)]
pub(super) struct Slot {
    bytes: Option<Box<[u8]>>,
    offset: usize,
}

impl Slot {
    pub(super) fn retained_bytes(&self) -> usize {
        self.bytes.as_ref().map_or(0, |bytes| bytes.len())
    }

    pub(super) fn pending_bytes(&self) -> usize {
        self.retained_bytes() - self.offset
    }

    // The caller already received credit for every byte in this immutable slot.
    // Actual progress retires that credit; it must never be returned again as
    // consumption of a later caller buffer. Only SSL_write belongs in this loop.
    pub(super) fn poll_drain<W: AsyncWrite + Unpin>(
        &mut self,
        io: &mut W,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        while let Some(bytes) = self.bytes.as_ref() {
            let remaining = &bytes[self.offset..];
            match Pin::new(&mut *io).poll_write(cx, remaining) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Ready(Ok(0)) => {
                    return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
                }
                Poll::Ready(Ok(count)) => {
                    if count > remaining.len() {
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "TLS output count exceeds the owned application chunk",
                        )));
                    }
                    self.offset += count;
                    if self.offset == bytes.len() {
                        self.bytes = None;
                        self.offset = 0;
                    }
                }
            }
        }
        Poll::Ready(Ok(()))
    }

    // A Pending real write admits one exact capped copy and grants its credit
    // once. With an older slot, Pending grants no credit for the new input;
    // callers may replace that unconsumed input before retrying this method.
    pub(super) fn poll_write<W: AsyncWrite + Unpin>(
        &mut self,
        io: &mut W,
        cx: &mut Context<'_>,
        input: &[u8],
    ) -> Poll<io::Result<usize>> {
        if input.is_empty() {
            return Poll::Ready(Ok(0));
        }
        match self.poll_drain(io, cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Pending => return Poll::Pending,
        }
        let chunk = &input[..input.len().min(LIMIT)];
        match Pin::new(io).poll_write(cx, chunk) {
            Poll::Pending => {
                self.bytes = Some(Box::from(chunk));
                self.offset = 0;
                Poll::Ready(Ok(chunk.len()))
            }
            result => result,
        }
    }

    // Match the ordinary non-vectored fallback: consume only a prefix of the
    // first nonempty slice. Never gather caller slices into another allocation.
    pub(super) fn poll_write_vectored<W: AsyncWrite + Unpin>(
        &mut self,
        io: &mut W,
        cx: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match buffers.iter().find(|buffer| !buffer.is_empty()) {
            Some(input) => self.poll_write(io, cx, input),
            None => Poll::Ready(Ok(0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
        task::Waker,
    };

    #[derive(Debug)]
    struct OriginalFailure(Arc<()>);
    impl std::fmt::Display for OriginalFailure {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("synthetic write failure")
        }
    }
    impl std::error::Error for OriginalFailure {}

    enum Step {
        Pending,
        Count(usize),
        RawCount(usize),
        Error(io::Error),
    }
    #[derive(Default)]
    struct Observed {
        steps: VecDeque<Step>,
        offered: Vec<Vec<u8>>,
        written: Vec<u8>,
        flushes: usize,
        shutdowns: usize,
    }
    struct Script(Arc<Mutex<Observed>>);
    impl AsyncWrite for Script {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            let mut state = self.0.lock().unwrap();
            state.offered.push(bytes.to_vec());
            match state.steps.pop_front().unwrap_or(Step::Count(usize::MAX)) {
                Step::Pending => {
                    // No user waker callout while holding the fixture mutex.
                    drop(state);
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
                Step::Error(error) => Poll::Ready(Err(error)),
                Step::RawCount(count) => Poll::Ready(Ok(count)),
                Step::Count(count) => {
                    let count = count.min(bytes.len());
                    state.written.extend_from_slice(&bytes[..count]);
                    Poll::Ready(Ok(count))
                }
            }
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.0.lock().unwrap().flushes += 1;
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.0.lock().unwrap().shutdowns += 1;
            Poll::Ready(Ok(()))
        }
    }
    fn fixture(steps: impl IntoIterator<Item = Step>) -> (Slot, Script, Arc<Mutex<Observed>>) {
        let state = Arc::new(Mutex::new(Observed {
            steps: steps.into_iter().collect(),
            ..Observed::default()
        }));
        (Slot::default(), Script(state.clone()), state)
    }

    #[test]
    fn direct_success_and_error_preserve_actual_count_and_payload() {
        let token = Arc::new(());
        let error = io::Error::new(io::ErrorKind::BrokenPipe, OriginalFailure(token.clone()));
        let (mut slot, mut io, state) = fixture([Step::Count(3), Step::Error(error)]);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(matches!(
            slot.poll_write(&mut io, &mut cx, b"hello"),
            Poll::Ready(Ok(3))
        ));
        assert_eq!((slot.retained_bytes(), slot.pending_bytes()), (0, 0));
        let Poll::Ready(Err(error)) = slot.poll_write(&mut io, &mut cx, b"world") else {
            panic!("real failure must be returned unchanged");
        };
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        let payload = error
            .get_ref()
            .unwrap()
            .downcast_ref::<OriginalFailure>()
            .unwrap();
        assert!(Arc::ptr_eq(&payload.0, &token));
        assert_eq!(state.lock().unwrap().written, b"hel");
        assert_eq!(slot.retained_bytes(), 0);
    }

    #[test]
    fn pending_admission_is_immutable_and_drain_never_recredits_old_bytes() {
        let (mut slot, mut io, state) = fixture([
            Step::Pending,
            Step::Pending,
            Step::Count(3),
            Step::Count(5),
            Step::Count(2),
        ]);
        let mut cx = Context::from_waker(Waker::noop());
        let mut caller = b"original".to_vec();
        assert!(matches!(
            slot.poll_write(&mut io, &mut cx, &caller),
            Poll::Ready(Ok(8))
        ));
        assert_eq!((slot.retained_bytes(), slot.pending_bytes()), (8, 8));
        caller.fill(b'X');
        assert!(
            slot.poll_write(&mut io, &mut cx, b"not consumed")
                .is_pending()
        );
        assert_eq!((slot.retained_bytes(), slot.pending_bytes()), (8, 8));
        // A new caller buffer is valid: the previous Pending consumed none of it.
        assert!(matches!(
            slot.poll_write(&mut io, &mut cx, b"next"),
            Poll::Ready(Ok(2))
        ));
        let state = state.lock().unwrap();
        assert_eq!(state.written, b"originalne");
        assert_eq!(
            state.offered,
            [
                b"original".to_vec(),
                b"original".to_vec(),
                b"original".to_vec(),
                b"ginal".to_vec(),
                b"next".to_vec(),
            ]
        );
        assert_eq!((slot.retained_bytes(), slot.pending_bytes()), (0, 0));
    }

    #[test]
    fn capped_and_vectored_credit_stays_within_one_owned_chunk() {
        let (mut slot, mut io, state) = fixture([Step::Pending]);
        let mut cx = Context::from_waker(Waker::noop());
        let large = vec![b'z'; LIMIT + 7];
        assert!(matches!(
            slot.poll_write(&mut io, &mut cx, &large),
            Poll::Ready(Ok(LIMIT))
        ));
        assert_eq!(
            (slot.retained_bytes(), slot.pending_bytes()),
            (LIMIT, LIMIT)
        );
        let before = state.lock().unwrap().offered.len();
        assert!(matches!(
            slot.poll_write(&mut io, &mut cx, b""),
            Poll::Ready(Ok(0))
        ));
        let empty = [io::IoSlice::new(b""), io::IoSlice::new(b"")];
        assert!(matches!(
            slot.poll_write_vectored(&mut io, &mut cx, &empty),
            Poll::Ready(Ok(0))
        ));
        assert_eq!(state.lock().unwrap().offered.len(), before);
        assert_eq!(slot.pending_bytes(), LIMIT);
        assert!(matches!(
            slot.poll_drain(&mut io, &mut cx),
            Poll::Ready(Ok(()))
        ));
        state.lock().unwrap().steps.push_back(Step::Pending);
        let chunks = [
            io::IoSlice::new(b""),
            io::IoSlice::new(b"abc"),
            io::IoSlice::new(b"def"),
        ];
        assert!(matches!(
            slot.poll_write_vectored(&mut io, &mut cx, &chunks),
            Poll::Ready(Ok(3))
        ));
        assert_eq!((slot.retained_bytes(), slot.pending_bytes()), (3, 3));
        assert!(matches!(
            slot.poll_drain(&mut io, &mut cx),
            Poll::Ready(Ok(()))
        ));
        let state = state.lock().unwrap();
        assert_eq!(&state.written[..LIMIT], &large[..LIMIT]);
        assert_eq!(&state.written[LIMIT..], b"abc");
        assert_eq!((slot.retained_bytes(), slot.pending_bytes()), (0, 0));
    }

    #[test]
    fn partial_drain_keeps_remaining_bytes_and_returns_original_failure() {
        let token = Arc::new(());
        let error = io::Error::new(
            io::ErrorKind::ConnectionReset,
            OriginalFailure(token.clone()),
        );
        let (mut slot, mut io, state) = fixture([
            Step::Pending,
            Step::Count(2),
            Step::Pending,
            Step::Error(error),
        ]);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(matches!(
            slot.poll_write(&mut io, &mut cx, b"abcdef"),
            Poll::Ready(Ok(6))
        ));
        assert!(slot.poll_drain(&mut io, &mut cx).is_pending());
        assert_eq!((slot.retained_bytes(), slot.pending_bytes()), (6, 4));
        let Poll::Ready(Err(error)) = slot.poll_drain(&mut io, &mut cx) else {
            panic!("drain failure must not be swallowed");
        };
        assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
        let payload = error
            .get_ref()
            .unwrap()
            .downcast_ref::<OriginalFailure>()
            .unwrap();
        assert!(Arc::ptr_eq(&payload.0, &token));
        assert_eq!(state.lock().unwrap().written, b"ab");
        assert_eq!((slot.retained_bytes(), slot.pending_bytes()), (6, 4));
    }

    #[test]
    fn zero_progress_drain_fails_without_discarding_admitted_bytes() {
        let (mut slot, mut io, state) = fixture([Step::Pending, Step::Count(0)]);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(matches!(
            slot.poll_write(&mut io, &mut cx, b"xyz"),
            Poll::Ready(Ok(3))
        ));
        let Poll::Ready(Err(error)) = slot.poll_drain(&mut io, &mut cx) else {
            panic!("zero progress must not spin or succeed");
        };
        assert_eq!(error.kind(), io::ErrorKind::WriteZero);
        assert_eq!((slot.retained_bytes(), slot.pending_bytes()), (3, 3));
        assert!(state.lock().unwrap().written.is_empty());
    }

    #[test]
    fn invalid_drain_count_cannot_advance_outside_the_owned_chunk() {
        let (mut slot, mut io, _) = fixture([Step::Pending, Step::RawCount(4)]);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(matches!(
            slot.poll_write(&mut io, &mut cx, b"xyz"),
            Poll::Ready(Ok(3))
        ));
        let Poll::Ready(Err(error)) = slot.poll_drain(&mut io, &mut cx) else {
            panic!("invalid writer count must remain bounded");
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!((slot.retained_bytes(), slot.pending_bytes()), (3, 3));
    }

    #[test]
    fn dropping_a_slot_never_flushes_shuts_down_or_emits_more_bytes() {
        let (mut slot, mut io, state) = fixture([Step::Pending]);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(matches!(
            slot.poll_write(&mut io, &mut cx, b"owned"),
            Poll::Ready(Ok(5))
        ));
        let offered = state.lock().unwrap().offered.clone();
        drop(slot);
        let state = state.lock().unwrap();
        assert_eq!(state.offered, offered);
        assert!(state.written.is_empty());
        assert_eq!((state.flushes, state.shutdowns), (0, 0));
    }
}
