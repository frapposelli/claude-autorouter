//! Conservative cross-model execution guards. Claude owns permission review;
//! this module never interprets classifier context or rewrites signed history.

use serde_json::{Value, json};

use crate::model_catalog::model_capabilities;
use crate::model_request::{sonnet_needs_adaptive, thinking_adaptation};
use crate::request_validation::nonnegative_safe_integer;

const CACHE_FIELDS: &[&str] = &["type", "ttl"];
const REQUEST_FIELDS: &[&str] = &[
    "model",
    "messages",
    "system",
    "max_tokens",
    "metadata",
    "stream",
    "stop_sequences",
    "temperature",
    "top_p",
    "top_k",
    "tools",
    "tool_choice",
    "thinking",
    "output_config",
    "context_management",
    "speed",
    "container",
    "mcp_servers",
    "compaction",
    "safeguards",
    "service_tier",
    "inference_geo",
    "cache_control",
];
const MESSAGE_FIELDS: &[&str] = &["role", "content", "output_config", "clear_at"];
const CUSTOM_TOOL_FIELDS: &[&str] = &[
    "type",
    "name",
    "description",
    "input_schema",
    "cache_control",
    "defer_loading",
    "strict",
    "input_examples",
    "allowed_callers",
    "eager_input_streaming",
];
const SEARCH_TOOL_FIELDS: &[&str] = &[
    "type",
    "name",
    "allowed_callers",
    "cache_control",
    "defer_loading",
    "strict",
];
const BASH_TOOL_FIELDS: &[&str] = &[
    "type",
    "name",
    "allowed_callers",
    "cache_control",
    "defer_loading",
    "strict",
    "input_examples",
];
const EDITOR_TOOL_FIELDS: &[&str] = &[
    "type",
    "name",
    "allowed_callers",
    "cache_control",
    "defer_loading",
    "strict",
    "input_examples",
    "max_characters",
];

fn known_fields(value: &Value, fields: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.keys().all(|key| fields.contains(&key.as_str())))
}

fn optional_envelope(value: Option<&Value>, fields: &[&str]) -> bool {
    value.is_none_or(|value| value.is_null() || known_fields(value, fields))
}

fn nonnull(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| !value.is_null())
}

fn kind(value: &Value) -> Option<&str> {
    value.get("type").and_then(Value::as_str)
}

fn in_list(value: Option<&Value>, list: &[&str]) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|value| list.contains(&value))
}

fn shared_tool(tool: &Value) -> bool {
    let tool_type = match tool.get("type") {
        None | Some(Value::Null) => "custom",
        Some(Value::String(value)) => value,
        _ => return false,
    };
    let fields = match tool_type {
        "custom" => CUSTOM_TOOL_FIELDS,
        "bash_20250124" => BASH_TOOL_FIELDS,
        "text_editor_20250728" => EDITOR_TOOL_FIELDS,
        "tool_search_tool_regex_20251119" | "tool_search_tool_bm25_20251119" => SEARCH_TOOL_FIELDS,
        _ => return false,
    };
    known_fields(tool, fields) && optional_envelope(tool.get("cache_control"), CACHE_FIELDS)
}

fn shared_edit(edit: &Value) -> bool {
    let fields: &[&str] = match kind(edit) {
        Some("clear_thinking_20251015") => &["type", "keep"],
        Some("clear_tool_uses_20250919") => &[
            "type",
            "clear_at_least",
            "clear_tool_inputs",
            "exclude_tools",
            "keep",
            "trigger",
        ],
        _ => return false,
    };
    known_fields(edit, fields)
        && ["keep", "trigger", "clear_at_least"].iter().all(|key| {
            edit.get(key)
                .is_none_or(|value| !value.is_object() || known_fields(value, &["type", "value"]))
        })
}

fn content_fields(content_type: &str) -> Option<&'static [&'static str]> {
    Some(match content_type {
        "text" => &["type", "text", "cache_control", "citations"],
        "image" => &["type", "source", "cache_control", "transformations"],
        "document" => &[
            "type",
            "source",
            "cache_control",
            "citations",
            "context",
            "title",
        ],
        "tool_use" => &[
            "type",
            "id",
            "input",
            "name",
            "cache_control",
            "caller",
            "toolset_name",
        ],
        "tool_result" => &[
            "type",
            "tool_use_id",
            "content",
            "is_error",
            "cache_control",
            "toolset_name",
        ],
        "thinking" => &["type", "signature", "thinking"],
        "redacted_thinking" => &["type", "data"],
        "tool_reference" => &["type", "tool_name", "cache_control"],
        "tool_search_tool_result" => &["type", "content", "tool_use_id", "cache_control"],
        "tool_search_tool_search_result" => &["type", "tool_references"],
        "tool_search_tool_result_error" => &["type", "error_code", "error_message"],
        "tool_addition" | "tool_removal" => &["type", "tool", "cache_control"],
        _ => return None,
    })
}

fn compatible() -> Value {
    json!({"compatible":true})
}
fn incompatible(reason: &str) -> Value {
    json!({"compatible":false,"reason":reason})
}

/// Only the reviewed outer safeguard contract is inspected. Versioned
/// classifier context remains opaque permission-review data on the wire.
pub fn has_routable_safeguards(body: &Value) -> bool {
    body.get("model")
        .and_then(Value::as_str)
        .and_then(model_capabilities)
        .is_some_and(|facts| facts.shared_auto)
        && body
            .get("safeguards")
            .and_then(Value::as_array)
            .is_some_and(|entries| {
                !entries.is_empty()
                    && entries.iter().all(|entry| {
                        kind(entry) == Some("dangerous_tool_use")
                            && known_fields(entry, &["type", "classifier_context"])
                            && entry.get("classifier_context").is_some_and(|context| {
                                context.is_object()
                                    && context.get("v").and_then(Value::as_f64) == Some(1.0)
                            })
                    })
            })
}

pub fn can_route_auto_request(body: &Value, target: &str) -> bool {
    check_target(body, target, true)["compatible"] == true
}

/// Same-model requests retain provider ownership even for unknown IDs and
/// extensions. This is a routing guard, not the provider's full API validator.
pub fn target_compatibility(body: &Value, target: &str, auto_mode: bool) -> Value {
    if body.get("model").and_then(Value::as_str) == Some(target) {
        return compatible();
    }
    check_target(body, target, auto_mode)
}

fn check_target(body: &Value, target: &str, auto_mode: bool) -> Value {
    let Some(source_facts) = body
        .get("model")
        .and_then(Value::as_str)
        .and_then(model_capabilities)
    else {
        return incompatible("unknown_model");
    };
    let Some(target_facts) = model_capabilities(target) else {
        return incompatible("unknown_model");
    };
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return incompatible("invalid_request_shape");
    };
    if !body.is_object() {
        return incompatible("invalid_request_shape");
    }
    if auto_mode && (!source_facts.shared_auto || !target_facts.shared_auto) {
        return incompatible("auto_model");
    }
    if !known_fields(body, REQUEST_FIELDS)
        || !optional_envelope(body.get("cache_control"), CACHE_FIELDS)
    {
        return incompatible("request_extension");
    }
    if body.get("safeguards").is_some()
        && (!source_facts.shared_auto
            || !target_facts.shared_auto
            || !has_routable_safeguards(body))
    {
        return incompatible("safeguards");
    }
    if body.get("max_tokens").is_some_and(|value| {
        nonnegative_safe_integer(value).is_none_or(|value| value > target_facts.max_output_tokens)
    }) {
        return incompatible("output_limit");
    }
    if body
        .get("speed")
        .is_some_and(|value| value.as_str() != Some("standard"))
    {
        return incompatible("speed");
    }
    if ["container", "mcp_servers", "compaction"]
        .iter()
        .any(|key| body.get(key).is_some())
    {
        return incompatible("execution_facility");
    }
    if body.get("tools").is_some_and(|tools| {
        !tools
            .as_array()
            .is_some_and(|tools| tools.iter().all(shared_tool))
    }) {
        return incompatible("tool_type");
    }
    let mut contents: Vec<&[Value]> = Vec::new();
    if let Some(system) = body.get("system").and_then(Value::as_array) {
        contents.push(system);
    }
    for message in messages {
        if !message.is_object() {
            return incompatible("invalid_request_shape");
        }
        if !known_fields(message, MESSAGE_FIELDS) {
            return incompatible("content_extension");
        }
        let is_system = message.get("role").and_then(Value::as_str) == Some("system");
        if is_system && !target_facts.mid_conversation_system {
            return incompatible("system_message");
        }
        if let Some(output) = nonnull(message.get("output_config"))
            && (!target_facts.per_message_effort
                || !known_fields(output, &["effort"])
                || nonnull(output.get("effort"))
                    .is_some_and(|value| !in_list(Some(value), target_facts.effort_levels)))
        {
            return incompatible("message_effort");
        }
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        contents.push(blocks);
        if !is_system {
            continue;
        }
        for block in blocks {
            if !block.is_object() {
                return incompatible("invalid_request_shape");
            }
            if !matches!(kind(block), Some("tool_addition" | "tool_removal")) {
                continue;
            }
            let Some(tool) = block.get("tool") else {
                return incompatible("inline_tool");
            };
            let valid = match kind(tool) {
                Some("tool_reference") => known_fields(tool, &["type", "name"]),
                Some("tool_definition") if kind(block) == Some("tool_addition") => {
                    known_fields(tool, &["type", "definition"])
                        && tool.get("definition").is_some_and(shared_tool)
                }
                _ => false,
            };
            if !valid {
                return incompatible("inline_tool");
            }
        }
    }
    // Only known content containers are traversed. Inputs, schemas, citations
    // and document/source payloads are opaque and may contain arbitrary data.
    while let Some(blocks) = contents.pop() {
        for block in blocks {
            let Some(content_type) = kind(block) else {
                return incompatible("content_extension");
            };
            let Some(fields) = content_fields(content_type) else {
                return incompatible("content_extension");
            };
            // This type is valid as the nested tool-search result only.
            if content_type == "tool_search_tool_result_error"
                || !known_fields(block, fields)
                || !optional_envelope(block.get("cache_control"), CACHE_FIELDS)
            {
                return incompatible("content_extension");
            }
            if content_type == "tool_use"
                && let Some(caller) = block.get("caller")
            {
                let valid = match kind(caller) {
                    Some("direct") => known_fields(caller, &["type"]),
                    Some("code_execution_20250825" | "code_execution_20260120") => {
                        known_fields(caller, &["type", "tool_id"])
                    }
                    _ => false,
                };
                if !valid {
                    return incompatible("content_extension");
                }
            }
            if content_type == "image"
                && block.get("transformations").is_some_and(|value| {
                    value.is_object() && !known_fields(value, &["oversized_image"])
                })
            {
                return incompatible("content_extension");
            }
            if content_type == "tool_result"
                && let Some(content) = block.get("content").and_then(Value::as_array)
            {
                contents.push(content);
            }
            if content_type == "tool_search_tool_result" {
                let Some(result) = block.get("content") else {
                    return incompatible("content_extension");
                };
                let Some(
                    result_type @ ("tool_search_tool_search_result"
                    | "tool_search_tool_result_error"),
                ) = kind(result)
                else {
                    return incompatible("content_extension");
                };
                if !known_fields(result, content_fields(result_type).unwrap_or(&[])) {
                    return incompatible("content_extension");
                }
                if result_type == "tool_search_tool_search_result" {
                    contents.push(std::slice::from_ref(result));
                }
            }
            if content_type == "tool_search_tool_search_result" {
                let Some(references) = block.get("tool_references").and_then(Value::as_array)
                else {
                    return incompatible("content_extension");
                };
                contents.push(references);
            }
        }
    }
    if !target_facts.assistant_prefill
        && messages
            .last()
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            == Some("assistant")
    {
        return incompatible("assistant_prefill");
    }
    if let Some(choice) = body.get("tool_choice") {
        let Some(choice_type) = kind(choice) else {
            return incompatible("tool_choice");
        };
        let fields: &[&str] = match choice_type {
            "auto" | "any" => &["type", "disable_parallel_tool_use"],
            "tool" => &["type", "name", "disable_parallel_tool_use"],
            "none" => &["type"],
            _ => return incompatible("tool_choice"),
        };
        if !known_fields(choice, fields) {
            return incompatible("tool_choice");
        }
        if matches!(choice_type, "any" | "tool")
            && (auto_mode
                || !target_facts.forced_tool_choice
                || body.get("thinking").and_then(kind) == Some("enabled"))
        {
            return incompatible("forced_tool_choice");
        }
    }
    if let Some(context) = nonnull(body.get("context_management"))
        && (!source_facts.shared_auto
            || !target_facts.shared_auto
            || !known_fields(context, &["edits"])
            || !context
                .get("edits")
                .and_then(Value::as_array)
                .is_some_and(|edits| edits.iter().all(shared_edit)))
    {
        return incompatible("context_management");
    }
    if let Some(thinking) = body.get("thinking") {
        if !thinking.is_object()
            || (auto_mode
                && !matches!(
                    kind(thinking),
                    Some("adaptive" | "disabled" | "between_tools")
                ))
        {
            return incompatible("thinking_mode");
        }
        let Some(thinking_type) = kind(thinking) else {
            return incompatible("thinking_extension");
        };
        let fields: &[&str] = match thinking_type {
            "enabled" => &["type", "budget_tokens", "display", "block_binding"],
            "adaptive" => &["type", "display", "block_binding"],
            "disabled" | "between_tools" => &["type"],
            _ => return incompatible("thinking_extension"),
        };
        if !known_fields(thinking, fields)
            || !optional_envelope(thinking.get("block_binding"), &["prefix_mismatch_behavior"])
        {
            return incompatible("thinking_extension");
        }
        if thinking_type == "between_tools"
            && (body.get("model").and_then(Value::as_str) != Some("claude-sonnet-5-5")
                || !known_fields(thinking, &["type"])
                || target == "claude-sonnet-5")
        {
            return incompatible("thinking_mode");
        }
        let adapted_type =
            thinking_adaptation(body, target).map_or(thinking_type, |(kind, _)| kind);
        if !target_facts.thinking_types.contains(&adapted_type) {
            return incompatible("thinking_mode");
        }
        if adapted_type == "between_tools" && sonnet_needs_adaptive(body) {
            return incompatible("thinking_effort");
        }
        if target == "claude-opus-5"
            && adapted_type == "disabled"
            && in_list(
                body.get("output_config")
                    .and_then(|value| value.get("effort")),
                &["xhigh", "max"],
            )
        {
            return incompatible("thinking_effort");
        }
    }
    if let Some(output) = body.get("output_config") {
        if !known_fields(output, &["effort", "format", "task_budget"]) {
            return incompatible("output_extension");
        }
        if !optional_envelope(output.get("format"), &["type", "schema"])
            || nonnull(output.get("format")).is_some_and(|value| kind(value) != Some("json_schema"))
        {
            return incompatible("output_extension");
        }
        if !optional_envelope(output.get("task_budget"), &["type", "total", "remaining"])
            || nonnull(output.get("task_budget")).is_some_and(|value| kind(value) != Some("tokens"))
        {
            return incompatible("task_budget");
        }
        if nonnull(output.get("effort"))
            .is_some_and(|value| !in_list(Some(value), target_facts.effort_levels))
        {
            return incompatible("effort");
        }
        if nonnull(output.get("task_budget")).is_some() && !target_facts.task_budget {
            return incompatible("task_budget");
        }
    }
    if target_facts.default_sampling_only
        && !source_facts.default_sampling_only
        && (body
            .get("temperature")
            .is_some_and(|value| value.as_f64() != Some(1.0))
            || body
                .get("top_p")
                .is_some_and(|value| value.as_f64() != Some(1.0))
            || body.get("top_k").is_some())
    {
        return incompatible("sampling");
    }
    compatible()
}

/// Original-node guard: opaque data is never projected or recursively cloned.
/// Content containers are walked iteratively without a semantic depth limit.
mod document_guard {
    use super::*;
    use crate::js_json::{JsDocument, JsNode, NodeId};
    use crate::model_catalog::{MODEL_IDS, ModelCapabilities};
    use crate::model_request::{sonnet_needs_adaptive_document, thinking_adaptation_document};

    #[derive(Clone, Copy)]
    struct View<'a> {
        doc: &'a JsDocument,
        id: NodeId,
    }
    impl<'a> View<'a> {
        fn get(self, key: &str) -> Option<Self> {
            self.doc
                .get(self.id, key)
                .map(|id| Self { doc: self.doc, id })
        }
        fn object(self) -> bool {
            matches!(self.doc.node(self.id), Some(JsNode::Object(_)))
        }
        fn null(self) -> bool {
            matches!(self.doc.node(self.id), Some(JsNode::Null))
        }
        fn is(self, value: &str) -> bool {
            self.doc
                .string(self.id)
                .is_some_and(|actual| actual.units().iter().copied().eq(value.encode_utf16()))
        }
        fn number(self) -> Option<f64> {
            if let Some(JsNode::Number(value)) = self.doc.node(self.id) {
                Some(*value)
            } else {
                None
            }
        }
        fn array(self) -> Option<&'a [NodeId]> {
            if let Some(JsNode::Array(values)) = self.doc.node(self.id) {
                Some(values)
            } else {
                None
            }
        }
        fn view(self, id: NodeId) -> Self {
            Self { doc: self.doc, id }
        }
        fn in_list(self, values: &[&str]) -> bool {
            values.iter().any(|value| self.is(value))
        }
        fn kind(self, values: &'static [&'static str]) -> Option<&'static str> {
            let kind = self.get("type")?;
            values.iter().copied().find(|value| kind.is(value))
        }
        fn known(self, fields: &[&str]) -> bool {
            if let Some(JsNode::Object(object)) = self.doc.node(self.id) {
                object.entries().iter().all(|(key, _)| {
                    fields
                        .iter()
                        .any(|field| key.units().iter().copied().eq(field.encode_utf16()))
                })
            } else {
                false
            }
        }
        fn facts(self) -> Option<&'static ModelCapabilities> {
            MODEL_IDS
                .iter()
                .find(|model| self.is(model))
                .and_then(|model| model_capabilities(model))
        }
    }
    fn is(value: Option<View<'_>>, expected: &str) -> bool {
        value.is_some_and(|value| value.is(expected))
    }
    fn nonnull(value: Option<View<'_>>) -> Option<View<'_>> {
        value.filter(|value| !value.null())
    }
    fn optional(value: Option<View<'_>>, fields: &[&str]) -> bool {
        value.is_none_or(|value| value.null() || value.known(fields))
    }
    fn kind(value: View<'_>) -> Option<&'static str> {
        value.kind(&[
            "text",
            "image",
            "document",
            "tool_use",
            "tool_result",
            "thinking",
            "redacted_thinking",
            "tool_reference",
            "tool_search_tool_result",
            "tool_search_tool_search_result",
            "tool_search_tool_result_error",
            "tool_addition",
            "tool_removal",
            "tool_definition",
            "direct",
            "code_execution_20250825",
            "code_execution_20260120",
            "adaptive",
            "disabled",
            "between_tools",
            "enabled",
            "clear_thinking_20251015",
            "clear_tool_uses_20250919",
            "auto",
            "any",
            "tool",
            "none",
            "json_schema",
            "tokens",
        ])
    }
    fn shared_tool(tool: View<'_>) -> bool {
        let fields = match nonnull(tool.get("type")) {
            None => CUSTOM_TOOL_FIELDS,
            Some(value) if value.is("custom") => CUSTOM_TOOL_FIELDS,
            Some(value) if value.is("bash_20250124") => BASH_TOOL_FIELDS,
            Some(value) if value.is("text_editor_20250728") => EDITOR_TOOL_FIELDS,
            Some(value)
                if value.in_list(&[
                    "tool_search_tool_regex_20251119",
                    "tool_search_tool_bm25_20251119",
                ]) =>
            {
                SEARCH_TOOL_FIELDS
            }
            _ => return false,
        };
        tool.known(fields) && optional(tool.get("cache_control"), CACHE_FIELDS)
    }
    fn shared_edit(edit: View<'_>) -> bool {
        let fields: &[&str] = match kind(edit) {
            Some("clear_thinking_20251015") => &["type", "keep"],
            Some("clear_tool_uses_20250919") => &[
                "type",
                "clear_at_least",
                "clear_tool_inputs",
                "exclude_tools",
                "keep",
                "trigger",
            ],
            _ => return false,
        };
        edit.known(fields)
            && ["keep", "trigger", "clear_at_least"].iter().all(|key| {
                edit.get(key)
                    .is_none_or(|value| !value.object() || value.known(&["type", "value"]))
            })
    }
    fn safeguards(body: View<'_>) -> bool {
        body.get("model")
            .and_then(View::facts)
            .is_some_and(|facts| facts.shared_auto)
            && body
                .get("safeguards")
                .and_then(View::array)
                .is_some_and(|entries| {
                    !entries.is_empty()
                        && entries.iter().all(|&id| {
                            let entry = body.view(id);
                            is(entry.get("type"), "dangerous_tool_use")
                                && entry.known(&["type", "classifier_context"])
                                && entry.get("classifier_context").is_some_and(|context| {
                                    context.object()
                                        && context.get("v").and_then(View::number) == Some(1.0)
                                })
                        })
                })
    }
    pub(super) fn has_safeguards(doc: &JsDocument) -> bool {
        safeguards(View {
            doc,
            id: doc.root(),
        })
    }
    pub(super) fn check(
        doc: &JsDocument,
        target: &str,
        auto_mode: bool,
        same_model: bool,
    ) -> Value {
        let body = View {
            doc,
            id: doc.root(),
        };
        if same_model && is(body.get("model"), target) {
            return compatible();
        }
        let Some(source) = body.get("model").and_then(View::facts) else {
            return incompatible("unknown_model");
        };
        let Some(target_facts) = model_capabilities(target) else {
            return incompatible("unknown_model");
        };
        let Some(messages) = body.get("messages").and_then(View::array) else {
            return incompatible("invalid_request_shape");
        };
        if !body.object() {
            return incompatible("invalid_request_shape");
        }
        if auto_mode && (!source.shared_auto || !target_facts.shared_auto) {
            return incompatible("auto_model");
        }
        if !body.known(REQUEST_FIELDS) || !optional(body.get("cache_control"), CACHE_FIELDS) {
            return incompatible("request_extension");
        }
        if body.get("safeguards").is_some()
            && (!source.shared_auto || !target_facts.shared_auto || !safeguards(body))
        {
            return incompatible("safeguards");
        }
        if body.get("max_tokens").is_some_and(|value| {
            value.number().is_none_or(|number| {
                !number.is_finite()
                    || number < 0.0
                    || number.fract() != 0.0
                    || number > 9_007_199_254_740_991.0
                    || number > target_facts.max_output_tokens as f64
            })
        }) {
            return incompatible("output_limit");
        }
        if body.get("speed").is_some_and(|value| !value.is("standard")) {
            return incompatible("speed");
        }
        if ["container", "mcp_servers", "compaction"]
            .iter()
            .any(|key| body.get(key).is_some())
        {
            return incompatible("execution_facility");
        }
        if body.get("tools").is_some_and(|tools| {
            tools
                .array()
                .is_none_or(|tools| !tools.iter().all(|&tool| shared_tool(body.view(tool))))
        }) {
            return incompatible("tool_type");
        }
        enum Contents<'a> {
            Slice(&'a [NodeId]),
            One(NodeId),
        }
        impl Contents<'_> {
            fn ids(&self) -> &[NodeId] {
                match self {
                    Self::Slice(ids) => ids,
                    Self::One(id) => std::slice::from_ref(id),
                }
            }
        }
        let mut contents = Vec::new();
        if let Some(system) = body.get("system").and_then(View::array) {
            contents.push(Contents::Slice(system));
        }
        for &message in messages {
            let message = body.view(message);
            if !message.object() {
                return incompatible("invalid_request_shape");
            }
            if !message.known(MESSAGE_FIELDS) {
                return incompatible("content_extension");
            }
            let system = is(message.get("role"), "system");
            if system && !target_facts.mid_conversation_system {
                return incompatible("system_message");
            }
            if nonnull(message.get("output_config")).is_some_and(|output| {
                !target_facts.per_message_effort
                    || !output.known(&["effort"])
                    || nonnull(output.get("effort"))
                        .is_some_and(|effort| !effort.in_list(target_facts.effort_levels))
            }) {
                return incompatible("message_effort");
            }
            let Some(blocks) = message.get("content").and_then(View::array) else {
                continue;
            };
            contents.push(Contents::Slice(blocks));
            if !system {
                continue;
            }
            for &block in blocks {
                let block = body.view(block);
                if !block.object() {
                    return incompatible("invalid_request_shape");
                }
                if !matches!(kind(block), Some("tool_addition" | "tool_removal")) {
                    continue;
                }
                let Some(tool) = block.get("tool") else {
                    return incompatible("inline_tool");
                };
                let valid = match kind(tool) {
                    Some("tool_reference") => tool.known(&["type", "name"]),
                    Some("tool_definition") if kind(block) == Some("tool_addition") => {
                        tool.known(&["type", "definition"])
                            && tool.get("definition").is_some_and(shared_tool)
                    }
                    _ => false,
                };
                if !valid {
                    return incompatible("inline_tool");
                }
            }
        }
        while let Some(blocks) = contents.pop() {
            for &block in blocks.ids() {
                let block = body.view(block);
                let Some(content_type) = kind(block) else {
                    return incompatible("content_extension");
                };
                let Some(fields) = content_fields(content_type) else {
                    return incompatible("content_extension");
                };
                if content_type == "tool_search_tool_result_error"
                    || !block.known(fields)
                    || !optional(block.get("cache_control"), CACHE_FIELDS)
                {
                    return incompatible("content_extension");
                }
                if content_type == "tool_use"
                    && let Some(caller) = block.get("caller")
                {
                    let valid = match kind(caller) {
                        Some("direct") => caller.known(&["type"]),
                        Some("code_execution_20250825" | "code_execution_20260120") => {
                            caller.known(&["type", "tool_id"])
                        }
                        _ => false,
                    };
                    if !valid {
                        return incompatible("content_extension");
                    }
                }
                if content_type == "image"
                    && block
                        .get("transformations")
                        .is_some_and(|value| value.object() && !value.known(&["oversized_image"]))
                {
                    return incompatible("content_extension");
                }
                if content_type == "tool_result"
                    && let Some(content) = block.get("content").and_then(View::array)
                {
                    contents.push(Contents::Slice(content));
                }
                if content_type == "tool_search_tool_result" {
                    let Some(result) = block.get("content") else {
                        return incompatible("content_extension");
                    };
                    let Some(
                        result_type @ ("tool_search_tool_search_result"
                        | "tool_search_tool_result_error"),
                    ) = kind(result)
                    else {
                        return incompatible("content_extension");
                    };
                    if !result.known(content_fields(result_type).unwrap_or(&[])) {
                        return incompatible("content_extension");
                    }
                    if result_type == "tool_search_tool_search_result" {
                        contents.push(Contents::One(result.id));
                    }
                }
                if content_type == "tool_search_tool_search_result" {
                    let Some(references) = block.get("tool_references").and_then(View::array)
                    else {
                        return incompatible("content_extension");
                    };
                    contents.push(Contents::Slice(references));
                }
            }
        }
        if !target_facts.assistant_prefill
            && messages
                .last()
                .is_some_and(|&message| is(body.view(message).get("role"), "assistant"))
        {
            return incompatible("assistant_prefill");
        }
        if let Some(choice) = body.get("tool_choice") {
            let Some(choice_type) = kind(choice) else {
                return incompatible("tool_choice");
            };
            let fields: &[&str] = match choice_type {
                "auto" | "any" => &["type", "disable_parallel_tool_use"],
                "tool" => &["type", "name", "disable_parallel_tool_use"],
                "none" => &["type"],
                _ => return incompatible("tool_choice"),
            };
            if !choice.known(fields) {
                return incompatible("tool_choice");
            }
            if matches!(choice_type, "any" | "tool")
                && (auto_mode
                    || !target_facts.forced_tool_choice
                    || body
                        .get("thinking")
                        .is_some_and(|thinking| kind(thinking) == Some("enabled")))
            {
                return incompatible("forced_tool_choice");
            }
        }
        if let Some(context) = nonnull(body.get("context_management"))
            && (!source.shared_auto
                || !target_facts.shared_auto
                || !context.known(&["edits"])
                || context
                    .get("edits")
                    .and_then(View::array)
                    .is_none_or(|edits| !edits.iter().all(|&edit| shared_edit(body.view(edit)))))
        {
            return incompatible("context_management");
        }
        if let Some(thinking) = body.get("thinking") {
            if !thinking.object()
                || (auto_mode
                    && !matches!(
                        kind(thinking),
                        Some("adaptive" | "disabled" | "between_tools")
                    ))
            {
                return incompatible("thinking_mode");
            }
            let Some(thinking_type) = kind(thinking) else {
                return incompatible("thinking_extension");
            };
            let fields: &[&str] = match thinking_type {
                "enabled" => &["type", "budget_tokens", "display", "block_binding"],
                "adaptive" => &["type", "display", "block_binding"],
                "disabled" | "between_tools" => &["type"],
                _ => return incompatible("thinking_extension"),
            };
            if !thinking.known(fields)
                || !optional(thinking.get("block_binding"), &["prefix_mismatch_behavior"])
            {
                return incompatible("thinking_extension");
            }
            if thinking_type == "between_tools"
                && (!is(body.get("model"), "claude-sonnet-5-5")
                    || !thinking.known(&["type"])
                    || target == "claude-sonnet-5")
            {
                return incompatible("thinking_mode");
            }
            let adapted =
                thinking_adaptation_document(doc, target).map_or(thinking_type, |(kind, _)| kind);
            if !target_facts.thinking_types.contains(&adapted) {
                return incompatible("thinking_mode");
            }
            if adapted == "between_tools" && sonnet_needs_adaptive_document(doc) {
                return incompatible("thinking_effort");
            }
            if target == "claude-opus-5"
                && adapted == "disabled"
                && body
                    .get("output_config")
                    .and_then(|output| output.get("effort"))
                    .is_some_and(|effort| effort.in_list(&["xhigh", "max"]))
            {
                return incompatible("thinking_effort");
            }
        }
        if let Some(output) = body.get("output_config") {
            if !output.known(&["effort", "format", "task_budget"]) {
                return incompatible("output_extension");
            }
            if !optional(output.get("format"), &["type", "schema"])
                || nonnull(output.get("format"))
                    .is_some_and(|format| !is(format.get("type"), "json_schema"))
            {
                return incompatible("output_extension");
            }
            if !optional(output.get("task_budget"), &["type", "total", "remaining"])
                || nonnull(output.get("task_budget"))
                    .is_some_and(|budget| !is(budget.get("type"), "tokens"))
            {
                return incompatible("task_budget");
            }
            if nonnull(output.get("effort"))
                .is_some_and(|effort| !effort.in_list(target_facts.effort_levels))
            {
                return incompatible("effort");
            }
            if nonnull(output.get("task_budget")).is_some() && !target_facts.task_budget {
                return incompatible("task_budget");
            }
        }
        if target_facts.default_sampling_only
            && !source.default_sampling_only
            && (body
                .get("temperature")
                .is_some_and(|value| value.number() != Some(1.0))
                || body
                    .get("top_p")
                    .is_some_and(|value| value.number() != Some(1.0))
                || body.get("top_k").is_some())
        {
            return incompatible("sampling");
        }
        compatible()
    }
}

pub fn has_routable_safeguards_document(document: &crate::js_json::JsDocument) -> bool {
    document_guard::has_safeguards(document)
}
pub fn can_route_auto_request_document(
    document: &crate::js_json::JsDocument,
    target: &str,
) -> bool {
    document_guard::check(document, target, true, false)["compatible"] == true
}
pub fn target_compatibility_document_exact(
    doc: &crate::js_json::JsDocument,
    target: &crate::js_json::JsString,
    auto_mode: bool,
) -> Value {
    if doc
        .get(doc.root(), "model")
        .and_then(|node| doc.string(node))
        == Some(target)
    {
        return compatible();
    }
    match target.to_scalar() {
        Some(target) => target_compatibility_document(doc, &target, auto_mode),
        None => incompatible("unknown_model"),
    }
}

pub fn target_compatibility_document(
    document: &crate::js_json::JsDocument,
    target: &str,
    auto_mode: bool,
) -> Value {
    document_guard::check(document, target, auto_mode, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODERN: &[&str] = &[
        "claude-sonnet-5",
        "claude-sonnet-5-5",
        "claude-opus-5",
        "claude-opus-5-5",
    ];

    fn document(extra: &str) -> crate::js_json::JsDocument {
        crate::js_json::JsDocument::parse(format!(r#"{{"model":"claude-sonnet-5-5","max_tokens":32000,"messages":[{{"role":"user","content":"Synthetic task"}}]{extra}}}"#).as_bytes()).unwrap()
    }

    #[test]
    fn authoritative_guard_preserves_overflow_truthiness_and_optional_envelopes() {
        for (extra, reason) in [
            (r#", "cache_control":1e999"#, "request_extension"),
            (r#", "tools":[{"type":1e999}]"#, "tool_type"),
            (
                r#", "tools":[{"type":"custom","cache_control":1e999}]"#,
                "tool_type",
            ),
            (r#", "context_management":1e999"#, "context_management"),
            (r#", "output_config":{"effort":1e999}"#, "effort"),
            (r#", "output_config":{"format":1e999}"#, "output_extension"),
            (r#", "output_config":{"task_budget":1e999}"#, "task_budget"),
            (
                r#", "thinking":{"type":"adaptive","block_binding":1e999}"#,
                "thinking_extension",
            ),
            (
                r#", "messages":[{"role":"user","content":"Synthetic task","output_config":1e999}]"#,
                "message_effort",
            ),
        ] {
            let document = document(extra);
            for auto in [false, true] {
                assert_eq!(
                    target_compatibility_document(&document, "claude-opus-5-5", auto),
                    json!({"compatible":false,"reason":reason}),
                    "{extra}"
                );
            }
        }
    }

    #[test]
    fn arbitrary_depth_content_is_checked_while_opaque_payloads_stay_opaque() {
        let nested = |leaf: &str| {
            "[{\"type\":\"tool_result\",\"content\":".repeat(2048) + leaf + &"}]".repeat(2048)
        };
        for (leaf, expected) in [
            (r#"[{"type":"text","text":"Synthetic text"}]"#, true),
            (r#"[{"type":"future_extension","opaque":true}]"#, false),
        ] {
            let doc = document(&format!(
                r#", "messages":[{{"role":"user","content":{}}}]"#,
                nested(leaf)
            ));
            assert_eq!(
                target_compatibility_document(&doc, "claude-opus-5-5", false)["compatible"],
                expected
            );
            assert_eq!(
                can_route_auto_request_document(&doc, "claude-opus-5-5"),
                expected
            );
        }
        let doc = document(&format!(
            r#", "tools":[{{"name":"Synthetic","input_schema":{{"type":"object","deep":{},"opaque":"\ud800"}}}}],"messages":[{{"role":"user","content":[{{"type":"tool_use","id":"synthetic","input":{{"deep":{},"opaque":1e999}}}}]}}]"#,
            nested(r#"[{"type":"future_extension"}]"#),
            nested(r#"[{"type":"future_extension"}]"#)
        ));
        assert_eq!(
            target_compatibility_document(&doc, "claude-opus-5-5", false),
            json!({"compatible":true})
        );
    }

    #[test]
    fn unknown_utf16_ids_do_not_compare_equal_to_replacement_characters() {
        let doc =
            crate::js_json::JsDocument::parse(br#"{"model":"synthetic-\ud800","messages":[]}"#)
                .unwrap();
        assert_eq!(
            target_compatibility_document(&doc, "synthetic-\u{fffd}", false),
            json!({"compatible":false,"reason":"unknown_model"})
        );
        let doc = document(r#", "tools":[{"type":"custom","\ud800":true}],"opaque":"\udfff""#);
        assert_eq!(
            target_compatibility_document(&doc, "claude-opus-5-5", false)["reason"],
            "request_extension"
        );
    }

    fn request(extra: Value) -> Value {
        let mut body = json!({"model":"claude-sonnet-5-5","max_tokens":32000,
            "messages":[{"role":"user","content":"Review the locking implementation."}]});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    }

    fn assert_reason(extra: Value, target: &str, reason: &str) {
        let body = request(extra);
        let before = body.clone();
        for auto_mode in [false, true] {
            assert_eq!(
                target_compatibility(&body, target, auto_mode),
                incompatible(reason),
                "{body}"
            );
            assert_eq!(
                target_compatibility(&body, body["model"].as_str().unwrap(), auto_mode),
                compatible()
            );
        }
        assert_eq!(body, before);
    }

    #[test]
    fn all_exact_modern_pairs_route_while_aliases_and_older_auto_targets_do_not() {
        for source in MODERN {
            for target in MODERN {
                assert!(can_route_auto_request(
                    &request(json!({"model":source})),
                    target
                ));
            }
        }
        for model in [
            "sonnet",
            "opus",
            "claude-sonnet-4-6",
            "claude-opus-4-8",
            "claude-haiku-4-5-20251001",
            "team/claude-opus-5-5",
            "claude-opus-5-5-future",
        ] {
            assert!(!can_route_auto_request(&request(json!({})), model));
            assert!(!can_route_auto_request(
                &request(json!({"model":model})),
                "claude-opus-5-5"
            ));
        }
        assert_eq!(
            target_compatibility(
                &request(json!({"model":"custom","future":true})),
                "custom",
                true
            ),
            compatible()
        );
    }

    #[test]
    fn guards_preserve_safeguards_version_and_unknown_contracts() {
        let known = json!({"type":"dangerous_tool_use","classifier_context":{"v":1,"policy":{"opaque":true}}});
        let body = request(json!({"safeguards":[known]}));
        assert!(has_routable_safeguards(&body));
        assert!(can_route_auto_request(&body, "claude-opus-5-5"));
        for safeguards in [
            Value::Null,
            json!({}),
            json!([]),
            json!([null]),
            json!([{"type":"future_safeguard","classifier_context":{"v":1}}]),
            json!([{"type":"dangerous_tool_use","classifier_context":{"v":2}}]),
            json!([{"type":"dangerous_tool_use","classifier_context":{"v":"1"}}]),
            json!([{"type":"dangerous_tool_use","classifier_context":[]}]),
            json!([{"type":"dangerous_tool_use"}]),
            json!([{"type":"dangerous_tool_use","classifier_context":{"v":1},"future":true}]),
        ] {
            let body = request(json!({"safeguards":safeguards}));
            assert!(!has_routable_safeguards(&body));
            assert_eq!(
                target_compatibility(&body, "claude-opus-5-5", false),
                incompatible("safeguards")
            );
        }
    }

    #[test]
    fn opaque_schema_and_signed_history_survive_all_shared_envelopes() {
        let opaque = json!({"future_contract":{"type":"future_block","arbitrary":[null,true]}});
        let cache = json!({"type":"ephemeral","ttl":"1h"});
        let body = request(json!({"cache_control":cache,
            "tool_choice":{"type":"auto","disable_parallel_tool_use":true},
            "tools":[
                {"name":"Read","input_schema":opaque,"input_examples":[opaque],"defer_loading":true,"strict":true,"allowed_callers":["direct"],"eager_input_streaming":null,"cache_control":cache},
                {"type":"bash_20250124","name":"bash","input_examples":[opaque]},
                {"type":"text_editor_20250728","name":"edit","max_characters":10000},
                {"type":"tool_search_tool_regex_20251119","name":"search"},
                {"type":"tool_search_tool_bm25_20251119","name":"search"}],
            "thinking":{"type":"adaptive","display":"updates","block_binding":{"prefix_mismatch_behavior":"error"}},
            "output_config":{"effort":"high","format":{"type":"json_schema","schema":opaque},"task_budget":{"type":"tokens","total":10000,"remaining":7000}},
            "context_management":{"edits":[{"type":"clear_thinking_20251015","keep":"all"},
                {"type":"clear_tool_uses_20250919","trigger":{"type":"input_tokens","value":900000},"keep":{"type":"tool_uses","value":5},"exclude_tools":["Read"]}]},
            "messages":[
                {"role":"assistant","content":[{"type":"thinking","thinking":"","signature":"signed+/=="},
                    {"type":"redacted_thinking","data":"opaque+/=="},
                    {"type":"tool_use","id":"call-1","name":"Read","input":opaque,"caller":{"type":"direct"},"toolset_name":null,"cache_control":cache}]},
                {"role":"user","content":[{"type":"text","text":"Task","citations":[opaque],"cache_control":cache},
                    {"type":"image","source":opaque,"transformations":{"oversized_image":"downsize"}},
                    {"type":"document","source":opaque,"citations":opaque,"title":"Title","context":"Context"},
                    {"type":"tool_result","tool_use_id":"call-1","is_error":false,"toolset_name":null,"content":[{"type":"tool_reference","tool_name":"Read","cache_control":cache}]},
                    {"type":"tool_search_tool_result","tool_use_id":"search-1","content":{"type":"tool_search_tool_search_result","tool_references":[{"type":"tool_reference","tool_name":"Read"}]}},
                    {"type":"tool_search_tool_result","tool_use_id":"search-2","content":{"type":"tool_search_tool_result_error","error_code":"unavailable","error_message":"Synthetic"}}]},
                {"role":"system","clear_at":"next_user_message","output_config":{"effort":"high"},"content":[
                    {"type":"tool_addition","tool":{"type":"tool_definition","definition":{"name":"Read","input_schema":opaque}}},
                    {"type":"tool_addition","tool":{"type":"tool_reference","name":"Read"}},
                    {"type":"tool_removal","tool":{"type":"tool_reference","name":"Old"}}]}]}));
        let before = body.clone();
        assert!(can_route_auto_request(&body, "claude-opus-5-5"));
        assert_eq!(
            target_compatibility(&body, "claude-opus-5-5", false),
            compatible()
        );
        assert_eq!(body, before);
    }

    #[test]
    fn unknown_semantic_envelopes_pin_instead_of_dropping_fields() {
        for (extra, reason) in [
            (
                json!({"future_parameter":{"opaque":true}}),
                "request_extension",
            ),
            (
                json!({"cache_control":{"type":"ephemeral","future":true}}),
                "request_extension",
            ),
            (
                json!({"messages":[{"role":"user","content":"Task","future":true}]}),
                "content_extension",
            ),
            (
                json!({"tools":[{"name":"Read","input_schema":{},"future":true}]}),
                "tool_type",
            ),
            (
                json!({"tools":[{"name":"Read","cache_control":{"type":"ephemeral","future":true}}]}),
                "tool_type",
            ),
            (
                json!({"thinking":{"type":"adaptive","block_binding":{"prefix_mismatch_behavior":"error","future":true}}}),
                "thinking_extension",
            ),
            (
                json!({"output_config":{"format":{"type":"json_schema","schema":{},"future":true}}}),
                "output_extension",
            ),
            (
                json!({"output_config":{"task_budget":{"type":"tokens","total":1000,"future":true}}}),
                "task_budget",
            ),
            (
                json!({"context_management":{"edits":[{"type":"clear_thinking_20251015","keep":"all","future":true}]}}),
                "context_management",
            ),
            (
                json!({"context_management":{"edits":[{"type":"clear_tool_uses_20250919","trigger":{"type":"input_tokens","value":100,"future":true}}]}}),
                "context_management",
            ),
            (
                json!({"messages":[{"role":"system","content":[{"type":"tool_addition","tool":{"type":"tool_reference","name":"Read","future":true}}]}]}),
                "inline_tool",
            ),
        ] {
            assert_reason(extra, "claude-opus-5-5", reason);
        }
        for tool_type in [
            "advisor_20260301",
            "computer_20251124",
            "computer_toolset_20260801",
            "web_search_20260318",
            "code_execution_20260521",
            "mcp_toolset",
            "future_tool_20270101",
        ] {
            assert_reason(
                json!({"tools":[{"type":tool_type,"name":"Synthetic"}]}),
                "claude-opus-5-5",
                "tool_type",
            );
            assert_reason(
                json!({"messages":[{"role":"system","content":[{"type":"tool_addition","tool":{"type":"tool_definition","definition":{"type":tool_type,"name":"Synthetic"}}}]}]}),
                "claude-opus-5-5",
                "inline_tool",
            );
        }
        for choice_type in ["auto", "none", "any", "tool"] {
            assert_reason(
                json!({"tool_choice":{"type":choice_type,"future":true}}),
                "claude-opus-5-5",
                "tool_choice",
            );
        }
    }

    #[test]
    fn unknown_nested_content_fields_cannot_cross_a_model_boundary() {
        for block in [
            json!({"type":"text","text":"Task","future":true}),
            json!({"type":"thinking","thinking":"","signature":"opaque","future":true}),
            json!({"type":"redacted_thinking","data":"opaque","future":true}),
            json!({"type":"tool_use","id":"call","name":"Read","input":{},"future":true}),
            json!({"type":"tool_result","content":"done","future":true}),
            json!({"type":"image","source":{},"future":true}),
            json!({"type":"document","source":{},"future":true}),
            json!({"type":"tool_reference","tool_name":"Read","future":true}),
            json!({"type":"text","text":"Task","cache_control":{"type":"ephemeral","future":true}}),
            json!({"type":"tool_use","name":"Read","input":{},"caller":{"type":"direct","future":true}}),
            json!({"type":"tool_search_tool_result","content":{"type":"tool_search_tool_search_result","tool_references":[{"type":"tool_reference","tool_name":"Read","future":true}]}}),
        ] {
            for wrapped in [false, true] {
                let content = if wrapped {
                    json!([{"type":"tool_result","content":[block]}])
                } else {
                    json!([block])
                };
                assert_reason(
                    json!({"messages":[{"role":"user","content":content}]}),
                    "claude-opus-5-5",
                    "content_extension",
                );
            }
        }
    }

    #[test]
    fn null_optional_envelopes_retain_defaults_without_being_omitted() {
        let body = request(json!({"cache_control":null,"context_management":null,
            "thinking":{"type":"adaptive","block_binding":null},
            "output_config":{"effort":null,"format":null,"task_budget":null},
            "tools":[{"type":null,"name":"Read","input_schema":{},"cache_control":null}],
            "messages":[{"role":"user","content":"Task"},
                {"role":"system","content":"Reminder","output_config":null},
                {"role":"system","content":"Default effort","output_config":{"effort":null}}]}));
        let before = body.clone();
        for auto_mode in [false, true] {
            assert_eq!(
                target_compatibility(&body, "claude-opus-5-5", auto_mode),
                compatible()
            );
        }
        assert_eq!(body, before);
    }

    #[test]
    fn all_profile_forced_tools_and_model_specific_facilities_are_preserved() {
        for choice in [json!({"type":"any"}), json!({"type":"tool","name":"Read"})] {
            let body = request(
                json!({"model":"claude-haiku-4-5-20251001","thinking":{"type":"disabled"},"tool_choice":choice}),
            );
            for model in ["claude-sonnet-5-5", "claude-opus-5-5"] {
                assert_eq!(
                    target_compatibility(&body, model, false),
                    incompatible("forced_tool_choice")
                );
            }
            for model in ["claude-sonnet-5", "claude-opus-5"] {
                assert_eq!(target_compatibility(&body, model, false), compatible());
            }
            assert!(!can_route_auto_request(
                &request(json!({"tool_choice":choice})),
                "claude-opus-5-5"
            ));
        }
        for extra in [
            json!({"container":{}}),
            json!({"mcp_servers":[]}),
            json!({"compaction":{}}),
            json!({"container":null}),
        ] {
            assert_reason(extra, "claude-opus-5-5", "execution_facility");
        }
        assert_reason(json!({"speed":"fast"}), "claude-opus-5-5", "speed");
        assert!(can_route_auto_request(
            &request(json!({"speed":"standard"})),
            "claude-opus-5-5"
        ));
    }

    #[test]
    fn output_thinking_effort_sampling_and_prefill_restrictions_have_exact_reasons() {
        for (extra, target, reason) in [
            (
                json!({"model":"claude-haiku-4-5","thinking":{"type":"enabled","budget_tokens":1000}}),
                "claude-opus-5-5",
                "thinking_mode",
            ),
            (
                json!({"model":"claude-haiku-4-5","thinking":{"type":"adaptive"}}),
                "claude-sonnet-4-5",
                "thinking_mode",
            ),
            (
                json!({"max_tokens":64001}),
                "claude-haiku-4-5",
                "output_limit",
            ),
            (
                json!({"max_tokens":128001}),
                "claude-opus-5-5",
                "output_limit",
            ),
            (
                json!({"output_config":{"effort":"max"}}),
                "claude-haiku-4-5",
                "effort",
            ),
            (
                json!({"output_config":{"effort":"xhigh"}}),
                "claude-sonnet-4-6",
                "effort",
            ),
            (
                json!({"model":"claude-haiku-4-5","temperature":0.4}),
                "claude-sonnet-5",
                "sampling",
            ),
            (
                json!({"model":"claude-haiku-4-5","top_p":0.8}),
                "claude-opus-5-5",
                "sampling",
            ),
            (
                json!({"model":"claude-haiku-4-5","top_k":42}),
                "claude-opus-4-8",
                "sampling",
            ),
            (
                json!({"model":"claude-haiku-4-5","messages":[{"role":"assistant","content":"Prefill"}]}),
                "claude-opus-5-5",
                "assistant_prefill",
            ),
        ] {
            assert_eq!(
                target_compatibility(&request(extra), target, false),
                incompatible(reason)
            );
        }
        for max_tokens in [json!(0), json!(1), json!(128000), json!(128000.0)] {
            assert!(can_route_auto_request(
                &request(json!({"max_tokens":max_tokens})),
                "claude-opus-5-5"
            ));
        }
        for max_tokens in [
            json!(-1),
            json!(1.5),
            json!(128001),
            json!("128000"),
            Value::Null,
        ] {
            assert_reason(
                json!({"max_tokens":max_tokens}),
                "claude-opus-5-5",
                "output_limit",
            );
        }
        assert_eq!(
            target_compatibility(
                &request(json!({"model":"claude-haiku-4-5","temperature":1,"top_p":1})),
                "claude-sonnet-5",
                false
            ),
            compatible()
        );
    }

    #[test]
    fn sonnet5_cannot_receive_modern_system_effort_or_task_budget() {
        for (extra, reason) in [
            (
                json!({"messages":[{"role":"system","content":"Instruction"}]}),
                "system_message",
            ),
            (
                json!({"messages":[{"role":"user","content":"Task","output_config":{"effort":"low"}}]}),
                "message_effort",
            ),
            (
                json!({"output_config":{"task_budget":{"type":"tokens","total":10000}}}),
                "task_budget",
            ),
        ] {
            let body = request(extra);
            assert_eq!(
                target_compatibility(&body, "claude-sonnet-5", true),
                incompatible(reason)
            );
            assert!(can_route_auto_request(&body, "claude-opus-5-5"));
        }
    }

    #[test]
    fn between_tools_requires_exact_source_and_no_extensions() {
        let body = request(json!({"thinking":{"type":"between_tools"}}));
        for model in ["claude-sonnet-5-5", "claude-opus-5", "claude-opus-5-5"] {
            assert!(can_route_auto_request(&body, model));
        }
        assert!(!can_route_auto_request(&body, "claude-sonnet-5"));
        for model in ["claude-sonnet-5", "claude-opus-5", "claude-opus-5-5"] {
            assert!(!can_route_auto_request(
                &request(json!({"model":model,"thinking":{"type":"between_tools"}})),
                "claude-opus-5-5"
            ));
        }
        for thinking in [
            Value::Null,
            json!([]),
            json!({"type":"enabled","budget_tokens":1000}),
            json!({"type":"future_mode"}),
            json!({"type":"between_tools","display":"summarized"}),
            json!({"type":"between_tools","budget_tokens":1000}),
            json!({"type":"between_tools","block_binding":{"prefix_mismatch_behavior":"error"}}),
        ] {
            assert!(!can_route_auto_request(
                &request(json!({"thinking":thinking})),
                "claude-opus-5-5"
            ));
        }
        let native =
            request(json!({"thinking":{"type":"between_tools"},"output_config":{"effort":"max"}}));
        assert!(!can_route_auto_request(&native, "claude-sonnet-5-5"));
        assert_eq!(
            target_compatibility(&native, "claude-sonnet-5-5", true),
            compatible()
        );
        assert!(can_route_auto_request(&native, "claude-opus-5-5"));
    }
}
