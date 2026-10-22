//! Routing policy split at the two asynchronous boundaries in the runtime.
//! No request body is retained across phases. Authoritative JSON provides exact
//! hash/context identity and direct field reads avoid copying opaque catalogs.
use crate::auto_routing::{can_route_auto_request_document, has_routable_safeguards_document};
use crate::config::{ClientProfile, Evaluator, RouterConfig};
use crate::contracts::{
    ClassifierDecision, ClassifierError, ContextCheck, ContinuityState, DecisionSource,
    RoutingDecision, Tier, TypedRouteDecision,
};
use crate::js_json::{JsDocument, JsNode, JsString, JsonDigest as TurnDigest, JsonWrite, NodeId};
use crate::model_catalog::{
    can_upgrade_context, has_native_million_context, supports_tool_references,
};
use crate::prompt_state::goal_feedback_indexes_document;
use crate::turn_state::{ContinuationEvidence, Pin, Selection, ToolOwner, TurnState};
use serde_json::{Value, json};
#[cfg(test)]
use sha2::{Digest, Sha256};
use std::collections::HashMap;

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
pub fn rank(model: &str) -> i32 {
    let lower = model.to_ascii_lowercase();
    if lower.contains("haiku") {
        0
    } else if lower.contains("sonnet") {
        1
    } else if lower.contains("opus") {
        2
    } else {
        -1
    }
}
#[cfg(test)]
fn property(doc: &JsDocument, node: NodeId, key: &str) -> Option<NodeId> {
    doc.get(node, key)
}
fn array(doc: &JsDocument, node: Option<NodeId>) -> Option<&[NodeId]> {
    match node.and_then(|n| doc.node(n)) {
        Some(JsNode::Array(items)) => Some(items),
        _ => None,
    }
}
fn needs_sonnet_document(doc: &JsDocument) -> bool {
    let root = doc.root();
    doc.string_is(doc.get_path(root, &["thinking", "type"]), "adaptive")
        || doc.is_truthy(doc.get_path(root, &["output_config", "effort"]))
        || doc
            .get(root, "max_tokens")
            .and_then(|node| doc.node(node))
            .is_some_and(|value| matches!(value, JsNode::Number(number) if *number > 64000.0))
}
fn is_key(key: &JsString, value: &str) -> bool {
    key.units().iter().copied().eq(value.encode_utf16())
}

enum TurnWrite<'a> {
    Content(Option<NodeId>),
    Block(NodeId, bool),
    Raw(NodeId),
    Key(&'a JsString),
    Index(usize),
    Punctuation(char),
}
fn write_turn_part<'a, W: JsonWrite + ?Sized>(
    doc: &'a JsDocument,
    first: TurnWrite<'a>,
    output: &mut W,
) {
    let mut stack = vec![first];
    while let Some(part) = stack.pop() {
        match part {
            TurnWrite::Content(content) => {
                if let Some(blocks) = array(doc, content) {
                    output.push('[');
                    stack.push(TurnWrite::Punctuation(']'));
                    for (index, &block) in blocks.iter().enumerate().rev() {
                        stack.push(TurnWrite::Block(block, true));
                        if index != 0 {
                            stack.push(TurnWrite::Punctuation(','));
                        }
                    }
                } else if let Some(node) = content {
                    doc.write_node(node, output);
                } else {
                    output.push_str("null");
                }
            }
            TurnWrite::Block(node, nested) => match doc.node(node) {
                Some(JsNode::Object(object)) => {
                    let recurse = nested
                        && doc.string_is(doc.get(node, "type"), "tool_result")
                        && array(doc, doc.get(node, "content")).is_some();
                    output.push('{');
                    stack.push(TurnWrite::Punctuation('}'));
                    let mut count = 0;
                    for (key, child) in object.ordered_entries().rev() {
                        if is_key(key, "cache_control") {
                            continue;
                        }
                        if count != 0 {
                            stack.push(TurnWrite::Punctuation(','));
                        }
                        stack.push(if recurse && is_key(key, "content") {
                            TurnWrite::Content(Some(*child))
                        } else {
                            TurnWrite::Raw(*child)
                        });
                        stack.push(TurnWrite::Punctuation(':'));
                        stack.push(TurnWrite::Key(key));
                        count += 1;
                    }
                }
                // Object spread of an array has enumerable decimal keys. Its
                // elements are opaque values, not recursively normalized blocks.
                Some(JsNode::Array(values)) => {
                    output.push('{');
                    stack.push(TurnWrite::Punctuation('}'));
                    for (index, &child) in values.iter().enumerate().rev() {
                        stack.push(TurnWrite::Raw(child));
                        stack.push(TurnWrite::Punctuation(':'));
                        stack.push(TurnWrite::Index(index));
                        if index != 0 {
                            stack.push(TurnWrite::Punctuation(','));
                        }
                    }
                }
                _ => doc.write_node(node, output),
            },
            TurnWrite::Raw(node) => doc.write_node(node, output),
            TurnWrite::Key(key) => key.write_json(output),
            TurnWrite::Index(index) => {
                output.push('"');
                output.push_str(&index.to_string());
                output.push('"');
            }
            TurnWrite::Punctuation(value) => output.push(value),
        }
    }
}
fn write_turn_preimage<W: JsonWrite + ?Sized>(
    doc: &JsDocument,
    scope: &str,
    messages: &[NodeId],
    index: Option<usize>,
    output: &mut W,
) {
    let root = doc.root();
    output.push('[');
    JsString::from_scalar(scope).write_json(output);
    output.push(',');
    write_turn_part(doc, TurnWrite::Content(doc.get(root, "system")), output);
    output.push(',');
    if let Some(tools) = array(doc, doc.get(root, "tools")) {
        output.push('[');
        for (position, &tool) in tools.iter().enumerate() {
            if position != 0 {
                output.push(',');
            }
            write_turn_part(doc, TurnWrite::Block(tool, false), output);
        }
        output.push(']');
    } else {
        output.push_str("null");
    }
    output.push_str(",[");
    let mut first = true;
    for &message in &messages[..index.map_or(0, |value| value + 1)] {
        let Some(JsNode::Object(object)) = doc.node(message) else {
            continue;
        };
        if !first {
            output.push(',');
        }
        first = false;
        output.push('{');
        for (position, (key, child)) in object.ordered_entries().enumerate() {
            if position != 0 {
                output.push(',');
            }
            key.write_json(output);
            output.push(':');
            if is_key(key, "content") {
                write_turn_part(doc, TurnWrite::Content(Some(*child)), output);
            } else {
                doc.write_node(*child, output);
            }
        }
        output.push('}');
    }
    output.push_str("]]");
}
fn write_prompt_preimage<W: JsonWrite + ?Sized>(scope: &str, prompt_id: &str, output: &mut W) {
    output.push_str("[\"prompt\",");
    JsString::from_scalar(scope).write_json(output);
    output.push(',');
    JsString::from_scalar(prompt_id).write_json(output);
    output.push(']');
}

#[derive(Clone, Debug)]
pub struct TurnInfo {
    pub index: Option<usize>,
    pub key: String,
    pub content_key: String,
    pub goal_feedback: bool,
    pub continuation: bool,
}
pub fn turn_info(doc: &JsDocument, scope: &str, prompt_id: &str) -> TurnInfo {
    turn_info_prefix(doc, scope, prompt_id, None)
}
fn turn_info_prefix(
    doc: &JsDocument,
    scope: &str,
    prompt_id: &str,
    end: Option<usize>,
) -> TurnInfo {
    let root = doc.root();
    let all_messages = array(doc, doc.get(root, "messages")).unwrap_or_default();
    let messages = &all_messages[..end.unwrap_or(all_messages.len()).min(all_messages.len())];
    // Recognition only reads earlier messages; later feedback cannot affect a
    // prefix's recognition, so one full scan also serves prior-turn recovery.
    let feedback = goal_feedback_indexes_document(doc, doc.get(root, "messages"));
    let index = messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, &message)| {
            (doc.string_is(doc.get(message, "role"), "user")
                && !feedback.contains(&index)
                && !array(doc, doc.get(message, "content")).is_some_and(|blocks| {
                    blocks
                        .iter()
                        .any(|b| doc.string_is(doc.get(*b, "type"), "tool_result"))
                }))
            .then_some(index)
        });
    // Preserve content aliases even when an explicit prompt identity exists.
    let mut content = TurnDigest::new();
    write_turn_preimage(doc, scope, messages, index, &mut content);
    let content_key = content.finish();
    let key = if prompt_id.is_empty() {
        content_key.clone()
    } else {
        let mut prompt = TurnDigest::new();
        write_prompt_preimage(scope, prompt_id, &mut prompt);
        prompt.finish()
    };
    TurnInfo {
        index,
        key,
        content_key,
        goal_feedback: !messages.is_empty() && feedback.contains(&(messages.len() - 1)),
        continuation: index.is_none()
            || messages[index.map_or(0, |i| i + 1)..]
                .iter()
                .any(|n| !doc.string_is(doc.get(*n, "role"), "system")),
    }
}

const CUSTOM_TOOL_FIELDS: &[&str] = &[
    "name",
    "description",
    "input_schema",
    "type",
    "defer_loading",
    "strict",
    "input_examples",
    "allowed_callers",
    "eager_input_streaming",
];
fn known_deferred_tool(doc: &JsDocument, tool: NodeId) -> bool {
    let Some(JsNode::Object(object)) = doc.node(tool) else {
        return false;
    };
    if !matches!(
        doc.get(tool, "defer_loading").and_then(|n| doc.node(n)),
        Some(JsNode::Bool(true))
    ) || doc
        .get(tool, "type")
        .is_some_and(|n| !doc.string_is(Some(n), "custom"))
        || !doc
            .get(tool, "name")
            .and_then(|n| doc.string(n))
            .is_some_and(|s| !s.units().is_empty())
        || !doc.get(tool, "input_schema").is_some_and(|n| {
            matches!(doc.node(n), Some(JsNode::Object(_)))
                && doc.string_is(doc.get(n, "type"), "object")
        })
    {
        return false;
    }
    for field in [
        "description",
        "strict",
        "eager_input_streaming",
        "input_examples",
        "allowed_callers",
    ] {
        if let Some(node) = doc.get(tool, field) {
            let valid = match field {
                "description" => matches!(doc.node(node), Some(JsNode::String(_))),
                "strict" | "eager_input_streaming" => {
                    matches!(doc.node(node), Some(JsNode::Bool(_)))
                }
                "input_examples" => matches!(doc.node(node), Some(JsNode::Array(_))),
                "allowed_callers" => array(doc, Some(node)).is_some_and(|items| {
                    items
                        .iter()
                        .all(|n| matches!(doc.node(*n), Some(JsNode::String(_))))
                }),
                _ => false,
            };
            if !valid {
                return false;
            }
        }
    }
    object.entries().iter().all(|(key, _)| {
        CUSTOM_TOOL_FIELDS
            .iter()
            .any(|allowed| key.units().iter().copied().eq(allowed.encode_utf16()))
    })
}
/// JavaScript Number byte charge, independent of the host pointer width.
/// A non-safe Number is not an exact integer byte metric.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContextByteCharge(f64);
impl ContextByteCharge {
    pub fn as_source_number(self) -> f64 {
        self.0
    }
    pub fn exact_safe_bytes(self) -> Option<u64> {
        (self.0.is_finite()
            && self.0 >= 0.0
            && self.0 <= 9_007_199_254_740_991.0
            && self.0.fract() == 0.0)
            .then_some(self.0 as u64)
    }
    pub fn exceeds(self, threshold: u64) -> bool {
        self.0 > threshold as f64
    }
    /// Preserve the Source JSON number shape, including -0 and nonfinite null.
    pub fn to_json_value(self) -> Value {
        if !self.0.is_finite() {
            return Value::Null;
        }
        let mut buffer = ryu_js::Buffer::new();
        let text = if self.0 == 0.0 {
            "0"
        } else {
            buffer.format_finite(self.0)
        };
        Value::Number(serde_json::from_str(text).expect("finite JavaScript JSON number"))
    }
}

/// The complete serialized length could not be represented. No byte count is
/// available; callers must not substitute a maximum integer as telemetry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextByteChargeUnavailable;
impl ContextByteChargeUnavailable {
    pub fn code(self) -> &'static str {
        "serialized_length_unavailable"
    }
}
impl std::fmt::Display for ContextByteChargeUnavailable {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str("Context byte charge unavailable")
    }
}
impl std::error::Error for ContextByteChargeUnavailable {}

fn increment_context_reference(count: &mut f64) {
    *count += 1.0;
}
fn repeated_context_bytes(repeated: f64, occurrences: f64, definition_bytes: usize) -> f64 {
    repeated + (occurrences - 1.0) * definition_bytes as f64
}
fn visible_context_charge(
    full: usize,
    original_tools: usize,
    visible_tools: usize,
    repeated: f64,
) -> Result<ContextByteCharge, ContextByteChargeUnavailable> {
    let visible = full
        .checked_sub(original_tools)
        .and_then(|rest| rest.checked_add(visible_tools))
        .ok_or(ContextByteChargeUnavailable)?;
    // Match Source's final visible JSON bytes + repeatedBytes operation.
    Ok(ContextByteCharge(visible as f64 + repeated))
}
fn requires_context_capacity_check(
    charge: Result<ContextByteCharge, ContextByteChargeUnavailable>,
) -> bool {
    charge.map_or(true, |charge| charge.exceeds(150000))
}

/// Replaces the unchecked native usize result with a checked Source Number.
/// Does not mutate the authoritative request or impose another request limit.
pub fn context_size_bytes(
    doc: &JsDocument,
    model: &str,
) -> Result<ContextByteCharge, ContextByteChargeUnavailable> {
    context_size_bytes_up_to(doc, model, usize::MAX)
}
fn context_size_bytes_up_to(
    doc: &JsDocument,
    model: &str,
    serialized_limit: usize,
) -> Result<ContextByteCharge, ContextByteChargeUnavailable> {
    // Count the exact JSON byte charge without materializing the full body.
    // Unavailable length cannot establish a small context, even with deferred schemas.
    let Some(full) = doc.serialized_len_up_to(serialized_limit) else {
        return Err(ContextByteChargeUnavailable);
    };
    let tools_node = doc.get(doc.root(), "tools");
    let Some(tools) = array(doc, tools_node) else {
        return Ok(ContextByteCharge(full as f64));
    };
    if !supports_tool_references(model)
        || !tools.iter().any(|n| known_deferred_tool(doc, *n))
        || !tools.iter().any(|n| {
            matches!(doc.node(*n), Some(JsNode::Object(_)))
                && !matches!(
                    doc.get(*n, "defer_loading").and_then(|n| doc.node(n)),
                    Some(JsNode::Bool(true))
                )
        })
    {
        return Ok(ContextByteCharge(full as f64));
    }
    #[allow(
        clippy::mutable_key_type,
        reason = "JsString only caches immutable code units"
    )]
    let mut references: HashMap<JsString, f64> = HashMap::new();
    let mut pending = array(doc, doc.get(doc.root(), "messages"))
        .unwrap_or_default()
        .to_vec();
    while let Some(node) = pending.pop() {
        match doc.node(node) {
            Some(JsNode::Array(items)) => pending.extend_from_slice(items),
            Some(JsNode::Object(object)) => {
                let kind = doc.get(node, "type");
                if doc.string_is(kind, "tool_reference") || doc.string_is(kind, "tool_use") {
                    let field = if doc.string_is(kind, "tool_reference") {
                        "tool_name"
                    } else {
                        "name"
                    };
                    let Some(name) = doc
                        .get(node, field)
                        .and_then(|n| doc.string(n))
                        .filter(|s| !s.units().is_empty())
                    else {
                        return Ok(ContextByteCharge(full as f64));
                    };
                    increment_context_reference(references.entry(name.clone()).or_default());
                }
                pending.extend(object.entries().iter().map(|(_, n)| *n));
            }
            _ => {}
        }
    }
    let mut repeated = 0.0;
    let mut visible = Vec::new();
    for &tool in tools {
        if known_deferred_tool(doc, tool) {
            let name = doc
                .get(tool, "name")
                .and_then(|n| doc.string(n))
                .expect("validated name");
            let occurrences = references.get(name).copied().unwrap_or_default();
            if occurrences > 1.0 {
                repeated =
                    repeated_context_bytes(repeated, occurrences, doc.stringify_node(tool).len());
            }
            if occurrences == 0.0 {
                continue;
            }
        }
        visible.push(doc.stringify_node(tool));
    }
    // Only the existing tools value changes. Keys and member punctuation keep
    // their byte charge, independently of property order or duplicate inputs.
    let original_tools = doc
        .stringify_node(tools_node.expect("checked tools array"))
        .len();
    visible_context_charge(
        full,
        original_tools,
        format!("[{}]", visible.join(",")).len(),
        repeated,
    )
}
fn has_content_block(doc: &JsDocument, types: &[&str]) -> bool {
    let mut pending: Vec<_> = array(doc, doc.get(doc.root(), "messages"))
        .unwrap_or_default()
        .iter()
        .filter_map(|n| doc.get(*n, "content"))
        .collect();
    while let Some(content) = pending.pop() {
        let Some(blocks) = array(doc, Some(content)) else {
            continue;
        };
        for &block in blocks {
            if types
                .iter()
                .any(|kind| doc.string_is(doc.get(block, "type"), kind))
            {
                return true;
            }
            if doc.string_is(doc.get(block, "type"), "tool_result")
                && let Some(child) = doc.get(block, "content")
            {
                pending.push(child);
            }
        }
    }
    false
}

#[derive(Clone, Default)]
pub struct RouteOptions {
    pub scope: String,
    pub request_class: String,
    pub prompt_id: String,
    pub request_id: Option<String>,
    pub count_tokens: bool,
}
pub struct RouteStart {
    /// Legacy observation adapter; first-party routing uses passthrough_contract.
    pub passthrough: Option<Value>,
    pub passthrough_contract: Option<RoutingDecision>,
    pub early_count_model: Option<String>,
    options: RouteOptions,
    sequence: u64,
    auto_mode: bool,
    has_system_message: bool,
    unknown_model: bool,
    model_specific_thinking: bool,
    model_specific_features: bool,
    thinking_history: bool,
    capacity_locked: bool,
    has_attachments: bool,
}
pub struct RoutePending {
    pub count_model: Option<String>,
    start: RouteStart,
    metadata: RoutingDecision,
    /// Preserves arbitrary custom metadata only for the explicit Value adapter.
    legacy_decision: Option<Value>,
    model: String,
    model_exact: JsString,
    reason: String,
    previous: Option<JsString>,
    preserved: bool,
    large_context: bool,
    shared_auto_request: bool,
    turn: TurnInfo,
    turn_pin: Option<Pin>,
    owner_key: Option<String>,
    ambiguous_continuity: bool,
}
impl RoutePending {
    fn preserve(&mut self, model: &str, reason: &str) {
        self.model = model.to_owned();
        self.model_exact = model.into();
        self.reason = reason.to_owned();
        self.preserved = true;
    }
}
/// Decision metadata is display-only; model retains the exact wire identity.
pub struct RouteDecision {
    pub decision: Value,
    pub model: JsString,
}
fn requested_identity(doc: &JsDocument) -> JsString {
    doc.get(doc.root(), "model")
        .and_then(|node| doc.string(node))
        .cloned()
        .unwrap_or_else(|| "".into())
}
pub struct Router {
    pub config: RouterConfig,
    pub turns: TurnState,
    sequence: u64,
}
impl Router {
    pub fn new(config: RouterConfig) -> Self {
        let turns = TurnState::new(config.turn_entries.unwrap_or(1000), config.turn_ttl_ms);
        Self {
            config,
            turns,
            sequence: 0,
        }
    }
    pub fn complete(&mut self, request_id: &str, evidence: &Value, now_ms: u64) -> bool {
        self.turns.complete(request_id, Some(evidence), now_ms)
    }
    pub fn complete_exact(
        &mut self,
        request_id: &str,
        evidence: Option<&ContinuationEvidence>,
        now_ms: u64,
    ) -> bool {
        self.turns.complete_exact(request_id, evidence, now_ms)
    }
    fn tier_model(&self, tier: &str) -> &str {
        match tier {
            "haiku" => &self.config.models.haiku,
            "opus" => &self.config.models.opus,
            _ => &self.config.models.sonnet,
        }
    }
    fn tier_model_exact(&self, tier: &str) -> JsString {
        let tier = if matches!(tier, "haiku" | "opus") {
            tier
        } else {
            "sonnet"
        };
        self.config
            .exact_models
            .get(tier)
            .cloned()
            .unwrap_or_else(|| self.tier_model(tier).into())
    }
    fn tier_rank_exact(&self, model: &JsString) -> i32 {
        ["haiku", "sonnet", "opus"]
            .iter()
            .position(|tier| self.tier_model_exact(tier) == *model)
            .map_or_else(|| rank(&model.to_well_formed()), |index| index as i32)
    }
    fn select(
        &mut self,
        keys: &[String],
        model: &JsString,
        requested: &JsString,
        start: &RouteStart,
        now_ms: u64,
    ) -> bool {
        self.turns.select_exact(
            keys,
            Pin::new(model.clone(), requested.clone()),
            Selection {
                scope: &start.options.scope,
                request_id: start.options.request_id.as_deref(),
                sequence: start.sequence,
            },
            now_ms,
        )
    }
    pub fn begin_route(
        &mut self,
        doc: &JsDocument,
        options: RouteOptions,
        now_ms: u64,
    ) -> RouteStart {
        self.sequence += 1;
        let requested_display = requested_identity(doc).to_well_formed();
        let requested = requested_display.as_str();
        let auto_mode = self.config.client_profile == ClientProfile::Auto
            || has_routable_safeguards_document(doc);
        let mut start = RouteStart {
            passthrough: None,
            passthrough_contract: None,
            early_count_model: None,
            options,
            sequence: self.sequence,
            auto_mode,
            has_system_message: false,
            unknown_model: false,
            model_specific_thinking: false,
            model_specific_features: false,
            thinking_history: false,
            capacity_locked: false,
            has_attachments: false,
        };
        if start.options.request_class == "auxiliary"
            || (doc.get(doc.root(), "safeguards").is_some()
                && (!has_routable_safeguards_document(doc)
                    || start.options.request_class == "compaction"))
        {
            let mut decision = RoutingDecision::passthrough(
                requested.into(),
                if start.options.request_class == "auxiliary" {
                    "internal_request"
                } else {
                    "auto_mode_safeguards"
                },
            );
            if !["auxiliary", "compaction"].contains(&start.options.request_class.as_str()) {
                let turn = turn_info(doc, &start.options.scope, &start.options.prompt_id);
                if (turn.index.is_some() || !start.options.prompt_id.is_empty())
                    && !self.select(
                        &[turn.key, turn.content_key],
                        &requested_identity(doc),
                        &requested_identity(doc),
                        &start,
                        now_ms,
                    )
                {
                    decision.continuity_state = Some(ContinuityState::CapacityExhausted);
                }
            }
            let mut legacy = decision.policy_observation();
            legacy["evaluation_latency_ms"] = json!(0);
            start.passthrough = Some(legacy);
            start.passthrough_contract = Some(decision);
            return start;
        }
        start.has_system_message =
            array(doc, doc.get(doc.root(), "messages")).is_some_and(|messages| {
                messages
                    .iter()
                    .any(|&message| doc.string_is(doc.get(message, "role"), "system"))
            });
        start.unknown_model = rank(requested) < 0
            && !["haiku", "sonnet", "opus"]
                .iter()
                .any(|tier| self.tier_model_exact(tier) == requested_identity(doc));
        start.model_specific_thinking = doc.is_truthy(doc.get(doc.root(), "thinking"))
            && !["disabled", "adaptive"]
                .iter()
                .any(|kind| doc.string_is(doc.get_path(doc.root(), &["thinking", "type"]), kind));
        start.model_specific_features = start.model_specific_thinking
            || ["context_management", "speed", "container", "mcp_servers"]
                .iter()
                .any(|key| doc.is_truthy(doc.get(doc.root(), key)))
            || array(doc, doc.get(doc.root(), "tools")).is_some_and(|tools| {
                tools.iter().any(|&tool| {
                    doc.is_truthy(doc.get(tool, "type"))
                        && !doc.string_is(doc.get(tool, "type"), "custom")
                })
            });
        start.thinking_history = has_content_block(doc, &["thinking", "redacted_thinking"]);
        start.capacity_locked = start.has_system_message
            || start.unknown_model
            || !(has_native_million_context(requested) || can_upgrade_context(requested))
            || start.model_specific_features
            || start.thinking_history;
        start.has_attachments = has_content_block(doc, &["image", "document"]);
        if !auto_mode
            && !start.capacity_locked
            && start.options.count_tokens
            && can_upgrade_context(&self.config.models.haiku)
            && (requires_context_capacity_check(context_size_bytes(doc, &self.config.models.haiku))
                || start.has_attachments)
        {
            start.early_count_model = Some(self.config.models.haiku.clone());
        }
        start
    }

    /// Legacy fixture/custom adapter. Unknown fields and their original JSON
    /// values remain observable; production classifiers use classified_typed.
    pub fn classified(
        &mut self,
        doc: &JsDocument,
        start: RouteStart,
        decision: Value,
        now_ms: u64,
    ) -> RoutePending {
        let mut metadata = RoutingDecision::passthrough(String::new(), text(&decision, "reason"));
        metadata.tier = Some(Tier::from_value(decision.get("tier")).unwrap_or(Tier::Sonnet));
        metadata.source =
            DecisionSource::parse(text(&decision, "source")).unwrap_or(DecisionSource::Passthrough);
        // These are observation-only in the legacy facade. The raw original is
        // retained separately rather than flattened into first-party contracts.
        metadata.classified_tier = Tier::from_value(decision.get("classified_tier"));
        metadata.confidence = decision.get("confidence").and_then(Value::as_f64);
        metadata.evaluator = match text(&decision, "evaluator") {
            "jev" => Some(Evaluator::Jev),
            "ollama" => Some(Evaluator::Ollama),
            _ => None,
        };
        metadata.classifier_error = ClassifierError::parse(text(&decision, "classifier_error"));
        metadata.classifier_status = decision
            .get("classifier_status")
            .and_then(Value::as_u64)
            .and_then(|n| u16::try_from(n).ok());
        self.classified_metadata(doc, start, metadata, Some(decision), now_ms)
    }
    pub fn classified_typed(
        &mut self,
        doc: &JsDocument,
        start: RouteStart,
        decision: ClassifierDecision,
        now_ms: u64,
    ) -> RoutePending {
        self.classified_metadata(
            doc,
            start,
            RoutingDecision::classified(&decision),
            None,
            now_ms,
        )
    }
    fn classified_metadata(
        &mut self,
        doc: &JsDocument,
        start: RouteStart,
        metadata: RoutingDecision,
        legacy_decision: Option<Value>,
        now_ms: u64,
    ) -> RoutePending {
        let requested_exact = requested_identity(doc);
        let requested_display = requested_exact.to_well_formed();
        let requested = requested_display.as_str();
        let mut model_exact = self.tier_model_exact(metadata.tier.unwrap_or(Tier::Sonnet).as_str());
        let mut model = model_exact.to_well_formed();
        let mut reason = metadata.reason.clone();
        if start.auto_mode && metadata.tier == Some(Tier::Haiku) {
            model_exact = self.tier_model_exact("sonnet");
            model = model_exact.to_well_formed();
            reason = "auto_mode_floor".into();
        }
        let turn = turn_info(doc, &start.options.scope, &start.options.prompt_id);
        let prompt_pin = (!start.options.prompt_id.is_empty())
            .then(|| self.turns.get_exact(&turn.key, now_ms).cloned())
            .flatten();
        let tool_ids: Vec<JsString> = array(doc, doc.get(doc.root(), "messages"))
            .and_then(|messages| {
                messages
                    .iter()
                    .rev()
                    .find(|node| doc.string_is(doc.get(**node, "role"), "user"))
            })
            .and_then(|message| array(doc, doc.get(*message, "content")))
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|block| doc.string_is(doc.get(**block, "type"), "tool_result"))
                    .filter_map(|block| {
                        doc.get(*block, "tool_use_id")
                            .and_then(|id| doc.string(id))
                            .cloned()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let owner = turn
            .continuation
            .then(|| self.turns.tool_owner_exact(&start.options.scope, &tool_ids))
            .flatten();
        let owner_ambiguous = matches!(owner, Some(ToolOwner::Ambiguous));
        let owner_pin = match &owner {
            Some(ToolOwner::Owned { pin, .. }) => Some(pin.clone()),
            _ => None,
        };
        let ambiguous_continuity = owner_ambiguous
            || (owner_pin.is_none()
                && prompt_pin.is_none()
                && self.turns.ambiguous(&turn.content_key));
        let turn_pin = if owner_ambiguous {
            None
        } else {
            owner_pin
                .or_else(|| prompt_pin.clone())
                .or_else(|| self.turns.get_exact(&turn.content_key, now_ms).cloned())
        };
        let mut previous = turn_pin.as_ref().map(|pin| pin.model.clone());
        let text_turn = !turn.continuation || turn.goal_feedback;
        let text_pin = prompt_pin.as_ref().or_else(|| {
            (start.options.prompt_id.is_empty() && turn.goal_feedback)
                .then_some(turn_pin.as_ref())
                .flatten()
        });
        let pinned_target = if turn.continuation
            || (text_turn && text_pin.is_some_and(|p| p.requested_model == requested_exact))
        {
            previous.as_ref().map(JsString::to_well_formed)
        } else {
            None
        };
        let shared_auto_request = start.auto_mode
            && can_route_auto_request_document(doc, pinned_target.as_deref().unwrap_or(&model));
        if !turn.continuation
            && array(doc, doc.get(doc.root(), "messages")).is_some_and(|m| m.len() > 1)
            && previous.is_none()
        {
            let earlier = turn_info_prefix(doc, &start.options.scope, "", turn.index);
            previous = self
                .turns
                .get_exact(&earlier.key, now_ms)
                .map(|pin| pin.model.clone());
        }
        let mut preserved = false;
        let mut chosen: Option<(JsString, &str)> = None;
        if start.options.request_class == "compaction" {
            chosen = Some((requested_exact.clone(), "internal_request"));
        } else if start.has_system_message && !shared_auto_request {
            chosen = Some((requested_exact.clone(), "mid_conversation_system"));
        } else if start.unknown_model {
            chosen = Some((requested_exact.clone(), "unknown_model"));
        } else if start.model_specific_thinking
            && !doc.string_is(doc.get_path(doc.root(), &["thinking", "type"]), "enabled")
            && text_turn
            && !shared_auto_request
        {
            chosen = Some((requested_exact.clone(), "model_specific_features"));
        } else if text_turn
            && let Some(pin) = text_pin
            && pin.requested_model == requested_exact
            && (!start.model_specific_features
                || (start.auto_mode
                    && can_route_auto_request_document(doc, &pin.model.to_well_formed())))
        {
            if needs_sonnet_document(doc)
                && (pin.model == self.tier_model_exact("haiku")
                    || rank(&pin.model.to_well_formed()) == 0)
            {
                chosen = Some((
                    self.tier_model_exact("sonnet"),
                    "requires_sonnet_capabilities",
                ));
            } else {
                chosen = Some((
                    pin.model.clone(),
                    if prompt_pin.is_some() {
                        "prompt_turn_pinned"
                    } else {
                        "goal_turn_pinned"
                    },
                ));
            }
        } else if turn.goal_feedback && turn_pin.is_none() {
            chosen = Some((requested_exact.clone(), "unknown_continuation"));
        } else if turn.continuation && !turn.goal_feedback {
            chosen = Some((
                previous.clone().unwrap_or_else(|| requested_exact.clone()),
                if previous.is_some() {
                    "tool_turn_pinned"
                } else {
                    "unknown_continuation"
                },
            ));
        } else if metadata.source == DecisionSource::Fallback && rank(requested) >= 1 {
            chosen = Some((
                previous.clone().unwrap_or_else(|| requested_exact.clone()),
                "classifier_unavailable",
            ));
        } else if start.model_specific_features && !shared_auto_request {
            chosen = Some((
                previous.clone().unwrap_or_else(|| requested_exact.clone()),
                "model_specific_features",
            ));
        } else if start.thinking_history && !shared_auto_request {
            chosen = Some((
                previous.clone().unwrap_or_else(|| requested_exact.clone()),
                "thinking_history",
            ));
        } else if needs_sonnet_document(doc) && metadata.tier == Some(Tier::Haiku) {
            model_exact = self.tier_model_exact("sonnet");
            model = model_exact.to_well_formed();
            if !start.auto_mode {
                reason = "requires_sonnet_capabilities".into();
            }
        }
        if let Some((selected, why)) = chosen {
            model = selected.to_well_formed();
            model_exact = selected;
            reason = why.to_owned();
            preserved = true;
        }
        let large_context = requires_context_capacity_check(context_size_bytes(doc, &model))
            || start.has_attachments;
        let count_model = (large_context && !start.capacity_locked && can_upgrade_context(&model))
            .then(|| model.clone());
        RoutePending {
            count_model,
            start,
            metadata,
            legacy_decision,
            model,
            model_exact,
            reason,
            previous,
            preserved,
            large_context,
            shared_auto_request,
            turn,
            turn_pin,
            owner_key: match owner {
                Some(ToolOwner::Owned { key, .. }) => Some(key),
                _ => None,
            },
            ambiguous_continuity,
        }
    }

    pub fn finish_route(
        &mut self,
        doc: &JsDocument,
        pending: RoutePending,
        count: Option<u64>,
        now_ms: u64,
    ) -> Value {
        self.finish_route_exact(doc, pending, count, now_ms)
            .decision
    }
    pub fn finish_route_exact(
        &mut self,
        doc: &JsDocument,
        pending: RoutePending,
        count: Option<u64>,
        now_ms: u64,
    ) -> RouteDecision {
        let (typed, legacy) = self.finish_policy(doc, pending, count, now_ms);
        RouteDecision {
            decision: legacy.unwrap_or_else(|| typed.metadata.policy_observation()),
            model: typed.model,
        }
    }
    pub fn finish_route_typed_exact(
        &mut self,
        doc: &JsDocument,
        pending: RoutePending,
        count: Option<u64>,
        now_ms: u64,
    ) -> TypedRouteDecision {
        self.finish_policy(doc, pending, count, now_ms).0
    }
    fn finish_policy(
        &mut self,
        doc: &JsDocument,
        mut pending: RoutePending,
        count: Option<u64>,
        now_ms: u64,
    ) -> (TypedRouteDecision, Option<Value>) {
        let requested_exact = requested_identity(doc);
        let requested_display = requested_exact.to_well_formed();
        let requested = requested_display.as_str();
        if pending.count_model.is_some() {
            if let Some(count) = count.filter(|n| *n <= crate::config::MAX_SAFE_INTEGER) {
                pending.large_context = count > 190000;
                pending.metadata.context_check = Some(if pending.large_context {
                    ContextCheck::OverBudget
                } else {
                    ContextCheck::WithinBudget
                });
                pending.metadata.counted_input_tokens = Some(count);
                if let Some(legacy) = &mut pending.legacy_decision {
                    legacy["context_check"] = json!(pending.metadata.context_check);
                    legacy["counted_input_tokens"] = json!(count);
                }
            } else {
                pending.metadata.context_check = Some(ContextCheck::CountUnavailable);
                if let Some(legacy) = &mut pending.legacy_decision {
                    legacy["context_check"] = json!(ContextCheck::CountUnavailable);
                }
            }
        }
        if !pending.preserved && pending.large_context && !pending.shared_auto_request {
            let baseline = pending
                .previous
                .clone()
                .unwrap_or_else(|| requested_exact.clone());
            if self.tier_rank_exact(&pending.model_exact) <= self.tier_rank_exact(&baseline) {
                pending.preserve(&baseline.to_well_formed(), "large_or_multimodal_request");
                pending.model_exact = baseline;
            }
        }
        let mut capacity_upgraded = false;
        if pending.large_context
            && !pending.start.capacity_locked
            && can_upgrade_context(&pending.model)
            && let Some(capable) = [&self.config.models.sonnet, &self.config.models.opus]
                .into_iter()
                .find(|candidate| {
                    has_native_million_context(candidate)
                        && self.tier_rank_exact(&JsString::from_scalar(candidate))
                            >= self.tier_rank_exact(&pending.model_exact)
                })
            && capable != &pending.model
        {
            pending.preserve(capable, "context_capacity");
            capacity_upgraded = true;
        }
        let compatibility = crate::auto_routing::target_compatibility_document_exact(
            doc,
            &pending.model_exact,
            pending.start.auto_mode,
        );
        if compatibility["compatible"] != true {
            pending.preserve(
                requested,
                if pending.start.auto_mode {
                    "auto_mode_incompatible"
                } else {
                    "model_incompatible"
                },
            );
            pending.model_exact = requested_exact.clone();
        }
        let identifiable_upgrade = capacity_upgraded
            && (pending.turn.index.is_some() || !pending.start.options.prompt_id.is_empty());
        let mut continuity = pending
            .turn_pin
            .as_ref()
            .map(|pin| {
                if pin.is_confirmed() {
                    ContinuityState::Confirmed
                } else {
                    ContinuityState::Selected
                }
            })
            .or_else(|| {
                pending
                    .turn
                    .continuation
                    .then_some(ContinuityState::Unknown)
            });
        if (!pending.turn.continuation
            || pending.previous.is_some()
            || identifiable_upgrade
            || pending.reason == "mid_conversation_system"
            || (pending
                .start
                .options
                .request_id
                .as_ref()
                .is_some_and(|s| !s.is_empty())
                && (pending.turn.index.is_some() || !pending.start.options.prompt_id.is_empty())))
            && pending.start.options.request_class != "compaction"
            && !pending.ambiguous_continuity
            && !self.select(
                &[
                    pending
                        .owner_key
                        .clone()
                        .unwrap_or_else(|| pending.turn.key.clone()),
                    pending.turn.content_key.clone(),
                ],
                &pending.model_exact,
                &requested_exact,
                &pending.start,
                now_ms,
            )
        {
            continuity = Some(ContinuityState::CapacityExhausted);
        }
        pending.metadata.model = pending.model;
        pending.metadata.reason = pending.reason;
        if compatibility["compatible"] != true {
            pending.metadata.compatibility_reason =
                compatibility["reason"].as_str().map(str::to_owned);
        }
        pending.metadata.continuity_state = continuity;
        if let Some(legacy) = &mut pending.legacy_decision {
            legacy["model"] = json!(pending.metadata.model);
            legacy["reason"] = json!(pending.metadata.reason);
            if compatibility["compatible"] != true {
                legacy["compatibility_reason"] = compatibility["reason"].clone();
            }
            if let Some(continuity) = continuity {
                legacy["continuity_state"] = json!(continuity);
            }
        }
        (
            TypedRouteDecision {
                metadata: pending.metadata,
                model: pending.model_exact,
            },
            pending.legacy_decision,
        )
    }
}
/// Synthetic deterministic scenarios used by the retained source oracle.
/// Classification and token counts are supplied, so this never performs I/O.
pub fn run_fixture(input: &Value) -> Result<Value, String> {
    let config = crate::config::read_config(
        input.get("env").unwrap_or(&json!({})),
        false,
        std::path::Path::new("/tmp"),
    )?;
    let mut router = Router::new(config);
    if let Some(limit) = input["turn_entries"].as_u64() {
        router.turns = TurnState::new(limit as usize, router.config.turn_ttl_ms);
    }
    let steps = input["steps"]
        .as_array()
        .ok_or("Invalid router fixture input")?;
    let mut now_ms = 0;
    let mut results = Vec::new();
    for step in steps {
        match text(step, "op") {
            "advance" => now_ms += step["ms"].as_u64().unwrap_or_default(),
            "complete" => results.push(json!(router.complete(
                text(step, "request_id"),
                &step["evidence"],
                now_ms
            ))),
            "route" => {
                let bytes = if let Some(bytes) = step["bytes"].as_array() {
                    bytes
                        .iter()
                        .map(|v| {
                            v.as_u64()
                                .and_then(|v| u8::try_from(v).ok())
                                .ok_or("Invalid router fixture bytes")
                        })
                        .collect::<Result<Vec<_>, _>>()?
                } else {
                    step["body"].to_string().into_bytes()
                };
                let doc = JsDocument::parse(&bytes).map_err(|_| "Invalid router fixture JSON")?;
                let options = &step["options"];
                let count_tokens = step.get("count").is_some();
                let options = RouteOptions {
                    scope: text(options, "scope").into(),
                    request_class: text(options, "requestClass").into(),
                    prompt_id: text(options, "promptId").into(),
                    request_id: options["requestId"].as_str().map(str::to_owned),
                    count_tokens,
                };
                let start = router.begin_route(&doc, options, now_ms);
                let mut count_models = Vec::new();
                let mut decision = if let Some(decision) = start.passthrough {
                    decision
                } else {
                    let early = start.early_count_model.clone();
                    if let Some(model) = early.as_ref() {
                        count_models.push(model.clone());
                    }
                    let decision = step.get("decision").cloned().unwrap_or_else(
                        || json!({"tier":"haiku","source":"jev","reason":"classified"}),
                    );
                    let pending = router.classified(&doc, start, decision, now_ms);
                    if count_tokens
                        && let Some(model) = pending.count_model.as_ref()
                        && early.as_ref() != Some(model)
                    {
                        count_models.push(model.clone());
                    }
                    let count = if count_tokens {
                        crate::request_validation::nonnegative_safe_integer(&step["count"])
                    } else {
                        None
                    };
                    router.finish_route(&doc, pending, count, now_ms)
                };
                if let Some(object) = decision.as_object_mut() {
                    object.remove("latency_ms");
                    object.remove("evaluation_latency_ms");
                }
                results.push(json!({"decision":decision,"count_models":count_models}));
            }
            _ => return Err("Invalid router fixture operation".into()),
        }
    }
    Ok(json!(results))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    fn config() -> RouterConfig {
        crate::config::read_config(&json!({}), false, Path::new("/tmp")).unwrap()
    }

    #[test]
    fn exact_configured_aliases_have_distinct_ranks_and_preserve_selected_wire_model() {
        let mut config = config();
        config.models.haiku = "custom-\u{fffd}".into();
        config.models.sonnet = "custom-\u{fffd}".into();
        let haiku = JsString::from_utf16("custom-".encode_utf16().chain([0xd800]).collect());
        let sonnet = JsString::from_utf16("custom-".encode_utf16().chain([0xd801]).collect());
        config.exact_models.insert("haiku".into(), haiku.clone());
        config.exact_models.insert("sonnet".into(), sonnet.clone());
        let mut router = Router::new(config);
        assert_eq!(router.tier_rank_exact(&haiku), 0);
        assert_eq!(router.tier_rank_exact(&sonnet), 1);
        for (raw, expected_reason) in [
            (
                br#"{"model":"custom-\ud800","messages":[{"role":"user","content":"Synthetic"}]}"#
                    .as_slice(),
                "classified",
            ),
            (
                br#"{"model":"custom-\ud802","messages":[{"role":"user","content":"Different"}]}"#
                    .as_slice(),
                "unknown_model",
            ),
        ] {
            let doc = JsDocument::parse(raw).unwrap();
            let start = router.begin_route(&doc, RouteOptions::default(), 0);
            let pending = router.classified(
                &doc,
                start,
                json!({"tier":"haiku","source":"jev","reason":"classified"}),
                0,
            );
            let result = router.finish_route_exact(&doc, pending, None, 0);
            assert_eq!(result.decision["reason"], expected_reason);
            assert_eq!(
                Some(&result.model),
                doc.get(doc.root(), "model")
                    .and_then(|node| doc.string(node))
            );
        }
    }
    fn document(body: Value) -> JsDocument {
        JsDocument::parse(body.to_string().as_bytes()).unwrap()
    }
    fn route(
        router: &mut Router,
        body: &JsDocument,
        tier: &str,
        options: RouteOptions,
        count: Option<u64>,
    ) -> Value {
        let start = router.begin_route(body, options, 0);
        if let Some(result) = start.passthrough {
            return result;
        }
        let pending = router.classified(
            body,
            start,
            json!({"tier":tier,"source":"jev","reason":"classified"}),
            0,
        );
        router.finish_route(body, pending, count, 0)
    }
    #[test]
    fn auxiliary_bypasses_evaluation_and_does_not_claim_turn() {
        let mut router = Router::new(config());
        let doc = document(
            json!({"model":"claude-opus-5-5","messages":[{"role":"user","content":"Hi"}]}),
        );
        let start = router.begin_route(
            &doc,
            RouteOptions {
                request_class: "auxiliary".into(),
                ..Default::default()
            },
            0,
        );
        assert_eq!(start.passthrough.unwrap()["reason"], "internal_request");
        assert_eq!(router.turns.record_count(), 0);
    }
    #[test]
    fn cache_markers_do_not_change_turn_identity_but_tool_inputs_do() {
        let make = |cache: bool, inside: bool| {
            document(
                json!({"model":"claude-opus-5-5","messages":[{"role":"user","content":[{"type":"text","text":"Task","cache_control":if cache {json!({"type":"ephemeral"})}else{Value::Null}},{"type":"tool_use","name":"Read","input":{"cache_control":inside}}]}]}),
            )
        };
        assert_eq!(
            turn_info(&make(true, false), "scope", "").key,
            turn_info(&make(false, false), "scope", "").key
        );
        assert_ne!(
            turn_info(&make(true, false), "scope", "").key,
            turn_info(&make(true, true), "scope", "").key
        );
    }
    #[test]
    fn turn_hash_matches_frozen_node_for_integer_order_surrogates_and_overflow() {
        // Captured from Router.turns.records after routing this synthetic body
        // with frozen 0.5.2 commit ea930c247626ce2af5ccdad721b5121417bf4ad8.
        // Raw insertion order differs from JSON.stringify integer-key order;
        // the nested schema and content retain their own exact semantics.
        let doc = JsDocument::parse(br#"{"model":"claude-opus-5-5","system":[{"9":"last","2":"first","type":"text","text":"Synthetic \ud800","cache_control":{"type":"ephemeral"}}],"tools":[{"8":"eight","1":"one","name":"Synthetic","input_schema":{"type":"object","properties":{"9":{},"2":{}}},"cache_control":null}],"messages":[{"9":true,"2":false,"role":"user","content":[{"9":1e400,"2":-0,"type":"text","text":"Synthetic task","cache_control":{"type":"ephemeral"}}]}]}"#).unwrap();
        let turn = turn_info(&doc, "synthetic/scope", "synthetic-prompt");
        assert_eq!(
            turn.key,
            "2ea384a0a362846992995b9ac063141136432ee2c6bd65ab29ee8e930d4193c1"
        );
        assert_eq!(
            turn.content_key,
            "9f5c78101dccdd43ffc17fa6468d193e0afbb5732b35e4fee15c1a3a7fde46dd"
        );
    }
    #[test]
    fn context_byte_charge_preserves_frozen_representation_and_guard_boundaries() {
        // Actual frozen Node contextSizeBytes results, including overwritten keys.
        for (raw, bytes) in [
            (
                r#"{"messages":[{"role":"user","content":"A😀é二\u0000\"\\\n"}]}"#,
                65,
            ),
            (r#"{"2":true,"1":null,"2":-0,"messages":[]}"#, 30),
            (
                r#"{"a":1e400,"b":-1e400,"c":-0,"d":1e21,"e":1e20,"f":1e-6,"g":1e-7,"h":9007199254740993,"i":1e-400}"#,
                110,
            ),
            (r#"{"messages":[],"\ud800":"\udc00","\ud800":"last"}"#, 31),
            (r#"{"messages":[],"tools":false,"opaque":{"q":2.1}}"#, 48),
            (
                r#"{"messages":[],"tools":[],"a":"\u2028\u2029\ud800\udc00\ud800"}"#,
                49,
            ),
        ] {
            let doc = JsDocument::parse(raw.as_bytes()).unwrap();
            for model in [
                "claude-haiku-4-5-20251001",
                "claude-sonnet-5",
                "claude-opus-5-5",
                "unknown",
            ] {
                assert_eq!(
                    context_size_bytes(&doc, model).unwrap().to_json_value(),
                    json!(bytes)
                );
            }
        }
        for bytes in [149999, 150000, 150001] {
            let doc =
                document(json!({"messages":[{"role":"user","content":"x".repeat(bytes - 43)}]}));
            assert_eq!(
                context_size_bytes(&doc, "claude-haiku-4-5-20251001")
                    .unwrap()
                    .to_json_value(),
                json!(bytes)
            );
        }
    }
    #[test]
    fn deferred_schemas_count_only_when_referenced_and_count_repeated_expansion() {
        let mut body = json!({"model":"claude-haiku-4-5","tools":[{"name":"Loaded","input_schema":{"type":"object"}},{"name":"Deferred","defer_loading":true,"input_schema":{"type":"object"},"description":"schema ".repeat(30000)}],"messages":[{"role":"user","content":"Hi"}]});
        let original = document(body.clone());
        assert!(
            context_size_bytes(&original, "claude-haiku-4-5")
                .unwrap()
                .as_source_number()
                < 1000.0
        );
        body["messages"][0]["content"] = json!([{"type":"tool_reference","tool_name":"Deferred"},{"type":"tool_reference","tool_name":"Deferred"}]);
        let referenced = document(body);
        assert!(
            context_size_bytes(&referenced, "claude-haiku-4-5")
                .unwrap()
                .as_source_number()
                > referenced.stringify().len() as f64
        );
    }
    #[test]
    fn deferred_context_charge_matches_actual_source_above_u32_without_mutation() {
        const CONTROL: &str =
            include_str!("../../../parity/cases/context-byte-charge-width-controls.json");
        assert!(CONTROL.len() <= 4096);
        assert_eq!(
            format!("{:x}", Sha256::digest(CONTROL.as_bytes())),
            "7390f47cc144b742e1de4ef29541e828c5ef67fe3bc65178d3e788115d07a9c6"
        );
        let control: Value = serde_json::from_str(CONTROL).unwrap();
        assert_eq!(
            control["frozen_source"]["router_sha256"],
            "aebf10268348f00b6c8cdc82ef9ced2d60c8e1cc0ddeee7c3498bcaf100b9d56"
        );
        assert_eq!(control["frozen_source"]["reference_files"], 111);
        assert_eq!(control["frozen_source"]["pure_calls"], 13);
        let make = |occurrences: usize| {
            let mut wire = String::from(
                r#"{"model":"claude-haiku-4-5","tools":[{"name":"L","input_schema":{"type":"object"}},{"name":"D","defer_loading":true,"input_schema":{"type":"object"},"description":""#,
            );
            wire.push_str(&"x".repeat(999907));
            wire.push_str(r#""}],"messages":[{"role":"user","content":["#);
            for index in 0..occurrences {
                if index != 0 {
                    wire.push(',');
                }
                wire.push_str(r#"{"type":"tool_use","name":"D"}"#);
            }
            wire.push_str("]}]}");
            wire
        };
        for row in control["cases"].as_array().unwrap() {
            let occurrences = usize::try_from(row["occurrences"].as_u64().unwrap()).unwrap();
            let wire = make(occurrences);
            assert!(wire.len() <= 2 * 1024 * 1024);
            assert!(wire.len() <= control["default_max_request_bytes"].as_u64().unwrap() as usize);
            assert_eq!(wire.len() as u64, row["wire_bytes"].as_u64().unwrap());
            assert_eq!(
                format!("{:x}", Sha256::digest(wire.as_bytes())),
                row["wire_sha256"].as_str().unwrap()
            );
            let doc = JsDocument::parse(wire.as_bytes()).unwrap();
            for model in control["models"].as_array().unwrap() {
                let charge = context_size_bytes(&doc, model.as_str().unwrap()).unwrap();
                assert_eq!(charge.exact_safe_bytes(), row["source_bytes"].as_u64());
                assert_eq!(charge.to_json_value(), row["source_bytes"]);
                assert!(charge.exceeds(150000));
                assert_eq!(doc.stringify(), wire);
            }
            // These are pure/core-policy controls, not gateway validation or
            // successful token counts/inference from the deliberately minimal wire.
            let mut router = Router::new(config());
            let count_model = router.config.models.haiku.clone();
            let start = router.begin_route(
                &doc,
                RouteOptions {
                    count_tokens: true,
                    ..Default::default()
                },
                0,
            );
            assert!(start.passthrough.is_none());
            assert_eq!(start.early_count_model.as_ref(), Some(&count_model));
            assert_eq!(
                context_size_bytes(&doc, "unknown-custom-model")
                    .unwrap()
                    .to_json_value(),
                json!(wire.len())
            );
            let mut malformed = doc.clone();
            let message = array(
                &malformed,
                property(&malformed, malformed.root(), "messages"),
            )
            .unwrap()[0];
            let block = array(&malformed, property(&malformed, message, "content")).unwrap()[0];
            malformed.set_field_json(block, "name", br#""""#).unwrap();
            assert_eq!(
                context_size_bytes(&malformed, "claude-haiku-4-5")
                    .unwrap()
                    .to_json_value(),
                json!(wire.len() - 1)
            );
            let mut all_deferred = doc.clone();
            let loaded = array(
                &all_deferred,
                property(&all_deferred, all_deferred.root(), "tools"),
            )
            .unwrap()[0];
            all_deferred
                .set_field_json(loaded, "defer_loading", b"true")
                .unwrap();
            assert_eq!(
                context_size_bytes(&all_deferred, "claude-haiku-4-5")
                    .unwrap()
                    .to_json_value(),
                json!(wire.len() + 21)
            );
            assert_eq!(doc.stringify(), wire);
        }
        let wire = make(4294);
        let doc = JsDocument::parse(wire.as_bytes()).unwrap();
        assert_eq!(
            context_size_bytes(&doc, "claude-haiku-4-5")
                .unwrap()
                .to_json_value(),
            control["adjacent"]["source_bytes"]
        );
    }

    #[test]
    fn context_charge_preserves_source_number_operation_order_and_json_shape() {
        for (number, expected) in [
            (0.0, json!(0)),
            (-0.0, json!(0)),
            (150000.0, json!(150000)),
            (150001.0, json!(150001)),
            (4295090321.0, json!(4295090321u64)),
            (5000105126.0, json!(5000105126u64)),
            (9007199254740991.0, json!(9007199254740991u64)),
        ] {
            let charge = ContextByteCharge(number);
            assert_eq!(charge.to_json_value(), expected);
            assert_eq!(charge.exact_safe_bytes(), expected.as_u64());
            assert_eq!(charge.exceeds(150000), number > 150000.0);
        }
        // IEEE-754-only operands; no enormous reference list or request was
        // allocated. Reassociation or a mathematical u64 sum changes this result.
        let mut reference_count = 9007199254740992.0;
        increment_context_reference(&mut reference_count);
        assert_eq!(reference_count, 9007199254740992.0);
        let big_occurrences = 140737488355329.0;
        let big_first = repeated_context_bytes(0.0, big_occurrences, 64);
        assert_eq!(big_first, 9007199254740992.0);
        let big_first = repeated_context_bytes(big_first, 2.0, 83);
        let big_first = repeated_context_bytes(big_first, 2.0, 83);
        let small_first = repeated_context_bytes(0.0, 2.0, 83);
        let small_first = repeated_context_bytes(small_first, 2.0, 83);
        let small_first = repeated_context_bytes(small_first, big_occurrences, 64);
        assert_eq!(big_first, 9007199254741160.0);
        assert_eq!(small_first, 9007199254741158.0);
        assert_ne!(big_first, small_first);
        for number in [big_first, small_first] {
            assert_eq!(ContextByteCharge(number).exact_safe_bytes(), None);
            assert!(ContextByteCharge(number).exceeds(150000));
            assert_eq!(
                ContextByteCharge(number).to_json_value().as_f64(),
                Some(number)
            );
        }
        assert_eq!(
            visible_context_charge(83, 0, 0, 9007199254740992.0)
                .unwrap()
                .as_source_number(),
            9007199254741076.0
        );
        assert_eq!(
            ContextByteCharge(f64::MAX).to_json_value().as_f64(),
            Some(f64::MAX)
        );
        for number in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            assert_eq!(ContextByteCharge(number).to_json_value(), Value::Null);
            assert_eq!(ContextByteCharge(number).exact_safe_bytes(), None);
        }
    }

    #[test]
    fn unavailable_context_length_requires_capacity_check_without_fake_bytes() {
        let doc = JsDocument::parse(br#"{"messages":[]}"#).unwrap();
        // Exercise the actual serialized-length rejection with a bounded
        // private test limit, not a fabricated arena or an enormous allocation.
        let unavailable = context_size_bytes_up_to(&doc, "claude-haiku-4-5", 0);
        assert_eq!(unavailable, Err(ContextByteChargeUnavailable));
        assert!(requires_context_capacity_check(unavailable));
        assert_eq!(
            ContextByteChargeUnavailable.code(),
            "serialized_length_unavailable"
        );
        assert_eq!(
            ContextByteChargeUnavailable.to_string(),
            "Context byte charge unavailable"
        );
        assert_eq!(
            visible_context_charge(0, 1, 0, 0.0),
            Err(ContextByteChargeUnavailable)
        );
        assert_eq!(
            visible_context_charge(usize::MAX, 0, 1, 0.0),
            Err(ContextByteChargeUnavailable)
        );
        assert!(!requires_context_capacity_check(Ok(ContextByteCharge(
            150000.0
        ))));
        assert!(requires_context_capacity_check(Ok(ContextByteCharge(
            150001.0
        ))));
        assert_eq!(
            context_size_bytes(&doc, "claude-haiku-4-5")
                .unwrap()
                .to_json_value(),
            json!(15)
        );
    }

    #[test]
    fn counted_haiku_capacity_and_auto_floor_follow_policy() {
        let mut router = Router::new(config());
        let doc = document(
            json!({"model":"claude-haiku-4-5-20251001","messages":[{"role":"user","content":"x".repeat(160000)}]}),
        );
        assert_eq!(
            route(
                &mut router,
                &doc,
                "haiku",
                RouteOptions::default(),
                Some(190000)
            )["model"],
            "claude-haiku-4-5-20251001"
        );
        assert_eq!(
            route(
                &mut router,
                &doc,
                "haiku",
                RouteOptions::default(),
                Some(190001)
            )["reason"],
            "context_capacity"
        );
        let mut config = config();
        config.client_profile = ClientProfile::Auto;
        config.models.sonnet = "claude-sonnet-5-5".into();
        let mut router = Router::new(config);
        let doc = document(
            json!({"model":"claude-opus-5-5","messages":[{"role":"user","content":"Hi"}]}),
        );
        assert_eq!(
            route(&mut router, &doc, "haiku", RouteOptions::default(), None)["reason"],
            "auto_mode_floor"
        );
    }
}

#[cfg(test)]
#[path = "turn_fingerprint_contracts.rs"]
mod turn_fingerprint_contracts;
