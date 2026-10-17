//! Validate only the structures routing reads, leaving provider extensions and
//! opaque tool input/document data under the provider's own validation.

use crate::js_json::{JsDocument, JsNode, NodeId};
use serde_json::{Value, json};

pub(crate) fn nonnegative_safe_integer(value: &Value) -> Option<u64> {
    // JSON numbers are JavaScript Numbers in the reference implementation;
    // integral exponent/decimal forms and -0 are valid, but strings are not.
    let number = value.as_f64()?;
    (number.is_finite()
        && (0.0..=9_007_199_254_740_991.0).contains(&number)
        && number.fract() == 0.0)
        .then_some(number as u64)
}

// JavaScript trim differs from Rust whitespace, notably U+0085 and U+FEFF.
fn js_whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}'
        | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}'
        | '\u{205f}' | '\u{3000}' | '\u{feff}')
}
fn invalid(field: &str) -> Value {
    json!({"valid":false,"error":format!("Invalid Messages API request shape: {field}")})
}
pub fn validate_request_shape(body: &Value) -> Value {
    validate_request_document(
        &JsDocument::parse(body.to_string().as_bytes()).expect("serialized JSON"),
    )
}

/// Validate consumed structure directly from the authoritative arena. Opaque
/// provider fields are never projected, traversed, or constrained by this check.
pub fn validate_request_document(document: &JsDocument) -> Value {
    let root = document.root();
    let get = |node, key| document.get(node, key);
    let node = |id: Option<NodeId>| id.and_then(|id| document.node(id));
    let object = |id| matches!(node(id), Some(JsNode::Object(_)));
    let string = |id: Option<NodeId>| id.and_then(|id| document.string(id));
    let text = |id| string(id).is_some_and(|s| !s.units().is_empty());
    let scalar = |id| string(id).and_then(|s| s.to_scalar());
    if !object(Some(root))
        || !string(get(root, "model")).is_some_and(|s| {
            s.units()
                .iter()
                .any(|unit| !char::from_u32(u32::from(*unit)).is_some_and(js_whitespace))
        })
    {
        return invalid("model");
    }
    let Some(JsNode::Array(messages)) = node(get(root, "messages")) else {
        return invalid("messages");
    };
    let mut pending = Vec::new();
    let mut content = |id: Option<NodeId>, field: &'static str, depth: usize| match node(id) {
        Some(JsNode::String(_)) => true,
        Some(JsNode::Array(blocks)) => {
            pending.push((blocks, field, depth));
            true
        }
        _ => false,
    };
    if let Some(system) = get(root, "system")
        && !content(Some(system), "system", 0)
    {
        return invalid("system");
    }
    for message in messages {
        if !object(Some(*message))
            || !matches!(
                scalar(get(*message, "role")).as_deref(),
                Some("user" | "assistant" | "system")
            )
        {
            return invalid("messages");
        }
        if !content(get(*message, "content"), "message content", 0) {
            return invalid("message content");
        }
        if let Some(output) = get(*message, "output_config")
            && !matches!(
                document.node(output),
                Some(JsNode::Null | JsNode::Object(_))
            )
        {
            return invalid("message output_config");
        }
    }
    if let Some(tools) = get(root, "tools") {
        let Some(JsNode::Array(tools)) = document.node(tools) else {
            return invalid("tools");
        };
        for tool in tools {
            let kind = get(*tool, "type");
            if !object(Some(*tool))
                || (kind.is_some() && !matches!(node(kind), Some(JsNode::Null)) && !text(kind))
                || (get(*tool, "name").is_some() && string(get(*tool, "name")).is_none())
            {
                return invalid("tools");
            }
        }
    }
    for field in [
        "thinking",
        "tool_choice",
        "output_config",
        "context_management",
    ] {
        if let Some(value) = get(root, field) {
            if field == "context_management" && matches!(document.node(value), Some(JsNode::Null)) {
                continue;
            }
            if !object(Some(value)) {
                return invalid(field);
            }
        }
    }
    for field in ["thinking", "tool_choice"] {
        if let Some(value) = get(root, field)
            && !text(get(value, "type"))
        {
            return invalid(field);
        }
    }
    if let Some(value) = get(root, "max_tokens") {
        let Some(JsNode::Number(number)) = document.node(value) else {
            return invalid("max_tokens");
        };
        if !number.is_finite()
            || !(0.0..=9_007_199_254_740_991.0).contains(number)
            || number.fract() != 0.0
        {
            return invalid("max_tokens");
        }
    }
    if get(root, "stream").is_some_and(|id| !matches!(document.node(id), Some(JsNode::Bool(_)))) {
        return invalid("stream");
    }
    while let Some((blocks, field, depth)) = pending.pop() {
        if depth > 32 {
            return invalid("content nesting");
        }
        for block in blocks {
            if !object(Some(*block)) || !text(get(*block, "type")) {
                return invalid(field);
            }
            match scalar(get(*block, "type")).as_deref() {
                Some("text") if string(get(*block, "text")).is_none() => {
                    return invalid("text content");
                }
                Some("tool_use")
                    if get(*block, "name").is_some() && string(get(*block, "name")).is_none() =>
                {
                    return invalid("tool name");
                }
                Some("tool_result") => {
                    if let Some(content) = get(*block, "content") {
                        match document.node(content) {
                            Some(JsNode::String(_)) => {}
                            Some(JsNode::Array(blocks)) => {
                                pending.push((blocks, "tool result content", depth + 1))
                            }
                            _ => return invalid("tool result content"),
                        }
                    }
                }
                _ => {}
            }
        }
    }
    json!({"valid":true})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(extra: Value) -> Value {
        let mut body =
            json!({"model":"claude-sonnet-5-5", "messages":[{"role":"user","content":"Task"}]});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    }

    #[test]
    fn malformed_consumed_shapes_have_content_free_errors() {
        let mut bodies = vec![Value::Null, json!([])];
        for extra in [
            json!({"model":""}),
            json!({"messages":{}}),
            json!({"messages":[null]}),
            json!({"messages":[{"role":"private-secret","content":"secret"}]}),
            json!({"messages":[{"role":"user","content":{}}]}),
            json!({"messages":[{"role":"user","content":[null]}]}),
            json!({"messages":[{"role":"user","content":[{"type":"text","text":{"secret":true}}]}]}),
            json!({"thinking":{}}),
            json!({"tool_choice":{"type":[]}}),
            json!({"system":{}}),
            json!({"max_tokens":1.2}),
            json!({"max_tokens":-1}),
            json!({"stream":"true"}),
            json!({"messages":[{"role":"system","content":"Task","output_config":[]}]}),
        ] {
            bodies.push(request(extra));
        }
        for tools in [
            json!({}),
            json!([null]),
            json!([[]]),
            json!([{"type":{}}]),
            json!([{"name":42}]),
        ] {
            bodies.push(request(json!({"tools":tools})));
        }
        for field in ["thinking", "tool_choice", "output_config"] {
            for value in [Value::Null, json!([]), json!(42)] {
                let mut body = request(json!({}));
                body[field] = value;
                bodies.push(body);
            }
        }
        for body in bodies {
            let result = validate_request_shape(&body);
            assert_eq!(result["valid"], false, "{body}");
            let error = result["error"].as_str().unwrap();
            assert!(error.len() < 100);
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn null_defaults_unknown_extensions_and_signed_blocks_pass_through() {
        let body = request(json!({"max_tokens":0, "context_management":null,
            "tools":[{"type":null,"name":"Read","input_schema":{"future":true}}],
            "thinking":{"type":"future_thinking","opaque":true},
            "tool_choice":{"type":"future_choice"}, "future_feature":[null,"data"],
            "system":[{"type":"future_system","opaque":{"text":["not parsed"]}}],
            "messages":[{"role":"system","content":"Instructions","output_config":null},
                {"role":"assistant","content":[{"type":"thinking","signature":"opaque","thinking":""},
                    {"type":"tool_use","name":"Read","input":{"type":"tool_result","content":null}}]},
                {"role":"user","content":[{"type":"future_block","content":{"opaque":true}},
                    {"type":"tool_result"},{"type":"tool_result","content":"done"}]}]}));
        let before = body.clone();
        assert_eq!(validate_request_shape(&body), json!({"valid":true}));
        assert_eq!(body, before);
    }

    #[test]
    fn known_content_nesting_is_bounded_but_opaque_data_is_not_walked() {
        for depth in [32, 33, 100] {
            let mut content = json!([{"type":"text","text":"leaf"}]);
            for _ in 0..depth {
                content = json!([{"type":"tool_result","content":content}]);
            }
            let body = request(json!({"messages":[{"role":"user","content":content}]}));
            assert_eq!(
                validate_request_shape(&body),
                if depth <= 32 {
                    json!({"valid":true})
                } else {
                    invalid("content nesting")
                }
            );
        }
        for content in [json!([null]), json!({})] {
            assert_eq!(
                validate_request_shape(&request(
                    json!({"messages":[{"role":"user","content":[{"type":"tool_result","content":content}]}]})
                ))["valid"],
                false
            );
        }
    }

    #[test]
    fn safe_integer_and_ecmascript_trim_edges_match_javascript() {
        for literal in ["0", "-0", "1.0", "1e3", "9007199254740991"] {
            let value: Value = serde_json::from_str(literal).unwrap();
            assert_eq!(
                validate_request_shape(&request(json!({"max_tokens":value})))["valid"],
                true
            );
        }
        for value in [
            json!(9_007_199_254_740_992_u64),
            json!("1"),
            Value::Null,
            json!(1.1),
        ] {
            assert_eq!(
                validate_request_shape(&request(json!({"max_tokens":value}))),
                invalid("max_tokens")
            );
        }
        assert_eq!(
            validate_request_shape(&request(json!({"model":"\u{feff}"}))),
            invalid("model")
        );
        assert_eq!(
            validate_request_shape(&request(json!({"model":"\u{0085}"})))["valid"],
            true
        );
    }
}
