//! Cancellation-aware, bounded response consumption shared by evaluator calls.
//! Transparent provider forwarding must never use these decoding adapters.

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use async_compression::tokio::bufread::{BrotliDecoder, DeflateDecoder, GzipDecoder, ZlibDecoder};
use autorouter_core::js_json::JsDocument;
use bytes::{Buf, Bytes};
use hyper::HeaderMap;
use hyper::body::Body;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader, ReadBuf};
use tokio_util::sync::CancellationToken;

pub const DECISION_RESPONSE_LIMIT: usize = 64 * 1024;
pub const MODEL_METADATA_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadError {
    Cancelled,
    InvalidResponse,
    Oversized,
    InvalidJson,
    Transport,
}

struct BodyState<B> {
    body: Pin<Box<B>>,
    chunk: Bytes,
    eof: bool,
}
struct BodyReader<B> {
    state: Arc<Mutex<BodyState<B>>>,
}
impl<B> Clone for BodyReader<B> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
        }
    }
}
impl<B: Body<Data = Bytes>> AsyncRead for BodyReader<B> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let mut state = self.state.lock().unwrap();
        loop {
            if !state.chunk.is_empty() {
                let length = output.remaining().min(state.chunk.len());
                output.put_slice(&state.chunk[..length]);
                state.chunk.advance(length);
                return Poll::Ready(Ok(()));
            }
            if state.eof {
                return Poll::Ready(Ok(()));
            }
            // A peer sending immediately ready empty frames must still allow
            // the outer deadline/cancellation future to run.
            let guard = std::task::ready!(tokio::task::coop::poll_proceed(cx));
            match state.body.as_mut().poll_frame(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    guard.made_progress();
                    state.eof = true;
                }
                Poll::Ready(Some(Err(_))) => {
                    guard.made_progress();
                    // Never represent a transport reset as decoder EOF.
                    return Poll::Ready(Err(io::Error::other("Response transport failed")));
                }
                Poll::Ready(Some(Ok(frame))) => {
                    guard.made_progress();
                    if let Ok(data) = frame.into_data() {
                        state.chunk = data;
                    }
                }
            }
        }
    }
}

type Reader = Pin<Box<dyn AsyncRead + Send>>;

fn check_limit(
    headers: &HeaderMap,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<(), ReadError> {
    if cancellation.is_cancelled() {
        return Err(ReadError::Cancelled);
    }
    if limit == 0 || limit as u128 > 9_007_199_254_740_991 {
        return Err(ReadError::InvalidResponse);
    }
    if let Some(length) = headers
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        && !length.is_empty()
        && length.bytes().all(|byte| byte.is_ascii_digit())
        && length
            .parse::<u128>()
            .map_or(true, |length| length > limit as u128)
    {
        return Err(ReadError::Oversized);
    }
    Ok(())
}

/// Incremental fetch-compatible response decoder. This owns the response body
/// and observes cancellation even while waiting for compression headers or HTTP
/// EOF. Callers apply their own cumulative/line limits and operation deadline.
pub struct DecodedResponseStream<B> {
    reader: Option<Reader>,
    raw: BodyReader<B>,
    cancellation: CancellationToken,
    decode: bool,
    done: bool,
}

impl<B: Body<Data = Bytes> + Send + 'static> DecodedResponseStream<B> {
    pub async fn new(
        body: B,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
    ) -> Result<Self, ReadError> {
        Self::with_decoding(body, headers, cancellation, true).await
    }

    async fn with_decoding(
        body: B,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        decode: bool,
    ) -> Result<Self, ReadError> {
        if cancellation.is_cancelled() {
            return Err(ReadError::Cancelled);
        }
        let raw = BodyReader {
            state: Arc::new(Mutex::new(BodyState {
                body: Box::pin(body),
                chunk: Bytes::new(),
                eof: false,
            })),
        };
        let mut reader: Reader = Box::pin(raw.clone());
        let codings: Vec<_> = headers
            .get(hyper::header::CONTENT_ENCODING)
            .and_then(|value| value.to_str().ok())
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .map(str::to_ascii_lowercase)
                    .collect()
            })
            .unwrap_or_default();
        // Node fetch ignores the complete Content-Encoding chain if any coding is
        // unfamiliar. Encoding layers apply in reverse header order.
        let decode = decode
            && !codings.is_empty()
            && codings
                .iter()
                .all(|coding| matches!(coding.as_str(), "gzip" | "x-gzip" | "deflate" | "br"));
        if decode {
            for coding in codings.iter().rev() {
                let mut buffered = BufReader::new(reader);
                reader = match coding.as_str() {
                    "gzip" | "x-gzip" => {
                        let mut decoder = GzipDecoder::new(buffered);
                        decoder.multiple_members(true);
                        Box::pin(decoder)
                    }
                    "deflate" => {
                        let first = tokio::select! {
                            biased;
                            _ = cancellation.cancelled() => return Err(ReadError::Cancelled),
                            bytes = buffered.fill_buf() => bytes.map_err(|_| ReadError::Transport)?.first().copied(),
                        };
                        // Undici accepts both zlib and raw deflate, selecting by
                        // the first byte's compression-method nibble.
                        if first.is_some_and(|byte| byte & 15 == 8) {
                            Box::pin(ZlibDecoder::new(buffered))
                        } else {
                            Box::pin(DeflateDecoder::new(buffered))
                        }
                    }
                    "br" => Box::pin(BrotliDecoder::new(buffered)),
                    _ => unreachable!(),
                };
            }
        }
        Ok(Self {
            reader: Some(reader),
            raw,
            cancellation: cancellation.clone(),
            decode,
            done: false,
        })
    }

    /// Fill a nonempty output slice; zero means clean HTTP and decoding EOF.
    /// On every error discard the stream; all error text is metadata only.
    pub async fn read(&mut self, output: &mut [u8]) -> Result<usize, ReadError> {
        if self.cancellation.is_cancelled() {
            return Err(ReadError::Cancelled);
        }
        if output.is_empty() {
            return Err(ReadError::InvalidResponse);
        }
        if self.done {
            return Ok(0);
        }
        let read = tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => return Err(ReadError::Cancelled),
            read = self.reader.as_mut().expect("reader until clean EOF").read(output) => read,
        };
        if self.cancellation.is_cancelled() {
            return Err(ReadError::Cancelled);
        }
        let count = match read {
            Ok(count) => count,
            // Fetch uses finishFlush=Z_SYNC_FLUSH (and Brotli FLUSH). A
            // truncated compression trailer at clean HTTP EOF may still yield
            // valid JSON. Never treat a transport reset as a compression EOF.
            Err(error)
                if self.decode
                    && (error.kind() == io::ErrorKind::UnexpectedEof
                // compression-codecs 0.4.45 represents flate2's incomplete
                // Finish status with this exact error, unlike gzip/Brotli.
                || (error.kind() == io::ErrorKind::Other && error.to_string() == "unexpected BufError"))
                    && self.raw.state.lock().unwrap().eof =>
            {
                0
            }
            Err(_) => return Err(ReadError::Transport),
        };
        if count > 0 {
            tokio::task::consume_budget().await;
            return Ok(count);
        }
        // A decoder may finish before HTTP EOF. Drain without retaining encoded
        // bytes so a stalled/reset transport cannot falsely complete a call.
        self.reader.take();
        while !self.raw.state.lock().unwrap().eof {
            let count = tokio::select! {
                biased;
                _ = self.cancellation.cancelled() => return Err(ReadError::Cancelled),
                read = self.raw.read(output) => read.map_err(|_| ReadError::Transport)?,
            };
            if count == 0 {
                break;
            }
            tokio::task::consume_budget().await;
        }
        if self.cancellation.is_cancelled() {
            return Err(ReadError::Cancelled);
        }
        self.done = true;
        Ok(0)
    }
}

async fn read_document<B>(
    body: B,
    headers: &HeaderMap,
    limit: usize,
    cancellation: &CancellationToken,
    decode: bool,
) -> Result<JsDocument, ReadError>
where
    B: Body<Data = Bytes> + Send + 'static,
{
    check_limit(headers, limit, cancellation)?;
    let mut reader =
        DecodedResponseStream::with_decoding(body, headers, cancellation, decode).await?;
    let mut buffer = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let remaining = chunk
            .len()
            .min(limit.saturating_sub(buffer.len()).saturating_add(1));
        let count = reader.read(&mut chunk[..remaining]).await?;
        if count == 0 {
            break;
        }
        if count > limit - buffer.len() {
            return Err(ReadError::Oversized);
        }
        let next = buffer.len() + count;
        if next > buffer.capacity() {
            let capacity = next
                .max(buffer.capacity().saturating_mul(2))
                .max(1024.min(limit))
                .min(limit);
            buffer
                .try_reserve_exact(capacity - buffer.len())
                .map_err(|_| ReadError::InvalidResponse)?;
        }
        buffer.extend_from_slice(&chunk[..count]);
    }
    JsDocument::parse(&buffer).map_err(|_| ReadError::InvalidJson)
}

/// Raw bounded JSON, for a body that is already decoded. The returned document
/// preserves JSON.parse strings/numbers; callers choose the fields to consume.
pub async fn read_bounded_document<B>(
    body: B,
    headers: &HeaderMap,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<JsDocument, ReadError>
where
    B: Body<Data = Bytes> + Send + 'static,
{
    read_document(body, headers, limit, cancellation, false).await
}

/// Explicit evaluator/count response decoding, matching Node fetch's supported
/// gzip, deflate and Brotli codings. Both the header hint and decoded bytes are
/// bounded; the body is dropped promptly on cancellation, overflow or failure.
pub async fn read_response_document<B>(
    body: B,
    headers: &HeaderMap,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<JsDocument, ReadError>
where
    B: Body<Data = Bytes> + Send + 'static,
{
    read_document(body, headers, limit, cancellation, true).await
}

/// Metadata-only convenience projection. Never use it for forwarding, hashing,
/// or truthiness/equality of fields that may contain nonfinite/UTF-16 values.
pub async fn read_bounded_json<B>(
    body: B,
    headers: &HeaderMap,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<Value, ReadError>
where
    B: Body<Data = Bytes> + Send + 'static,
{
    Ok(read_bounded_document(body, headers, limit, cancellation)
        .await?
        .to_serde_observation_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::body::Frame;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::io;
    use std::pin::Pin;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::task::{Context, Poll};

    struct TestBody {
        chunks: VecDeque<Bytes>,
        stall: bool,
        fail: bool,
        dropped: Arc<AtomicBool>,
    }

    impl Body for TestBody {
        type Data = Bytes;
        type Error = io::Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
            if let Some(chunk) = self.chunks.pop_front() {
                Poll::Ready(Some(Ok(Frame::data(chunk))))
            } else if self.fail {
                self.fail = false;
                Poll::Ready(Some(Err(io::Error::other("synthetic reset"))))
            } else if self.stall {
                Poll::Pending
            } else {
                Poll::Ready(None)
            }
        }
    }
    impl Drop for TestBody {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    fn body(bytes: &[u8], chunk_size: usize, stall: bool) -> (TestBody, Arc<AtomicBool>) {
        let dropped = Arc::new(AtomicBool::new(false));
        (
            TestBody {
                chunks: bytes
                    .chunks(chunk_size)
                    .map(Bytes::copy_from_slice)
                    .collect(),
                stall,
                fail: false,
                dropped: dropped.clone(),
            },
            dropped,
        )
    }

    #[tokio::test]
    async fn exact_limit_and_many_tiny_chunks_parse_without_retaining_chunk_objects() {
        let bytes = br#"{"synthetic":"value"}"#;
        let (body, dropped) = body(bytes, 1, false);
        assert_eq!(
            read_bounded_json(
                body,
                &HeaderMap::new(),
                bytes.len(),
                &CancellationToken::new()
            )
            .await
            .unwrap(),
            json!({"synthetic":"value"})
        );
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn streamed_limit_is_authoritative_and_all_failures_drop_the_body() {
        for length in [
            None,
            Some("1"),
            Some("999999999999999999999999999999999999999999999999999"),
        ] {
            let (body, dropped) = body(br#"{"a":"123456789"}"#, 1, false);
            let mut headers = HeaderMap::new();
            if let Some(length) = length {
                headers.insert("content-length", length.parse().unwrap());
            }
            assert_eq!(
                read_bounded_json(body, &headers, 8, &CancellationToken::new()).await,
                Err(ReadError::Oversized)
            );
            assert!(dropped.load(Ordering::SeqCst));
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_stalled_body_without_waiting_for_cleanup() {
        let (body, dropped) = body(b"{", 1, true);
        let cancellation = CancellationToken::new();
        let caller = cancellation.clone();
        let task =
            tokio::spawn(
                async move { read_bounded_json(body, &HeaderMap::new(), 64, &caller).await },
            );
        tokio::task::yield_now().await;
        cancellation.cancel();
        assert_eq!(task.await.unwrap(), Err(ReadError::Cancelled));
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn cancelled_and_malformed_inputs_are_distinct_safe_errors() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let (first, dropped) = body(b"{}", 2, false);
        assert_eq!(
            read_bounded_json(first, &HeaderMap::new(), 64, &cancellation).await,
            Err(ReadError::Cancelled)
        );
        assert!(dropped.load(Ordering::SeqCst));
        for bytes in [b"".as_slice(), b"PRIVATE_FAILURE", b"{broken}"] {
            let (body, _) = body(bytes, 1, false);
            assert_eq!(
                read_bounded_json(body, &HeaderMap::new(), 64, &CancellationToken::new()).await,
                Err(ReadError::InvalidJson)
            );
        }
    }

    const GZIP: &[u8] = &[
        31, 139, 8, 0, 0, 0, 0, 0, 0, 19, 171, 86, 42, 174, 204, 43, 201, 72, 45, 201, 76, 86, 178,
        82, 42, 75, 204, 41, 77, 85, 170, 5, 0, 30, 124, 246, 119, 21, 0, 0, 0,
    ];
    const ZLIB: &[u8] = &[
        120, 156, 171, 86, 42, 174, 204, 43, 201, 72, 45, 201, 76, 86, 178, 82, 42, 75, 204, 41,
        77, 85, 170, 5, 0, 86, 234, 7, 179,
    ];
    const DEFLATE: &[u8] = &[
        171, 86, 42, 174, 204, 43, 201, 72, 45, 201, 76, 86, 178, 82, 42, 75, 204, 41, 77, 85, 170,
        5, 0,
    ];
    const BROTLI: &[u8] = &[
        11, 10, 128, 123, 34, 115, 121, 110, 116, 104, 101, 116, 105, 99, 34, 58, 34, 118, 97, 108,
        117, 101, 34, 125, 3,
    ];
    const STACKED: &[u8] = &[
        11, 20, 128, 31, 139, 8, 0, 0, 0, 0, 0, 0, 19, 171, 86, 42, 174, 204, 43, 201, 72, 45, 201,
        76, 86, 178, 82, 42, 75, 204, 41, 77, 85, 170, 5, 0, 30, 124, 246, 119, 21, 0, 0, 0, 3,
    ];
    const MEMBERS: &[u8] = &[
        31, 139, 8, 0, 0, 0, 0, 0, 0, 19, 171, 86, 42, 174, 204, 43, 201, 72, 45, 201, 76, 86, 178,
        2, 0, 56, 23, 87, 246, 13, 0, 0, 0, 31, 139, 8, 0, 0, 0, 0, 0, 0, 19, 83, 42, 75, 204, 41,
        77, 85, 170, 5, 0, 36, 11, 88, 85, 8, 0, 0, 0,
    ];

    fn encoding(coding: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("content-encoding", coding.parse().unwrap());
        headers
    }

    #[tokio::test]
    async fn supported_decoders_handle_every_chunk_boundary_stacks_and_gzip_members() {
        // Generated using Node's built-in zlib with a synthetic JSON object.
        for (coding, bytes) in [
            ("gzip", GZIP),
            ("x-gzip", GZIP),
            ("deflate", ZLIB),
            ("deflate", DEFLATE),
            ("br", BROTLI),
            ("gzip, br", STACKED),
            ("gzip", MEMBERS),
        ] {
            for chunk_size in 1..=bytes.len() {
                let (body, dropped) = body(bytes, chunk_size, false);
                let document =
                    read_response_document(body, &encoding(coding), 64, &CancellationToken::new())
                        .await
                        .unwrap();
                assert_eq!(
                    document.to_serde_observation_lossy(),
                    json!({"synthetic":"value"}),
                    "{coding} chunk {chunk_size}"
                );
                assert!(dropped.load(Ordering::SeqCst));
            }
        }
    }

    #[tokio::test]
    async fn decoded_byte_bound_and_compressed_header_hint_are_both_enforced() {
        for coding in ["gzip", "deflate", "br"] {
            let bytes = match coding {
                "gzip" => GZIP,
                "deflate" => ZLIB,
                _ => BROTLI,
            };
            let (body, dropped) = body(bytes, 1, false);
            assert!(matches!(
                read_response_document(body, &encoding(coding), 8, &CancellationToken::new()).await,
                Err(ReadError::Oversized)
            ));
            assert!(dropped.load(Ordering::SeqCst));
        }
        let mut headers = encoding("gzip");
        headers.insert("content-length", "9999".parse().unwrap());
        let (body, dropped) = body(GZIP, 1, false);
        assert!(matches!(
            read_response_document(body, &headers, 64, &CancellationToken::new()).await,
            Err(ReadError::Oversized)
        ));
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn unknown_coding_skips_the_whole_decode_chain_like_fetch() {
        let (body, _) = body(br#"{"synthetic":"value"}"#, 1, false);
        assert_eq!(
            read_response_document(
                body,
                &encoding("gzip, unknown"),
                64,
                &CancellationToken::new()
            )
            .await
            .unwrap()
            .to_serde_observation_lossy(),
            json!({"synthetic":"value"})
        );
    }

    #[tokio::test]
    async fn decoder_eof_never_hides_stalled_or_failed_http_body() {
        for (coding, bytes) in [("gzip", GZIP), ("deflate", ZLIB), ("br", BROTLI)] {
            let (body, dropped) = body(bytes, 1, true);
            let cancellation = CancellationToken::new();
            let caller = cancellation.clone();
            let task = tokio::spawn(async move {
                read_response_document(body, &encoding(coding), 64, &caller).await
            });
            tokio::task::yield_now().await;
            cancellation.cancel();
            assert!(matches!(task.await.unwrap(), Err(ReadError::Cancelled)));
            assert!(dropped.load(Ordering::SeqCst));
            let (mut body, dropped) = self::body(bytes, 1, false);
            body.fail = true;
            assert!(matches!(
                read_response_document(body, &encoding(coding), 64, &CancellationToken::new())
                    .await,
                Err(ReadError::Transport)
            ));
            assert!(dropped.load(Ordering::SeqCst));
        }
    }

    #[tokio::test]
    async fn clean_truncated_trailer_matches_fetch_flush_but_invalid_json_and_reset_do_not() {
        for (coding, bytes) in [
            ("gzip", &GZIP[..GZIP.len() - 8]),
            ("deflate", &ZLIB[..ZLIB.len() - 4]),
            ("br", &BROTLI[..BROTLI.len() - 1]),
        ] {
            let (body, _) = body(bytes, 1, false);
            assert_eq!(
                read_response_document(body, &encoding(coding), 64, &CancellationToken::new())
                    .await
                    .unwrap_or_else(|error| panic!("{coding}: {error:?}"))
                    .to_serde_observation_lossy(),
                json!({"synthetic":"value"}),
                "{coding}"
            );
            let (mut body, _) = self::body(bytes, 1, false);
            body.fail = true;
            assert!(matches!(
                read_response_document(body, &encoding(coding), 64, &CancellationToken::new())
                    .await,
                Err(ReadError::Transport)
            ));
        }
        let (body, _) = body(&GZIP[..15], 1, false);
        assert!(
            read_response_document(body, &encoding("gzip"), 64, &CancellationToken::new())
                .await
                .is_err()
        );
    }
}
