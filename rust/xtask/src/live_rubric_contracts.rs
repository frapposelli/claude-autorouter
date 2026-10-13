//! Original label-before-invocation contract. This module tests the same private
//! expected-tier operation called by LiveRouter; it never invokes a provider.
use super::*;
fn declared(scenario: &Value, document: &JsDocument) -> Option<Value> {
    expected_tier(
        scenario,
        &build_state_document(document, 12000).current_task,
    )
}

#[test]
fn all_bundled_live_prompts_have_declared_labels_before_any_provider_call() {
    let scenarios = cases();
    let capture = include_str!("../../parity/cases/tooling-rubric-protocol.capture.json");
    assert_eq!(
        digest(capture.as_bytes()),
        "12c4788d8d515cf8698975b72ce9759d7be3d541eae16d0d4a063cea5da4ed6b"
    );
    let captured: Value = serde_json::from_str(capture).unwrap();
    let original = captured["live_cases"].as_object().unwrap();
    assert_eq!(
        scenarios.as_object().unwrap().keys().collect::<Vec<_>>(),
        original.keys().collect::<Vec<_>>()
    );
    for (name, scenario) in scenarios.as_object().unwrap() {
        for field in [
            "expectedClassifiedTiers",
            "expectedClassifiedTier",
            "expectedTier",
        ] {
            assert_eq!(
                scenario.get(field),
                original[name].get(field),
                "{name}: {field}"
            );
        }
        let prompts = scenario["prompts"].as_array().unwrap();
        assert_eq!(
            prompts.len(),
            original[name]["prompts"].as_array().unwrap().len()
        );
        if name != "coding" {
            assert_eq!(scenario["prompts"], original[name]["prompts"], "{name}");
        }
        for (index, prompt) in prompts.iter().enumerate() {
            let mut messages = Vec::new();
            for previous in prompts.iter().take(index) {
                messages.push(json!({"role":"user","content":previous}));
                messages.push(json!({"role":"assistant","content":"Synthetic completed reply."}));
            }
            messages.push(json!({"role":"user","content":prompt}));
            let document =
                JsDocument::parse(json!({"messages":messages}).to_string().as_bytes()).unwrap();
            let expected = declared(scenario, &document).unwrap();
            assert!(
                ["haiku", "sonnet", "opus"].contains(&expected.as_str().unwrap()),
                "{name}"
            );
        }
    }
    let thinking = &scenarios["thinking_continuation"];
    let document = JsDocument::parse(
        json!({"messages":[
            {"role":"user","content":thinking["prompts"][0]},
            {"role":"assistant","content":"Synthetic reasoning."},
            {"role":"user","content":thinking["prompts"][1]},
        ]})
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    assert_eq!(declared(thinking, &document), Some(json!("haiku")));
}

#[test]
fn unmatched_followup_cannot_invent_a_declared_label() {
    let document = JsDocument::parse(
        br#"{"messages":[{"role":"user","content":"Unlisted synthetic follow-up"}]}"#,
    )
    .unwrap();
    assert!(declared(&cases()["thinking_continuation"], &document).is_none());
}
