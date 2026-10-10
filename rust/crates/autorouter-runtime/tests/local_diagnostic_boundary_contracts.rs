//! Full original diagnostic boundary callbacks with explicit Rust API migration.
#[path = "support/local_diagnostic_boundary.rs"]
mod support;
use autorouter_core::config::read_config;
use autorouter_runtime::evaluator::EvaluationError;
use autorouter_runtime::local_diagnostic::{
    run_local_diagnostic, run_local_diagnostic_with_progress,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use support::{Replay, captured, check_progress, check_report, settings};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn original_invalid_representable_settings_return_safe_reports_without_io() {
    let rows = captured();
    for index in [0, 1, 2, 4] {
        let row = &rows[index];
        let token = CancellationToken::new();
        let replay = Replay::new(row, &token);
        let report = run_local_diagnostic(replay.clone(), &settings(row), &token)
            .await
            .unwrap();
        assert_eq!(report["passed"], false); // diagnostic#6:assert-2 (four representable cases)
        assert_eq!(report["error"]["code"], row["output"]["error"]["code"]); // diagnostic#6:assert-3
        assert!(!report.to_string().contains("PRIVATE_")); // diagnostic#6:assert-4
        check_report(&report, &[], row);
        assert!(replay.verified());
        assert_eq!(replay.counts().requests, 0); // #6:assert-1 remains unreachable
        assert!(!token.is_cancelled());
    }
}
#[test]
fn nan_timeout_is_a_typed_configuration_admission_boundary_not_a_native_report() {
    let rows = captured();
    assert_eq!(
        rows[3]["config"]["ollamaTimeoutMs"],
        json!({"$js_type":"nan"})
    );
    // RouterConfig.ollama_timeout_ms is u64 and cannot hold JavaScript NaN.
    // Exercise the real string configuration admission instead; this is an
    // explicit API migration, not the source's fifth diagnostic report.
    let result = read_config(
        &json!({"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_OLLAMA_MODEL":"tev1:4b-q4_K_M","AUTOROUTER_OLLAMA_TIMEOUT_MS":"NaN"}),
        false,
        std::path::Path::new("/synthetic"),
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("NaN must not enter typed configuration"),
    };
    assert_eq!(
        error,
        "AUTOROUTER_OLLAMA_TIMEOUT_MS must be an integer between 0 and 30000 (0 disables the runtime deadline)"
    );
    assert!(!error.contains("PRIVATE_"));
}
#[tokio::test]
async fn original_cancellation_before_io_and_at_second_inference_releases_owned_work() {
    let rows = captured();
    for index in [5, 6] {
        let row = &rows[index];
        let token = CancellationToken::new();
        let replay = Replay::new(row, &token);
        if index == 5 {
            token.cancel();
        }
        let mut progress = Vec::new();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            run_local_diagnostic_with_progress(replay.clone(), &settings(row), &token, |event| {
                progress.push(event.clone());
                Ok(())
            }),
        )
        .await
        .expect("bounded original cancellation");
        // diagnostic#12:assert-1/3: typed cancellation; arbitrary Error reason
        // regexes cancelled-before/cancelled-during are explicitly unrepresented.
        assert!(matches!(result, Err(EvaluationError::Cancelled)));
        assert!(token.is_cancelled());
        assert!(replay.verified());
        assert_eq!(replay.counts().requests, if index == 5 { 0 } else { 9 });
        assert_eq!(replay.counts().requests_dropped, replay.counts().requests);
        assert_eq!(
            replay.counts().bodies_dropped,
            if index == 5 { 0 } else { 8 }
        );
        check_progress(&progress, row);
        if index == 6 {
            assert_eq!(
                replay
                    .calls()
                    .iter()
                    .filter(|r| r["url"].as_str().unwrap().ends_with("/v1/systemone"))
                    .count(),
                2
            ); // diagnostic#12:assert-4
            assert_eq!(settings(row).ollama_timeout_ms, 0);
        }
        // Zero-request before branch preserves diagnostic#12:assert-2 unreachability.
    }
}
#[tokio::test]
async fn original_stalled_residency_cancellation_drops_reader_before_return_without_task_text() {
    let row = &captured()[7];
    let token = CancellationToken::new();
    let replay = Replay::new(row, &token);
    let mut progress = Vec::new();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        run_local_diagnostic_with_progress(replay.clone(), &settings(row), &token, |event| {
            progress.push(event.clone());
            Ok(())
        }),
    )
    .await
    .expect("bounded residency cancellation");
    assert!(matches!(result, Err(EvaluationError::Cancelled))); // diagnostic#13:assert-1 typed migration; arbitrary reason unavailable
    assert_eq!(replay.counts().stalled_created, 1);
    assert_eq!(replay.counts().stalled_dropped, 1); // diagnostic#13:assert-2
    assert!(replay.verified());
    assert_eq!(replay.counts().bodies_dropped, 4);
    assert_eq!(
        replay
            .calls()
            .iter()
            .filter(|r| r["url"].as_str().unwrap().ends_with("/v1/systemone"))
            .count(),
        0
    ); // diagnostic#13:assert-3
    assert!(replay.calls().iter().all(|r| r["body"].is_null()
        || r["body"].get("model").is_some() && r["body"].as_object().unwrap().len() == 1));
    check_progress(&progress, row);
}
async fn callback_case(index: usize, panic_event: Option<&'static str>) {
    let rows = captured();
    let row = rows[index].clone();
    let token = CancellationToken::new();
    let replay = Replay::new(&row, &token);
    let observed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let owned = replay.clone();
    let progress = observed.clone();
    let config = settings(&row);
    let task_token = token.clone();
    let mut task = tokio::spawn(async move {
        run_local_diagnostic_with_progress(owned, &config, &task_token, |event| {
            {
                let mut events = progress.lock().unwrap();
                assert!(events.len() < 16);
                events.push(event.clone());
            }
            // The hook is caller-controlled and still runs. No global hook is
            // installed/replaced, and no silent panic handling is claimed.
            if panic_event.is_some_and(|kind| kind == "all" || event["event"] == kind) {
                panic!("synthetic unavailable");
            }
            Err(())
        })
        .await
    });
    let outcome = match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
        Ok(outcome) => outcome,
        Err(_) => {
            token.cancel();
            task.abort();
            let _ = task.await;
            panic!("bounded progress diagnostic completion");
        }
    };
    assert!(
        replay.idle(),
        "all owned I/O must already be released after task join"
    );
    // This is the deterministic pre-fix failure: the original callback panic
    // escapes at preflight before any I/O, instead of completing the diagnostic.
    eprintln!(
        "DIAGNOSTIC_PROGRESS_BOUNDARY requests={} request_live={} bodies_live={} progress={} task_panicked={}",
        replay.counts().requests,
        replay.counts().requests_live,
        replay.counts().bodies_live,
        observed.lock().unwrap().len(),
        outcome.as_ref().is_err_and(|error| error.is_panic()),
    );
    let report = outcome
        .expect("original diagnostic#14 progress panic must not escape")
        .unwrap();
    assert_eq!(report["passed"], true); // diagnostic#14:assert-1 (throw and Result-error migration)
    let events = observed.lock().unwrap().clone();
    assert_eq!(events.len(), 15);
    assert!(replay.verified());
    assert_eq!(replay.counts().requests, 24);
    assert_eq!(replay.counts().requests_dropped, 24);
    assert_eq!(replay.counts().bodies_dropped, 24);
    assert!(!token.is_cancelled());
    check_report(&report, &events, &row);
}
#[tokio::test]
async fn original_progress_callback_panic_cannot_prevent_complete_diagnostic() {
    callback_case(8, Some("all")).await;
}
#[tokio::test]
async fn original_rejected_progress_promise_migrates_to_synchronous_result_error() {
    callback_case(9, None).await;
}
#[tokio::test]
async fn callback_panic_containment_covers_each_original_progress_site_and_repeats() {
    for kind in [
        "preflight",
        "startup",
        "startup_complete",
        "case_start",
        "case_complete",
    ] {
        callback_case(8, Some(kind)).await;
    }
}
