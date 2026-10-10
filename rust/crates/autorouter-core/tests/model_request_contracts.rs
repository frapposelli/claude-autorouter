//! Complete request-preparation scenarios from the frozen JavaScript baseline.
//! Rust returns owned values; JS reference-equality assertions are represented
//! by complete subtree equality and an unchanged authoritative input document.
use autorouter_core::js_json::{JsDocument, JsString};
use autorouter_core::model_request::{prepare_request, prepare_request_document_exact};
use serde_json::{Value, json};

fn check(body: Value, target: &str, thinking: Option<Value>, adjustments: &[&str]) {
    let before = body.clone();
    let mut expected = before.clone();
    expected["model"] = json!(target);
    if let Some(thinking) = thinking {
        expected["thinking"] = thinking;
    }
    assert_eq!(
        prepare_request(&body, target),
        json!({"request":expected,"adjustments":adjustments})
    );
    assert_eq!(body, before);
    let document = JsDocument::parse(&serde_json::to_vec(&body).unwrap()).unwrap();
    let original = document.stringify();
    let (prepared, observed) =
        prepare_request_document_exact(&document, &JsString::from_scalar(target));
    assert_eq!(observed, adjustments);
    assert_eq!(
        prepared.stringify(),
        JsDocument::parse(&serde_json::to_vec(&expected).unwrap())
            .unwrap()
            .stringify()
    );
    assert_eq!(document.stringify(), original);
}

#[test]
fn opus_adaptation_preserves_the_complete_source_request() {
    check(
        json!({"model":"claude-haiku-4-5-20251001","thinking":{"type":"disabled"},
            "messages":[{"role":"user","content":"Task"}],"max_tokens":32000}),
        "claude-opus-5-5",
        Some(json!({"type":"adaptive"})),
        &["adaptive_thinking_required"],
    );
}

#[test]
fn sonnet_lowest_setting_covers_missing_low_medium_and_high_effort() {
    for effort in [None, Some("low"), Some("medium"), Some("high")] {
        let mut body = json!({"model":"claude-haiku-4-5-20251001","thinking":{"type":"disabled"},
            "messages":[{"role":"user","content":"Investigate the dashboard discrepancy."}],"max_tokens":32000});
        if let Some(effort) = effort {
            body["output_config"] = json!({"effort":effort});
        }
        check(
            body,
            "claude-sonnet-5-5",
            Some(json!({"type":"between_tools"})),
            &["between_tools_thinking_required"],
        );
    }
}

#[test]
fn sonnet_effort_transitions_retain_all_messages_and_adjustments() {
    let base = json!({"model":"claude-haiku-4-5-20251001","thinking":{"type":"disabled"},
        "messages":[{"role":"user","content":"Task"}]});
    for overrides in [
        json!({"output_config":{"effort":"xhigh"}}),
        json!({"output_config":{"effort":"max"}}),
        json!({"messages":[{"role":"user","content":"Task"},
            {"role":"system","content":"Next turn","output_config":{"effort":"medium"}}]}),
        json!({"output_config":{"effort":"low"},"messages":[{"role":"user","content":"Task"},
            {"role":"system","content":"Next turn","output_config":{"effort":"high"}}]}),
    ] {
        let mut body = base.clone();
        for (key, value) in overrides.as_object().unwrap() {
            body[key] = value.clone();
        }
        check(
            body,
            "claude-sonnet-5-5",
            Some(json!({"type":"adaptive"})),
            &["adaptive_thinking_required"],
        );
    }
    check(
        json!({"model":"claude-haiku-4-5-20251001","thinking":{"type":"disabled"},
            "output_config":{"effort":"low"},"messages":[{"role":"user","content":"Task"},
                {"role":"system","content":"Next turn","output_config":{"effort":"low"}}]}),
        "claude-sonnet-5-5",
        Some(json!({"type":"between_tools"})),
        &["between_tools_thinking_required"],
    );
}

#[test]
fn unchanged_contracts_cover_every_original_source_target_and_setting() {
    for (source, target, thinking) in [
        ("haiku", "claude-sonnet-5", Some(json!({"type":"disabled"}))),
        ("haiku", "claude-opus-4-6", Some(json!({"type":"disabled"}))),
        ("haiku", "claude-opus-5-5", Some(json!({"type":"adaptive"}))),
        ("haiku", "claude-opus-5-5", None),
        (
            "claude-opus-5-5",
            "claude-opus-5-5",
            Some(json!({"type":"disabled"})),
        ),
        (
            "haiku",
            "team/claude-sonnet-5-5",
            Some(json!({"type":"disabled"})),
        ),
        (
            "haiku",
            "claude-sonnet-5-5-future",
            Some(json!({"type":"disabled"})),
        ),
        (
            "haiku",
            "claude-sonnet-5-5",
            Some(json!({"type":"adaptive","display":"summarized"})),
        ),
        ("haiku", "claude-sonnet-5-5", None),
        (
            "claude-sonnet-5-5",
            "claude-sonnet-5-5",
            Some(json!({"type":"between_tools"})),
        ),
        (
            "claude-sonnet-5-5",
            "claude-sonnet-5-5",
            Some(json!({"type":"disabled"})),
        ),
    ] {
        let mut body = json!({"model":source,"messages":[]});
        if let Some(thinking) = thinking {
            body["thinking"] = thinking;
        }
        check(body, target, None, &[]);
    }
}

#[test]
fn signed_history_safeguards_and_each_opaque_subtree_survive_both_opus_targets() {
    let body = json!({
        "model":"claude-sonnet-5-5","thinking":{"type":"between_tools"},
        "system":[{"type":"text","text":"Synthetic system instructions."}],
        "tools":[{"name":"Read","input_schema":{"type":"object","properties":{}}}],
        "safeguards":[{"type":"dangerous_tool_use","classifier_context":{"v":1,"permission_mode":"auto"}}],
        "context_management":{"edits":[{"type":"clear_thinking_20251015","keep":"all"}]},
        "output_config":{"effort":"high"},
        "messages":[{"role":"user","content":"Inspect the synthetic lock."},
            {"role":"assistant","content":[
                {"type":"thinking","thinking":"","signature":"opaque-synthetic-signature"},
                {"type":"redacted_thinking","data":"opaque-synthetic-data"},
                {"type":"text","text":"The fence must be monotonic."}]},
            {"role":"user","content":"Now review duplicate retries."},
            {"role":"system","content":"The task now requires deeper review.","output_config":{"effort":"high"}}]
    });
    for target in ["claude-opus-5", "claude-opus-5-5"] {
        check(
            body.clone(),
            target,
            Some(json!({"type":"adaptive"})),
            &["adaptive_thinking_required"],
        );
    }
}

#[test]
fn aliases_source_models_and_extended_between_tools_contracts_never_adapt() {
    let mut cases = vec![
        (
            "claude-sonnet-5",
            "claude-opus-5-5",
            json!({"type":"between_tools"}),
        ),
        (
            "claude-opus-5",
            "claude-opus-5-5",
            json!({"type":"between_tools"}),
        ),
        ("sonnet", "claude-opus-5-5", json!({"type":"between_tools"})),
        (
            "team/claude-sonnet-5-5",
            "claude-opus-5-5",
            json!({"type":"between_tools"}),
        ),
        (
            "claude-sonnet-5-5-future",
            "claude-opus-5-5",
            json!({"type":"between_tools"}),
        ),
        ("claude-sonnet-5-5", "opus", json!({"type":"between_tools"})),
        (
            "claude-sonnet-5-5",
            "claude-opus-4-8",
            json!({"type":"between_tools"}),
        ),
        (
            "claude-sonnet-5-5",
            "claude-sonnet-5",
            json!({"type":"between_tools"}),
        ),
        (
            "claude-sonnet-5-5",
            "team/claude-opus-5-5",
            json!({"type":"between_tools"}),
        ),
        (
            "claude-sonnet-5-5",
            "claude-opus-5-5-future",
            json!({"type":"between_tools"}),
        ),
    ];
    for thinking in [
        json!({"type":"between_tools","display":"summarized"}),
        json!({"type":"between_tools","budget_tokens":1000}),
        json!({"type":"between_tools","block_binding":{"prefix_mismatch_behavior":"drop_block"}}),
        json!({"type":"between_tools","future_setting":true}),
    ] {
        cases.push(("claude-sonnet-5-5", "claude-opus-5-5", thinking));
    }
    for (source, target, thinking) in cases {
        check(
            json!({"model":source,"thinking":thinking,"messages":[{"role":"user","content":"Task"}]}),
            target,
            None,
            &[],
        );
    }
}

#[test]
fn disabled_thinking_extensions_remain_intact_for_sonnet_and_opus() {
    for thinking in [
        json!({"type":"disabled","future_setting":true}),
        json!({"type":"disabled","display":"summarized"}),
        json!({"type":"disabled","block_binding":{"prefix_mismatch_behavior":"error"}}),
    ] {
        for target in ["claude-sonnet-5-5", "claude-opus-5-5"] {
            check(
                json!({"model":"claude-haiku-4-5","thinking":thinking,"messages":[{"role":"user","content":"Task"}]}),
                target,
                None,
                &[],
            );
        }
    }
}
