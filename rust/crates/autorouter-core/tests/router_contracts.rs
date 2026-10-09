//! Frozen assertions supply complete schedules; this replay proves synchronous
//! policy, not the classifier/transport/cache that produced its observed input.
use autorouter_core::config::read_config;
use autorouter_core::router::{RouteOptions, Router, context_size_bytes};
use serde_json::{Value, json};
use std::path::Path;

#[path = "support/router_contract.rs"]
mod support;
use support::*;

fn count(outcome: &Value) -> Option<u64> {
    match text(outcome, "kind") {
        "json" => outcome["value"]
            .as_u64()
            .filter(|value| *value <= autorouter_core::config::MAX_SAFE_INTEGER),
        // These tags preserve the original API boundary in the corpus/report;
        // Option::None replays only its downstream policy, not JS value types.
        "undefined" | "nan" | "positive_infinity" | "negative_infinity" | "throw" => None,
        _ => panic!("Unknown count outcome"),
    }
}
fn same(actual: &Value, expected: &Value) -> Result<(), String> {
    if actual == expected {
        Ok(())
    } else {
        Err("Frozen router contract differs".into())
    }
}

#[test]
fn complete_contract_comparison_rejects_missing_fields_wrong_counts_and_input_changes() {
    let expected = json!({"decision":{"model":"haiku","classified_tier":"haiku","counted_input_tokens":54481},"count_models":["haiku"],"input_sha256":"original"});
    let mut missing = expected.clone();
    missing["decision"]
        .as_object_mut()
        .unwrap()
        .remove("classified_tier");
    assert!(same(&missing, &expected).is_err());
    for (key, value) in [
        ("count_models", json!([])),
        ("input_sha256", json!("changed")),
    ] {
        let mut wrong = expected.clone();
        wrong[key] = value;
        assert!(same(&wrong, &expected).is_err());
    }
    let mut number = expected.clone();
    number["decision"]["counted_input_tokens"] = json!(54482);
    assert!(same(&number, &expected).is_err());
}

#[test]
fn dictionary_reconstruction_rejects_changed_missing_or_reordered_input_chunks() {
    let input = json!({"body_chunks":[0,1],"body_sha256":digest(br#"{"model":"synthetic"}"#)});
    let dictionary = json!({"chunks":["{\"model\":", "\"synthetic\"}"]});
    assert_eq!(
        decode_body(&input, &dictionary).unwrap(),
        r#"{"model":"synthetic"}"#
    );
    let mut changed = dictionary.clone();
    changed["chunks"][1] = json!("\"different\"}");
    assert!(decode_body(&input, &changed).is_err());
    assert!(decode_body(&input, &json!({"chunks":[]})).is_err());
    let mut reversed = input.clone();
    reversed["body_chunks"] = json!([1, 0]);
    assert!(decode_body(&reversed, &dictionary).is_err());
}

#[test]
fn frozen_router_instances_match_complete_policy_schedules_and_pure_calls() {
    assert_eq!(digest(CASES.as_bytes()), CASES_SHA256);
    let mut lines = CASES.lines();
    let dictionary: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(dictionary["kind"], "string_dictionary");
    let mut instances = 0;
    let mut routes = 0;
    let mut pure = [0; 3];
    let mut boundaries = 0;
    for line in lines {
        let row: Value = serde_json::from_str(line).unwrap();
        let input = &row["input"];
        match text(&row, "kind") {
            "router" => {
                instances += 1;
                let mut router = Router::new(configuration(&input["config"]));
                for step in input["steps"].as_array().unwrap() {
                    routes += 1;
                    assert_eq!(
                        step["config"], input["config"],
                        "Live configuration mutation needs an explicit contract"
                    );
                    let document = document(step, &dictionary);
                    assert_eq!(
                        digest(document.stringify().as_bytes()),
                        step["body_before_sha256"]
                    );
                    let options = &step["options"];
                    let options = RouteOptions {
                        scope: text(options, "scope").into(),
                        prompt_id: text(options, "promptId").into(),
                        request_class: text(options, "requestClass").into(),
                        request_id: options["requestId"].as_str().map(str::to_owned),
                        count_tokens: step["count_present"] == true,
                    };
                    let now = step["now"].as_u64().unwrap();
                    let present = options.count_tokens;
                    let start = router.begin_route(&document, options, now);
                    let mut models = Vec::new();
                    let mut decision = if let Some(result) = start.passthrough {
                        assert_eq!(step["classify_calls"], 0);
                        result
                    } else {
                        assert_eq!(step["classify_calls"], 1);
                        let early = start.early_count_model.clone();
                        if let Some(model) = &early {
                            models.push(model.clone());
                        }
                        let pending = router.classified(
                            &document,
                            start,
                            step["classification"].clone(),
                            now,
                        );
                        if present
                            && let Some(model) = &pending.count_model
                            && early.as_ref() != Some(model)
                        {
                            models.push(model.clone());
                        }
                        let input_count = if present && pending.count_model.is_some() {
                            let call = step["count_calls"].as_array().unwrap().last().unwrap();
                            count(&call["outcome"])
                        } else {
                            None
                        };
                        router.finish_route(&document, pending, input_count, now)
                    };
                    decision.as_object_mut().unwrap().remove("latency_ms");
                    decision
                        .as_object_mut()
                        .unwrap()
                        .remove("evaluation_latency_ms");
                    let calls = step["count_calls"].as_array().unwrap();
                    for call in calls {
                        assert_eq!(call["received_sha256"], step["body_before_sha256"]);
                        if call["outcome"]["kind"] != "json" {
                            boundaries += 1;
                        }
                    }
                    let observed = json!({"decision":decision,"count_models":models,"input_sha256":digest(document.stringify().as_bytes())});
                    let expected = json!({"decision":step["node_expected"],"count_models":calls.iter().map(|v|v["model"].clone()).collect::<Vec<_>>(),"input_sha256":step["body_after_sha256"]});
                    assert!(
                        same(&observed, &expected).is_ok(),
                        "{} route {routes}: actual={observed}, expected={expected}",
                        row["id"]
                    );
                }
            }
            "context_size" => {
                pure[0] += 1;
                let document = document(input, &dictionary);
                assert_eq!(
                    json!(context_size_bytes(&document, text(input, "model"))),
                    row["node_expected"],
                    "{}",
                    row["id"]
                );
            }
            "build_state" => {
                pure[1] += 1;
                let document = document(input, &dictionary);
                let state = autorouter_core::prompt_state::build_state_document(
                    &document,
                    input["limit"].as_u64().unwrap() as usize,
                );
                assert_eq!(
                    state.stringify(),
                    row["node_expected"].as_str().unwrap(),
                    "{}",
                    row["id"]
                );
            }
            "read_config_error" => {
                pure[2] += 1;
                let result = read_config(&input["env"], false, Path::new("/synthetic"));
                assert_eq!(
                    result.err().as_deref(),
                    row["node_expected"]["message"].as_str()
                );
            }
            other => panic!("Unknown captured operation {other}"),
        }
    }
    assert_eq!(
        (instances, routes, pure, boundaries),
        (121, 263, [21, 4, 4], 3)
    );
}
