//! Pure validation and exact categorical comparison, never inference or clocks.
use crate::ollama_evaluation::{body_for, order_for_round, stress_case};
use autorouter_core::evaluation_report::{create_evaluation_policy, evaluate_routing_report};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const TIERS: &[&str] = &["haiku", "sonnet", "opus"];
// JSON's numeric value is the contract (Node writes1, serde may write1.0).
// This does not introduce a tolerance or change any threshold/sample value.
include!("../../crates/autorouter-core/src/json_compare_macros.rs");
contract_json_equal!(equivalent, a, b => a.as_f64() == b.as_f64());
pub(super) fn policy() -> Value {
    json!({"profile":"compatible","minAgreement":1,"maxUnderRouteRate":0,"requiredTiers":["haiku","sonnet","opus"]})
}
pub(super) fn fixture(bytes: &[u8]) -> Result<Vec<Value>, String> {
    let items: Value =
        serde_json::from_slice(bytes).map_err(|_| "Invalid frozen local comparison fixture")?;
    let items = items
        .as_array()
        .ok_or("Invalid frozen local comparison fixture")?;
    let mut ids = BTreeSet::new();
    for item in items {
        if !item["id"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 200 && ids.insert(s))
            || !item["expected"]
                .as_str()
                .is_some_and(|s| TIERS.contains(&s))
            || !item["split"]
                .as_str()
                .is_some_and(|s| ["heldout", "tuning"].contains(&s))
            || (!item["prompt"].is_string() && !item["messages"].is_array())
        {
            return Err("Invalid frozen local comparison fixture".into());
        }
    }
    let selected: Vec<_> = items
        .iter()
        .filter(|v| v["split"] == "heldout")
        .cloned()
        .collect();
    if selected.len() != 30 {
        return Err("Frozen held-out comparison requires exactly30 cases".into());
    }
    Ok(selected)
}
fn valid_row(row: &Value, item: &Value, round: Option<u32>) -> Result<(), String> {
    let fields = row
        .as_object()
        .ok_or("Invalid local comparison row object")?;
    for required in [
        "case",
        "split",
        "expected",
        "actual",
        "classified_tier",
        "source",
        "evaluator",
        "selected_model",
        "confirmed_model",
        "wall_ms",
        "state_bytes",
    ] {
        if !fields.contains_key(required) {
            return Err("Missing original local comparison row field".into());
        }
    }
    if fields.keys().any(|key| {
        ![
            "case",
            "split",
            "expected",
            "actual",
            "classified_tier",
            "source",
            "evaluator",
            "selected_model",
            "confirmed_model",
            "wall_ms",
            "state_bytes",
            "round",
            "metrics",
            "error",
            "http_status",
        ]
        .contains(&key.as_str())
    }) {
        return Err("Unknown local comparison row schema".into());
    }
    if row["case"] != item["id"]
        || row["expected"] != item["expected"]
        || row["split"] != item["split"]
        || row.get("round") != round.map(Value::from).as_ref()
        || row["evaluator"] != "ollama"
        || row["selected_model"] != Value::Null
        || row["confirmed_model"] != Value::Null
        || !row["wall_ms"]
            .as_f64()
            .is_some_and(|n| n.is_finite() && n >= 0.)
        || !row["state_bytes"]
            .as_u64()
            .is_some_and(|n| (1..=65536).contains(&n))
    {
        return Err("Invalid or mismatched local comparison row".into());
    }
    if row["actual"].is_null() {
        if row["classified_tier"] != Value::Null
            || row["source"] != "error"
            || !row["error"].as_str().is_some_and(|s| {
                ["timeout", "http_error", "invalid_response", "local_error"].contains(&s)
            })
        {
            return Err("Invalid failed local comparison row".into());
        }
    } else if !row["actual"].as_str().is_some_and(|s| TIERS.contains(&s))
        || row["classified_tier"] != row["actual"]
        || row["source"] != "ollama"
        || row.get("error").is_some()
    {
        return Err("Invalid classified local comparison row".into());
    }
    if let Some(status) = row.get("http_status")
        && (row["error"] != "http_error"
            || !status.as_u64().is_some_and(|s| (100..=599).contains(&s)))
    {
        return Err("Invalid local comparison HTTP status".into());
    }
    if let Some(metrics) = row.get("metrics") {
        let object = metrics
            .as_object()
            .ok_or("Invalid provider-token metrics")?;
        if object
            .keys()
            .any(|k| !["input_tokens", "output_tokens"].contains(&k.as_str()))
            || object
                .values()
                .any(|n| !n.as_u64().is_some_and(|n| n <= 9_007_199_254_740_991))
        {
            return Err("Invalid provider-token metrics".into());
        }
    }
    // Verify extraction deterministically; this does not make a model call.
    let state = autorouter_core::prompt_state::build_ollama_state_document(&body_for(item), 3000);
    if row["state_bytes"].as_u64() != Some(state.stringify().len() as u64) {
        return Err("Local comparison task-state size differs from the frozen fixture".into());
    }
    Ok(())
}
fn acceptance(rows: &Value, required: &[&str]) -> Result<Value, String> {
    let mut value = policy();
    value["requiredTiers"] = json!(required);
    let typed = create_evaluation_policy(Some(&value))?;
    let value = serde_json::to_value(typed).map_err(|_| "Invalid fixed policy")?;
    evaluate_routing_report(
        rows,
        &json!({"evaluator":"ollama","classifierOnly":true,"policy":value}),
    )
}
pub(super) struct Validated {
    pub rows: Vec<(String, Value)>,
    pub quality_passed: bool,
    pub complete_matrix: bool,
    pub context: Value,
    pub context_observed_stable: bool,
    pub provider_metrics_complete: bool,
}
fn context(model: &Value) -> (Value, bool) {
    let observe = |key: &str| json!({"context_length":model[key]["context_length"],"parameter_size":model[key]["parameter_size"],"quantization":model[key]["quantization"]});
    let cold = observe("resident_memory");
    let warm = observe("final_resident_memory");
    let identified = |v: &Value| {
        v["context_length"].as_f64().is_some_and(|n| {
            n.is_finite() && n > 0. && n.fract() == 0. && n <= 9_007_199_254_740_991.
        }) && v["parameter_size"].is_string()
            && v["quantization"].is_string()
    };
    let stable = identified(&cold) && identified(&warm) && cold == warm;
    (json!({"cold":cold,"warm":warm}), stable)
}
pub(super) fn report(
    report: &Value,
    declaration: &Value,
    selected: &[Value],
) -> Result<Validated, String> {
    for (key, expected) in [
        ("type", json!("ollama_routing_evaluation")),
        ("fixture_sha256", declaration["fixture_sha256"].clone()),
        ("quality_policy", policy()),
        (
            "ollama_version",
            declaration["model"]["ollama_version"].clone(),
        ),
        ("split", json!("heldout")),
        ("rounds", json!(3)),
        ("stress_rounds", json!(8)),
        ("warm_timeout_ms", declaration["warm_timeout_ms"].clone()),
        ("cold_timeout_ms", json!(60000)),
        ("state_character_budget", json!(3000)),
        ("complete", json!(true)),
    ] {
        if !equivalent(&report[key], &expected) {
            return Err("Local comparison report metadata or policy differs".into());
        }
    }
    let models = report["models"]
        .as_array()
        .filter(|m| m.len() == 1)
        .ok_or("Local comparison report must contain exactly one model")?;
    let model = &models[0];
    for (key, expected) in [
        ("model", declaration["model"]["tag"].clone()),
        ("digest", declaration["model"]["digest"].clone()),
        (
            "download_bytes",
            declaration["model"]["download_bytes"].clone(),
        ),
        ("protocol", json!("/v1/systemone")),
        ("rubric_sha256", declaration["rubric_sha256"].clone()),
    ] {
        if !equivalent(&model[key], &expected) {
            return Err("Local comparison report model or rubric identity differs".into());
        }
    }
    valid_row(&model["cold"], &selected[0], None)?;
    let mut compared = vec![("cold".into(), model["cold"].clone())];
    let warm = model["rows"]
        .as_array()
        .ok_or("Missing warm comparison rows")?;
    if model["cold"]["actual"].is_null() {
        if !warm.is_empty() || !model["skipped"].is_string() || report["passed"] != false {
            return Err("Cold failure cannot be represented as a completed warm run".into());
        }
        let (context, context_observed_stable) = context(model);
        return Ok(Validated {
            rows: compared,
            quality_passed: false,
            complete_matrix: false,
            context,
            context_observed_stable,
            provider_metrics_complete: false,
        });
    }
    if model.get("skipped").is_some() || warm.len() != 90 {
        return Err("Incomplete fixed warm comparison matrix".into());
    }
    let mut at = 0;
    for round in 0..3 {
        for item in order_for_round(selected, round) {
            valid_row(&warm[at], &item, Some(round + 1))?;
            compared.push((
                format!("warm:{}:{}", round + 1, item["id"].as_str().unwrap()),
                warm[at].clone(),
            ));
            at += 1;
        }
    }
    let strict = acceptance(&model["rows"], TIERS)?;
    if !equivalent(&strict, &model["acceptance"]) {
        return Err("Stored warm acceptance differs from unchanged strict policy".into());
    }
    let stress = model["max_budget_stress"]["rows"]
        .as_array()
        .filter(|v| v.len() == 8)
        .ok_or("Incomplete fixed stress comparison matrix")?;
    for (index, row) in stress.iter().enumerate() {
        let item = stress_case(index);
        valid_row(row, &item, None)?;
        compared.push((
            format!("stress:{}", item["id"].as_str().unwrap()),
            row.clone(),
        ));
    }
    let stress_strict = acceptance(&model["max_budget_stress"]["rows"], &["haiku"])?;
    if !equivalent(&stress_strict, &model["max_budget_stress"]["acceptance"]) {
        return Err("Stored stress acceptance differs from unchanged strict policy".into());
    }
    let quality_passed = strict["passed"] == true && stress_strict["passed"] == true;
    if report["passed"] != quality_passed {
        return Err("Stored quality outcome differs from strict warm/stress outcomes".into());
    }
    let (context, context_observed_stable) = context(model);
    let provider_metrics_complete = compared.iter().all(|(_, row)| {
        row["actual"].is_null()
            || (row["metrics"]["input_tokens"].as_u64().is_some()
                && row["metrics"]["output_tokens"].as_u64().is_some())
    });
    Ok(Validated {
        rows: compared,
        quality_passed,
        complete_matrix: true,
        context,
        context_observed_stable,
        provider_metrics_complete,
    })
}
fn categorical(row: &Value) -> Value {
    let mut value = row.clone();
    value.as_object_mut().unwrap().remove("wall_ms");
    value
}
pub(super) fn compare(node: &Validated, native: &Validated) -> Value {
    let mut keys: BTreeSet<String> = node
        .rows
        .iter()
        .chain(&native.rows)
        .map(|(key, _)| key.clone())
        .collect();
    let mut rows = vec![];
    let mut differences = 0;
    let mut availability = 0;
    for key in std::mem::take(&mut keys) {
        let n = node
            .rows
            .iter()
            .find(|(k, _)| k == &key)
            .map(|(_, v)| categorical(v));
        let r = native
            .rows
            .iter()
            .find(|(k, _)| k == &key)
            .map(|(_, v)| categorical(v));
        let matched = n == r;
        let availability_difference =
            n.as_ref().map(|v| v["actual"].is_null()) != r.as_ref().map(|v| v["actual"].is_null());
        differences += usize::from(!matched);
        availability += usize::from(availability_difference);
        rows.push(json!({"key":key,"matched":matched,"availability_difference":availability_difference,"node":n,"native":r}));
    }
    let context_available_and_equal = node.context_observed_stable
        && native.context_observed_stable
        && equivalent(&node.context, &native.context);
    let provider_token_metrics_complete =
        node.provider_metrics_complete && native.provider_metrics_complete;
    json!({"complete_matrices":node.complete_matrix&&native.complete_matrix,"language_port_equal":differences==0&&node.complete_matrix&&native.complete_matrix&&context_available_and_equal&&provider_token_metrics_complete,"categorical_state_token_equal":differences==0,"categorical_state_token_differences":differences,"bidirectional_availability_differences":availability,"context_available_stable_and_equal":context_available_and_equal,"observed_context":{"node":node.context,"native":native.context},"provider_token_metrics_complete":provider_token_metrics_complete,"node_quality_passed":node.quality_passed,"native_quality_passed":native.quality_passed,"rows":rows,"clock_exclusion":"Only per-request wall_ms is excluded from categorical equality; all original reports retain every cold/warm/stress wall measurement. Other fields, missing fields, errors, state byte counts and input/output token metrics are compared exactly. Missing context/token evidence stays unverified."})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(actual: Option<&str>, state: u64, tokens: Option<u64>) -> Value {
        let mut r = json!({"case":"synthetic","actual":actual,"wall_ms":1.,"state_bytes":state});
        if let Some(tokens) = tokens {
            r["metrics"] = json!({"input_tokens":tokens,"output_tokens":0});
        }
        if actual.is_none() {
            r["error"] = json!("timeout");
        }
        r
    }
    fn outcome(row: Value) -> Validated {
        Validated {
            rows: vec![("stress:full-budget-0".into(), row)],
            quality_passed: false,
            complete_matrix: true,
            context: json!({"cold":{"context_length":2048,"parameter_size":"synthetic","quantization":"synthetic"},"warm":{"context_length":2048,"parameter_size":"synthetic","quantization":"synthetic"}}),
            context_observed_stable: true,
            provider_metrics_complete: true,
        }
    }
    #[test]
    fn finite_deadline_bidirectional_availability_failures_are_never_erased() {
        let pass = outcome(row(Some("haiku"), 3000, Some(1040)));
        let fail = outcome(row(None, 3000, None));
        for comparison in [compare(&pass, &fail), compare(&fail, &pass)] {
            assert_eq!(comparison["bidirectional_availability_differences"], 1);
            assert_eq!(comparison["language_port_equal"], false);
            assert_eq!(comparison["native_quality_passed"], false);
            assert_eq!(comparison["rows"][0]["matched"], false);
        }
    }
    #[test]
    fn token_state_and_absent_metrics_differences_survive_clock_exclusion() {
        let base = outcome(row(Some("haiku"), 3000, Some(1040)));
        let mut changed = row(Some("haiku"), 3000, Some(1040));
        changed["wall_ms"] = json!(9999.);
        assert_eq!(
            compare(&base, &outcome(changed))["language_port_equal"],
            true
        );
        for other in [
            row(Some("haiku"), 2999, Some(1040)),
            row(Some("haiku"), 3000, Some(1041)),
            row(Some("haiku"), 3000, None),
        ] {
            assert_eq!(
                compare(&base, &outcome(other))["categorical_state_token_differences"],
                1
            );
        }
    }
    #[test]
    fn incomplete_or_skipped_cold_matrix_cannot_qualify_even_when_labels_match() {
        let complete = outcome(row(Some("haiku"), 3000, Some(1040)));
        let mut incomplete = outcome(row(Some("haiku"), 3000, Some(1040)));
        incomplete.complete_matrix = false;
        assert_eq!(
            compare(&complete, &incomplete)["language_port_equal"],
            false
        );
        assert_eq!(compare(&complete, &incomplete)["complete_matrices"], false);
    }
    fn synthetic_complete_report() -> (Value, Value, Vec<Value>) {
        let bytes = include_bytes!("../../../test/fixtures/ollama-routing.json");
        let selected = fixture(bytes).unwrap();
        let rubric = crate::evaluation::digest(
            autorouter_runtime::evaluator::ollama_questions()
                .to_string()
                .as_bytes(),
        );
        let declaration = json!({"fixture_sha256":crate::evaluation::digest(bytes),"warm_timeout_ms":1500,"rubric_sha256":rubric,"model":{"tag":"synthetic:1","digest":"a".repeat(64),"download_bytes":100,"ollama_version":"0.40.0"}});
        let make = |item: &Value, round: Option<u32>| {
            let state =
                autorouter_core::prompt_state::build_ollama_state_document(&body_for(item), 3000);
            let mut value = json!({"case":item["id"],"split":item["split"],"expected":item["expected"],"actual":item["expected"],"classified_tier":item["expected"],"source":"ollama","evaluator":"ollama","selected_model":null,"confirmed_model":null,"wall_ms":1.,"state_bytes":state.stringify().len(),"metrics":{"input_tokens":700,"output_tokens":0}});
            if let Some(round) = round {
                value["round"] = json!(round);
            }
            value
        };
        let mut warm = vec![];
        for round in 0..3 {
            for item in order_for_round(&selected, round) {
                warm.push(make(&item, Some(round + 1)));
            }
        }
        let stress: Vec<_> = (0..8).map(|i| make(&stress_case(i), None)).collect();
        let context =
            json!({"context_length":2048,"parameter_size":"synthetic","quantization":"synthetic"});
        let report = json!({"type":"ollama_routing_evaluation","fixture_sha256":declaration["fixture_sha256"],"quality_policy":policy(),"ollama_version":"0.40.0","split":"heldout","rounds":3,"stress_rounds":8,"warm_timeout_ms":1500,"cold_timeout_ms":60000,"state_character_budget":3000,"complete":true,"passed":true,"models":[{"model":"synthetic:1","digest":"a".repeat(64),"download_bytes":100,"protocol":"/v1/systemone","rubric_sha256":rubric,"cold":make(&selected[0],None),"resident_memory":context,"final_resident_memory":context,"rows":warm,"acceptance":acceptance(&json!(warm),TIERS).unwrap(),"max_budget_stress":{"rows":stress,"acceptance":acceptance(&json!(stress),&["haiku"]).unwrap()}}]});
        (report, declaration, selected)
    }
    #[test]
    fn complete_frozen_schema_rejects_omission_relabeling_deduplication_policy_and_private_extensions()
     {
        let (original, declaration, selected) = synthetic_complete_report();
        let accepted = report(&original, &declaration, &selected).unwrap();
        assert!(accepted.quality_passed && accepted.complete_matrix);
        for mutation in 0..6 {
            let mut changed = original.clone();
            match mutation {
                0 => {
                    changed["models"][0]["rows"].as_array_mut().unwrap().pop();
                }
                1 => changed["models"][0]["rows"][0]["expected"] = json!("relabeled"),
                2 => changed["models"][0]["rows"][1] = changed["models"][0]["rows"][0].clone(),
                3 => changed["quality_policy"]["minAgreement"] = json!(0.9),
                4 => {
                    changed["models"][0]["rows"][0]
                        .as_object_mut()
                        .unwrap()
                        .remove("actual");
                }
                _ => {
                    changed["models"][0]["rows"][0]["unfamiliar_private_extension"] =
                        json!("synthetic-private-marker")
                }
            }
            let error = report(&changed, &declaration, &selected).err().unwrap();
            assert!(!error.contains("synthetic-private-marker"));
        }
    }
    #[test]
    fn unchanged_strict_policy_keeps_inherited_wrong_labels_and_missing_context_unqualified() {
        let (mut value, declaration, selected) = synthetic_complete_report();
        let rows = value["models"][0]["rows"].as_array_mut().unwrap();
        let wrong = rows.iter_mut().find(|r| r["expected"] == "opus").unwrap();
        wrong["actual"] = json!("haiku");
        wrong["classified_tier"] = json!("haiku");
        value["models"][0]["acceptance"] = acceptance(&value["models"][0]["rows"], TIERS).unwrap();
        value["passed"] = json!(false);
        let result = report(&value, &declaration, &selected).unwrap();
        assert!(!result.quality_passed);
        assert!(result.complete_matrix);
        value["models"][0]["resident_memory"]
            .as_object_mut()
            .unwrap()
            .remove("context_length");
        let unknown = report(&value, &declaration, &selected).unwrap();
        let compared = compare(&result, &unknown);
        assert_eq!(compared["context_available_stable_and_equal"], false);
        assert_eq!(compared["language_port_equal"], false);
        assert!(equivalent(&json!(1), &json!(1.0)));
        assert!(!equivalent(&json!(1), &json!(0.999999999999)));
    }
}
