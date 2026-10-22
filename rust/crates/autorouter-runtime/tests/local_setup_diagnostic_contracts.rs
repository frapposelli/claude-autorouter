//! Literal finite baseline replay plus independently bounded cancellation evidence.
#[path = "support/local_ollama.rs"]
mod local;
use autorouter_runtime::evaluator::EvaluationError;
use autorouter_runtime::local_diagnostic::run_local_diagnostic;
use autorouter_runtime::ollama_setup::{SetupOptions, inspect_ollama, setup_ollama};
use local::{Lifetime, Mock, Step, config, details, tags, tags_for, version};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn json_observation(value: &Value) -> Value {
    // Reports cross the CLI boundary as JSON.stringify-compatible JSON. Keep
    // every field while treating 0.0 and 0 as the same JavaScript Number.
    let document =
        autorouter_core::js_json::JsDocument::parse(&serde_json::to_vec(value).unwrap()).unwrap();
    serde_json::from_str(&document.stringify()).unwrap()
}

#[tokio::test]
async fn frozen_inspection_and_residency_failure_transcripts_match_without_private_diagnostics() {
    let corpus = include_str!("../../../parity/cases/local-setup-diagnostic.jsonl");
    let mut count = 0;
    for line in corpus.lines() {
        let case: Value = serde_json::from_str(line).unwrap();
        let transport = Mock::new(
            case["responses"]
                .as_array()
                .unwrap()
                .iter()
                .map(Step::captured)
                .collect(),
        );
        let config = autorouter_core::config::read_config(
            &case["env"],
            false,
            std::path::Path::new("/synthetic"),
        )
        .unwrap();
        let observed = match case["kind"].as_str().unwrap() {
            "inspect" => {
                let error =
                    inspect_ollama(transport.as_ref(), &config, &CancellationToken::new(), 5000)
                        .await
                        .unwrap_err();
                assert!(std::error::Error::source(&error).is_none());
                json!({"error":{"code":error.code,"message":error.message,"has_cause":false}})
            }
            "diagnostic" => {
                let report =
                    run_local_diagnostic(transport.clone(), &config, &CancellationToken::new())
                        .await
                        .unwrap();
                assert_eq!(report["passed"], false);
                assert_eq!(report["error"]["code"], "residency_unavailable");
                assert!(!transport.paths().iter().any(|p| p == "/v1/systemone"));
                json!({"report":report})
            }
            _ => panic!("unknown finite contract"),
        };
        assert!(!observed.to_string().contains("PRIVATE_"));
        let mut observed = observed;
        observed["calls"] = json!(*transport.calls.lock().unwrap());
        assert_eq!(
            json_observation(&observed),
            case["node_expected"],
            "{}",
            case["id"]
        );
        transport.assert_consumed_and_released();
        count += 1;
    }
    assert_eq!(count, 9);
}

#[tokio::test(start_paused = true)]
async fn setup_warm_and_pull_deadlines_cancel_pending_bodies_without_leaking_reason_or_warming_after_pull()
 {
    for pull in [false, true] {
        for caller_cancel in [false, true] {
            // Repeat warmup under zero runtime budget: its own deadline and
            // caller cancellation remain active while config stays unchanged.
            for runtime_zero in if pull { vec![false] } else { vec![false, true] } {
                let lifetime = Arc::new(Lifetime::default());
                let mut config = config();
                config.ollama_model = "nimble:9b-q4_K_M".into();
                let mut steps = vec![version(), tags_for(!pull, &config.ollama_model)];
                if !pull {
                    steps.extend([
                        Step::json(json!({"details":{"parameter_size":"9B"},"capabilities":["completion"]})),
                        Step::json(json!({"details":{"parameter_size":"9B"},"capabilities":["completion"]})),
                    ]);
                }
                steps.push(Step::pending(lifetime.clone()));
                let transport = Mock::new(steps);
                if runtime_zero {
                    config.ollama_timeout_ms = 0;
                }
                let original_runtime = config.ollama_timeout_ms;
                let cancel = CancellationToken::new();
                let requested = cancel.clone();
                let options = SetupOptions {
                    pull,
                    warm: !pull,
                    pull_timeout_ms: if caller_cancel { 1000 } else { 20 },
                    warm_timeout_ms: if caller_cancel { 1000 } else { 20 },
                    ..Default::default()
                };
                let stop = async {
                    lifetime.ready.notified().await;
                    assert!(lifetime.polled.load(Ordering::SeqCst));
                    if caller_cancel {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        requested.cancel();
                    }
                };
                let start = tokio::time::Instant::now();
                let wall_start = std::time::Instant::now();
                let mut progress = Vec::new();
                let mut sink = |line| progress.push(line);
                let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
                    tokio::join!(
                        setup_ollama(transport.as_ref(), &config, &cancel, &options, &mut sink),
                        stop
                    )
                })
                .await
                .expect("setup cancellation/deadline must finish within the fixture bound");
                let error = result.unwrap_err();
                assert_eq!(
                    error.code,
                    if caller_cancel {
                        "OLLAMA_CANCELLED"
                    } else {
                        "OLLAMA_TIMEOUT"
                    }
                );
                assert!(!error.message.contains("PRIVATE_"));
                assert!(std::error::Error::source(&error).is_none());
                assert!(start.elapsed() < Duration::from_millis(500));
                assert!(wall_start.elapsed() < Duration::from_millis(500));
                assert_eq!(
                    start.elapsed(),
                    Duration::from_millis(if caller_cancel { 10 } else { 20 })
                );
                assert_eq!(config.ollama_timeout_ms, original_runtime);
                assert!(lifetime.dropped.load(Ordering::SeqCst));
                assert_eq!(
                    transport.paths(),
                    if pull {
                        vec!["/api/version", "/api/tags", "/api/pull"]
                    } else {
                        vec![
                            "/api/version",
                            "/api/tags",
                            "/api/show",
                            "/api/show",
                            "/v1/systemone",
                        ]
                    }
                );
                if pull {
                    assert!(!progress.iter().any(|s: &String| s.contains("Preloading")));
                } else {
                    let calls = transport.calls.lock().unwrap();
                    let body = &calls.last().unwrap()["body"];
                    assert_eq!(
                        body["state"]["current_task"],
                        "Return the literal word ready."
                    );
                    assert_eq!(body["keep_alive"], "5m");
                    assert!(
                        body.get("messages").is_none()
                            && body.get("think").is_none()
                            && body.get("options").is_none()
                    );
                }
                transport.assert_consumed_and_released();
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn cancellation_of_stalled_residency_releases_reader_before_any_synthetic_task() {
    let lifetime = Arc::new(Lifetime::default());
    let transport = Mock::new(vec![
        version(),
        tags(true),
        details(),
        Step::pending(lifetime.clone()),
    ]);
    let cancel = CancellationToken::new();
    let requested = cancel.clone();
    let stop = async {
        lifetime.ready.notified().await;
        assert!(lifetime.polled.load(Ordering::SeqCst));
        requested.cancel();
    };
    let config = config();
    let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(
            run_local_diagnostic(transport.clone(), &config, &cancel),
            stop
        )
    })
    .await
    .expect("residency cancellation must finish within the fixture bound");
    assert_eq!(result.unwrap_err(), EvaluationError::Cancelled);
    assert!(lifetime.dropped.load(Ordering::SeqCst));
    assert_eq!(
        transport.paths(),
        ["/api/version", "/api/tags", "/api/show", "/api/ps"]
    );
    transport.assert_consumed_and_released();
}

// Public setup uses one default inspection budget across version, tags and show.
#[tokio::test(start_paused = true)]
async fn default_inspection_deadline_spans_metadata_and_retries_after_owned_timeout() {
    use autorouter_runtime::http_client::{HttpError, HttpTransport};
    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::{Request, Response};
    use std::pin::Pin;
    use std::task::Poll;

    // Reuse the existing full request projection and owned bodies. Only the
    // first version and tags response heads are delayed in the late cases.
    struct InspectionTransport {
        mock: Arc<Mock>,
        delay_first_two: bool,
    }
    impl HttpTransport for InspectionTransport {
        type ResponseBody = local::MockBody;
        async fn request(
            &self,
            request: Request<Full<Bytes>>,
        ) -> Result<Response<Self::ResponseBody>, HttpError> {
            if self.delay_first_two && self.mock.paths().len() < 2 {
                tokio::time::sleep(Duration::from_millis(2000)).await;
            }
            self.mock.request(request).await
        }
    }
    async fn pending_once<F: std::future::Future>(mut future: Pin<&mut F>) {
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
    async fn ready_once<F: std::future::Future>(mut future: Pin<&mut F>) -> F::Output {
        std::future::poll_fn(|cx| match future.as_mut().poll(cx) {
            Poll::Ready(result) => Poll::Ready(result),
            Poll::Pending => panic!("default inspection must be ready at 5000ms"),
        })
        .await
    }

    for hold_show in [false, true] {
        for hold_body in [false, true] {
            let config = autorouter_core::config::read_config(
                &json!({}),
                false,
                std::path::Path::new("/synthetic"),
            )
            .unwrap();
            assert_eq!(config.ollama_model, "nimble:9b-q4_K_M");
            let config_before = serde_json::to_value(&config).unwrap();
            let options = SetupOptions {
                warm: false,
                ..Default::default()
            };
            assert_eq!(options.inspect_timeout_ms, 5000);
            let held = Arc::new(Lifetime::default());
            let metadata = || {
                vec![
                    version(),
                    tags_for(true, &config.ollama_model),
                    Step::json(
                        json!({"details":{"parameter_size":"9B"},"capabilities":["completion"]}),
                    ),
                ]
            };
            let mut steps = if hold_show {
                vec![version(), tags_for(true, &config.ollama_model)]
            } else {
                Vec::new()
            };
            steps.push(if hold_body {
                Step::pending(held.clone())
            } else {
                Step::PendingHead(held.clone())
            });
            steps.extend(metadata());
            let mock = Mock::new(steps);
            let transport = InspectionTransport {
                mock: mock.clone(),
                delay_first_two: hold_show,
            };
            let cancel = CancellationToken::new();
            let started = tokio::time::Instant::now();
            let mut progress = Vec::new();
            let failure = {
                let mut sink = |line| progress.push(line);
                let mut running = Box::pin(setup_ollama(
                    &transport, &config, &cancel, &options, &mut sink,
                ));
                pending_once(running.as_mut()).await;
                if hold_show {
                    assert!(mock.paths().is_empty());
                    tokio::time::advance(Duration::from_millis(2000)).await;
                    pending_once(running.as_mut()).await;
                    assert_eq!(mock.paths(), ["/api/version"]);
                    tokio::time::advance(Duration::from_millis(2000)).await;
                    pending_once(running.as_mut()).await;
                    assert_eq!(mock.paths(), ["/api/version", "/api/tags", "/api/show"]);
                } else {
                    assert_eq!(mock.paths(), ["/api/version"]);
                }
                assert!(held.polled.load(Ordering::SeqCst));
                assert!(!held.dropped.load(Ordering::SeqCst));
                let remaining = Duration::from_millis(4999) - started.elapsed();
                tokio::time::advance(remaining).await;
                pending_once(running.as_mut()).await;
                assert_eq!(started.elapsed(), Duration::from_millis(4999));
                assert!(!held.dropped.load(Ordering::SeqCst));
                assert!(!cancel.is_cancelled());
                tokio::time::advance(Duration::from_millis(1)).await;
                ready_once(running.as_mut()).await.unwrap_err()
            };
            assert_eq!(started.elapsed(), Duration::from_millis(5000));
            assert_eq!(failure.code, "OLLAMA_TIMEOUT");
            assert_eq!(
                failure.message,
                "Ollama operation timed out. Check Ollama and retry."
            );
            assert!(std::error::Error::source(&failure).is_none());
            assert!(held.dropped.load(Ordering::SeqCst));
            assert!(!cancel.is_cancelled());
            assert!(progress.is_empty());
            assert_eq!(mock.paths().len(), if hold_show { 3 } else { 1 });
            assert_eq!(serde_json::to_value(&config).unwrap(), config_before);

            let result = setup_ollama(&transport, &config, &cancel, &options, &mut |line| {
                progress.push(line)
            })
            .await
            .unwrap();
            assert_eq!(
                serde_json::to_value(result).unwrap(),
                json!({"model":"nimble:9b-q4_K_M","pulled":false,"warmed":false})
            );
            assert!(progress.is_empty());
            assert!(!cancel.is_cancelled());
            assert_eq!(started.elapsed(), Duration::from_millis(5000));
            assert_eq!(serde_json::to_value(&config).unwrap(), config_before);
            let expected = [
                json!({"url":"http://127.0.0.1:11434/api/version","method":"GET","headers":{},"body":null}),
                json!({"url":"http://127.0.0.1:11434/api/tags","method":"GET","headers":{},"body":null}),
                json!({"url":"http://127.0.0.1:11434/api/show","method":"POST","headers":{"content-type":"application/json"},"body":{"model":"nimble:9b-q4_K_M"}}),
            ];
            let failed_requests = if hold_show { 3 } else { 1 };
            let expected_calls = expected[..failed_requests]
                .iter()
                .chain(expected.iter())
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(*mock.calls.lock().unwrap(), expected_calls);
            mock.assert_consumed_and_released();
        }
    }
}

// Exercise the actual default operation timers, without waiting for real model
// startup/downloads or turning the routing timeout back on. Clock observations
// below are policy boundaries, not model latency or real-clock measurements.
#[tokio::test(start_paused = true)]
async fn default_pull_and_warmup_budgets_timeout_owned_work_and_allow_fresh_retries() {
    use autorouter_runtime::http_client::{HttpError, HttpTransport};
    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::{Request, Response};
    use std::pin::Pin;
    use std::sync::Mutex;
    use std::task::Poll;
    use tokio::time::Instant;

    struct Transport {
        mock: Arc<Mock>,
        pulling: bool,
        late: bool,
        phase_started: Mutex<Option<Instant>>,
    }
    impl HttpTransport for Transport {
        type ResponseBody = local::MockBody;
        async fn request(
            &self,
            request: Request<Full<Bytes>>,
        ) -> Result<Response<Self::ResponseBody>, HttpError> {
            let index = self.mock.paths().len();
            if index == if self.pulling { 2 } else { 3 } {
                let mut phase = self.phase_started.lock().unwrap();
                assert!(phase.is_none());
                *phase = Some(Instant::now());
            }
            if self.late && index < 2 {
                tokio::time::sleep(Duration::from_millis(2000)).await;
            }
            // Native warmup owns one budget across its model-metadata lookup
            // and classification. Spending40s here must leave20s, not60s.
            if self.late && !self.pulling && index == 3 {
                tokio::time::sleep(Duration::from_millis(40000)).await;
            }
            self.mock.request(request).await
        }
    }
    async fn pending<F: std::future::Future>(mut future: Pin<&mut F>) {
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
    async fn ready<F: std::future::Future>(mut future: Pin<&mut F>) -> F::Output {
        std::future::poll_fn(|cx| match future.as_mut().poll(cx) {
            Poll::Ready(result) => Poll::Ready(result),
            Poll::Pending => panic!("default operation not ready at its deadline"),
        })
        .await
    }

    for pulling in [false, true] {
        for late in [false, true] {
            for body in [false, true] {
                let wall = std::time::Instant::now();
                let config = autorouter_core::config::read_config(
                    &json!({"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_OLLAMA_MODEL":"tev1:4b-q4_K_M","AUTOROUTER_OLLAMA_TIMEOUT_MS":"0"}),
                    false,
                    std::path::Path::new("/synthetic"),
                )
                .unwrap();
                assert_eq!(config.ollama_timeout_ms, 0);
                let before = serde_json::to_value(&config).unwrap();
                let options = SetupOptions {
                    pull: pulling,
                    warm: !pulling,
                    ..Default::default()
                };
                assert_eq!(options.inspect_timeout_ms, 5000);
                assert_eq!(options.pull_timeout_ms, 900000);
                assert_eq!(options.warm_timeout_ms, 60000);
                let budget = if pulling { 900000 } else { 60000 };
                let held = Arc::new(Lifetime::default());
                let mut steps = vec![version(), tags(!pulling)];
                if !pulling {
                    steps.extend([details(), details()]);
                }
                steps.push(if body {
                    Step::pending(held.clone())
                } else {
                    Step::PendingHead(held.clone())
                });
                // These responses belong to the fresh call after timeout.
                steps.extend([version(), tags(!pulling)]);
                if pulling {
                    steps.extend([
                        Step::json(json!({"status":"success"})),
                        version(),
                        tags(true),
                        details(),
                    ]);
                } else {
                    steps.extend([
                        details(),
                        details(),
                        Step::json(json!({
                            "model":"tev1:4b-q4_K_M",
                            "answers":{"tier":{"type":"choice","choice":"haiku","probabilities":{"haiku":1,"sonnet":0,"opus":0},"confidence":1}},
                            "usage":{"input_tokens":900,"output_tokens":1}
                        })),
                    ]);
                }
                let mock = Mock::new(steps);
                let transport = Transport {
                    mock: mock.clone(),
                    pulling,
                    late,
                    phase_started: Mutex::new(None),
                };
                let caller = CancellationToken::new();
                let started = Instant::now();
                let mut progress = Vec::new();
                let (failure, phase) = {
                    let mut write = |line| progress.push(line);
                    let mut running = Box::pin(setup_ollama(
                        &transport, &config, &caller, &options, &mut write,
                    ));
                    pending(running.as_mut()).await;
                    if late {
                        tokio::time::advance(Duration::from_millis(2000)).await;
                        pending(running.as_mut()).await;
                        tokio::time::advance(Duration::from_millis(2000)).await;
                        pending(running.as_mut()).await;
                        if !pulling {
                            tokio::time::advance(Duration::from_millis(40000)).await;
                            pending(running.as_mut()).await;
                        }
                    }
                    let phase = transport.phase_started.lock().unwrap().unwrap();
                    assert_eq!(
                        phase - started,
                        Duration::from_millis(if late { 4000 } else { 0 })
                    );
                    assert!(held.polled.load(Ordering::SeqCst));
                    assert!(!held.dropped.load(Ordering::SeqCst));
                    let before_deadline = phase + Duration::from_millis(budget - 1);
                    tokio::time::advance(before_deadline - Instant::now()).await;
                    pending(running.as_mut()).await;
                    assert_eq!(Instant::now(), before_deadline);
                    assert!(!held.dropped.load(Ordering::SeqCst));
                    assert!(!caller.is_cancelled());
                    tokio::time::advance(Duration::from_millis(1)).await;
                    (ready(running.as_mut()).await.unwrap_err(), phase)
                };
                assert_eq!(Instant::now() - phase, Duration::from_millis(budget));
                assert_eq!(failure.code, "OLLAMA_TIMEOUT");
                assert_eq!(
                    failure.message,
                    "Ollama operation timed out. Check Ollama and retry."
                );
                assert!(std::error::Error::source(&failure).is_none());
                assert!(held.dropped.load(Ordering::SeqCst));
                assert!(!caller.is_cancelled());
                assert_eq!(mock.paths().len(), if pulling { 3 } else { 5 });
                assert_eq!(serde_json::to_value(&config).unwrap(), before);
                let start_line = if pulling {
                    "Downloading tev1:4b-q4_K_M with local Ollama. Model files are fetched from the model registry."
                } else {
                    "Preloading tev1:4b-q4_K_M in local Ollama."
                };
                assert_eq!(progress, [start_line]);

                let result = setup_ollama(&transport, &config, &caller, &options, &mut |line| {
                    progress.push(line)
                })
                .await
                .unwrap();
                assert_eq!(
                    serde_json::to_value(result).unwrap(),
                    json!({"model":"tev1:4b-q4_K_M","pulled":pulling,"warmed":!pulling})
                );
                assert_eq!(Instant::now() - phase, Duration::from_millis(budget));
                assert!(!caller.is_cancelled());
                assert_eq!(serde_json::to_value(&config).unwrap(), before);
                let expected = if pulling {
                    vec![
                        "/api/version",
                        "/api/tags",
                        "/api/pull",
                        "/api/version",
                        "/api/tags",
                        "/api/pull",
                        "/api/version",
                        "/api/tags",
                        "/api/show",
                    ]
                } else {
                    vec![
                        "/api/version",
                        "/api/tags",
                        "/api/show",
                        "/api/show",
                        "/v1/systemone",
                        "/api/version",
                        "/api/tags",
                        "/api/show",
                        "/api/show",
                        "/v1/systemone",
                    ]
                };
                assert_eq!(mock.paths(), expected);
                if pulling {
                    assert_eq!(
                        progress,
                        [start_line, start_line, "Ollama model download complete."]
                    );
                } else {
                    assert_eq!(progress, [start_line, start_line]);
                }
                mock.assert_consumed_and_released();
                assert!(wall.elapsed() < Duration::from_secs(5));
                eprintln!(
                    "DEFAULT_SETUP_BUDGET {}",
                    json!({"operation":if pulling{"pull"}else{"warmup"},"late":late,"held":if body{"body"}else{"head"},"default_ms":budget,"phase_offset_ms":if late{4000}else{0},"owned_timeout_ms":budget,"same_caller_retry":true,"requests":*mock.calls.lock().unwrap()})
                );
            }
        }
    }
}
