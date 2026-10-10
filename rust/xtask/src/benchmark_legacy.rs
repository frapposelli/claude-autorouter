//! Read-only comparison of the frozen JavaScript benchmark report format.
//! These heap/event-loop gates never qualify the native paired protocols.
use serde_json::{Value, json};
use std::path::Path;

const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ROWS: usize = 4096;
const SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
const INVALID: &str = "Invalid historical benchmark report; use bounded finite schema 1 inputs";
const INPUTS: [&str; 5] = [
    "iterations",
    "rounds",
    "concurrency",
    "evaluator_delay_ms",
    "large_request_bytes",
];
const ENVIRONMENT: [&str; 4] = ["node", "platform", "arch", "cpu"];

fn number(value: &Value) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|value| value.is_finite() && *value >= 0.0)
        .ok_or_else(|| INVALID.into())
}
fn integer(value: &Value) -> Result<f64, String> {
    let value = number(value)?;
    if value.fract() != 0.0 || value > SAFE_INTEGER {
        return Err(INVALID.into());
    }
    Ok(value)
}
fn string(value: &Value, bound: usize) -> Result<&str, String> {
    value
        .as_str()
        .filter(|value| value.len() <= bound)
        .ok_or_else(|| INVALID.into())
}
fn native_marker(value: &Value) -> bool {
    ["kind", "protocol", "protocol_version"]
        .iter()
        .any(|key| value.get(key).is_some())
}
fn validate(value: &Value, baseline: bool) -> Result<bool, String> {
    if !value.is_object()
        || native_marker(value)
        || !value["inputs"].is_object()
        || !value["environment"].is_object()
    {
        return Err(INVALID.into());
    }
    let marked = match (value.get("schema_version"), value.get("type")) {
        (None, None) => false,
        (Some(version), Some(kind))
            if version.as_f64() == Some(1.0)
                && kind.as_str() == Some("synthetic_router_benchmark") =>
        {
            true
        }
        _ => return Err(INVALID.into()),
    };
    for key in INPUTS {
        if key == "evaluator_delay_ms" {
            number(&value["inputs"][key])?;
        } else {
            integer(&value["inputs"][key])?;
        }
    }
    if number(&value["inputs"]["concurrency"])? == 0.0 {
        return Err(INVALID.into());
    }
    for key in ENVIRONMENT {
        string(&value["environment"][key], 4096)?;
    }
    integer(&value["environment"]["total_memory_bytes"])?;
    let rows = value["results"].as_array().ok_or(INVALID)?;
    if rows.len() > MAX_ROWS || (baseline && rows.is_empty()) {
        return Err(INVALID.into());
    }
    for row in rows {
        if !row.is_object() {
            return Err(INVALID.into());
        }
        string(&row["scenario"], 256)?;
        integer(&row["requests"])?;
        integer(&row["evaluator_calls"])?;
        number(&row["route_ms"]["p95"])?;
        number(&row["memory_bytes"]["sampled_peak_heap_delta"])?;
        number(&row["event_loop_delay_ms"]["p95"])?;
    }
    Ok(marked)
}
fn wire_number(value: f64) -> Value {
    // JSON.stringify emits null for nonfinite values and 0 for negative zero.
    if value == 0.0 {
        json!(0)
    } else {
        serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
    }
}
fn rounded(value: f64) -> Value {
    // All admitted metrics are nonnegative. Rust round and Math.round agree
    // there, including intermediate positive overflow; empty maxima are -Inf.
    wire_number((value * 1000.0).round() / 1000.0)
}
fn max(rows: &[&Value], group: &str, field: &str) -> f64 {
    rows.iter().fold(f64::NEG_INFINITY, |before, row| {
        before.max(row[group][field].as_f64().expect("validated metric"))
    })
}
pub(super) fn compare_document(document: &Value) -> Result<Value, String> {
    if !document.is_object() || native_marker(document) {
        return Err(INVALID.into());
    }
    let baseline = &document["baseline"];
    let candidate = &document["candidate"];
    if validate(baseline, true)? != validate(candidate, false)? {
        return Err(INVALID.into());
    }
    let comparable = INPUTS
        .iter()
        .all(|key| baseline["inputs"][key].as_f64() == candidate["inputs"][key].as_f64())
        && ENVIRONMENT.iter().all(|key| {
            baseline["environment"][key].as_str() == candidate["environment"][key].as_str()
        })
        && baseline["environment"]["total_memory_bytes"].as_f64()
            == candidate["environment"]["total_memory_bytes"].as_f64();
    let before_rows = baseline["results"].as_array().expect("validated rows");
    let after_rows = candidate["results"].as_array().expect("validated rows");
    // Vec preserves the original Set's first-seen scenario order.
    let mut scenarios = Vec::new();
    let mut latency = Vec::new();
    let mut resources = Vec::new();
    for row in before_rows {
        let scenario = row["scenario"].as_str().expect("validated scenario");
        if scenarios.contains(&scenario) {
            continue;
        }
        scenarios.push(scenario);
        let before: Vec<_> = before_rows
            .iter()
            .filter(|row| row["scenario"] == scenario)
            .collect();
        let after: Vec<_> = after_rows
            .iter()
            .filter(|row| row["scenario"] == scenario)
            .collect();
        let p95 = max(&before, "route_ms", "p95");
        let limit = p95 * 2.0;
        latency.push(json!({
            "scenario":scenario,
            "baseline_max_p95_ms":rounded(p95),
            "candidate_max_p95_ms":rounded(max(&after,"route_ms","p95")),
            "limit_ms":rounded(limit),
            "passed":after.len()==before.len() && after.iter().all(|row|row["route_ms"]["p95"].as_f64().unwrap()<=limit),
            "rationale":"Local regression guard: twice the largest p95 across three pre-change rounds; not a universal latency target."
        }));
        let heap = max(&before, "memory_bytes", "sampled_peak_heap_delta");
        let delay = max(&before, "event_loop_delay_ms", "p95");
        resources.push(json!({
            "scenario":scenario,
            "sampled_peak_heap_limit_bytes":wire_number(heap*2.0),
            "candidate_sampled_peak_heap_bytes":wire_number(max(&after,"memory_bytes","sampled_peak_heap_delta")),
            "event_loop_p95_limit_ms":rounded(delay*2.0),
            "candidate_event_loop_p95_ms":rounded(max(&after,"event_loop_delay_ms","p95")),
            "passed":after.len()==before.len() && after.iter().all(|row|
                row["memory_bytes"]["sampled_peak_heap_delta"].as_f64().unwrap()<=heap*2.0
                && row["event_loop_delay_ms"]["p95"].as_f64().unwrap()<=delay*2.0)
        }));
    }
    let coalesced: Vec<_> = after_rows
        .iter()
        .filter(|row| {
            matches!(
                row["scenario"].as_str(),
                Some("concurrent_identical" | "concurrent_identical_agents")
            )
        })
        .collect();
    let concurrency = candidate["inputs"]["concurrency"].as_f64().unwrap();
    let evaluator_calls = !coalesced.is_empty()
        && coalesced.iter().all(|row| {
            row["evaluator_calls"].as_f64().unwrap()
                == row["requests"].as_f64().unwrap() / concurrency
        });
    Ok(json!({
        "passed": comparable && evaluator_calls && latency.iter().all(|row|row["passed"]==true)
            && resources.iter().all(|row|row["passed"]==true),
        "comparable":{
            "passed":comparable,
            "expected":"Same workload parameters, Node version and hardware; background load remains reported separately."
        },
        "evaluator_calls":{
            "passed":evaluator_calls,
            "expected":"One evaluation per group of eight identical concurrent requests, including different agents."
        },
        "latency":latency,"resources":resources
    }))
}

pub(super) fn run(path: &Path) -> Result<bool, String> {
    let bytes = crate::process::read_bounded(path, MAX_BYTES).map_err(
        |_| "Cannot read historical benchmark report; expected a regular file at most 16 MiB",
    )?;
    let document: Value = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
    let comparison = compare_document(&document)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&comparison)
            .map_err(|_| "Cannot serialize historical benchmark comparison")?
    );
    Ok(comparison["passed"] == true)
}

#[cfg(test)]
#[path = "benchmark_legacy_contracts.rs"]
mod contracts;
