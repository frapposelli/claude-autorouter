//! Complete frozen renderer calls with individually recorded native API projections.
//! The capture replays every projected call in the frozen renderer as well.
use autorouter_core::statusline::render_status_line;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const CASES: &str = include_str!("../../../parity/cases/statusline-boundary-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../parity/cases/statusline-boundary-contracts.capture.json");
const SHA256: &str = "1e2c8f8e9b3a8fcc7075f37fc37f7b1f620b741339cb364f495f2c647053b7f0";
const INVENTORY: &[(usize, usize)] = &[
    (5, 10),
    (7, 9),
    (8, 13),
    (9, 38),
    (12, 9),
    (16, 7),
    (18, 10),
    (19, 7),
    (23, 12),
    (27, 9),
];

fn parse(bytes: &[u8], capture: &Value) -> Result<Vec<Value>, &'static str> {
    if bytes.len() > 1024 * 1024 {
        return Err("corpus exceeds declared bound");
    }
    let hash = format!("{:x}", Sha256::digest(bytes));
    if hash != SHA256 || capture["corpus_sha256"] != hash {
        return Err("corpus differs from independently frozen capture");
    }
    let source = std::str::from_utf8(bytes).map_err(|_| "corpus must be UTF-8")?;
    let rows: Vec<Value> = source
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .map_err(|_| "invalid corpus JSON")?;
    if rows.len() != 124 {
        return Err("incomplete frozen call inventory");
    }
    let mut inventory = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut adaptations = BTreeMap::new();
    for row in &rows {
        let source = row["source_test"]
            .as_str()
            .ok_or("missing source identity")?;
        let number = source
            .strip_prefix("test/statusline.test.mjs#")
            .and_then(|text| text.parse::<usize>().ok())
            .ok_or("invalid source identity")?;
        *inventory.entry(number).or_insert(0) += 1;
        if !ids.insert(row["id"].as_str().ok_or("missing case identity")?) {
            return Err("duplicate case");
        }
        let args = row["arguments"].as_array().ok_or("missing arguments")?;
        if args.len() != 3 || !args[2].is_object() || args[2]["now"] != 1_750_000_000_000_u64 {
            return Err("argument shape or fixed source clock changed");
        }
        for adaptation in row["adaptations"].as_array().ok_or("missing adaptations")? {
            let path = adaptation["path"]
                .as_array()
                .ok_or("missing adaptation path")?;
            let first = path
                .first()
                .and_then(Value::as_u64)
                .ok_or("missing argument index")?;
            let mut value = args.get(usize::try_from(first).map_err(|_| "invalid argument index")?);
            for key in path.iter().skip(1) {
                value = value.and_then(|parent| parent.get(key.as_str()?));
            }
            let kind = adaptation["kind"]
                .as_str()
                .ok_or("missing adaptation kind")?;
            *adaptations.entry(kind.to_owned()).or_insert(0) += 1;
            match kind {
                "omit_own_undefined" if value.is_none() && path.len() > 1 => {}
                "undefined_argument_to_null" if value == Some(&Value::Null) && path.len() == 1 => {}
                "invalid_nonfinite_to_null"
                    if value == Some(&Value::Null)
                        && matches!(adaptation["original"].as_str(), Some("Infinity" | "NaN")) => {}
                "capture_process_pid"
                    if path == &vec![json!(1), json!("pid")] && value == Some(&json!(1)) => {}
                "liveness_callback"
                    if path == &vec![json!(2), json!("alive")]
                        && value == Some(&Value::Bool(false))
                        && row["liveness"].as_array().is_some_and(|v| v.len() == 1) => {}
                _ => return Err("undeclared or invalid native API projection"),
            }
        }
    }
    let expected: BTreeMap<String, usize> = [
        ("capture_process_pid", 114),
        ("omit_own_undefined", 15),
        ("undefined_argument_to_null", 10),
        ("invalid_nonfinite_to_null", 4),
        ("liveness_callback", 2),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v))
    .collect();
    if inventory != INVENTORY.iter().copied().collect() || adaptations != expected {
        return Err("source calls or explicit projections changed");
    }
    Ok(rows)
}

fn check(row: &Value) -> Result<String, &'static str> {
    let args = row["arguments"].as_array().ok_or("missing arguments")?;
    if args.len() != 3 {
        return Err("missing argument");
    }
    let before = args.clone();
    let actual = render_status_line(&args[0], &args[1], &args[2]);
    if *args != before {
        return Err("renderer mutated input");
    }
    if row["expected"].as_str() != Some(&actual) {
        return Err("complete renderer output differs from frozen source");
    }
    Ok(actual)
}

fn run(number: usize) {
    let capture = serde_json::from_str(CAPTURE).unwrap();
    let rows = parse(CASES.as_bytes(), &capture).unwrap();
    let id = format!("test/statusline.test.mjs#{number}");
    let selected: Vec<_> = rows.iter().filter(|row| row["source_test"] == id).collect();
    assert_eq!(
        selected.len(),
        INVENTORY.iter().find(|(n, _)| *n == number).unwrap().1
    );
    for row in selected {
        check(row).unwrap_or_else(|error| panic!("{}: {error}", row["id"]));
    }
}

#[test]
fn evaluator_labels_keep_confirmation_separate() {
    run(5);
}
#[test]
fn absent_malformed_and_prototype_session_inputs_are_isolated() {
    run(7);
}
#[test]
fn fallback_errors_dead_liveness_and_stale_snapshots_stay_visible() {
    run(8);
}
#[test]
fn every_original_narrow_width_preserves_qualifiers_and_guards() {
    run(9);
    let capture = serde_json::from_str(CAPTURE).unwrap();
    let rows = parse(CASES.as_bytes(), &capture).unwrap();
    let mut tiny_calls = 0;
    for row in rows
        .iter()
        .filter(|row| row["source_test"] == "test/statusline.test.mjs#9")
    {
        let args = &row["arguments"];
        let columns = args[2]["columns"].as_u64().unwrap();
        let state = &args[1]["sessions"]["session-a"];
        if columns <= 23 && state["selected_model"] == "claude-sonnet-5" {
            tiny_calls += 1;
            let output = check(row).unwrap();
            assert!(output.chars().count() <= columns as usize);
            // Original line155 is conditional and never runs for these21calls.
            // Verify the same false predicate, without claiming branch execution.
            assert!(!output.contains("Sonnet"));
            if output.contains("… ") && !output.starts_with('●') {
                assert!(output.contains("selected") || output.contains("unconfirmed"));
            }
        }
    }
    assert_eq!(tiny_calls, 21);
}
#[test]
fn unknown_models_and_invalid_usage_never_imply_headroom() {
    run(12);
}
#[test]
fn unpriced_counts_never_displace_selected_model_or_guard() {
    run(16);
}
#[test]
fn savings_never_cross_exact_session_or_invalid_input_boundary() {
    run(18);
}
#[test]
fn invalid_savings_never_inject_claims_or_terminal_controls() {
    run(19);
}
#[test]
fn classifier_status_does_not_replace_provider_errors() {
    run(23);
}
#[test]
fn auto_floor_width_and_color_matrix_preserves_timing_precedence() {
    run(27);
}

#[test]
fn canonical_capture_rejects_self_consistent_fixture_replacement() {
    let mut capture: Value = serde_json::from_str(CAPTURE).unwrap();
    let changed = CASES.replace("Sonnet 5.5 selected", "Sonnet 5.5 confirmed");
    assert_ne!(changed, CASES);
    capture["corpus_sha256"] = format!("{:x}", Sha256::digest(changed.as_bytes())).into();
    assert!(parse(changed.as_bytes(), &capture).is_err());
    assert!(parse(&vec![b' '; 1024 * 1024 + 1], &capture).is_err());
}

#[test]
fn complete_comparison_detects_forged_qualifier_guard_privacy_and_ansi() {
    let capture = serde_json::from_str(CAPTURE).unwrap();
    let rows = parse(CASES.as_bytes(), &capture).unwrap();
    for (needle, replacement) in [
        ("selected", "confirmed"),
        ("Auto floor", "ready"),
        ("savings unavailable", "est saved $999"),
        ("\u{1b}[", "["),
    ] {
        let mut row = rows
            .iter()
            .find(|row| row["expected"].as_str().unwrap().contains(needle))
            .unwrap()
            .clone();
        check(&row).unwrap();
        row["expected"] = row["expected"]
            .as_str()
            .unwrap()
            .replace(needle, replacement)
            .into();
        assert!(check(&row).is_err(), "{needle}");
    }
}
