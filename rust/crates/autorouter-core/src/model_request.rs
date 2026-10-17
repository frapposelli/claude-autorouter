//! The only explicit thinking adaptations applied when changing models.

use serde_json::{Map, Value, json};

use crate::js_json::{JsDocument, JsNode, NodeId};
use crate::model_catalog::model_capabilities;

fn document_string_is(document: &JsDocument, node: Option<NodeId>, expected: &str) -> bool {
    node.and_then(|node| document.string(node))
        .is_some_and(|value| value.units().iter().copied().eq(expected.encode_utf16()))
}
fn document_nonnull(document: &JsDocument, node: Option<NodeId>) -> Option<NodeId> {
    node.filter(|&node| !matches!(document.node(node), Some(JsNode::Null)))
}
fn document_equal(document: &JsDocument, left: NodeId, right: NodeId) -> bool {
    match (document.node(left), document.node(right)) {
        (Some(JsNode::Null), Some(JsNode::Null)) => true,
        (Some(JsNode::Bool(a)), Some(JsNode::Bool(b))) => a == b,
        (Some(JsNode::Number(a)), Some(JsNode::Number(b))) => a == b,
        (Some(JsNode::String(a)), Some(JsNode::String(b))) => a == b,
        (
            Some(JsNode::Object(_) | JsNode::Array(_)),
            Some(JsNode::Object(_) | JsNode::Array(_)),
        ) => left == right,
        _ => false,
    }
}

pub(crate) fn sonnet_needs_adaptive_document(document: &JsDocument) -> bool {
    let root = document.root();
    let effort = document_nonnull(
        document,
        document
            .get(root, "output_config")
            .and_then(|node| document.get(node, "effort")),
    );
    if ["xhigh", "max"]
        .iter()
        .any(|level| document_string_is(document, effort, level))
    {
        return true;
    }
    let Some(JsNode::Array(messages)) = document
        .get(root, "messages")
        .and_then(|node| document.node(node))
    else {
        return false;
    };
    messages.iter().any(|&message| {
        document
            .get(message, "output_config")
            .and_then(|node| document.get(node, "effort"))
            .is_some_and(|value| {
                if let Some(effort) = effort {
                    !document_equal(document, value, effort)
                } else {
                    !document_string_is(document, Some(value), "high")
                }
            })
    })
}

/// The inference/count adapter reads only original semantic nodes. It must not
/// compare replacement-decoded UTF-16 strings or collapse overflowing numbers.
pub fn thinking_adaptation_document(
    document: &JsDocument,
    model: &str,
) -> Option<(&'static str, &'static str)> {
    let root = document.root();
    if document_string_is(document, document.get(root, "model"), model) {
        return None;
    }
    let adaptation = model_capabilities(model)?.disabled_thinking_adaptation?;
    let thinking = document.get(root, "thinking")?;
    let Some(JsNode::Object(object)) = document.node(thinking) else {
        return None;
    };
    if object.entries().len() != 1 {
        return None;
    }
    let kind = document.get(thinking, "type");
    if document_string_is(document, kind, "between_tools")
        && document_string_is(document, document.get(root, "model"), "claude-sonnet-5-5")
        && adaptation == "adaptive"
    {
        return Some(("adaptive", "adaptive_thinking_required"));
    }
    if !document_string_is(document, kind, "disabled") {
        return None;
    }
    match adaptation {
        "between_tools" if !sonnet_needs_adaptive_document(document) => {
            Some(("between_tools", "between_tools_thinking_required"))
        }
        "adaptive" | "between_tools" => Some(("adaptive", "adaptive_thinking_required")),
        _ => None,
    }
}

/// Preserve the complete authoritative tree while changing only the selected
/// model and the one catalog-approved thinking adaptation.
pub fn prepare_request_document(
    document: &JsDocument,
    model: &str,
) -> (JsDocument, Vec<&'static str>) {
    let adaptation = thinking_adaptation_document(document, model);
    let mut prepared = document.clone();
    prepared
        .set_root_field_json(
            "model",
            &serde_json::to_vec(model).expect("scalar model JSON"),
        )
        .expect("validated request object");
    let mut adjustments = Vec::new();
    if let Some((kind, adjustment)) = adaptation {
        prepared
            .set_root_field_json(
                "thinking",
                &serde_json::to_vec(&json!({"type":kind})).expect("fixed thinking JSON"),
            )
            .expect("validated request object");
        adjustments.push(adjustment);
    }
    (prepared, adjustments)
}

/// Typed product path. Unlike the legacy display-string wrapper, this never
/// infers identity from a repaired Unicode string.
pub fn prepare_request_document_exact(
    document: &JsDocument,
    model: &crate::js_json::JsString,
) -> (JsDocument, Vec<&'static str>) {
    let (mut prepared, adjustments) = prepare_request_document(document, &model.to_well_formed());
    prepared
        .set_root_field_json("model", model.stringify().as_bytes())
        .expect("validated request object");
    (prepared, adjustments)
}

pub(crate) fn sonnet_needs_adaptive(body: &Value) -> bool {
    let default_effort = Value::String("high".into());
    let effort = body
        .get("output_config")
        .and_then(|v| v.get("effort"))
        .filter(|v| !v.is_null())
        .unwrap_or(&default_effort);
    matches!(effort.as_str(), Some("xhigh" | "max"))
        || body
            .get("messages")
            .and_then(Value::as_array)
            .is_some_and(|messages| {
                messages.iter().any(|message| {
                    message
                        .get("output_config")
                        .and_then(|v| v.get("effort"))
                        .is_some_and(|value| value != effort)
                })
            })
}

/// Inspect without cloning the full request, so compatibility checks do not
/// duplicate large opaque tool schemas. The preparation operation uses this
/// same decision; counting and inference therefore cannot disagree.
pub(crate) fn thinking_adaptation(
    body: &Value,
    model: &str,
) -> Option<(&'static str, &'static str)> {
    if body.get("model").and_then(Value::as_str) == Some(model) {
        return None;
    }
    let adaptation = model_capabilities(model)?.disabled_thinking_adaptation?;
    let thinking = body.get("thinking")?.as_object()?;
    if thinking.len() != 1 {
        return None;
    }
    let thinking_type = thinking.get("type").and_then(Value::as_str)?;
    if thinking_type == "between_tools"
        && body.get("model").and_then(Value::as_str) == Some("claude-sonnet-5-5")
        && adaptation == "adaptive"
    {
        return Some(("adaptive", "adaptive_thinking_required"));
    }
    if thinking_type != "disabled" {
        return None;
    }
    match adaptation {
        "between_tools" if !sonnet_needs_adaptive(body) => {
            Some(("between_tools", "between_tools_thinking_required"))
        }
        "adaptive" | "between_tools" => Some(("adaptive", "adaptive_thinking_required")),
        _ => None,
    }
}

/// Prepare a validated Messages API envelope, preserving every opaque field.
/// The input is never mutated. Unknown target IDs receive no adaptation.
pub fn prepare_request(body: &Value, model: &str) -> Value {
    let mut request = body.as_object().cloned().unwrap_or_else(Map::new);
    request.insert("model".into(), Value::String(model.into()));
    let mut adjustments = Vec::new();
    if let Some((thinking_type, adjustment)) = thinking_adaptation(body, model) {
        request.insert("thinking".into(), json!({"type": thinking_type}));
        adjustments.push(adjustment);
    }
    json!({"request": request, "adjustments": adjustments})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Value {
        json!({"model":"claude-haiku-4-5-20251001", "thinking":{"type":"disabled"},
            "messages":[{"role":"user", "content":"Task"}], "max_tokens":32000})
    }

    #[test]
    fn document_preparation_preserves_opaque_utf16_and_original_unknown_model() {
        let document = JsDocument::parse(br#"{"model":"claude-haiku-4-5-20251001","thinking":{"type":"disabled"},"messages":[{"role":"user","content":"Synthetic \ud800"}],"opaque":{"2":1e999,"1":-0}}"#).unwrap();
        let before = document.stringify();
        let (prepared, adjustments) = prepare_request_document(&document, "claude-opus-5-5");
        assert_eq!(adjustments, vec!["adaptive_thinking_required"]);
        assert!(
            prepared
                .stringify()
                .contains(r#""content":"Synthetic \ud800""#)
        );
        assert!(
            prepared
                .stringify()
                .contains(r#""opaque":{"1":0,"2":null}"#)
        );
        assert_eq!(document.stringify(), before);
        let unknown = JsDocument::parse(br#"{"model":"synthetic-\ud800","messages":[]}"#).unwrap();
        let identity = unknown
            .get(unknown.root(), "model")
            .and_then(|node| unknown.string(node))
            .unwrap();
        let (prepared, adjustments) = prepare_request_document_exact(&unknown, identity);
        assert_eq!(prepared.stringify(), unknown.stringify());
        let (replaced, _) = prepare_request_document(&unknown, "synthetic-\u{fffd}");
        assert_ne!(replaced.stringify(), unknown.stringify());
        assert!(adjustments.is_empty());
    }

    #[test]
    fn original_effort_values_use_js_identity_and_utf16_equality() {
        for (body, expected) in [
            (
                r#"{"output_config":{"effort":"high"},"messages":[{"output_config":{"effort":"high"}}]}"#,
                false,
            ),
            (
                r#"{"output_config":{"effort":"\ud800"},"messages":[{"output_config":{"effort":"\udfff"}}]}"#,
                true,
            ),
            (
                r#"{"output_config":{"effort":"\ud800"},"messages":[{"output_config":{"effort":"\ud800"}}]}"#,
                false,
            ),
            (
                r#"{"output_config":{"effort":{}},"messages":[{"output_config":{"effort":{}}}]}"#,
                true,
            ),
            (
                r#"{"output_config":{"effort":1e999},"messages":[{"output_config":{"effort":1e999}}]}"#,
                false,
            ),
            (r#"{"messages":[{"output_config":{"effort":null}}]}"#, true),
        ] {
            let document = JsDocument::parse(body.as_bytes()).unwrap();
            assert_eq!(
                sonnet_needs_adaptive_document(&document),
                expected,
                "{body}"
            );
        }
    }

    #[test]
    fn disabled_thinking_adapts_only_to_exact_targets_without_mutation() {
        let body = request();
        let before = body.clone();
        for (model, expected, reason) in [
            (
                "claude-sonnet-5-5",
                "between_tools",
                "between_tools_thinking_required",
            ),
            ("claude-opus-5", "adaptive", "adaptive_thinking_required"),
            ("claude-opus-5-5", "adaptive", "adaptive_thinking_required"),
        ] {
            let result = prepare_request(&body, model);
            let mut expected_body = body.clone();
            expected_body["model"] = model.into();
            expected_body["thinking"] = json!({"type":expected});
            assert_eq!(
                result,
                json!({"request": expected_body, "adjustments":[reason]})
            );
        }
        for model in [
            "claude-haiku-4-5-20251001",
            "claude-sonnet-5",
            "claude-opus-4-6",
            "team/claude-sonnet-5-5",
            "claude-opus-5-5-future",
        ] {
            let result = prepare_request(&body, model);
            assert_eq!(result["request"]["thinking"], body["thinking"]);
            assert_eq!(result["adjustments"], json!([]));
        }
        assert_eq!(body, before);
    }

    #[test]
    fn sonnet_preserves_high_and_per_message_effort_by_using_adaptive() {
        for effort in [Value::Null, json!("low"), json!("medium"), json!("high")] {
            let mut body = request();
            body["output_config"] = json!({"effort": effort});
            assert_eq!(
                prepare_request(&body, "claude-sonnet-5-5")["request"]["thinking"],
                json!({"type":"between_tools"})
            );
        }
        for effort in ["xhigh", "max"] {
            let mut body = request();
            body["output_config"] = json!({"effort":effort});
            assert_eq!(
                prepare_request(&body, "claude-sonnet-5-5")["request"]["thinking"],
                json!({"type":"adaptive"})
            );
        }
        for effort in [json!("medium"), Value::Null] {
            let mut body = request();
            body["messages"][0]["output_config"] = json!({"effort":effort});
            assert_eq!(
                prepare_request(&body, "claude-sonnet-5-5")["request"]["thinking"],
                json!({"type":"adaptive"})
            );
        }
        let mut body = request();
        body["output_config"] = json!({"effort":"low"});
        body["messages"][0]["output_config"] = json!({"effort":"low"});
        assert_eq!(
            prepare_request(&body, "claude-sonnet-5-5")["request"]["thinking"],
            json!({"type":"between_tools"})
        );
    }

    #[test]
    fn signed_history_and_safeguards_survive_between_tools_adaptation() {
        let body = json!({"model":"claude-sonnet-5-5", "thinking":{"type":"between_tools"},
            "safeguards":[{"type":"dangerous_tool_use", "classifier_context":{"v":1,"opaque":true}}],
            "messages":[{"role":"assistant", "content":[{"type":"thinking","thinking":"", "signature":"opaque+/=="},
                {"type":"redacted_thinking","data":"opaque-encrypted"}]},
                {"role":"user", "content":"Task"}]});
        for model in ["claude-opus-5", "claude-opus-5-5"] {
            let result = prepare_request(&body, model);
            assert_eq!(result["request"]["thinking"], json!({"type":"adaptive"}));
            assert_eq!(result["request"]["messages"], body["messages"]);
            assert_eq!(result["request"]["safeguards"], body["safeguards"]);
        }
    }

    #[test]
    fn native_and_extended_contracts_are_never_rewritten() {
        for source in [
            "claude-sonnet-5",
            "claude-opus-5",
            "sonnet",
            "team/claude-sonnet-5-5",
        ] {
            let body = json!({"model":source, "thinking":{"type":"between_tools"}, "messages":[]});
            assert_eq!(
                prepare_request(&body, "claude-opus-5-5")["adjustments"],
                json!([])
            );
        }
        for thinking_type in ["disabled", "between_tools"] {
            for extension in [
                json!({"future_setting":true}),
                json!({"display":"summarized"}),
                json!({"block_binding":{"prefix_mismatch_behavior":"error"}}),
            ] {
                let mut thinking = extension;
                thinking["type"] = thinking_type.into();
                let body = json!({"model":"claude-sonnet-5-5", "thinking":thinking,"messages":[]});
                for model in ["claude-sonnet-5-5", "claude-opus-5-5"] {
                    let result = prepare_request(&body, model);
                    assert_eq!(result["request"]["thinking"], body["thinking"]);
                    assert_eq!(result["adjustments"], json!([]));
                }
            }
        }
    }
}
