use super::*;
use sha2::{Digest, Sha256};

const CASES: &str = include_str!("../../parity/cases/benchmark-router-contracts.jsonl");
const CAPTURE: &str = include_str!("../../parity/cases/benchmark-router-contracts.capture.json");
const CASE_SHA: &str = "ef49a1758c74c0b27bf17f61d7dd71d032a1819a75908ec99d76f605f7d9b3cb";
const CAPTURE_SHA: &str = "9383073e2e785a7c452a4b131d57023fd9bf818d8598940de5a69d57f700ae75";
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| equal(a, b)))
        }
        _ => a == b,
    }
}
fn verify_corpus(cases: &str, capture: &str) -> Result<Vec<Value>, String> {
    if cases.len() > 64 * 1024
        || capture.len() > 16 * 1024
        || digest(cases.as_bytes()) != CASE_SHA
        || digest(capture.as_bytes()) != CAPTURE_SHA
    {
        return Err("Canonical corpus identity changed".into());
    }
    let capture: Value = serde_json::from_str(capture).map_err(|e| e.to_string())?;
    if capture["corpus_sha256"] != CASE_SHA
        || capture["calls"] != 8
        || capture["static_assertions"] != 6
        || capture["expanded_assertions"] != 11
    {
        return Err("Capture cardinality changed".into());
    }
    let rows: Vec<Value> = cases
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    if rows.len() != 8 {
        return Err("Corpus cardinality changed".into());
    }
    for (index, row) in rows.iter().enumerate() {
        let (definition, call) = if index < 2 {
            (1, index + 1)
        } else {
            (2, index - 1)
        };
        if row["id"] != format!("benchmark-router-contract-{definition}-{call}")
            || row["source_test"] != format!("test/benchmark-router.test.mjs#{definition}")
            || capture["definitions"][definition - 1]["case_ids"][call - 1] != row["id"]
        {
            return Err("Source schedule changed".into());
        }
    }
    Ok(rows)
}
fn compare_row(row: &Value) -> Result<(), String> {
    let before = row["input"].clone();
    let actual = compare_document(&row["input"])?;
    if !equal(&actual, &row["expected"]) || before != row["input"] {
        return Err(format!("Full result or input differs for {}", row["id"]));
    }
    Ok(())
}
fn document() -> Value {
    verify_corpus(CASES, CAPTURE).unwrap()[0]["input"].clone()
}

#[test]
fn all_frozen_callbacks_replay_complete_outputs_with_nonfinite_wire_boundary() {
    let rows = verify_corpus(CASES, CAPTURE).unwrap();
    for row in &rows {
        compare_row(row).unwrap();
    }
    for row in &rows[..7] {
        assert_eq!(row["nonfinite"], json!([]));
    }
    let nulls = [
        "/latency/0/candidate_max_p95_ms",
        "/resources/0/candidate_sampled_peak_heap_bytes",
        "/resources/0/candidate_event_loop_p95_ms",
    ];
    assert_eq!(rows[7]["nonfinite"].as_array().unwrap().len(), nulls.len());
    let last = compare_document(&rows[7]["input"]).unwrap();
    for (index, path) in nulls.iter().enumerate() {
        assert_eq!(
            rows[7]["nonfinite"][index],
            json!({"path":path,"value":"-Infinity"})
        );
        assert_eq!(last.pointer(path), Some(&Value::Null));
    }
    assert_eq!(last["passed"], false);
}

#[test]
fn corpus_and_comparator_controls_reject_mutation_omission_and_self_consistent_forgery() {
    let rows = verify_corpus(CASES, CAPTURE).unwrap();
    let mut changed = rows[0].clone();
    changed["expected"]["latency"][0]["limit_ms"] = json!(4000);
    assert!(compare_row(&changed).is_err());
    changed = rows[0].clone();
    changed["input"]["candidate"]["results"][0]["evaluator_calls"] = json!(8);
    assert!(compare_row(&changed).is_err());
    let mut changed_rows = rows.clone();
    changed_rows[0]["input"] = changed["input"].clone();
    changed_rows[0]["expected"] = compare_document(&changed["input"]).unwrap();
    let forged = changed_rows
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let mut capture: Value = serde_json::from_str(CAPTURE).unwrap();
    capture["corpus_sha256"] = json!(digest(forged.as_bytes()));
    assert!(verify_corpus(&forged, &capture.to_string()).is_err());
    let omitted = rows[..7]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(verify_corpus(&omitted, CAPTURE).is_err());
    assert!(verify_corpus(&(CASES.to_owned() + &rows[0].to_string()), CAPTURE).is_err());
    assert!(verify_corpus(&"x".repeat(65 * 1024), CAPTURE).is_err());
}

#[test]
fn multi_round_limits_order_coalescing_and_equality_boundaries_keep_original_semantics() {
    let mut input = document();
    let mut z = input["baseline"]["results"][0].clone();
    z["scenario"] = json!("z-first");
    z["route_ms"]["p95"] = json!(4);
    z["memory_bytes"]["sampled_peak_heap_delta"] = json!(2000);
    z["event_loop_delay_ms"]["p95"] = json!(20);
    let original = input["baseline"]["results"][0].clone();
    let mut larger = z.clone();
    larger["scenario"] = json!("concurrent_identical");
    input["baseline"]["results"] = json!([z.clone(), original, larger]);
    let mut boundary = input["candidate"]["results"][0].clone();
    boundary["route_ms"]["p95"] = json!(8);
    boundary["memory_bytes"]["sampled_peak_heap_delta"] = json!(4000);
    boundary["event_loop_delay_ms"]["p95"] = json!(40);
    input["candidate"]["results"] = json!([z, boundary.clone(), boundary]);
    let result = compare_document(&input).unwrap();
    assert_eq!(result["passed"], true);
    assert_eq!(result["latency"][0]["scenario"], "z-first");
    assert_eq!(result["latency"][1]["scenario"], "concurrent_identical");
    assert_eq!(result["latency"][0]["limit_ms"].as_f64(), Some(8.0));
    assert_eq!(result["latency"][1]["limit_ms"].as_f64(), Some(8.0));
    input["candidate"]["results"].as_array_mut().unwrap().pop();
    assert_eq!(compare_document(&input).unwrap()["passed"], false);
    // Extra candidate scenarios are not baseline latency/resource scenarios,
    // but recognized concurrent names still participate in the calls gate.
    let mut input = document();
    let mut extra = input["candidate"]["results"][0].clone();
    extra["scenario"] = json!("concurrent_identical_agents");
    input["candidate"]["results"]
        .as_array_mut()
        .unwrap()
        .push(extra);
    assert_eq!(compare_document(&input).unwrap()["passed"], true);
    input["candidate"]["results"][1]["evaluator_calls"] = json!(2);
    assert_eq!(
        compare_document(&input).unwrap()["evaluator_calls"]["passed"],
        false
    );
    assert_eq!(rounded(1.2345).as_f64(), Some(1.235));
    assert_eq!(rounded(f64::MAX), Value::Null);
    assert_eq!(wire_number(-0.0), json!(0));
}

#[test]
fn marker_admission_ignores_stale_comparison_but_rejects_mixed_native_and_incomplete_inputs() {
    let input = document();
    let expected = compare_document(&input).unwrap();
    let mut marked = input.clone();
    for key in ["baseline", "candidate"] {
        marked[key]["schema_version"] = json!(1);
        marked[key]["type"] = json!("synthetic_router_benchmark");
    }
    marked["comparison"] = json!({"passed":false});
    assert!(equal(&compare_document(&marked).unwrap(), &expected));
    // JSON integer and float spellings are equal JavaScript numbers.
    marked["candidate"]["inputs"]["iterations"] = json!(1.0);
    assert_eq!(
        compare_document(&marked).unwrap()["comparable"]["passed"],
        true
    );
    for pointer in ["/baseline/schema_version", "/baseline/type"] {
        let mut changed = marked.clone();
        *changed.pointer_mut(pointer).unwrap() = Value::Null;
        assert_eq!(compare_document(&changed).unwrap_err(), INVALID);
    }
    let mut mixed = marked.clone();
    mixed["baseline"] = input["baseline"].clone();
    assert_eq!(compare_document(&mixed).unwrap_err(), INVALID);
    for key in ["kind", "protocol", "protocol_version"] {
        let mut changed = input.clone();
        changed["candidate"][key] = json!("native");
        assert_eq!(compare_document(&changed).unwrap_err(), INVALID);
    }
    for (pointer, value) in [
        ("/candidate/results/0/route_ms/p95", json!("2")),
        (
            "/candidate/results/0/memory_bytes/sampled_peak_heap_delta",
            json!(-1),
        ),
        ("/candidate/results/0/event_loop_delay_ms/p95", Value::Null),
        ("/candidate/inputs/concurrency", json!(0)),
        ("/candidate/inputs/iterations", json!(1.5)),
        (
            "/candidate/inputs/large_request_bytes",
            json!(9_007_199_254_740_992_u64),
        ),
        ("/baseline/results", json!([])),
        ("/candidate/results/0/scenario", json!("x".repeat(257))),
        ("/candidate/environment/cpu", json!("x".repeat(4097))),
    ] {
        let mut changed = input.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert_eq!(
            compare_document(&changed).unwrap_err(),
            INVALID,
            "{pointer}"
        );
    }
    let mut oversized = input;
    oversized["candidate"]["results"] = json!(vec![
        oversized["candidate"]["results"][0].clone();
        MAX_ROWS + 1
    ]);
    assert_eq!(compare_document(&oversized).unwrap_err(), INVALID);
}

#[test]
fn explicit_mode_rejects_conflicts_before_any_workload_or_output_creation() {
    let scratch = crate::tool_process::Scratch::new("benchmark-legacy-mode").unwrap();
    for args in [
        vec!["--compare-legacy"],
        vec!["--compare-legacy", "input.json", "--validate"],
        vec![
            "--output",
            "should-not-exist",
            "--compare-legacy",
            "input.json",
        ],
        vec!["--compare-legacy", "--help"],
    ] {
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        assert_eq!(
            super::super::run(&args, &scratch.0).unwrap_err(),
            "Usage: cargo xtask benchmark --compare-legacy REPORT.json"
        );
        assert_eq!(std::fs::read_dir(&scratch.0).unwrap().count(), 0);
    }
    let input = scratch.0.join("input.json");
    let bytes = serde_json::to_vec(&document()).unwrap();
    std::fs::write(&input, &bytes).unwrap();
    let args = vec!["--compare-legacy".into(), "input.json".into()];
    assert!(super::super::run(&args, &scratch.0).unwrap());
    assert_eq!(std::fs::read(&input).unwrap(), bytes);
    let mut failure = document();
    failure["candidate"]["results"] = json!([]);
    std::fs::write(&input, serde_json::to_vec(&failure).unwrap()).unwrap();
    assert!(!super::super::run(&args, &scratch.0).unwrap());
    std::fs::write(&input, b"synthetic-private-invalid-input").unwrap();
    assert_eq!(super::super::run(&args, &scratch.0).unwrap_err(), INVALID);
    assert_eq!(std::fs::read_dir(&scratch.0).unwrap().count(), 1);
}

#[test]
fn read_boundary_rejects_directory_and_oversized_regular_file_without_echoing_content() {
    let scratch = crate::tool_process::Scratch::new("benchmark-legacy-bound").unwrap();
    let expected =
        "Cannot read historical benchmark report; expected a regular file at most 16 MiB";
    assert_eq!(run(&scratch.0).unwrap_err(), expected);
    let file = scratch.0.join("synthetic-private-name.json");
    std::fs::File::create(&file)
        .unwrap()
        .set_len(MAX_BYTES + 1)
        .unwrap();
    assert_eq!(run(&file).unwrap_err(), expected);
    std::fs::write(&file, br#"{"baseline":1e400}"#).unwrap();
    assert_eq!(run(&file).unwrap_err(), INVALID);
}
