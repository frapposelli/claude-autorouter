//! Complete capacity callbacks with every original event and snapshot preserved.
//! No per-update snapshot is added; capacity is observable behavior, not RSS.
use autorouter_core::savings::SavingsTracker;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const CASES: &str = include_str!("../../../parity/cases/savings-capacity-contracts.jsonl");
const CAPTURE: &str = include_str!("../../../parity/cases/savings-capacity-contracts.capture.json");
const CORPUS_SHA256: &str = "e626ef908777de02de8f61a4ca431d6fa140667cbc0802e43e59862b8cec9a55";
const MAX_CORPUS_BYTES: usize = 1024 * 1024;
const INVENTORY: &[(usize, usize, usize)] = &[(14, 1013, 4), (15, 114, 5)];

fn validate_corpus(bytes: &[u8], capture: &Value) -> Result<(), &'static str> {
    if bytes.len() > MAX_CORPUS_BYTES {
        return Err("corpus exceeds declared bound");
    }
    let digest = format!("{:x}", Sha256::digest(bytes));
    if digest != CORPUS_SHA256 || capture["corpus_sha256"] != digest {
        return Err("corpus differs from pinned original capture");
    }
    Ok(())
}

fn rows() -> Vec<Value> {
    CASES
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn inventory(rows: &[Value]) -> Result<(), &'static str> {
    if rows.len() != INVENTORY.len() {
        return Err("expected both complete capacity schedules");
    }
    let mut ids = BTreeSet::new();
    for (row, (number, updates, snapshots)) in rows.iter().zip(INVENTORY) {
        if row["id"] != format!("savings-capacity-{number}-1")
            || !ids.insert(row["id"].as_str().ok_or("missing schedule identity")?)
            || row["source_test"] != format!("test/savings.test.mjs#{number}")
            || row["kind"] != "tracker"
            || row["default_options"] != true
            || row["options"] != json!({})
            || row["options_undefined_paths"] != json!([])
        {
            return Err("capacity schedule identity or construction changed");
        }
        let steps = row["steps"].as_array().ok_or("missing capacity steps")?;
        if steps.len() > 2048 {
            return Err("capacity step bound");
        }
        let (mut update_count, mut snapshot_count) = (0, 0);
        for step in steps {
            match step["op"].as_str() {
                Some("update") => {
                    update_count += 1;
                    if step.get("expected_snapshot").is_some()
                        || step["event"].to_string().len() > 65536
                    {
                        return Err("unexpected per-update snapshot or oversized event");
                    }
                    if step["undefined_paths"] != json!([])
                        && step["undefined_paths"] != json!([["pricing_context"]])
                    {
                        return Err("unexpected own-undefined adaptation");
                    }
                    if step["undefined_paths"] == json!([["pricing_context"]])
                        && step["event"].get("pricing_context").is_some()
                    {
                        return Err("undefined field must be absent at JSON boundary");
                    }
                }
                Some("snapshot") => {
                    snapshot_count += 1;
                    if step["source_assertion"]
                        != format!("test/savings.test.mjs#{number}:assert-{snapshot_count}")
                        || step["expected_snapshot"].to_string().len() > 65536
                    {
                        return Err("source snapshot identity or bound changed");
                    }
                }
                _ => return Err("unrecognized capacity operation"),
            }
        }
        if (update_count, snapshot_count) != (*updates, *snapshots) {
            return Err("original operation inventory changed");
        }
    }
    Ok(())
}

// The source has one Number type; compare exact f64 values while retaining all
// object keys, array slots and other primitive types. No tolerance or rounding.
fn same_json(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_json(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, value)| b.get(key).is_some_and(|other| same_json(value, other)))
        }
        _ => actual == expected,
    }
}

fn replay(row: &Value) -> Result<(), &'static str> {
    let mut tracker = SavingsTracker::default();
    for step in row["steps"].as_array().ok_or("missing steps")? {
        match step["op"].as_str() {
            Some("update") => tracker.update(step.get("event").ok_or("missing event")?),
            Some("snapshot") => {
                if !same_json(&tracker.snapshot(), &step["expected_snapshot"]) {
                    return Err("complete original snapshot differs");
                }
            }
            _ => return Err("unrecognized capacity operation"),
        }
    }
    Ok(())
}

fn original(number: usize) {
    let capture: Value = serde_json::from_str(CAPTURE).unwrap();
    validate_corpus(CASES.as_bytes(), &capture).unwrap();
    let rows = rows();
    inventory(&rows).unwrap();
    let row = rows
        .iter()
        .find(|row| row["source_test"] == format!("test/savings.test.mjs#{number}"))
        .unwrap();
    assert_eq!(replay(row), Ok(()));
}

#[test]
fn original_inflight_capacity_preserves_eviction_and_late_request_identity() {
    original(14);
}

#[test]
fn original_session_capacity_ignores_old_work_and_marks_returning_session_partial() {
    original(15);
}

#[test]
fn snapshot_and_assertion_inventory_are_exactly_the_original_calls() {
    let capture: Value = serde_json::from_str(CAPTURE).unwrap();
    validate_corpus(CASES.as_bytes(), &capture).unwrap();
    let rows = rows();
    inventory(&rows).unwrap();
    assert_eq!(
        capture["baseline_commit"],
        "ea930c247626ce2af5ccdad721b5121417bf4ad8"
    );
    assert_eq!(capture["selected_definitions"], 2);
    assert_eq!(capture["cases"], 2);
    assert_eq!(capture["trackers"], 2);
    assert_eq!(capture["updates"], 1127);
    assert_eq!(capture["snapshots"], 9);
    assert_eq!(capture["total_steps"], 1136);
    assert_eq!(capture["static_assertions"], 9);
    assert_eq!(capture["expanded_assertions"], 9);
    assert_eq!(capture["bounds"]["corpus_bytes"], MAX_CORPUS_BYTES);
    let mut undefined = 0;
    for (row, definition) in rows.iter().zip(capture["definitions"].as_array().unwrap()) {
        assert_eq!(definition["execution"], "complete_unchanged_callback");
        assert_eq!(definition["case_ids"], json!([row["id"]]));
        let assertion_ids: Vec<_> = row["steps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|step| step["op"] == "snapshot")
            .map(|step| step["source_assertion"].clone())
            .collect();
        assert_eq!(
            assertion_ids,
            definition["assertions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a["id"].clone())
                .collect::<Vec<_>>()
        );
        assert!(
            definition["assertions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|a| a["expanded_executions"] == 1)
        );
        for step in row["steps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|step| step["op"] == "update")
        {
            undefined += step["undefined_paths"].as_array().unwrap().len();
        }
    }
    assert_eq!(undefined, 4);
    assert!(same_json(&json!({"cost":6}), &json!({"cost":6.0})));
    assert!(!same_json(
        &json!({"cost":0}),
        &json!({"cost":0.000000000000001})
    ));
    assert!(!same_json(&json!({"session":null}), &json!({})));
    assert!(!same_json(&json!([1, 2]), &json!([2, 1])));
    assert!(!same_json(&json!("6"), &json!(6)));
}

#[test]
fn exact_public_boundaries_do_not_evict_before_the_limit() {
    let event = |session: &str, request: &str| json!({"event":"request_start","session_id":session,"request_id":request});
    let mut inflight = SavingsTracker::default();
    for index in 0..1000 {
        inflight.update(&event("session-a", &format!("request-{index}")));
    }
    assert_eq!(inflight.snapshot()["session-a"]["unpriced_requests"], 0);
    assert!(inflight.snapshot()["session-a"].get("partial").is_none());
    inflight.update(&event("session-a", "request-1000"));
    assert_eq!(
        inflight.snapshot()["session-a"]["unpriced_reasons"],
        json!({"request_evicted":1})
    );
    assert_eq!(inflight.snapshot()["session-a"]["partial"], true);

    let mut sessions = SavingsTracker::default();
    for index in 0..100 {
        sessions.update(&event(&format!("session-{index}"), "request-1"));
    }
    let before = sessions.snapshot();
    assert_eq!(before.as_object().unwrap().len(), 100);
    assert!(before.get("session-0").is_some());
    assert!(
        before
            .as_object()
            .unwrap()
            .values()
            .all(|session| session.get("partial").is_none())
    );
    sessions.update(&event("session-100", "request-1"));
    let after = sessions.snapshot();
    assert_eq!(after.as_object().unwrap().len(), 100);
    assert!(after.get("session-0").is_none());
    assert!(after.get("session-100").is_some());
}

#[test]
fn forged_snapshots_missing_and_reordered_events_cannot_pass() {
    let rows = rows();
    let mut forged = rows[0].clone();
    let snapshot = forged["steps"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|s| s["op"] == "snapshot")
        .unwrap();
    snapshot["expected_snapshot"]["session-a"]["unpriced_requests"] = json!(0);
    assert_eq!(replay(&forged), Err("complete original snapshot differs"));

    let mut missing = rows[0].clone();
    missing["steps"][0]["event"]["event"] = json!("ignored_synthetic_event");
    assert_eq!(replay(&missing), Err("complete original snapshot differs"));

    let mut reordered = rows[0].clone();
    let steps = reordered["steps"].as_array_mut().unwrap();
    let usage = steps
        .iter()
        .position(|s| {
            s["event"]["request_id"] == "request-1000" && s["event"]["event"] == "upstream_usage"
        })
        .unwrap();
    let completion = steps
        .iter()
        .position(|s| {
            s["event"]["request_id"] == "request-1000" && s["event"]["event"] == "request_complete"
        })
        .unwrap();
    steps.swap(usage, completion);
    assert_eq!(
        replay(&reordered),
        Err("complete original snapshot differs")
    );

    let mut sessions = rows[1].clone();
    sessions["steps"].as_array_mut().unwrap().swap(0, 101);
    assert_eq!(replay(&sessions), Err("complete original snapshot differs"));

    let mut late = rows[1].clone();
    for step in late["steps"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .skip(102)
        .take(7)
    {
        if step["op"] == "update" {
            step["event"]["request_id"] = json!("premature-new-request");
        }
    }
    assert_eq!(replay(&late), Err("complete original snapshot differs"));
}

#[test]
fn inventory_and_fixed_capture_pin_reject_self_consistent_replacements() {
    let all = rows();
    assert_eq!(
        inventory(&all[1..]),
        Err("expected both complete capacity schedules")
    );
    let mut duplicate = all.clone();
    duplicate[1] = duplicate[0].clone();
    assert!(inventory(&duplicate).is_err());
    let mut missing = all.clone();
    missing[0]["steps"].as_array_mut().unwrap().remove(0);
    assert_eq!(
        inventory(&missing),
        Err("original operation inventory changed")
    );
    let mut incomplete = all.clone();
    let steps = incomplete[1]["steps"].as_array_mut().unwrap();
    steps.pop();
    assert_eq!(
        inventory(&incomplete),
        Err("original operation inventory changed")
    );
    let mut replacement = all;
    replacement[0]["steps"][0]["event"]["synthetic_ignored_field"] = json!(true);
    assert_eq!(inventory(&replacement), Ok(()));
    assert_eq!(replay(&replacement[0]), Ok(()));
    let bytes = replacement
        .iter()
        .map(|row| format!("{row}\n"))
        .collect::<String>();
    let mut capture: Value = serde_json::from_str(CAPTURE).unwrap();
    capture["corpus_sha256"] = json!(format!("{:x}", Sha256::digest(bytes.as_bytes())));
    assert_eq!(
        validate_corpus(bytes.as_bytes(), &capture),
        Err("corpus differs from pinned original capture")
    );
    assert_eq!(
        validate_corpus(&vec![0; MAX_CORPUS_BYTES + 1], &capture),
        Err("corpus exceeds declared bound")
    );
}
