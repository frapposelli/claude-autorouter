//! Exact functional calls from the frozen synthetic redaction assertions.
//! The 28 source wall-clock assertions remain unqualified and are not timed here.
use autorouter_core::{
    js_json::JsDocument,
    prompt_state::{build_ollama_state_document, build_state_document, prompt_excerpt_document},
    redaction::redact_sensitive,
    telemetry_event::normalize_session_document,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const CASES: &str = include_str!("../../../parity/cases/redaction-contracts.jsonl");
const CAPTURE: &str = include_str!("../../../parity/cases/redaction-contracts.capture.json");
const CORPUS_SHA256: &str = "ae0ad22d07ba8c36090945501841d498c7a5bc48ed0d220d271f2957320c2951";
const MAX_CORPUS_BYTES: usize = 512 * 1024;
const COUNTS: &[(usize, usize)] = &[
    (1, 12),
    (2, 7),
    (3, 11),
    (4, 5),
    (5, 2),
    (6, 2),
    (7, 2),
    (9, 3),
    (11, 7),
    (12, 15),
    (13, 12),
    (14, 11),
    (16, 2),
    (17, 2),
];

fn cases() -> Vec<Value> {
    CASES
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn validate_corpus(bytes: &[u8], capture: &Value) -> Result<(), &'static str> {
    if bytes.len() > MAX_CORPUS_BYTES {
        return Err("corpus exceeds byte bound");
    }
    let sha256 = format!("{:x}", Sha256::digest(bytes));
    if sha256 != CORPUS_SHA256 || capture["corpus_sha256"] != sha256 {
        return Err("corpus identity differs from frozen capture");
    }
    Ok(())
}

fn validate_inventory(rows: &[Value]) -> Result<(), &'static str> {
    if rows.len() != 93 {
        return Err("expected all 93 original functional calls");
    }
    let mut counts = BTreeMap::new();
    let mut ids = std::collections::BTreeSet::new();
    for row in rows {
        let source = row["source_test"].as_str().ok_or("missing source test")?;
        let number = source
            .strip_prefix("test/redaction.test.mjs#")
            .and_then(|text| text.parse::<usize>().ok())
            .ok_or("unexpected source test")?;
        *counts.entry(number).or_insert(0) += 1;
        if !ids.insert(row["id"].as_str().ok_or("missing case id")?) {
            return Err("duplicate case id");
        }
    }
    if counts != COUNTS.iter().copied().collect() {
        return Err("source call counts changed");
    }
    Ok(())
}

fn actual(row: &Value) -> Result<Value, &'static str> {
    let args = row["args"].as_array().ok_or("missing arguments")?;
    let input = args.first().ok_or("missing first argument")?;
    let now = row["now"].as_str().ok_or("missing explicit clock")?;
    let document = || {
        JsDocument::parse(serde_json::to_vec(input).unwrap().as_slice())
            .map_err(|_| "invalid input document")
    };
    let limit = |default| match args.get(1) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or("invalid explicit limit"),
    };
    let value = match row["op"].as_str().ok_or("missing operation")? {
        "redact" => {
            if args.len() != 1 {
                return Err("redaction arity");
            }
            Value::String(redact_sensitive(
                input.as_str().ok_or("redaction requires string")?,
            ))
        }
        "build_state" => {
            serde_json::from_str(&build_state_document(&document()?, limit(12000)?).stringify())
                .map_err(|_| "invalid native state JSON")?
        }
        "build_ollama_state" => serde_json::from_str(
            &build_ollama_state_document(&document()?, limit(3000)?).stringify(),
        )
        .map_err(|_| "invalid native Ollama state JSON")?,
        "prompt_excerpt" => Value::String(prompt_excerpt_document(&document()?, limit(500)?)),
        "normalize_session" => {
            if args.len() != 1 {
                return Err("session normalization arity");
            }
            normalize_session_document(&document()?, true, now).ok_or("missing normalized value")?
        }
        _ => return Err("unknown native operation"),
    };
    Ok(json!({"kind":"value","value":value}))
}

fn verify_case(row: &Value) -> Result<(), &'static str> {
    if actual(row)? != row["expected"] {
        return Err("complete native result differs from frozen result");
    }
    Ok(())
}

#[test]
fn exact_frozen_redaction_prompt_and_historical_excerpt_outputs() {
    let capture: Value = serde_json::from_str(CAPTURE).unwrap();
    validate_corpus(CASES.as_bytes(), &capture).unwrap();
    let rows = cases();
    validate_inventory(&rows).unwrap();
    assert_eq!(capture["calls"], 93);
    assert_eq!(capture["expanded_functional_assertions"], 141);
    assert_eq!(capture["executed_static_assertions"], 35);
    assert_eq!(capture["unexecuted_timing_assertions"], 28);
    let mut captured_ids = Vec::new();
    for definition in capture["definitions"].as_array().unwrap() {
        captured_ids.extend(definition["case_ids"].as_array().unwrap().iter().cloned());
    }
    assert_eq!(
        captured_ids,
        rows.iter().map(|row| row["id"].clone()).collect::<Vec<_>>()
    );
    for row in &rows {
        assert_eq!(verify_case(row), Ok(()), "{}", row["id"]);
    }
}

#[test]
fn replay_rejects_private_output_lost_state_wrong_clock_and_incomplete_inventory() {
    let rows = cases();
    let mut unredacted = rows
        .iter()
        .find(|row| row["op"] == "redact")
        .unwrap()
        .clone();
    unredacted["expected"]["value"] = unredacted["args"][0].clone();
    assert_eq!(
        verify_case(&unredacted),
        Err("complete native result differs from frozen result")
    );

    let mut lost_state = rows
        .iter()
        .find(|row| row["op"] == "build_state")
        .unwrap()
        .clone();
    lost_state["expected"]["value"]
        .as_object_mut()
        .unwrap()
        .remove("current_task");
    assert_eq!(
        verify_case(&lost_state),
        Err("complete native result differs from frozen result")
    );

    let mut wrong_clock = rows
        .iter()
        .find(|row| row["op"] == "normalize_session")
        .unwrap()
        .clone();
    wrong_clock["now"] = json!("2030-01-01T00:00:00.000Z");
    assert_eq!(
        verify_case(&wrong_clock),
        Err("complete native result differs from frozen result")
    );

    let mut unknown = rows[0].clone();
    unknown["op"] = json!("echo_input");
    assert_eq!(verify_case(&unknown), Err("unknown native operation"));
    assert_eq!(
        validate_inventory(&rows[1..]),
        Err("expected all 93 original functional calls")
    );
    let mut duplicate = rows.clone();
    duplicate[1] = duplicate[0].clone();
    assert_eq!(validate_inventory(&duplicate), Err("duplicate case id"));

    // Matching edited input/output and a recomputed capture hash still cannot
    // replace the exact frozen corpus with an easier, self-consistent fixture.
    let mut replacement = rows;
    replacement[0]["args"][0] = json!("ordinary engineering text");
    replacement[0]["expected"]["value"] = json!("ordinary engineering text");
    assert_eq!(verify_case(&replacement[0]), Ok(()));
    assert_eq!(validate_inventory(&replacement), Ok(()));
    let encoded = replacement
        .iter()
        .map(|row| format!("{row}\n"))
        .collect::<String>();
    let mut capture: Value = serde_json::from_str(CAPTURE).unwrap();
    capture["corpus_sha256"] = json!(format!("{:x}", Sha256::digest(encoded.as_bytes())));
    assert_eq!(
        validate_corpus(encoded.as_bytes(), &capture),
        Err("corpus identity differs from frozen capture")
    );
    assert_eq!(
        validate_corpus(&vec![0; MAX_CORPUS_BYTES + 1], &capture),
        Err("corpus exceeds byte bound")
    );
}
