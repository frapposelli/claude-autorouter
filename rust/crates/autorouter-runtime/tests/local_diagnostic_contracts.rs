//! Seven unchanged frozen diagnostic callbacks over synthetic finite transport.
#[path = "support/local_diagnostic.rs"]
mod support;
use autorouter_runtime::local_diagnostic::format_local_diagnostic;
use serde_json::{Value, json};
use support::{
    Replay, cases, comparison, execute, expected_progress, request_observation, verify_undefined,
};

#[tokio::test]
async fn original_finite_diagnostic_reports_progress_requests_and_formatter_match() {
    let mut request_count = 0;
    let mut event_count = 0;
    let mut timing_count = 0;
    let mut source_formatter_calls = 0;
    for row in cases() {
        let requests = row["requests"].as_array().unwrap().clone();
        let replay = Replay::new(requests.clone());
        let (report, progress) = execute(&row, replay.clone()).await;
        let expected_events = expected_progress(&row);
        let (expected, paths) = comparison(&row["report"]["value"], &expected_events).unwrap();
        let (actual, actual_paths) = comparison(&report, &progress).unwrap();
        assert_eq!(actual_paths, paths, "{} timing presence", row["id"]);
        assert_eq!(actual, expected, "{} full report and progress", row["id"]);
        assert_eq!(
            replay.observed(),
            requests.iter().map(request_observation).collect::<Vec<_>>(),
            "{} complete request bytes and values",
            row["id"]
        );
        assert!(replay.verified(), "{} request sequence", row["id"]);
        verify_undefined(
            &report,
            &row["report"]["tags"],
            &["classifier_error", "classifier_status", "classified_tier"],
        );
        for (event, captured) in progress.iter().zip(row["progress"].as_array().unwrap()) {
            verify_undefined(
                event,
                &captured["tags"],
                &["classifier_error", "classifier_status", "classified_tier"],
            );
        }
        // Reuse the original report's raw elapsed numbers. No formatter string
        // or input field is excluded, rounded, rewritten or replaced here.
        assert_eq!(
            json!(format_local_diagnostic(&row["report"]["value"])),
            row["formatted"],
            "{} exact original formatter",
            row["id"]
        );
        for request in &requests {
            assert_eq!(request["node_options"]["redirect"], "error");
            assert_eq!(request["node_options"]["signal_kind"], "AbortSignal");
            assert_eq!(request["node_options"]["signal_aborted_before"], false);
            assert_eq!(request["node_options"]["signal_aborted_after"], false);
            assert_eq!(request["response"]["json_input_tags"], json!([]));
        }
        request_count += requests.len();
        event_count += progress.len();
        timing_count += paths.len();
        source_formatter_calls += row["source_formatter_calls"].as_u64().unwrap();
    }
    assert_eq!(
        (
            request_count,
            event_count,
            timing_count,
            source_formatter_calls
        ),
        (102, 58, 48, 3)
    );
}

#[tokio::test]
async fn wrong_request_cannot_hide_behind_matching_safe_failure_or_success_report() {
    for id in [
        "baseline-local-diagnostic-5-3",
        "baseline-local-diagnostic-1-1",
    ] {
        let row = cases().into_iter().find(|row| row["id"] == id).unwrap();
        let mut requests = row["requests"].as_array().unwrap().clone();
        if id.ends_with("5-3") {
            requests[0]["url"] = json!("http://127.0.0.1:11434/wrong-path");
        } else {
            let request = requests
                .iter_mut()
                .find(|r| r["url"].as_str().unwrap().ends_with("/v1/systemone"))
                .unwrap();
            request["body"]["state"]["current_task"] = json!("wrong synthetic task");
            request["body_text"] = json!(request["body"].to_string());
        }
        let replay = Replay::new(requests);
        let (report, progress) = execute(&row, replay.clone()).await;
        assert_eq!(
            comparison(&report, &progress).unwrap(),
            comparison(&row["report"]["value"], &expected_progress(&row)).unwrap()
        );
        assert!(
            !replay.verified(),
            "Matching product report must not conceal mismatched request"
        );
    }
}

#[test]
fn comparison_preserves_semantics_and_requires_every_timing_observation() {
    let row = cases().remove(0);
    let report = &row["report"]["value"];
    let progress = expected_progress(&row);
    let original = comparison(report, &progress).unwrap();
    for replacement in [Value::Null, json!("0"), json!(-1)] {
        let mut changed = report.clone();
        changed["startup"]["latency_ms"] = replacement;
        assert!(comparison(&changed, &progress).is_err());
    }
    let mut missing = report.clone();
    missing["rows"][0]
        .as_object_mut()
        .unwrap()
        .remove("latency_ms");
    assert!(comparison(&missing, &progress).is_err());
    let mut changed = report.clone();
    changed["gates"]["coverage"]["passed"] = json!(false);
    assert_ne!(comparison(&changed, &progress).unwrap(), original);
    let mut changed = progress.clone();
    changed[1]["residency_before"] = json!("wrong-state");
    assert_ne!(comparison(report, &changed).unwrap(), original);
    let mut extra = report.clone();
    extra["unexpected"] = json!({"latency_ms":123});
    assert_ne!(
        comparison(&extra, &progress).unwrap(),
        original,
        "Unlisted elapsed-name fields must not be erased"
    );
    let mut formatter_input = report.clone();
    formatter_input["startup"]["latency_ms"] = json!(1234567.89);
    assert_ne!(
        json!(format_local_diagnostic(&formatter_input)),
        row["formatted"],
        "Formatter cannot normalize elapsed values"
    );
}
