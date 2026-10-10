//! Frozen envelope-validation scenarios, including every malformed-container variant.
use autorouter_core::js_json::JsDocument;
use autorouter_core::request_validation::validate_request_document;
use serde_json::{Value, json};

fn request(extra: Value) -> Value {
    let mut body =
        json!({"model":"claude-sonnet-5-5","messages":[{"role":"user","content":"Task"}]});
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    body
}

fn validate(body: Value) -> Value {
    let document = JsDocument::parse(&serde_json::to_vec(&body).unwrap()).unwrap();
    let before = document.stringify();
    let result = validate_request_document(&document);
    assert_eq!(document.stringify(), before);
    result
}

#[test]
fn malformed_consumed_containers_return_bounded_private_diagnostics() {
    let mut variants = vec![
        Value::Null,
        json!([]),
        request(json!({"model":""})),
        request(json!({"messages":{}})),
        request(json!({"messages":[null]})),
        request(json!({"messages":[{"role":"private-secret","content":"secret"}]})),
        request(json!({"messages":[{"role":"user","content":{}}]})),
        request(json!({"messages":[{"role":"user","content":[null]}]})),
        request(
            json!({"messages":[{"role":"user","content":[{"type":"text","text":{"secret":true}}]}]}),
        ),
        request(json!({"thinking":{}})),
        request(json!({"tool_choice":{"type":[]}})),
        request(json!({"system":{}})),
        request(json!({"max_tokens":1.2})),
        request(json!({"max_tokens":-1})),
        request(json!({"stream":"true"})),
        request(json!({"messages":[{"role":"system","content":"Task","output_config":[]}]})),
    ];
    for tools in [
        json!({}),
        json!([null]),
        json!([[]]),
        json!([{"type":{}}]),
        json!([{"name":42}]),
    ] {
        variants.push(request(json!({"tools":tools})));
    }
    for field in ["thinking", "tool_choice", "output_config"] {
        for value in [Value::Null, json!([]), json!(42)] {
            let mut extra = json!({});
            extra[field] = value;
            variants.push(request(extra));
        }
    }
    for value in [json!([]), json!(42)] {
        variants.push(request(json!({"context_management":value})));
    }
    for body in variants {
        let result = validate(body);
        assert_eq!(result["valid"], false);
        let error = result["error"].as_str().unwrap();
        assert!(error.len() < 100);
        assert!(!error.contains("secret"));
    }
}

#[test]
fn cache_population_accepts_zero_output_without_mutation() {
    assert_eq!(
        validate(request(json!({"max_tokens":0}))),
        json!({"valid":true})
    );
}

#[test]
fn nullable_context_message_output_and_custom_tool_type_stay_intact() {
    assert_eq!(
        validate(request(json!({"context_management":null,
        "messages":[{"role":"system","content":"A turn-scoped instruction","output_config":null},
            {"role":"user","content":"Task"}],
        "tools":[{"type":null,"name":"Read","input_schema":{"type":"object"}}]}))),
        json!({"valid":true})
    );
}

#[test]
fn unknown_provider_contracts_and_signed_content_are_preserved() {
    assert_eq!(
        validate(request(json!({
            "system":[{"type":"future_system","opaque":{"text":["not a parsed text field"]}}],
            "tools":[{"type":"future_tool","name":"Tool","private_settings":{"nested":true}}],
            "safeguards":{"future_contract":true},
            "thinking":{"type":"future_thinking","budget_policy":{"custom":true}},
            "context_management":{"future_strategy":[null,42]},"tool_choice":{"type":"future_choice"},
            "future_feature":{"opaque":[null,"data"]},
            "messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"","signature":"opaque"}]},
                {"role":"user","content":[{"type":"future_block","content":{"opaque":true}},
                    {"type":"tool_result","content":[{"type":"image","source":{"type":"future_source","data":[]}}]}]}]
        }))),
        json!({"valid":true})
    );
}

#[test]
fn nested_results_are_checked_but_tool_inputs_and_schemas_remain_opaque() {
    for content in [json!([null]), json!({})] {
        assert_eq!(
            validate(request(
                json!({"messages":[{"role":"user","content":[{"type":"tool_result","content":content}]}]})
            ))["valid"],
            false
        );
    }
    assert_eq!(
        validate(request(json!({
            "tools":[{"name":"Read","input_schema":{"type":"object","properties":{"content":{"type":"array"}}}}],
            "messages":[{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"type":"tool_result","content":null}}]},
                {"role":"user","content":[{"type":"tool_result"},{"type":"tool_result","content":"done"}]}]
        }))),
        json!({"valid":true})
    );
    let mut content = json!([{"type":"text","text":"leaf"}]);
    for _ in 0..100 {
        content = json!([{"type":"tool_result","content":content}]);
    }
    assert_eq!(
        validate(request(
            json!({"messages":[{"role":"user","content":content}]})
        )),
        json!({"valid":false,"error":"Invalid Messages API request shape: content nesting"})
    );
}
