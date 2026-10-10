//! Five unchanged frozen finite Ollama definitions, with exact state and wire data.
#[path = "support/ollama_evaluator_finite.rs"]
mod support;
use serde_json::json;
use support::{Replay, cases, evaluate, request_observation, state};

#[tokio::test]
async fn original_finite_ollama_state_alias_metadata_and_error_contracts_match() {
    let rows = cases();
    let mut state_calls = 0;
    let mut requests = 0;
    let mut evaluated = 0;
    for row in &rows {
        let calls = row["state_calls"].as_array().unwrap();
        assert!(!calls.is_empty() && calls.len() <= 4);
        let actual_states: Vec<_> = calls.iter().map(state).collect();
        for (actual, call) in actual_states.iter().zip(calls) {
            assert_eq!(
                actual,
                call["output_json"].as_str().unwrap(),
                "{}",
                row["id"]
            );
        }
        state_calls += calls.len();
        let expected = row["requests"].as_array().unwrap().clone();
        if row["kind"] == "state" {
            assert!(expected.is_empty());
            assert!(row.get("result").is_none());
            continue;
        }
        assert_eq!(calls.len(), 1);
        let replay = Replay::new(expected.clone());
        let result = evaluate(row, &replay, &actual_states[0]).await;
        assert_eq!(result, row["result"], "{}", row["id"]);
        assert_eq!(
            replay.observed(),
            expected.iter().map(request_observation).collect::<Vec<_>>(),
            "{}",
            row["id"]
        );
        assert!(
            replay.verified(),
            "Complete request verification: {}",
            row["id"]
        );
        for request in &expected {
            assert_eq!(
                request["node_options"],
                json!({
                    "redirect":"error", "signal_kind":"AbortSignal", "signal_aborted_before":false,
                    "omitted_fields":[], "signal_aborted_after":false,
                })
            );
            assert_eq!(request["response"]["json_input_tags"], json!([]));
        }
        evaluated += 1;
        requests += expected.len();
    }
    assert_eq!(
        (rows.len(), state_calls, evaluated, requests),
        (15, 24, 10, 18)
    );
}

#[tokio::test]
async fn wrong_request_is_detected_even_when_scripted_success_or_safe_failure_matches() {
    for (id, mutation) in [
        ("baseline-ollama-evaluator-finite-6-1", "metadata_path"),
        ("baseline-ollama-evaluator-finite-3-1", "native_state"),
    ] {
        let row = cases().into_iter().find(|row| row["id"] == id).unwrap();
        let mut requests = row["requests"].as_array().unwrap().clone();
        if mutation == "metadata_path" {
            requests[0]["url"] = json!("http://127.0.0.1:11434/wrong-metadata-path");
        } else {
            requests[1]["body"]["state"]["current_task"] = json!("wrong synthetic task");
            requests[1]["body_text"] = json!(requests[1]["body"].to_string());
        }
        let replay = Replay::new(requests);
        let actual_state = state(&row["state_calls"][0]);
        let result = evaluate(&row, &replay, &actual_state).await;
        assert_eq!(result, row["result"], "The scripted outcome still matches");
        assert!(
            !replay.verified(),
            "Matching outcome must not hide wrong request"
        );
    }
}
