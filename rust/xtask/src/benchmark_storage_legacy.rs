//! Four unchanged historical status gates. Native protocols are ineligible.
use serde_json::{Value, json};
fn number(value: &Value) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.)
        .ok_or_else(|| "Historical status metric must be finite and nonnegative".into())
}
fn rounded(value: f64) -> Value {
    let scaled = value * 1000.;
    let floor = scaled.floor();
    let value = (if scaled - floor >= 0.5 {
        floor + 1.
    } else {
        floor
    }) / 1000.;
    if value.is_finite() {
        json!(value)
    } else {
        Value::Null
    }
}
fn scenario(run: &Value, delay: u64) -> Result<&Value, String> {
    let scenarios = run["scenarios"]
        .as_array()
        .ok_or("Missing historical status scenarios")?;
    if scenarios.len() != 2 {
        return Err("Historical status requires exactly two scenarios".into());
    }
    let mut matching = scenarios
        .iter()
        .filter(|v| v["injected_write_delay_ms"] == delay);
    let value = matching
        .next()
        .ok_or("Missing historical status scenario")?;
    if matching.next().is_some() {
        return Err("Duplicate historical status scenario".into());
    }
    Ok(value)
}
fn historical(run: &Value) -> Result<(), String> {
    if run["schema_version"] != 1
        || run.get("kind").is_some()
        || run.get("protocol_sha256").is_some()
        || run.get("implementation").is_some()
        || run.get("acceptance_qualified").is_some()
    {
        return Err("Only historical status reports can use these gates".into());
    }
    let method = &run["method"];
    for (key, expected) in [
        ("rounds_per_run", 60),
        ("repetitions", 3),
        ("events_per_round", 6),
        ("concurrent_sessions", 20),
        ("producer_interval_ms", 5),
        ("mock_stream_interval_ms", 5),
    ] {
        if method[key] != expected {
            return Err("Historical status workload identity mismatch".into());
        }
    }
    for delay in [0, 20] {
        let s = scenario(run, delay)?;
        for metric in ["update_ms", "flush_call_ms", "stream_timer_lateness_ms"] {
            number(&s["timings"][metric]["p95"])?;
        }
    }
    Ok(())
}
pub(super) fn compare(bytes: &[u8], label: &str) -> Result<Value, String> {
    if bytes.len() > 16 * 1024 * 1024
        || label.is_empty()
        || label.len() > 100
        || label == "synchronous-baseline"
    {
        return Err("Invalid historical status comparison input".into());
    }
    let document: Value =
        serde_json::from_slice(bytes).map_err(|_| "Invalid historical status JSON")?;
    if document["schema_version"] != 1 || document.get("kind").is_some() {
        return Err("Invalid historical status envelope".into());
    }
    let runs = document["runs"]
        .as_array()
        .filter(|r| r.len() <= 4096)
        .ok_or("Historical status runs missing or oversized")?;
    let lookup = |name: &str| -> Result<&Value, String> {
        let mut rows = runs.iter().filter(|r| r["label"] == name);
        let row = rows
            .next()
            .ok_or("Historical baseline or candidate label missing")?;
        if rows.next().is_some() {
            return Err("Duplicate historical status label".into());
        }
        historical(row)?;
        Ok(row)
    };
    let baseline = lookup("synchronous-baseline")?;
    let candidate = lookup(label)?;
    let mut gates = Vec::new();
    for (name, delay, metric, factor) in [
        (
            "slow_storage_stream_p95_ms",
            20,
            "stream_timer_lateness_ms",
            0.25,
        ),
        ("slow_storage_flush_call_p95_ms", 20, "flush_call_ms", 0.1),
        (
            "normal_storage_stream_p95_ms",
            0,
            "stream_timer_lateness_ms",
            1.25,
        ),
        ("normal_storage_update_p95_ms", 0, "update_ms", 2.),
    ] {
        let actual = number(&scenario(candidate, delay)?["timings"][metric]["p95"])?;
        let base = number(&scenario(baseline, delay)?["timings"][metric]["p95"])?;
        let maximum = match name {
            "slow_storage_stream_p95_ms" => base / 4.,
            "slow_storage_flush_call_p95_ms" => base / 10.,
            _ => base * factor,
        };
        if !maximum.is_finite() {
            return Err("Historical status gate arithmetic overflow".into());
        }
        gates.push(json!({"name":name,"actual":rounded(actual),"maximum":rounded(maximum),"passed":actual<=maximum}));
    }
    Ok(
        json!({"schema_version":1,"kind":"historical_status_comparison","baseline_label":"synchronous-baseline","candidate_label":label,"passed":gates.iter().all(|g|g["passed"]==true),"gates":gates,"native_acceptance_qualified":false,"scope":"Four original recorded synchronous-baseline gates; local historical reports only. Comparisons precede display rounding; no files changed."}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Value {
        let run = |label| json!({"schema_version":1,"label":label,"method":{"rounds_per_run":60,"repetitions":3,"events_per_round":6,"concurrent_sessions":20,"producer_interval_ms":5,"mock_stream_interval_ms":5},"scenarios":[{"injected_write_delay_ms":0,"timings":{"stream_timer_lateness_ms":{"p95":8},"flush_call_ms":{"p95":8},"update_ms":{"p95":8}}},{"injected_write_delay_ms":20,"timings":{"stream_timer_lateness_ms":{"p95":40},"flush_call_ms":{"p95":40},"update_ms":{"p95":8}}}]});
        json!({"schema_version":1,"runs":[run("synchronous-baseline"),run("candidate")]})
    }
    #[test]
    fn original_formulas_and_each_failure_are_independent() {
        let mut f = fixture();
        f["runs"][1]["scenarios"][1]["timings"]["stream_timer_lateness_ms"]["p95"] = json!(10);
        f["runs"][1]["scenarios"][1]["timings"]["flush_call_ms"]["p95"] = json!(4);
        assert_eq!(
            compare(f.to_string().as_bytes(), "candidate").unwrap()["passed"],
            true
        );
        for (index, metric, bad) in [
            (1, "stream_timer_lateness_ms", 10.001),
            (1, "flush_call_ms", 4.001),
            (0, "stream_timer_lateness_ms", 10.001),
            (0, "update_ms", 16.001),
        ] {
            let mut changed = f.clone();
            changed["runs"][1]["scenarios"][index]["timings"][metric]["p95"] = json!(bad);
            let r = compare(changed.to_string().as_bytes(), "candidate").unwrap();
            assert_eq!(
                r["gates"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|g| g["passed"] == false)
                    .count(),
                1
            );
        }
    }
    #[test]
    fn tiny_real_regression_cannot_hide_behind_display_rounding() {
        let mut f = fixture();
        f["runs"][1]["scenarios"][1]["timings"]["stream_timer_lateness_ms"]["p95"] =
            json!(10.00001);
        f["runs"][1]["scenarios"][1]["timings"]["flush_call_ms"]["p95"] = json!(4);
        let r = compare(f.to_string().as_bytes(), "candidate").unwrap();
        assert_eq!(r["gates"][0]["actual"], r["gates"][0]["maximum"]);
        assert_eq!(r["passed"], false);
    }
    #[test]
    fn absent_duplicate_native_malformed_and_oversized_reports_reject() {
        let f = fixture();
        for bad in [
            json!({}),
            json!({"schema_version":1,"runs":[]}),
            json!({"schema_version":1,"runs":[f["runs"][0].clone(),f["runs"][0].clone(),f["runs"][1].clone()]}),
        ] {
            assert!(compare(bad.to_string().as_bytes(), "candidate").is_err());
        }
        for key in [
            "kind",
            "protocol_sha256",
            "implementation",
            "acceptance_qualified",
        ] {
            let mut bad = f.clone();
            bad["runs"][1][key] = json!("native");
            assert!(compare(bad.to_string().as_bytes(), "candidate").is_err());
        }
        for number in [Value::Null, json!(-1), json!("NaN")] {
            let mut bad = f.clone();
            bad["runs"][1]["scenarios"][0]["timings"]["update_ms"]["p95"] = number;
            assert!(compare(bad.to_string().as_bytes(), "candidate").is_err());
        }
        assert!(compare(b"{", "candidate").is_err());
        assert!(compare(&vec![b' '; 16 * 1024 * 1024 + 1], "candidate").is_err());
    }
    #[test]
    fn frozen_division_operator_boundary_is_not_algebraic_multiplication() {
        let mut f = fixture();
        f["runs"][0]["scenarios"][1]["timings"]["flush_call_ms"]["p95"] = json!(0.21);
        f["runs"][1]["scenarios"][1]["timings"]["flush_call_ms"]["p95"] = json!(0.021);
        let report = compare(f.to_string().as_bytes(), "candidate").unwrap();
        assert_eq!(
            report["gates"][1]["passed"], false,
            "Frozen JS divides by 10 before comparing, without tolerance"
        );
    }
    #[test]
    fn display_rounding_does_not_add_a_half_to_a_large_integral_scaled_value() {
        assert_eq!(rounded(4503599627370.497), json!(4503599627370.497));
        assert_eq!(rounded(0.0005), json!(0.001));
        assert_eq!(rounded(0.0004999999999999999), json!(0.0));
    }
}
