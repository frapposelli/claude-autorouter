//! Six original setup definitions replayed over bounded synthetic transport.
#[path = "support/local_finite.rs"]
mod support;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use support::{Replay, cases, execute, request_observation};

#[tokio::test]
async fn original_finite_setup_version_warmup_alias_and_model_contracts_match() {
    let rows = cases();
    let mut grouped = BTreeMap::<u64, Vec<Value>>::new();
    for row in &rows {
        grouped
            .entry(row["fetch_id"].as_u64().unwrap())
            .or_default()
            .extend(row["requests"].as_array().unwrap().iter().cloned());
    }
    assert_eq!(grouped.len(), 23);
    let replays: BTreeMap<_, _> = grouped
        .into_iter()
        .map(|(id, requests)| (id, Replay::new(requests)))
        .collect();
    let mut requests = 0;
    let mut omitted_version = 0;
    for row in &rows {
        if row["node_expected"]["ok"] == true {
            assert_eq!(row["result_tags"], json!([]));
        } else {
            assert_eq!(row["error_name"], "Error");
            assert!(row.get("result_tags").is_none());
        }
        let replay = &replays[&row["fetch_id"].as_u64().unwrap()];
        let start = replay.observed().len();
        let (actual, progress) = execute(row, replay).await;
        assert_eq!(actual, row["node_expected"], "{}", row["id"]);
        assert_eq!(json!(progress), row["progress"], "{}", row["id"]);
        let expected: Vec<_> = row["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(request_observation)
            .collect();
        assert_eq!(
            &replay.observed()[start..],
            expected.as_slice(),
            "{}",
            row["id"]
        );
        for request in row["requests"].as_array().unwrap() {
            let response = &request["response"];
            if !response["json_input_tags"].as_array().unwrap().is_empty() {
                assert_eq!(row["id"], "baseline-local-finite-1-5");
                assert_eq!(
                    response["json_input_tags"],
                    json!([{"path":"$.version","kind":"undefined"}])
                );
                assert_eq!(response["body"]["text"], "{}");
                omitted_version += 1;
            }
        }
        requests += expected.len();
    }
    // The source's read-only inspect and missing setup share the same fetch
    // closure. Replay both against one queue, retaining the exact2+2GET trace.
    let pair: Vec<_> = rows
        .iter()
        .filter(|row| row["source_tests"] == json!(["test/ollama-setup.test.mjs#5"]))
        .collect();
    assert_eq!(pair.len(), 2);
    assert_eq!(pair[0]["fetch_id"], pair[1]["fetch_id"]);
    assert!(replays.values().all(|replay| replay.verified()));
    assert_eq!((rows.len(), requests, omitted_version), (24, 63, 1));
}

#[tokio::test]
async fn request_verification_rejects_wrong_path_even_when_safe_error_still_matches() {
    let row = cases()
        .into_iter()
        .find(|row| row["id"] == "baseline-local-finite-1-1")
        .unwrap();
    let mut requests = row["requests"].as_array().unwrap().clone();
    requests[0]["url"] = json!("http://127.0.0.1:11434/wrong-path");
    let replay = Replay::new(requests);
    let (actual, progress) = execute(&row, &replay).await;
    assert_eq!(actual, row["node_expected"]);
    assert_eq!(json!(progress), row["progress"]);
    assert!(
        !replay.verified(),
        "matching error cannot mask an incorrect request"
    );
}
