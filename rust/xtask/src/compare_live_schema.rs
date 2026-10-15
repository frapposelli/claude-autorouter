//! Strict fresh report admission and unchanged relative live gates.
#[path = "compare_live_shapes.rs"]
mod shapes;
use super::{CASES, private_material};
use autorouter_core::evaluation_report::{
    create_evaluation_policy, evaluate_live_case, evaluate_live_report,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const FINAL_FIELDS: &[&str] = &[
    "type",
    "cli_version",
    "auth_mode",
    "fixture_version",
    "fixture_sha256",
    "historical_fixture_version",
    "historical_fixture_sha256",
    "fixture_adaptation",
    "native_repair_source_sha256",
    "native_repair_test_sha256",
    "native_repair_manifest_sha256",
    "evaluator",
    "classifier_timeout_ms",
    "client_profile",
    "client_model",
    "thinking_disabled",
    "simulated_classifier_outage",
    "quality_policy",
    "coverage_scope",
    "cost_basis",
    "max_turns_supported",
    "cases",
    "engine",
    "client_permission_mode",
    "reference",
    "reference_runtime",
    "passed",
    "gates",
    "classifier_succeeded",
    "model_changed",
];

fn same_number(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_number(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, a)| b.get(k).is_some_and(|b| same_number(a, b)))
        }
        _ => a == b,
    }
}
fn keys(value: &Value, allowed: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.keys().all(|k| allowed.contains(&k.as_str())))
}
fn family(model: &Value) -> String {
    let model = model.as_str().unwrap_or("").to_ascii_lowercase();
    ["haiku", "sonnet", "opus"]
        .iter()
        .find(|tier| model.contains(**tier))
        .copied()
        .unwrap_or(&model)
        .to_owned()
}
fn derived(item: &Value, name: &str, profile: &str, evaluator: &Value) -> Result<(), String> {
    use autorouter_core::evaluation_report::model_tier;
    let routes = item["routes"].as_array().unwrap();
    let observations = item["request_observations"].as_array().unwrap();
    let statuses = item["upstream_statuses"].as_array().unwrap();
    let mut expected = json!({"claude_success":item["exit_code"].as_f64()==Some(0.0)&&item["timed_out"]!=true&&item["output_limit_exceeded"]!=true&&item["failures"].as_array().unwrap().is_empty(),"requests_reached_router":!routes.is_empty(),"upstream_model_evidence":!observations.is_empty()&&observations.iter().all(|row|model_tier(row["confirmed_model"].as_str().unwrap_or("")).is_some()&&model_tier(row["confirmed_model"].as_str().unwrap_or(""))==model_tier(row["selected_model"].as_str().unwrap_or(""))),"no_upstream_api_errors":!statuses.is_empty()&&statuses.iter().all(|s|s.as_f64().is_some_and(|n|(200.0..300.0).contains(&n))),"no_proxy_errors":item["proxy_errors"].as_array().unwrap().is_empty(),"no_permission_denials":item["permission_denials"].as_f64()==Some(0.0)});
    if name == "coding" {
        expected["read_edit_bash_exercised"] = json!(["Read", "Edit", "Bash"].iter().all(|tool| {
            item["successful_tools"]
                .as_array()
                .unwrap()
                .contains(&json!(tool))
        }));
        expected["tool_continuation_reached_router"] = json!(
            routes
                .iter()
                .any(|r| r["tool_result_count"].as_f64().is_some_and(|n| n > 0.0))
        );
    }
    if name == "thinking_continuation" {
        expected["first_request_routed_to_opus"] = json!(
            routes
                .first()
                .is_some_and(|r| family(&r["model"]) == "opus")
        );
        expected["thinking_history_present"] = json!(routes.iter().skip(1).any(|r| {
            r["thinking_history_count"]
                .as_f64()
                .is_some_and(|n| n > 0.0)
        }));
        if profile == "auto" {
            expected["auto_thinking_uses_supported_models"] = json!(
                routes
                    .iter()
                    .all(|r| ["sonnet", "opus"].contains(&family(&r["model"]).as_str()))
            );
        } else {
            expected["thinking_continuation_pinned"] = json!(
                routes
                    .iter()
                    .skip(1)
                    .any(|r| r["reason"] == "thinking_history" && family(&r["model"]) == "opus")
            );
            expected["actual_upstream_stayed_opus"] = json!(
                !item["upstream_models"].as_array().unwrap().is_empty()
                    && item["upstream_models"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|m| family(m) == "opus")
            );
        }
    }
    if expected
        .as_object()
        .unwrap()
        .iter()
        .any(|(key, value)| item["checks"][key] != *value)
    {
        return Err("Original checks disagree with public execution evidence".into());
    }
    if item["classifier_succeeded"] != routes.iter().any(|r| r["source"] == *evaluator)
        || item["model_changed"] != routes.iter().any(|r| r["requested_model"] != r["model"])
    {
        return Err("Original routing summaries disagree with rows".into());
    }
    let prompts = crate::live::cases()[name]["prompts"]
        .as_array()
        .unwrap()
        .len();
    if item["result_count"].as_f64() != Some(prompts as f64)
        && item["checks"]["expected_result"] == true
    {
        return Err("Task success cannot omit an original result".into());
    }
    Ok(())
}
fn mapping_complete(item: &Value) -> bool {
    let fixtures = crate::live::cases();
    let Some(name) = item["case"].as_str() else {
        return false;
    };
    let prompts = fixtures[name]["prompts"].as_array().unwrap().len();
    let main: Vec<_> = item["routes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| {
            ["main", "unspecified", ""].contains(&r["request_class"].as_str().unwrap_or(""))
        })
        .collect();
    if main.iter().any(|r| {
        !r["scenario_prompt_index"]
            .as_f64()
            .is_some_and(|n| n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n < prompts as f64)
    }) {
        return false;
    }
    let indices: BTreeSet<_> = main
        .iter()
        .filter_map(|r| r["scenario_prompt_index"].as_f64().map(|i| i as usize))
        .collect();
    (0..prompts).all(|index| indices.contains(&index))
}
fn case(item: &Value, profile: &str, evaluator: &Value) -> Result<(), String> {
    if !keys(item, shapes::CASE_FIELDS) {
        return Err("Unknown original live case field".into());
    }
    if !shapes::case(item) {
        return Err("Incomplete or invalid original live report shape".into());
    }
    let name = item["case"]
        .as_str()
        .filter(|name| CASES.contains(name))
        .ok_or("Unknown live case")?;
    let checks = item["checks"].as_object().ok_or("Missing live checks")?;
    let mut required = vec![
        "claude_success",
        "expected_result",
        "requests_reached_router",
        "upstream_model_evidence",
        "no_upstream_api_errors",
        "no_proxy_errors",
        "no_permission_denials",
    ];
    if name == "coding" {
        required.extend([
            "original_tests_preserved",
            "independent_tests_pass",
            "read_edit_bash_exercised",
            "tool_continuation_reached_router",
        ]);
    }
    if name == "thinking_continuation" {
        required.extend(["first_request_routed_to_opus", "thinking_history_present"]);
        if profile == "auto" {
            required.push("auto_thinking_uses_supported_models");
        } else {
            required.extend([
                "thinking_continuation_pinned",
                "actual_upstream_stayed_opus",
            ]);
        }
    }
    if checks.len() != required.len()
        || required
            .iter()
            .any(|k| !checks.get(*k).is_some_and(Value::is_boolean))
        || checks.values().any(|v| !v.is_boolean())
    {
        return Err("Original live check is absent or ambiguous".into());
    }
    for key in ["timed_out", "output_limit_exceeded"] {
        if !item[key].is_boolean() {
            return Err("Original child result omitted required state".into());
        }
    }
    let fixtures = crate::live::cases();
    let scenario = &fixtures[name];
    for route in item["routes"].as_array().unwrap() {
        let index = route.get("scenario_prompt_index").filter(|v| !v.is_null());
        if index.is_some_and(|v| {
            v.as_f64().is_none_or(|n| {
                n < 0.0
                    || n.fract() != 0.0
                    || n >= scenario["prompts"].as_array().unwrap().len() as f64
            })
        }) {
            return Err("Out-of-range synthetic prompt index".into());
        }
        let expected = if let Some(tiers) = scenario["expectedClassifiedTiers"].as_array() {
            index
                .and_then(Value::as_f64)
                .and_then(|i| tiers.get(i as usize))
                .cloned()
                .unwrap_or(Value::Null)
        } else {
            scenario["expectedClassifiedTier"].clone()
        };
        if route
            .get("expected_classified_tier")
            .cloned()
            .unwrap_or(Value::Null)
            != expected
        {
            return Err("Fixture-bound live label differs; no relabeling permitted".into());
        }
    }
    derived(item, name, profile, evaluator)?;
    let mut input = json!({"checks":item["checks"],"routes":item["routes"],"evaluator":evaluator,"expectOutage":false});
    if let Some(tier) = fixtures[name].get("expectedClassifiedTier") {
        input["expectedClassifiedTier"] = tier.clone();
    }
    let recomputed = evaluate_live_case(&input)?;
    if !same_number(&item["gates"], &recomputed["gates"]) || item["passed"] != recomputed["passed"]
    {
        return Err("Original live acceptance differs from frozen rubric".into());
    }
    if !keys(
        &item["gateway_cleanup"],
        &[
            "awaited",
            "listener_observation",
            "router_close_returned",
            "native_join_result_available",
            "reference_closed",
            "reference_protocol_valid",
            "reference_process_exit_code",
            "reference_process_timed_out",
            "reference_process_output_limit_exceeded",
        ],
    ) || !item["fixture_removed"].is_boolean()
    {
        return Err("Fresh ordinary live cleanup observations are absent".into());
    }
    Ok(())
}
pub(super) fn extract(
    bytes: &[u8],
    status: &Value,
    declaration: &Value,
    profile: &str,
    engine: &str,
    secrets: &[String],
) -> Result<Value, String> {
    if status["complete"] != true || !matches!(status["exit_code"].as_i64(), Some(0 | 1)) {
        return Err("Live child did not complete within ownership bounds".into());
    }
    let mut progress = Vec::new();
    let mut final_report = None;
    for line in bytes
        .split(|b| *b == b'\n')
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
    {
        let value: Value = serde_json::from_slice(line)
            .map_err(|_| "Non-JSON live output rejected without persistence")?;
        if private_material(&value, secrets) {
            return Err("Credential-like live output rejected without persistence".into());
        }
        if final_report.is_some() {
            return Err("Unexpected output after final live report".into());
        }
        match value["type"].as_str() {
            Some("case_result") => {
                let mut item = value;
                item.as_object_mut().unwrap().remove("type");
                case(
                    &item,
                    profile,
                    &declaration["configuration"][profile]["config"]["evaluator"],
                )?;
                progress.push(item);
            }
            Some("live_validation") => final_report = Some(value),
            _ => return Err("Unknown live output is never logged".into()),
        }
    }
    let report = final_report.ok_or("Missing final live report")?;
    if !keys(&report, FINAL_FIELDS) {
        return Err("Unknown final live field rejected".into());
    }
    for key in FINAL_FIELDS
        .iter()
        .copied()
        .filter(|key| !["reference", "reference_runtime"].contains(key))
    {
        if report.get(key).is_none() {
            return Err("Complete original final report field is absent".into());
        }
    }
    for key in [
        "thinking_disabled",
        "simulated_classifier_outage",
        "max_turns_supported",
        "passed",
        "classifier_succeeded",
        "model_changed",
    ] {
        if !report[key].is_boolean() {
            return Err("Invalid final original boolean state".into());
        }
    }
    if report["cost_basis"] != "CLI list-price estimate, not subscription charges" {
        return Err("Original cost interpretation differs".into());
    }
    if engine == "native"
        && (report.get("reference").is_some() || report.get("reference_runtime").is_some())
    {
        return Err("Native report claimed an unrelated reference runtime".into());
    }
    if engine == "node-reference"
        && (!keys(
            &report["reference"],
            &[
                "baseline_commit",
                "verified_source_files",
                "adapter_sha256",
                "temporary_reference_only",
            ],
        ) || !keys(
            &report["reference_runtime"],
            &["address", "node_version", "adapter_version"],
        ))
    {
        return Err("Unknown nested reference metadata".into());
    }
    let fixture: Value =
        serde_json::from_str(include_str!("../fixtures/live-cases-v2.json")).unwrap();
    let all = crate::live::cases();
    let rows:Vec<_>=all.as_object().unwrap().iter().map(|(name,scenario)|json!({"name":name,"prompts":scenario["prompts"],"expected":scenario.get("expectedClassifiedTiers").or_else(||scenario.get("expectedClassifiedTier")).or_else(||scenario.get("expectedTier")).cloned().unwrap_or(Value::Null)})).collect();
    let declared_config = &declaration["configuration"][profile];
    if report["engine"] != engine
        || report["client_profile"] != profile
        || report["client_permission_mode"] != declared_config["permission_mode"]
        || report["client_model"] != declared_config["client_model"]
        || report["evaluator"] != "jev"
        || report["auth_mode"] != "subscription"
        || report["coverage_scope"] != "full_profile"
        || report["simulated_classifier_outage"] != false
        || report["cli_version"] != declaration["metadata"]["claude_version"]
        || report["fixture_version"] != 2
        || report["fixture_sha256"] != crate::evaluation::digest(json!(rows).to_string().as_bytes())
        || report["historical_fixture_sha256"] != fixture["historical_fixture_sha256"]
        || report["fixture_adaptation"] != fixture["adaptation"]
        || report["max_turns_supported"] != declaration["metadata"]["max_turns_supported"]
        || report["classifier_timeout_ms"] != declared_config["config"]["jevTimeoutMs"]
    {
        return Err("Live profile, policy, evaluator, client or fixture identity differs".into());
    }
    for (key, path) in [
        (
            "native_repair_source_sha256",
            "rust/xtask/fixtures/merge-intervals.rs",
        ),
        (
            "native_repair_test_sha256",
            "rust/xtask/fixtures/merge-intervals.test.rs",
        ),
        (
            "native_repair_manifest_sha256",
            "rust/xtask/fixtures/repair-cargo.toml",
        ),
    ] {
        if report[key] != declaration["identities"]["files"][path]["sha256"] {
            return Err("Independent repair fixture identity differs".into());
        }
    }
    if engine == "node-reference"
        && (report["reference"]["verified_source_files"] != 111
            || report["reference"]["baseline_commit"] != "ea930c247626ce2af5ccdad721b5121417bf4ad8"
            || report["reference"]["adapter_sha256"]
                != declaration["identities"]["files"]["rust/parity/live-gateway.mjs"]["sha256"]
            || report["reference"]["temporary_reference_only"] != true
            || report["reference_runtime"]["node_version"] != "v22.14.0"
            || report["reference_runtime"]["adapter_version"].as_f64() != Some(1.0)
            || !report["reference_runtime"]["address"]
                .as_str()
                .is_some_and(|address| {
                    address
                        .parse::<std::net::SocketAddr>()
                        .is_ok_and(|address| address.ip().is_loopback() && address.port() > 0)
                }))
    {
        return Err("Actual frozen Node gateway identity differs".into());
    }
    let policy = create_evaluation_policy(Some(&json!({"profile":profile})))?;
    let policy = serde_json::to_value(policy).map_err(|_| "Cannot encode frozen live policy")?;
    if !same_number(&report["quality_policy"], &policy) {
        return Err("Default full-profile tier coverage or strict rubric differs".into());
    }
    let cases = report["cases"]
        .as_array()
        .ok_or("Missing full live cases")?;
    let names: BTreeSet<_> = cases.iter().filter_map(|c| c["case"].as_str()).collect();
    let defaults = crate::live::parse_args(&[]).unwrap();
    let order: Vec<_> = cases.iter().map(|c| c["case"].clone()).collect();
    if cases.len() != 6
        || names != CASES.iter().copied().collect()
        || json!(order) != defaults["cases"]
        || !same_number(&json!(progress), &report["cases"])
    {
        return Err("Live cases omitted, repeated, reordered or inconsistent with progress".into());
    }
    for item in cases {
        case(item, profile, &report["evaluator"])?;
        let cleanup_keys: &[&str] = if engine == "native" {
            &[
                "awaited",
                "listener_observation",
                "router_close_returned",
                "native_join_result_available",
            ]
        } else {
            &[
                "awaited",
                "listener_observation",
                "reference_closed",
                "reference_protocol_valid",
                "reference_process_exit_code",
                "reference_process_timed_out",
                "reference_process_output_limit_exceeded",
            ]
        };
        if !keys(&item["gateway_cleanup"], cleanup_keys)
            || cleanup_keys
                .iter()
                .any(|key| item["gateway_cleanup"].get(*key).is_none())
        {
            return Err("Complete original engine cleanup fields are absent or mismatched".into());
        }
    }
    if report["classifier_succeeded"] != cases.iter().all(|c| c["classifier_succeeded"] == true)
        || report["model_changed"] != cases.iter().any(|c| c["model_changed"] == true)
    {
        return Err("Original final routing summary disagrees with cases".into());
    }
    let acceptance = evaluate_live_report(
        &report["cases"],
        &json!({"policy":policy,"expectOutage":false}),
    )?;
    if report["historical_fixture_version"] != fixture["historical_fixture_version"] {
        return Err("Original fixture provenance version differs".into());
    }
    if report["passed"] != acceptance["passed"]
        || !same_number(&report["gates"], &acceptance["gates"])
        || status["exit_code"] != if report["passed"] == true { 0 } else { 1 }
    {
        return Err("Final original live acceptance or process status differs".into());
    }
    let thinking = profile == "compatible" || declared_config["thinking_env"] == "0";
    if report["thinking_disabled"] != thinking {
        return Err("Declared thinking condition differs".into());
    }
    Ok(report)
}
fn cleanup(item: &Value, engine: &str) -> bool {
    let value = &item["gateway_cleanup"];
    let common = value["awaited"] == true
        && value["listener_observation"] == "connection_refused"
        && item["fixture_removed"] == true;
    common
        && if engine == "native" {
            value["router_close_returned"] == true && value["native_join_result_available"] == false
        } else {
            value["reference_closed"] == true
                && value["reference_protocol_valid"] == true
                && value["reference_process_exit_code"] == 0
                && value["reference_process_timed_out"] == false
                && value["reference_process_output_limit_exceeded"] == false
        }
}
fn count(report: &Value, gate: &str) -> usize {
    report["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["gates"][gate]["passed"] == true)
        .count()
}
pub(super) fn compare(reports: &Value, profile: &str) -> Value {
    let baseline = &reports["node-reference"];
    let native = &reports["native"];
    if !baseline["cases"].is_array() || !native["cases"].is_array() {
        return json!({"complete":false,"relative_gates_passed":false,"absolute_original_quality_passed":false,"reason":"Complete admitted original reports required","nested_abnormal_group_cleanup_verified":false});
    }
    let mut details = Vec::new();
    let mut transport = 0;
    let mut permission = 0;
    let mut coding = 0;
    let mut cleanup_failures = 0;
    for name in CASES {
        let baseline = baseline["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["case"] == *name)
            .unwrap();
        let native = native["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["case"] == *name)
            .unwrap();
        let transport_failure = baseline["gates"]["transport"]["passed"] == true
            && native["gates"]["transport"]["passed"] != true;
        let permission_failure = (baseline["checks"]["no_permission_denials"] == true
            && native["checks"]["no_permission_denials"] != true)
            || (name == &"coding"
                && baseline["checks"]["original_tests_preserved"] == true
                && native["checks"]["original_tests_preserved"] != true);
        let coding_failure = name == &"coding"
            && baseline["checks"]["independent_tests_pass"] == true
            && native["checks"]["independent_tests_pass"] != true;
        let baseline_cleanup = cleanup(baseline, "node-reference");
        let native_cleanup = cleanup(native, "native");
        transport += usize::from(transport_failure);
        permission += usize::from(permission_failure);
        coding += usize::from(coding_failure);
        cleanup_failures += usize::from(baseline_cleanup && !native_cleanup);
        details.push(json!({"case":name,"native_only_transport_failure":transport_failure,"native_only_permission_or_private_fixture_failure":permission_failure,"native_only_independent_coding_failure":coding_failure,"baseline_observed_cleanup":baseline_cleanup,"native_observed_cleanup":native_cleanup,"baseline_fixture_mapping_complete":mapping_complete(baseline),"native_fixture_mapping_complete":mapping_complete(native),"original_baseline_quality_passed":baseline["passed"],"original_native_quality_passed":native["passed"]}));
    }
    let task = count(native, "task") as i64 - count(baseline, "task") as i64;
    let rubric = count(native, "rubric") as i64 - count(baseline, "rubric") as i64;
    let missing = |r: &Value| r["gates"]["coverage"]["missing"].as_array().unwrap().len() as i64;
    let coverage = missing(baseline) - missing(native);
    let observed_cleanup = details
        .iter()
        .all(|d| d["baseline_observed_cleanup"] == true && d["native_observed_cleanup"] == true);
    let mapping = details.iter().all(|d| {
        d["baseline_fixture_mapping_complete"] == true
            && d["native_fixture_mapping_complete"] == true
    });
    json!({"complete":true,"profile":profile,"native_only_transport_protocol_cleanup_failures":transport+cleanup_failures,"native_only_permission_or_private_state_failures":permission,"native_only_independent_coding_verifier_failures":coding,"native_minus_baseline_task_completion_count":task,"task_denominator":6,"native_minus_baseline_rubric_acceptance_count":rubric,"rubric_denominator":6,"native_minus_baseline_tier_coverage_count":coverage,"tier_denominator":if profile=="auto"{2}else{3},"relative_gates_passed":transport+cleanup_failures==0&&permission==0&&coding==0&&task>=0&&rubric>=0&&coverage>=0&&observed_cleanup&&mapping,"fixture_mapping_complete":mapping,"observed_normal_cleanup_complete":observed_cleanup,"native_worker_join_outcome_verified":false,"nested_abnormal_group_cleanup_verified":false,"absolute_original_quality_passed":baseline["passed"]==true&&native["passed"]==true,"cases":details,"token_cache_evaluator_counts":"retained in separate original reports; descriptive, not assumed equal across stochastic live trials"})
}

#[cfg(test)]
pub(super) fn original_route_shape(value: &Value) -> bool {
    shapes::route(value)
}
