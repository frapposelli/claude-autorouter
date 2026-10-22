// Included at item scope inside transport_completion::tests. These controlled
// parser/driver tests use the actual vendored Hyper through its public API.
mod parser_timing_contracts {
    use super::*;
    use http_body_util::{BodyExt, Empty};
    use hyper::ext::{
        NodeHttpResponseTiming, NodeHttpServerTimeout, NodeHttpServerTimingControl,
        NodeHttpServerTimingEvent,
    };

    const RAW_408: &[u8] = b"HTTP/1.1 408 Request Timeout\r\nConnection: close\r\n\r\n";

    #[derive(Debug, Default)]
    struct Timing {
        batches: Mutex<Vec<Vec<NodeHttpServerTimingEvent>>>,
        timeout: Mutex<Option<NodeHttpServerTimeout>>,
        reject_headers: bool,
    }
    impl Timing {
        fn count(&self, predicate: impl Fn(&NodeHttpServerTimingEvent) -> bool) -> usize {
            self.batches
                .lock()
                .unwrap()
                .iter()
                .flatten()
                .filter(|event| predicate(event))
                .count()
        }
        fn stop(&self) {
            *self.timeout.lock().unwrap() = Some(NodeHttpServerTimeout::Idle);
        }
    }
    impl NodeHttpServerTimingControl for Timing {
        fn commit(&self, events: &[NodeHttpServerTimingEvent]) -> bool {
            let mut batches = self.batches.lock().unwrap();
            assert!(batches.len() < 512, "bounded controlled parser trace");
            batches.push(events.to_vec());
            let mut timeout = self.timeout.lock().unwrap();
            if self.reject_headers
                && events
                    .iter()
                    .any(|event| matches!(event, NodeHttpServerTimingEvent::HeadersComplete { .. }))
            {
                *timeout = Some(NodeHttpServerTimeout::Request {
                    write_raw_408: true,
                });
            }
            timeout.is_none()
        }
        fn poll_timeout(&self, _: &mut Context<'_>) -> Poll<NodeHttpServerTimeout> {
            self.timeout
                .lock()
                .unwrap()
                .map_or(Poll::Pending, Poll::Ready)
        }
    }

    fn poll_once<F: Future>(connection: Pin<&mut F>) -> Poll<F::Output> {
        connection.poll(&mut Context::from_waker(Waker::noop()))
    }
    fn finish<F: Future>(mut connection: Pin<&mut F>) -> F::Output {
        for _ in 0..32 {
            if let Poll::Ready(result) = poll_once(connection.as_mut()) {
                return result;
            }
        }
        panic!("controlled parser driver did not terminate in 32 polls");
    }
    fn input(io: &FakeIo, bytes: &[u8]) {
        io.0.lock().unwrap().input.extend(bytes.iter().copied());
    }

    #[test]
    fn fragmented_leading_crlf_emits_no_message_begin_until_valid_or_invalid_method_byte() {
        for first in *b"G!" {
            let io = FakeIo::request(0);
            let timing = Arc::new(Timing::default());
            let calls = Arc::new(AtomicUsize::new(0));
            let counted = calls.clone();
            let service = service_fn(move |_: Request<Incoming>| {
                counted.fetch_add(1, Ordering::SeqCst);
                async { Ok::<_, Infallible>(Response::new(Empty::<Bytes>::new())) }
            });
            let mut builder = hyper::server::conn::http1::Builder::new();
            builder.node_http_server_timing(timing.clone());
            let mut connection =
                Box::pin(builder.serve_connection(TokioIo::new(io.clone()), service));
            for byte in b"\r\n\r\n" {
                input(&io, &[*byte]);
                assert!(poll_once(connection.as_mut()).is_pending());
                assert_eq!(
                    timing.count(|event| matches!(event, NodeHttpServerTimingEvent::MessageBegin)),
                    0
                );
                assert_eq!(
                    timing.count(|event| matches!(event, NodeHttpServerTimingEvent::ParserAborted)),
                    0
                );
            }
            assert_eq!(
                timing.count(|event| matches!(event, NodeHttpServerTimingEvent::PositiveInput)),
                4
            );
            input(&io, &[first]);
            let _ = poll_once(connection.as_mut());
            assert_eq!(
                timing.count(|event| matches!(event, NodeHttpServerTimingEvent::MessageBegin)),
                1
            );
            assert_eq!(
                timing.count(|event| matches!(
                    event,
                    NodeHttpServerTimingEvent::HeadersComplete { .. }
                )),
                0
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            let aborted =
                timing.count(|event| matches!(event, NodeHttpServerTimingEvent::ParserAborted));
            if first == b'!' {
                assert!(
                    aborted > 0,
                    "invalid first method byte aborts receive eligibility"
                );
            } else {
                assert_eq!(aborted, 0, "valid incomplete method remains active");
            }
            drop(connection); // No service/response or task was admitted.
        }
    }

    #[test]
    fn zero_body_head_is_one_atomic_batch_with_the_request_marker_before_service() {
        let io = FakeIo::request(0);
        input(&io, b"\r\nGET / HTTP/1.1\r\nHost: example.test\r\n\r\n");
        let timing = Arc::new(Timing::default());
        let observed = timing.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let service = service_fn(move |request: Request<Incoming>| {
            counted.fetch_add(1, Ordering::SeqCst);
            let from_request = request
                .extensions()
                .get::<NodeHttpResponseTiming>()
                .unwrap();
            let batches = observed.batches.lock().unwrap();
            let heads: Vec<_> = batches
                .iter()
                .filter(|batch| {
                    batch.iter().any(|event| {
                        matches!(event, NodeHttpServerTimingEvent::HeadersComplete { .. })
                    })
                })
                .collect();
            assert_eq!(heads.len(), 1);
            assert!(matches!(
                heads[0].as_slice(),
                [
                    NodeHttpServerTimingEvent::PositiveInput,
                    NodeHttpServerTimingEvent::MessageBegin,
                    NodeHttpServerTimingEvent::HeadersComplete {
                        message_complete: true,
                        ..
                    }
                ]
            ));
            let NodeHttpServerTimingEvent::HeadersComplete { response, .. } = &heads[0][2] else {
                unreachable!()
            };
            assert!(!response.is_prepared());
            assert!(!from_request.is_prepared());
            from_request.mark_prepared();
            assert!(
                response.is_prepared(),
                "request extension shares the committed response marker"
            );
            drop(batches);
            let timing = observed.clone();
            async move {
                timing.stop();
                Ok::<_, Infallible>(Response::new(Empty::<Bytes>::new()))
            }
        });
        let mut builder = hyper::server::conn::http1::Builder::new();
        builder.node_http_server_timing(timing.clone());
        let mut connection = Box::pin(builder.serve_connection(TokioIo::new(io), service));
        finish(connection.as_mut()).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            timing.count(|event| matches!(event, NodeHttpServerTimingEvent::MessageBegin)),
            1
        );
        assert_eq!(
            timing.count(|event| matches!(event, NodeHttpServerTimingEvent::MessageComplete)),
            0
        );
    }

    #[test]
    fn final_body_frame_commits_completion_before_the_service_observes_it() {
        let io = FakeIo::request(0);
        input(
            &io,
            b"POST / HTTP/1.1\r\nHost: example.test\r\nContent-Length: 1\r\n\r\nx",
        );
        let timing = Arc::new(Timing::default());
        let observed = timing.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let service = service_fn(move |request: Request<Incoming>| {
            counted.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                observed.count(|event| matches!(event, NodeHttpServerTimingEvent::MessageComplete)),
                0
            );
            assert_eq!(
                observed.count(|event| matches!(
                    event,
                    NodeHttpServerTimingEvent::HeadersComplete {
                        message_complete: false,
                        ..
                    }
                )),
                1
            );
            let timing = observed.clone();
            async move {
                let mut body = request.into_body();
                let frame = body.frame().await.unwrap().unwrap();
                assert_eq!(frame.data_ref().unwrap().as_ref(), b"x");
                assert_eq!(
                    timing
                        .count(|event| matches!(event, NodeHttpServerTimingEvent::MessageComplete)),
                    1
                );
                {
                    let batches = timing.batches.lock().unwrap();
                    let completed = batches
                        .iter()
                        .find(|batch| {
                            batch.iter().any(|event| {
                                matches!(event, NodeHttpServerTimingEvent::MessageComplete)
                            })
                        })
                        .unwrap();
                    assert!(matches!(
                        completed.as_slice(),
                        [NodeHttpServerTimingEvent::MessageComplete]
                    ));
                }
                assert!(body.frame().await.is_none());
                timing.stop();
                Ok::<_, Infallible>(Response::new(Empty::<Bytes>::new()))
            }
        });
        let mut builder = hyper::server::conn::http1::Builder::new();
        builder.node_http_server_timing(timing);
        let mut connection = Box::pin(builder.serve_connection(TokioIo::new(io), service));
        finish(connection.as_mut()).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn rejected_header_commit_prevents_service_admission_and_writes_only_raw408() {
        let io = FakeIo::request(1);
        let timing = Arc::new(Timing {
            reject_headers: true,
            ..Timing::default()
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let service = service_fn(move |_: Request<Incoming>| {
            counted.fetch_add(1, Ordering::SeqCst);
            async { Ok::<_, Infallible>(Response::new(Empty::<Bytes>::new())) }
        });
        let mut builder = hyper::server::conn::http1::Builder::new();
        builder.node_http_server_timing(timing.clone());
        let mut connection = Box::pin(builder.serve_connection(TokioIo::new(io.clone()), service));
        finish(connection.as_mut()).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            timing
                .count(|event| matches!(event, NodeHttpServerTimingEvent::HeadersComplete { .. })),
            1
        );
        assert_eq!(io.0.lock().unwrap().output, RAW_408);
    }

    #[test]
    fn latched_timeout_wins_over_a_service_future_error_in_the_same_poll() {
        let io = FakeIo::request(1);
        let timing = Arc::new(Timing::default());
        let observed = timing.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let service = service_fn(move |_: Request<Incoming>| {
            let timing = observed.clone();
            let calls = counted.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                *timing.timeout.lock().unwrap() = Some(NodeHttpServerTimeout::Request {
                    write_raw_408: true,
                });
                Err::<Response<Empty<Bytes>>, _>(io::Error::from(io::ErrorKind::ConnectionAborted))
            }
        });
        let mut builder = hyper::server::conn::http1::Builder::new();
        builder.node_http_server_timing(timing);
        let mut connection = Box::pin(builder.serve_connection(TokioIo::new(io.clone()), service));
        finish(connection.as_mut()).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(io.0.lock().unwrap().output, RAW_408);
    }

    #[derive(Clone, Copy)]
    enum WriteMode {
        Pending,
        Partial,
        Error,
    }
    #[derive(Default)]
    struct IoCounts {
        reads: usize,
        writes: usize,
        flushes: usize,
        shutdowns: usize,
        drops: usize,
    }
    struct PendingIo {
        mode: WriteMode,
        counts: Arc<Mutex<IoCounts>>,
    }
    impl AsyncRead for PendingIo {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            self.counts.lock().unwrap().reads += 1;
            Poll::Pending
        }
    }
    impl AsyncWrite for PendingIo {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.counts.lock().unwrap().writes += 1;
            assert_eq!(bytes, RAW_408);
            match self.mode {
                WriteMode::Pending => Poll::Pending,
                WriteMode::Partial => Poll::Ready(Ok(1)),
                WriteMode::Error => Poll::Ready(Err(io::ErrorKind::BrokenPipe.into())),
            }
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.counts.lock().unwrap().flushes += 1;
            Poll::Pending
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.counts.lock().unwrap().shutdowns += 1;
            Poll::Pending
        }
    }
    impl Drop for PendingIo {
        fn drop(&mut self) {
            self.counts.lock().unwrap().drops += 1;
        }
    }

    #[test]
    fn terminal_timeout_is_one_nonblocking_write_without_read_flush_or_shutdown() {
        for mode in [WriteMode::Pending, WriteMode::Partial, WriteMode::Error] {
            for timeout in [
                NodeHttpServerTimeout::Request {
                    write_raw_408: true,
                },
                NodeHttpServerTimeout::Request {
                    write_raw_408: false,
                },
                NodeHttpServerTimeout::Idle,
            ] {
                let timing = Arc::new(Timing {
                    timeout: Mutex::new(Some(timeout)),
                    ..Timing::default()
                });
                let counts = Arc::new(Mutex::new(IoCounts::default()));
                let calls = Arc::new(AtomicUsize::new(0));
                let counted = calls.clone();
                let service = service_fn(move |_: Request<Incoming>| {
                    counted.fetch_add(1, Ordering::SeqCst);
                    async { Ok::<_, Infallible>(Response::new(Empty::<Bytes>::new())) }
                });
                let stream = PendingIo {
                    mode,
                    counts: counts.clone(),
                };
                let mut builder = hyper::server::conn::http1::Builder::new();
                builder.node_http_server_timing(timing);
                let mut connection =
                    Box::pin(builder.serve_connection(TokioIo::new(stream), service));
                assert!(matches!(
                    poll_once(connection.as_mut()),
                    Poll::Ready(Ok(()))
                ));
                assert_eq!(calls.load(Ordering::SeqCst), 0);
                let trace = counts.lock().unwrap();
                assert_eq!(trace.reads, 0);
                assert_eq!(
                    trace.writes,
                    usize::from(matches!(
                        timeout,
                        NodeHttpServerTimeout::Request {
                            write_raw_408: true
                        }
                    ))
                );
                assert_eq!(trace.flushes, 0);
                assert_eq!(trace.shutdowns, 0);
                assert_eq!(trace.drops, 0);
                drop(trace);
                drop(connection);
                assert_eq!(counts.lock().unwrap().drops, 1);
            }
        }
    }
}
