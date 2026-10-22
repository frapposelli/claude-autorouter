//! Original timeout-disabled and finite-timeout diagnostic definitions.
#[path = "support/local_diagnostic_timeout.rs"]
mod support;
use autorouter_runtime::evaluator::EvaluationError;
use autorouter_runtime::local_diagnostic::{
    format_local_diagnostic, run_local_diagnostic, run_local_diagnostic_with_progress,
};
use serde_json::{Value, json};
use std::time::Duration;
use support::{
    Replay, Schedule, captured, comparison, expected_progress, request_observation, settings,
    verify_undefined,
};
use tokio_util::sync::CancellationToken;

async fn original(index: usize) {
    let (rows, _) = captured();
    let row = &rows[index];
    let replay = Replay::new(row, &rows[0], Schedule::RuntimeDelay);
    let cancellation = CancellationToken::new();
    let mut progress = Vec::new();
    let report = tokio::time::timeout(
        Duration::from_secs(2),
        run_local_diagnostic_with_progress(
            replay.clone(),
            &settings(row),
            &cancellation,
            |event| {
                assert!(progress.len() < 64);
                progress.push(event.clone());
                Ok(())
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        comparison(&report, &progress, index == 0).unwrap(),
        comparison(&row["report"]["value"], &expected_progress(row), index == 0).unwrap()
    );
    assert!(replay.verified());
    assert_eq!(
        replay.observed(),
        row["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(request_observation)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        replay.counts(),
        if index == 0 {
            (0, 6, 6, 0)
        } else {
            (0, 6, 0, 6)
        }
    );
    assert!(!cancellation.is_cancelled());
    verify_undefined(&report, &row["report"]["tags"]);
    for (event, expected) in progress.iter().zip(row["progress"].as_array().unwrap()) {
        verify_undefined(event, &expected["tags"]);
        if event["event"] == "startup_complete" {
            assert_eq!(event["latency_ms"], report["startup"]["latency_ms"]);
        } else if event["event"] == "case_complete" {
            let matching = report["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["case"] == event["case"])
                .unwrap();
            assert_eq!(event["latency_ms"], matching["latency_ms"]);
        }
    }
    assert_eq!(
        json!(format_local_diagnostic(&row["report"]["value"])),
        row["formatted"]
    );
    assert_eq!(report["runtime_timeout_ms"], if index == 0 { 0 } else { 2 });
    assert_eq!(report["startup"]["source"], "ollama");
    assert_eq!(report["startup"]["timeout_ms"], 60000);
    assert_eq!(report["passed"], index == 0);
    assert_eq!(report["rows"].as_array().unwrap().len(), 6);
    let requests = row["requests"].as_array().unwrap();
    for request in requests {
        assert_eq!(request["node_options"]["redirect"], "error");
        assert_eq!(request["node_options"]["signal_kind"], "AbortSignal");
        assert_eq!(request["node_options"]["signal_aborted_before"], false);
        let timed_out = request.get("error").is_some();
        assert_eq!(request["node_options"]["signal_aborted_after"], timed_out);
        if timed_out {
            assert_eq!(request["error"]["name"], "TimeoutError");
            assert_eq!(request["error"]["same_signal_reason"], true);
        }
    }
    if index == 0 {
        assert_eq!(requests[5]["node_options"]["signal_same_as_caller"], false);
        assert!(
            report["rows"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["source"] == "ollama" && r["latency_ms"].as_f64().unwrap() >= 4.0)
        );
        assert!(format_local_diagnostic(&report)[1].contains("Runtime deadline: disabled"));
    } else {
        assert_eq!(report["gates"]["evaluator"]["fallbacks"], 6);
        assert_eq!(
            report["gates"]["coverage"]["missing"],
            json!(["haiku", "sonnet", "opus"])
        );
        assert!(
            report["rows"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["source"] == "fallback"
                    && r["classifier_error"] == "timeout"
                    && r.get("classified_tier").is_none()
                    && r["tier"] == "sonnet")
        );
    }
}

#[tokio::test]
async fn original_zero_deadline_completes_all_delayed_cases_with_real_elapsed_predicate() {
    original(0).await;
}
#[tokio::test(start_paused = true)]
async fn original_finite_deadline_reports_six_actual_timeouts_without_false_classification() {
    original(1).await;
}

#[tokio::test(start_paused = true)]
async fn zero_runtime_keeps_independent_exact_startup_deadline_and_releases_owned_request() {
    let (rows, _) = captured();
    let replay = Replay::new(&rows[0], &rows[0], Schedule::PendingStartup);
    let config = settings(&rows[0]);
    let cancellation = CancellationToken::new();
    let mut tasks = tokio::task::JoinSet::new();
    let owned = replay.clone();
    let token = cancellation.clone();
    tasks.spawn(async move { run_local_diagnostic(owned, &config, &token).await });
    tokio::time::timeout(Duration::from_secs(2), replay.wait_pending())
        .await
        .expect("owned pending request barrier");
    let start = tokio::time::Instant::now();
    tokio::time::advance(Duration::from_millis(59999)).await;
    tokio::task::yield_now().await;
    assert!(tasks.try_join_next().is_none());
    assert_eq!(replay.counts(), (1, 1, 0, 0));
    tokio::time::advance(Duration::from_millis(1)).await;
    let report = tokio::time::timeout(Duration::from_millis(1), tasks.join_next())
        .await
        .expect("startup deadline must fire at 60000ms")
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(start.elapsed(), Duration::from_millis(60000));
    assert_eq!(report["startup"]["classifier_error"], "timeout");
    assert_eq!(report["startup"]["timeout_ms"], 60000);
    assert_eq!(report["runtime_timeout_ms"], 0);
    assert_eq!(report["error"]["code"], "startup_failed");
    assert_eq!(report["rows"], json!([]));
    assert_eq!(replay.observed().len(), 6);
    assert_eq!(replay.counts(), (0, 1, 0, 1));
    assert!(!cancellation.is_cancelled());
}

#[tokio::test(start_paused = true)]
async fn zero_runtime_stays_pending_beyond_startup_budget_but_caller_can_cancel() {
    let (rows, _) = captured();
    let replay = Replay::new(&rows[0], &rows[0], Schedule::PendingRuntime);
    let config = settings(&rows[0]);
    let cancellation = CancellationToken::new();
    let mut tasks = tokio::task::JoinSet::new();
    let owned = replay.clone();
    let token = cancellation.clone();
    tasks.spawn(async move { run_local_diagnostic(owned, &config, &token).await });
    tokio::time::timeout(Duration::from_secs(2), replay.wait_pending())
        .await
        .expect("owned pending request barrier");
    tokio::time::advance(Duration::from_millis(60001)).await;
    tokio::task::yield_now().await;
    assert!(tasks.try_join_next().is_none());
    assert_eq!(replay.counts(), (1, 1, 0, 0));
    assert!(!cancellation.is_cancelled());
    cancellation.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), tasks.join_next())
            .await
            .expect("caller cancellation must complete")
            .unwrap()
            .unwrap(),
        Err(EvaluationError::Cancelled)
    ));
    // Classifier cancellation owns cleanup; drain it before declaring quiescence.
    for _ in 0..32 {
        if replay.counts().0 == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(replay.counts(), (0, 1, 0, 1));
    assert_eq!(replay.observed().len(), 9);
}

#[tokio::test(start_paused = true)]
async fn wrong_request_cannot_hide_behind_same_timeout_report() {
    let (rows, _) = captured();
    let mut row = rows[1].clone();
    row["requests"][8]["body"]["state"]["current_task"] = json!("incorrect synthetic task");
    row["requests"][8]["body_text"] = json!(row["requests"][8]["body"].to_string());
    let replay = Replay::new(&row, &rows[0], Schedule::RuntimeDelay);
    let mut progress = Vec::new();
    let report = run_local_diagnostic_with_progress(
        replay.clone(),
        &settings(&row),
        &CancellationToken::new(),
        |v| {
            progress.push(v.clone());
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(
        comparison(&report, &progress, false).unwrap(),
        comparison(&row["report"]["value"], &expected_progress(&row), false).unwrap()
    );
    assert!(!replay.verified());
    assert_eq!(replay.counts(), (0, 6, 0, 6));
}

#[test]
fn comparison_rejects_missing_elapsed_predicate_or_changed_semantics_and_corpus() {
    let (rows, capture) = captured();
    let row = &rows[0];
    let progress = expected_progress(row);
    for value in [json!(3.99), json!(-1), Value::Null, json!("6")] {
        let mut report = row["report"]["value"].clone();
        report["rows"][0]["latency_ms"] = value;
        assert!(comparison(&report, &progress, true).is_err());
    }
    let original = comparison(&row["report"]["value"], &progress, true).unwrap();
    let mut report = row["report"]["value"].clone();
    report["runtime_timeout_ms"] = json!(2);
    assert_ne!(comparison(&report, &progress, true).unwrap(), original);
    let changed =
        support::CORPUS.replacen("Runtime deadline: disabled", "Runtime deadline: enabled", 1);
    assert_ne!(changed, support::CORPUS);
    let mut capture = capture;
    use sha2::{Digest, Sha256};
    capture["cases_sha256"] = json!(format!("{:x}", Sha256::digest(&changed)));
    assert!(support::validate(&changed, &capture).is_err());
}
