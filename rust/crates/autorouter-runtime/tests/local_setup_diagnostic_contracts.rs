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
