//! Complete finite savings schedules captured from unchanged baseline callbacks.
//! Own undefined object fields are recorded and omitted at the JSON boundary;
//! JavaScript prototypes/accessors and timing/capacity tests are separate gates.
use autorouter_core::savings::{
    PRICING_DATE, PRICING_SOURCE, PRICING_VERSION, SavingsTracker, estimate_outcome_savings,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const CASES: &str = include_str!("../../../parity/cases/savings-finite-contracts.jsonl");
const CAPTURE: &str = include_str!("../../../parity/cases/savings-finite-contracts.capture.json");
const CORPUS_SHA256: &str = "a21e7d67a339695afaa9ed792e4888d750964f53f524f9c509596236d3357789";
const MAX_CORPUS_BYTES: usize = 1024 * 1024;
const INVENTORY: &[(usize, usize)] = &[
    (1, 1),
    (2, 1),
    (3, 8),
    (4, 1),
    (5, 1),
    (6, 1),
    (7, 7),
    (9, 1),
    (10, 9),
    (11, 8),
    (12, 4),
    (13, 3),
    (19, 1),
    (20, 1),
    (22, 21),
];

fn validate_corpus(bytes: &[u8], capture: &Value) -> Result<(), &'static str> {
    if bytes.len() > MAX_CORPUS_BYTES {
        return Err("corpus exceeds declared bound");
    }
    let digest = format!("{:x}", Sha256::digest(bytes));
    if digest != CORPUS_SHA256 || capture["corpus_sha256"] != digest {
        return Err("corpus differs from pinned frozen capture");
    }
    Ok(())
}

fn rows() -> Vec<Value> {
    CASES
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn validate_inventory(rows: &[Value]) -> Result<(), &'static str> {
    if rows.len() != 68 {
        return Err("expected all 68 finite schedules");
    }
    let mut counts = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let (mut trackers, mut outcomes, mut steps) = (0, 0, 0);
    for row in rows {
        let source = row["source_test"]
            .as_str()
            .ok_or("missing source identity")?;
        let number = source
            .strip_prefix("test/savings.test.mjs#")
            .and_then(|text| text.parse::<usize>().ok())
            .ok_or("invalid source identity")?;
        *counts.entry(number).or_insert(0) += 1;
        if !ids.insert(row["id"].as_str().ok_or("missing case identity")?) {
            return Err("duplicate finite schedule");
        }
        match row["kind"].as_str() {
            Some("tracker") => {
                trackers += 1;
                let ops = row["steps"].as_array().ok_or("missing tracker steps")?;
                if ops.is_empty() || ops.len() > 256 {
                    return Err("tracker exceeds declared step bound");
                }
                steps += ops.len();
            }
            Some("outcome") => outcomes += 1,
            _ => return Err("unknown schedule kind"),
        }
    }
    if counts != INVENTORY.iter().copied().collect() || (trackers, outcomes, steps) != (47, 21, 525)
    {
        return Err("complete source schedule inventory changed");
    }
    Ok(())
}

// JavaScript has one Number type. Serde distinguishes integral and floating
// JSON representations (6 versus 6.0), which are identical source numbers.
// Compare exact f64 values without tolerance; preserve every key and array slot.
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

fn verify_snapshot(tracker: &SavingsTracker, expected: &Value) -> Result<(), &'static str> {
    let mut detached = tracker.snapshot();
    if !same_json(&detached, expected) {
        return Err("complete native snapshot differs from original");
    }
    // Exercise the native counterpart of both source detached-value mutations,
    // including nested reasons. Nothing returned may retain write access to the
    // tracker, even before the next update or while requests remain unsettled.
    let sessions = detached
        .as_object_mut()
        .ok_or("snapshot is not an object")?;
    for aggregate in sessions.values_mut() {
        let totals = aggregate
            .as_object_mut()
            .ok_or("aggregate is not an object")?;
        totals.insert("actual_usd".into(), json!(999));
        if let Some(reasons) = totals
            .get_mut("unpriced_reasons")
            .and_then(Value::as_object_mut)
        {
            reasons.insert("invalid_usage".into(), json!(123));
        }
    }
    sessions.insert("synthetic-new-session".into(), json!({"requests":999}));
    if !same_json(&tracker.snapshot(), expected) {
        return Err("detached snapshot mutation changed tracker state");
    }
    Ok(())
}

fn verify_case(row: &Value) -> Result<(), &'static str> {
    match row["kind"].as_str() {
        Some("tracker") => {
            let options = row["options"]
                .as_object()
                .ok_or("missing tracker options")?;
            if options.keys().any(|key| key != "baselineModel") {
                return Err("unsupported tracker option");
            }
            let mut tracker = match options.get("baselineModel") {
                Some(model) => {
                    SavingsTracker::new(model.as_str().ok_or("invalid captured baseline")?)
                }
                None => SavingsTracker::default(),
            };
            verify_snapshot(&tracker, &row["initial_snapshot"])?;
            for step in row["steps"].as_array().ok_or("missing tracker steps")? {
                match step["op"].as_str() {
                    Some("update") => tracker.update(step.get("event").ok_or("missing event")?),
                    Some("snapshot") => {}
                    Some("clear") => tracker.clear(),
                    _ => return Err("unknown native tracker operation"),
                }
                verify_snapshot(&tracker, &step["expected_snapshot"])?;
            }
            Ok(())
        }
        Some("outcome") => {
            let input = row.get("input").ok_or("missing recorded outcome")?;
            if !same_json(&estimate_outcome_savings(input), &row["expected"]) {
                return Err("complete native outcome differs from original");
            }
            Ok(())
        }
        _ => Err("unknown schedule kind"),
    }
}

fn run_definition(number: usize) {
    let capture: Value = serde_json::from_str(CAPTURE).unwrap();
    validate_corpus(CASES.as_bytes(), &capture).unwrap();
    let all = rows();
    validate_inventory(&all).unwrap();
    let source = format!("test/savings.test.mjs#{number}");
    let selected: Vec<_> = all
        .iter()
        .filter(|row| row["source_test"] == source)
        .collect();
    assert_eq!(
        selected.len(),
        INVENTORY.iter().find(|row| row.0 == number).unwrap().1
    );
    for row in selected {
        assert_eq!(verify_case(row), Ok(()), "schedule {}", row["id"]);
    }
}

macro_rules! source_test {
    ($name:ident, $number:literal) => {
        #[test]
        fn $name() {
            run_definition($number);
        }
    };
}
source_test!(
    provider_confirmed_usage_settles_only_at_completion_and_keeps_aggregate_privacy,
    1
);
source_test!(
    cache_reads_both_write_ttls_and_accumulated_percentage_match,
    2
);
source_test!(
    all_exact_aliases_baseline_override_and_negative_savings_match,
    3
);
source_test!(
    overlapping_main_subagent_and_auxiliary_requests_settle_independently,
    4
);
source_test!(sessions_and_literal_prototype_keys_remain_isolated, 5);
source_test!(
    duplicate_terminals_and_detached_snapshot_mutation_do_not_change_totals,
    6
);
source_test!(unknown_models_and_invalid_baselines_never_guess_prices, 7);
source_test!(zero_usage_and_split_only_cache_creation_match, 9);
source_test!(
    request_and_actual_usage_modifiers_have_exact_unpriced_behavior,
    10
);
source_test!(unavailable_geography_is_limited_to_exact_haiku_models, 11);
source_test!(
    http_streaming_transport_and_cancelled_failures_never_become_savings,
    12
);
source_test!(
    conflicting_model_and_usage_observations_invalidate_requests,
    13
);
source_test!(unpriced_reason_counts_remain_detached_and_private, 19);
source_test!(
    distinct_serving_models_are_unpriced_even_with_identical_rates,
    20
);
source_test!(
    saved_outcomes_require_recorded_provenance_and_confirmed_completion,
    22
);

#[test]
fn source_inventory_provenance_undefined_adaptation_and_native_map_isolation() {
    let capture: Value = serde_json::from_str(CAPTURE).unwrap();
    validate_corpus(CASES.as_bytes(), &capture).unwrap();
    let all = rows();
    validate_inventory(&all).unwrap();
    assert_eq!(
        capture["baseline_commit"],
        "ea930c247626ce2af5ccdad721b5121417bf4ad8"
    );
    assert_eq!(capture["selected_definitions"], 15);
    assert_eq!(capture["static_assertions"], 61);
    assert_eq!(capture["expanded_assertions"], 130);
    let mut captured_ids = Vec::new();
    for definition in capture["definitions"].as_array().unwrap() {
        assert_eq!(definition["execution"], "complete_unchanged_callback");
        captured_ids.extend(definition["case_ids"].as_array().unwrap().iter().cloned());
        assert!(
            definition["assertions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["expanded_executions"].as_u64().unwrap() > 0)
        );
    }
    assert_eq!(
        captured_ids,
        all.iter().map(|row| row["id"].clone()).collect::<Vec<_>>()
    );
    let first = &all[0];
    let final_snapshot = &first["steps"].as_array().unwrap().last().unwrap()["expected_snapshot"];
    assert_eq!(
        final_snapshot["session-a"]["pricing_version"],
        PRICING_VERSION
    );
    assert_eq!(final_snapshot["session-a"]["pricing_date"], PRICING_DATE);
    assert_eq!(
        final_snapshot["session-a"]["pricing_source"],
        PRICING_SOURCE
    );

    // Unlike the source language, native JSON maps have no inherited mutable
    // prototype. The same literal keys must remain ordinary, isolated data.
    let mut tracker = SavingsTracker::default();
    tracker.update(
        &json!({"event":"request_start","session_id":"__proto__","request_id":"synthetic"}),
    );
    let snapshot = tracker.snapshot();
    assert!(snapshot.get("__proto__").is_some());
    assert!(json!({}).get("actual_usd").is_none());
    assert_eq!(SavingsTracker::default().snapshot(), json!({}));

    assert_ne!(json!(6), json!(6.0));
    assert!(same_json(&json!({"cost":6.0}), &json!({"cost":6})));
    assert!(!same_json(
        &json!({"cost":0.000000000000001}),
        &json!({"cost":0})
    ));
    assert!(!same_json(
        &json!({"cost":6,"extra":null}),
        &json!({"cost":6})
    ));
    assert!(!same_json(&json!([1, 2]), &json!([2, 1])));
    assert!(!same_json(&json!("6"), &json!(6)));
}

#[test]
fn replay_rejects_forged_costs_incomplete_schedules_and_recomputed_corpus_hashes() {
    let all = rows();
    let mut forged = all[0].clone();
    let last = forged["steps"].as_array_mut().unwrap().last_mut().unwrap();
    last["expected_snapshot"]["session-a"]["actual_usd"] = json!(0);
    assert_eq!(
        verify_case(&forged),
        Err("complete native snapshot differs from original")
    );

    let mut missing_completion = all[0].clone();
    let step = missing_completion["steps"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|step| step["event"]["event"] == "request_complete")
        .unwrap();
    step["event"]["event"] = json!("ignored_synthetic_event");
    assert_eq!(
        verify_case(&missing_completion),
        Err("complete native snapshot differs from original")
    );

    let mut guessed_outcome = all
        .iter()
        .find(|row| row["kind"] == "outcome" && row["expected"]["priced"] == false)
        .unwrap()
        .clone();
    guessed_outcome["expected"] = json!({"priced":true,"saved_usd":18});
    assert_eq!(
        verify_case(&guessed_outcome),
        Err("complete native outcome differs from original")
    );
    assert_eq!(
        validate_inventory(&all[1..]),
        Err("expected all 68 finite schedules")
    );
    let mut duplicate = all.clone();
    duplicate[1] = duplicate[0].clone();
    assert_eq!(
        validate_inventory(&duplicate),
        Err("duplicate finite schedule")
    );

    let mut replacement = all;
    replacement[0]["steps"][0]["event"]["body"] = json!("ordinary synthetic text");
    assert_eq!(verify_case(&replacement[0]), Ok(()));
    assert_eq!(validate_inventory(&replacement), Ok(()));
    let bytes = replacement
        .iter()
        .map(|row| format!("{row}\n"))
        .collect::<String>();
    let mut capture: Value = serde_json::from_str(CAPTURE).unwrap();
    capture["corpus_sha256"] = json!(format!("{:x}", Sha256::digest(bytes.as_bytes())));
    assert_eq!(
        validate_corpus(bytes.as_bytes(), &capture),
        Err("corpus differs from pinned frozen capture")
    );
    assert_eq!(
        validate_corpus(&vec![0; MAX_CORPUS_BYTES + 1], &capture),
        Err("corpus exceeds declared bound")
    );
}
