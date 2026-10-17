//! Declared acceptance gates for opt-in evaluator and live harnesses.
//! Transport, fresh evaluation, rubric agreement, selection, coverage and
//! independently verified task outcomes remain separate evidence.
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

const TIERS: &[&str] = &["haiku", "sonnet", "opus"];
const TRANSPORT_CHECKS: &[&str] = &[
    "claude_success",
    "requests_reached_router",
    "upstream_model_evidence",
    "no_upstream_api_errors",
    "no_proxy_errors",
];
const TASK_CHECKS: &[&str] = &[
    "expected_result",
    "original_tests_preserved",
    "independent_tests_pass",
    "read_edit_bash_exercised",
];
fn tier(value: &Value) -> bool {
    value.as_str().is_some_and(|v| TIERS.contains(&v))
}
fn text<'a>(value: &'a Value, field: &str) -> &'a str {
    value.get(field).and_then(Value::as_str).unwrap_or("")
}
fn nullish<'a>(value: Option<&'a Value>, fallback: &'a Value) -> &'a Value {
    value.filter(|v| !v.is_null()).unwrap_or(fallback)
}
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null | Value::Bool(false)) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|v| v != 0.0 && !v.is_nan()),
        _ => true,
    }
}
fn all_passed(gates: &Value) -> bool {
    gates
        .as_object()
        .is_some_and(|gates| gates.values().all(|gate| gate["passed"] != false))
}

pub fn model_tier(model: &str) -> Option<&'static str> {
    let model = model.strip_prefix("claude-")?;
    TIERS.iter().copied().find(|tier| {
        model
            .strip_prefix(tier)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
    })
}
pub fn profile_tier<'a>(tier: &'a str, profile: &str) -> &'a str {
    if profile == "auto" && tier == "haiku" {
        "sonnet"
    } else {
        tier
    }
}
pub fn parse_quality_threshold(value: &Value) -> Result<f64, String> {
    let malformed = "Quality thresholds must be decimal numbers between 0 and 1";
    let value = value.as_str().ok_or(malformed)?;
    let valid = if let Some((whole, fraction)) = value.split_once('.') {
        (whole.is_empty() || whole.bytes().all(|v| v.is_ascii_digit()))
            && !fraction.is_empty()
            && fraction.bytes().all(|v| v.is_ascii_digit())
    } else {
        !value.is_empty() && value.bytes().all(|v| v.is_ascii_digit())
    };
    if !valid {
        return Err(malformed.into());
    }
    let number = value
        .parse::<f64>()
        .map_err(|_| "Quality thresholds must be between 0 and 1")?;
    if !number.is_finite() || !(0.0..=1.0).contains(&number) {
        return Err("Quality thresholds must be between 0 and 1".into());
    }
    Ok(number)
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationPolicy {
    pub profile: String,
    pub min_agreement: f64,
    pub max_under_route_rate: f64,
    pub required_tiers: Vec<String>,
}
impl Default for EvaluationPolicy {
    fn default() -> Self {
        Self {
            profile: "compatible".into(),
            min_agreement: 1.0,
            max_under_route_rate: 0.0,
            required_tiers: TIERS.iter().map(|v| (*v).into()).collect(),
        }
    }
}
pub fn create_evaluation_policy(options: Option<&Value>) -> Result<EvaluationPolicy, String> {
    let empty = json!({});
    let options = options.unwrap_or(&empty);
    let profile = options.get("profile").unwrap_or(&Value::Null);
    let profile = if options.get("profile").is_none() {
        "compatible"
    } else {
        profile.as_str().unwrap_or("")
    };
    if !["compatible", "native", "auto"].contains(&profile) {
        return Err("Evaluation profile must be compatible, native, or auto".into());
    }
    let mut thresholds = [1.0, 0.0];
    for (i, name) in ["minAgreement", "maxUnderRouteRate"].iter().enumerate() {
        if let Some(value) = options.get(*name) {
            thresholds[i] = value
                .as_f64()
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
                .ok_or_else(|| format!("{name} must be between 0 and 1"))?;
        }
    }
    let default = json!(match profile {
        "native" => Vec::new(),
        "auto" => vec!["sonnet", "opus"],
        _ => TIERS.to_vec(),
    });
    let tiers = nullish(options.get("requiredTiers"), &default)
        .as_array()
        .ok_or("Invalid required tier coverage for evaluation profile")?;
    let mut seen = BTreeSet::new();
    let mut required = Vec::new();
    for value in tiers {
        let value = value
            .as_str()
            .filter(|v| TIERS.contains(v) && (profile != "auto" || *v != "haiku"))
            .ok_or("Invalid required tier coverage for evaluation profile")?;
        if !seen.insert(value) {
            return Err("Invalid required tier coverage for evaluation profile".into());
        }
        required.push(value.into());
    }
    Ok(EvaluationPolicy {
        profile: profile.into(),
        min_agreement: thresholds[0],
        max_under_route_rate: thresholds[1],
        required_tiers: required,
    })
}
pub fn tier_coverage(tiers: &[Value], policy: &EvaluationPolicy) -> Value {
    let observed: BTreeSet<&str> = tiers
        .iter()
        .filter(|v| tier(v))
        .filter_map(Value::as_str)
        .collect();
    let missing: Vec<&str> = policy
        .required_tiers
        .iter()
        .map(String::as_str)
        .filter(|v| !observed.contains(v))
        .collect();
    json!({"passed":missing.is_empty(),"required":policy.required_tiers,"observed":observed,"missing":missing})
}
fn evaluator_gate(rows: &[Value], evaluator: &str, expect_outage: bool) -> Result<Value, String> {
    if !["jev", "ollama"].contains(&evaluator) {
        return Err("Unknown evaluation backend".into());
    }
    let active: Vec<&Value> = rows
        .iter()
        .filter(|row| text(row, "source") != "passthrough")
        .collect();
    let fresh = active
        .iter()
        .filter(|row| text(row, "source") == evaluator)
        .count();
    let fallbacks = active
        .iter()
        .filter(|row| text(row, "source") == "fallback")
        .count();
    let passed = !active.is_empty()
        && if expect_outage {
            fallbacks == active.len()
        } else {
            fresh > 0
                && active.iter().all(|row| {
                    text(row, "source") == evaluator
                        || (text(row, "source") == "cache" && text(row, "evaluator") == evaluator)
                })
        };
    Ok(
        json!({"passed":passed,"expected":if expect_outage {"fallback"} else {evaluator},"evaluated_requests":active.len(),"successful_evaluations":fresh,"fallbacks":fallbacks}),
    )
}
fn selected_tier(row: &Value) -> Value {
    row.get("selected_tier")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| json!(model_tier(text(row, "selected_model"))))
}
pub fn evaluate_routing_report(rows: &Value, options: &Value) -> Result<Value, String> {
    let policy = create_evaluation_policy(options.get("policy"))?;
    let rows = rows
        .as_array()
        .filter(|rows| rows.iter().all(|row| tier(&row["expected"])))
        .ok_or("Invalid evaluation rows")?;
    let evaluator = text(options, "evaluator");
    let classifier_only = truthy(options.get("classifierOnly"));
    let valid: Vec<&Value> = rows
        .iter()
        .filter(|row| {
            tier(&row["classified_tier"])
                && (text(row, "source") == evaluator
                    || (text(row, "source") == "cache" && text(row, "evaluator") == evaluator))
        })
        .collect();
    let agreement = if rows.is_empty() {
        0.0
    } else {
        valid
            .iter()
            .filter(|row| row["classified_tier"] == row["expected"])
            .count() as f64
            / rows.len() as f64
    };
    let under_routes = valid
        .iter()
        .filter(|row| {
            TIERS
                .iter()
                .position(|v| *v == text(row, "classified_tier"))
                < TIERS.iter().position(|v| *v == text(row, "expected"))
        })
        .count();
    let under_route_rate = if rows.is_empty() {
        0.0
    } else {
        under_routes as f64 / rows.len() as f64
    };
    let eligible: Vec<Value> = valid
        .iter()
        .map(|row| {
            if classifier_only {
                row["classified_tier"].clone()
            } else {
                selected_tier(row)
            }
        })
        .collect();
    let policy_gate = if classifier_only {
        json!({"passed":null,"reason":"Classifier-only benchmark; routing policy not exercised"})
    } else {
        json!({"passed": !rows.is_empty() && rows.iter().all(|row| {
            let selected = selected_tier(row);
            tier(&selected) && (policy.profile != "auto" || selected != "haiku") && row.get("expected_selected_tier").is_none_or(|v| *v == selected)
                && row.get("expected_reason").is_none_or(|v| Some(v) == row.get("reason"))
        })})
    };
    let gates = json!({
        "transport":{"passed":null,"reason":"Claude transport not exercised"},
        "evaluator":evaluator_gate(rows,evaluator,false)?,
        "rubric":{"passed":!rows.is_empty() && valid.len() == rows.len() && agreement >= policy.min_agreement && under_route_rate <= policy.max_under_route_rate,
            "agreement":agreement,"under_routes":under_routes,"under_route_rate":under_route_rate,"min_agreement":policy.min_agreement,"max_under_route_rate":policy.max_under_route_rate},
        "policy":policy_gate,"coverage":tier_coverage(&eligible,&policy),"task":{"passed":null,"reason":"No Claude task completion measured"}
    });
    Ok(
        json!({"schema_version":1,"policy":policy,"passed":all_passed(&gates),"gates":gates,"requests":rows.len()}),
    )
}
fn check_gate(checks: &Value, include: impl Fn(&str) -> bool) -> Value {
    let selected: serde_json::Map<String, Value> = checks
        .as_object()
        .into_iter()
        .flat_map(|o| o.iter())
        .filter(|(name, _)| include(name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    json!({"passed":if selected.is_empty(){Value::Null}else{json!(selected.values().all(|v| *v == true))},"checks":selected})
}
fn main_route(row: &Value) -> bool {
    row.get("request_class").is_none_or(|v| {
        v.as_str()
            .is_some_and(|v| ["main", "unspecified", ""].contains(&v))
    })
}
pub fn evaluate_live_case(input: &Value) -> Result<Value, String> {
    let checks = &input["checks"];
    let routes = input["routes"]
        .as_array()
        .ok_or("Invalid evaluation routes")?;
    let expect_outage = truthy(input.get("expectOutage"));
    let main: Vec<&Value> = routes.iter().filter(|row| main_route(row)).collect();
    let labels: Vec<Value> = main
        .iter()
        .map(|row| {
            nullish(
                input.get("expectedClassifiedTier"),
                &row["expected_classified_tier"],
            )
            .clone()
        })
        .collect();
    let rubric = if expect_outage {
        json!({"passed":null,"reason":"Explicit evaluator outage"})
    } else {
        let mut rubric = json!({"passed":!main.is_empty() && labels.iter().all(tier) && main.iter().zip(labels.iter()).all(|(row,label)| row["classified_tier"] == *label),
                "expected":nullish(input.get("expectedClassifiedTier"), &json!(labels))});
        if labels.iter().any(|v| !tier(v)) {
            rubric["reason"] = json!("Missing declared live classifier label");
        }
        rubric
    };
    let mut gates = json!({"transport":check_gate(checks,|n| TRANSPORT_CHECKS.contains(&n)),
        "evaluator":evaluator_gate(routes,text(input,"evaluator"),expect_outage)?,"rubric":rubric,
        "policy":check_gate(checks,|n| !TRANSPORT_CHECKS.contains(&n) && !TASK_CHECKS.contains(&n)),
        "task":check_gate(checks,|n| TASK_CHECKS.contains(&n))});
    if routes.is_empty()
        || gates["transport"]["passed"].is_null()
        || gates["task"]["passed"].is_null()
    {
        gates["transport"]["passed"] = json!(false);
    }
    Ok(json!({"passed":all_passed(&gates),"gates":gates}))
}
pub fn evaluate_live_report(cases: &Value, options: &Value) -> Result<Value, String> {
    let policy = create_evaluation_policy(options.get("policy"))?;
    let cases = cases.as_array().ok_or("Invalid evaluation cases")?;
    let tiers: Vec<Value> = cases
        .iter()
        .flat_map(|item| item["routes"].as_array().into_iter().flatten())
        .filter(|row| main_route(row) && ["jev", "ollama", "cache"].contains(&text(row, "source")))
        .map(|row| json!(model_tier(text(row, "model"))))
        .collect();
    let coverage = if truthy(options.get("expectOutage")) {
        json!({"passed":null,"reason":"Explicit evaluator outage"})
    } else {
        tier_coverage(&tiers, &policy)
    };
    Ok(
        json!({"policy":policy,"passed":!cases.is_empty() && cases.iter().all(|item| item["passed"] == true) && coverage["passed"] != false,"gates":{"coverage":coverage}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rows() -> Value {
        json!(TIERS.iter().map(|tier| json!({"expected":tier,"classified_tier":tier,"selected_model":format!("claude-{tier}-5"),"source":"jev","evaluator":"jev"})).collect::<Vec<_>>())
    }
    #[test]
    fn classifier_labels_do_not_claim_task_or_transport_success() {
        let report =
            evaluate_routing_report(&rows(), &json!({"evaluator":"jev","classifierOnly":true}))
                .unwrap();
        assert_eq!(report["passed"], true);
        for gate in ["policy", "task", "transport"] {
            assert!(report["gates"][gate]["passed"].is_null());
        }
    }
    #[test]
    fn permissive_rubric_does_not_hide_missing_coverage_or_fallbacks() {
        let mut rows = rows();
        for row in rows.as_array_mut().unwrap() {
            row["classified_tier"] = json!("sonnet");
            row["selected_model"] = json!("claude-sonnet-5");
        }
        let report = evaluate_routing_report(
            &rows,
            &json!({"evaluator":"jev","policy":{"minAgreement":0,"maxUnderRouteRate":1}}),
        )
        .unwrap();
        assert_eq!(report["gates"]["rubric"]["passed"], true);
        assert_eq!(
            report["gates"]["coverage"]["missing"],
            json!(["haiku", "opus"])
        );
        assert_eq!(report["passed"], false);
        for row in rows.as_array_mut().unwrap() {
            row["source"] = json!("fallback");
        }
        assert_eq!(
            evaluate_routing_report(&rows, &json!({"evaluator":"jev"})).unwrap()["gates"]["evaluator"]
                ["passed"],
            false
        );
    }
    #[test]
    fn declared_thresholds_are_strict_decimal_before_evaluation() {
        for input in ["NaN", "2", " ", "0x0", "0.5 ", "1e-1", "1."] {
            assert!(parse_quality_threshold(&json!(input)).is_err());
        }
        assert_eq!(parse_quality_threshold(&json!(".75")).unwrap(), 0.75);
        assert!(
            create_evaluation_policy(Some(&json!({"profile":"auto","requiredTiers":["haiku"]})))
                .is_err()
        );
        assert!(create_evaluation_policy(Some(&json!({"requiredTiers":["opus","opus"]}))).is_err());
    }
    #[test]
    fn explicit_outage_still_requires_task_and_transport_evidence() {
        let input = json!({"evaluator":"jev","expectOutage":true,"checks":{"claude_success":true,"expected_result":true},"routes":[{"source":"fallback"},{"source":"passthrough","request_class":"auxiliary"}]});
        let report = evaluate_live_case(&input).unwrap();
        assert_eq!(report["passed"], true);
        assert!(report["gates"]["rubric"]["passed"].is_null());
        let mut missing = input;
        missing["checks"] = json!({});
        assert_eq!(evaluate_live_case(&missing).unwrap()["passed"], false);
    }
}
