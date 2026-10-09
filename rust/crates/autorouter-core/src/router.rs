//! Routing policy split at the two asynchronous boundaries in the runtime.
//! No request body is retained across phases. Authoritative JSON provides exact
//! hash/context identity and direct field reads avoid copying opaque catalogs.
use crate::auto_routing::{can_route_auto_request_document, has_routable_safeguards_document};
use crate::config::{ClientProfile, RouterConfig};
use crate::js_json::{JsDocument, JsNode, JsString, NodeId};
use crate::model_catalog::{
    can_upgrade_context, has_native_million_context, supports_tool_references,
};
use crate::prompt_state::goal_feedback_indexes_document;
use crate::turn_state::{ContinuationEvidence, Pin, Selection, ToolOwner, TurnState};
use serde_json::{Value, json};
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
fn property(doc: &JsDocument, node: NodeId, key: &str) -> Option<NodeId> {
    doc.get(node, key)
}
fn is(doc: &JsDocument, node: Option<NodeId>, value: &str) -> bool {
    node.and_then(|n| doc.string(n))
        .is_some_and(|s| s.units().iter().copied().eq(value.encode_utf16()))
}
fn array(doc: &JsDocument, node: Option<NodeId>) -> Option<&[NodeId]> {
    match node.and_then(|n| doc.node(n)) {
        Some(JsNode::Array(items)) => Some(items),
        _ => None,
    }
}
fn document_truthy(doc: &JsDocument, node: Option<NodeId>) -> bool {
    match node.and_then(|node| doc.node(node)) {
        None | Some(JsNode::Null | JsNode::Bool(false)) => false,
        Some(JsNode::Number(value)) => *value != 0.0 && !value.is_nan(),
        Some(JsNode::String(value)) => !value.units().is_empty(),
        _ => true,
    }
}
fn needs_sonnet_document(doc: &JsDocument) -> bool {
    let root = doc.root();
    is(
        doc,
        doc.get(root, "thinking")
            .and_then(|node| doc.get(node, "type")),
        "adaptive",
    ) || document_truthy(
        doc,
        doc.get(root, "output_config")
            .and_then(|node| doc.get(node, "effort")),
    ) || doc
        .get(root, "max_tokens")
        .and_then(|node| doc.node(node))
        .is_some_and(|value| matches!(value, JsNode::Number(number) if *number > 64000.0))
}
fn hash(json: &str) -> String {
    // Callers compose already serialized values and JS-ordered object fields.
    // Parsing the complete catalog again cannot change their representation.
    format!("{:x}", Sha256::digest(json.as_bytes()))
}
fn raw(doc: &JsDocument, node: Option<NodeId>) -> String {
    node.map(|n| doc.stringify_node(n))
        .unwrap_or_else(|| "null".into())
}

fn object_without_cache(doc: &JsDocument, node: NodeId, nested: bool) -> String {
    let entries: Vec<(JsString, NodeId)> = match doc.node(node) {
        Some(JsNode::Object(object)) => object.ordered_entries().cloned().collect(),
        Some(JsNode::Array(array)) => array
            .iter()
            .enumerate()
            .map(|(i, n)| (JsString::from_scalar(&i.to_string()), *n))
            .collect(),
        _ => return doc.stringify_node(node),
    };
    let recurse = nested
        && is(doc, property(doc, node, "type"), "tool_result")
        && array(doc, property(doc, node, "content")).is_some();
    let mut members = Vec::new();
    for (key, child) in entries {
        if key
            .units()
            .iter()
            .copied()
            .eq("cache_control".encode_utf16())
        {
            continue;
        }
        let value = if recurse && key.units().iter().copied().eq("content".encode_utf16()) {
            turn_content(doc, Some(child))
        } else {
            doc.stringify_node(child)
        };
        members.push(format!("{}:{value}", key.stringify()));
    }
    format!("{{{}}}", members.join(","))
}
fn turn_content(doc: &JsDocument, content: Option<NodeId>) -> String {
    let Some(blocks) = array(doc, content) else {
        return raw(doc, content);
    };
    format!(
        "[{}]",
        blocks
            .iter()
            .map(|n| object_without_cache(doc, *n, true))
            .collect::<Vec<_>>()
            .join(",")
    )
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
    let all_messages = array(doc, property(doc, root, "messages")).unwrap_or_default();
    let messages = &all_messages[..end.unwrap_or(all_messages.len()).min(all_messages.len())];
    // Recognition only reads earlier messages; later feedback cannot affect a
    // prefix's recognition, so one full scan also serves prior-turn recovery.
    let feedback = goal_feedback_indexes_document(doc, property(doc, root, "messages"));
    let index = messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, &message)| {
            (is(doc, property(doc, message, "role"), "user")
                && !feedback.contains(&index)
                && !array(doc, property(doc, message, "content")).is_some_and(|blocks| {
                    blocks
                        .iter()
                        .any(|b| is(doc, property(doc, *b, "type"), "tool_result"))
                }))
            .then_some(index)
        });
    let tools = array(doc, property(doc, root, "tools"))
        .map(|tools| {
            format!(
                "[{}]",
                tools
                    .iter()
                    .map(|n| object_without_cache(doc, *n, false))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        })
        .unwrap_or_else(|| "null".into());
    let mut history = Vec::new();
    for &message in &messages[..index.map_or(0, |i| i + 1)] {
        let Some(JsNode::Object(object)) = doc.node(message) else {
            continue;
        };
        let members = object
            .ordered_entries()
            .map(|(key, child)| {
                let value = if key.units().iter().copied().eq("content".encode_utf16()) {
                    turn_content(doc, Some(*child))
                } else {
                    doc.stringify_node(*child)
                };
                format!("{}:{value}", key.stringify())
            })
            .collect::<Vec<_>>();
        history.push(format!("{{{}}}", members.join(",")));
    }
    let content_key = hash(&format!(
        "[{},{},{},[{}]]",
        JsString::from_scalar(scope).stringify(),
        turn_content(doc, property(doc, root, "system")),
        tools,
        history.join(",")
    ));
    let key = if prompt_id.is_empty() {
        content_key.clone()
    } else {
        hash(&format!(
            "[\"prompt\",{},{}]",
            JsString::from_scalar(scope).stringify(),
            JsString::from_scalar(prompt_id).stringify()
        ))
    };
    TurnInfo {
        index,
        key,
        content_key,
        goal_feedback: !messages.is_empty() && feedback.contains(&(messages.len() - 1)),
        continuation: index.is_none()
            || messages[index.map_or(0, |i| i + 1)..]
                .iter()
                .any(|n| !is(doc, property(doc, *n, "role"), "system")),
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
        property(doc, tool, "defer_loading").and_then(|n| doc.node(n)),
        Some(JsNode::Bool(true))
    ) || property(doc, tool, "type").is_some_and(|n| !is(doc, Some(n), "custom"))
        || !property(doc, tool, "name")
            .and_then(|n| doc.string(n))
            .is_some_and(|s| !s.units().is_empty())
        || !property(doc, tool, "input_schema").is_some_and(|n| {
            matches!(doc.node(n), Some(JsNode::Object(_)))
                && is(doc, property(doc, n, "type"), "object")
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
        if let Some(node) = property(doc, tool, field) {
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
pub fn context_size_bytes(doc: &JsDocument, model: &str) -> usize {
    let full = doc.stringify().len();
    let Some(tools) = array(doc, property(doc, doc.root(), "tools")) else {
        return full;
    };
    if !supports_tool_references(model)
        || !tools.iter().any(|n| known_deferred_tool(doc, *n))
        || !tools.iter().any(|n| {
            matches!(doc.node(*n), Some(JsNode::Object(_)))
                && !matches!(
                    property(doc, *n, "defer_loading").and_then(|n| doc.node(n)),
                    Some(JsNode::Bool(true))
                )
        })
    {
        return full;
    }
    let mut references: HashMap<JsString, usize> = HashMap::new();
    let mut pending = array(doc, property(doc, doc.root(), "messages"))
        .unwrap_or_default()
        .to_vec();
    while let Some(node) = pending.pop() {
        match doc.node(node) {
            Some(JsNode::Array(items)) => pending.extend_from_slice(items),
            Some(JsNode::Object(object)) => {
                let kind = property(doc, node, "type");
                if is(doc, kind, "tool_reference") || is(doc, kind, "tool_use") {
                    let field = if is(doc, kind, "tool_reference") {
                        "tool_name"
                    } else {
                        "name"
                    };
                    let Some(name) = property(doc, node, field)
                        .and_then(|n| doc.string(n))
                        .filter(|s| !s.units().is_empty())
                    else {
                        return full;
                    };
                    *references.entry(name.clone()).or_default() += 1;
                }
                pending.extend(object.entries().iter().map(|(_, n)| *n));
            }
            _ => {}
        }
    }
    let mut repeated = 0;
    let mut visible = Vec::new();
    for &tool in tools {
        if known_deferred_tool(doc, tool) {
            let name = property(doc, tool, "name")
                .and_then(|n| doc.string(n))
                .expect("validated name");
            let occurrences = references.get(name).copied().unwrap_or_default();
            if occurrences > 1 {
                repeated += (occurrences - 1) * doc.stringify_node(tool).len();
            }
            if occurrences == 0 {
                continue;
            }
        }
        visible.push(doc.stringify_node(tool));
    }
    // Only a top-level member changes; the original member position and all
    // other opaque values remain authoritative, including integer-key order.
    let Some(JsNode::Object(root)) = doc.node(doc.root()) else {
        return full;
    };
    let members = root
        .ordered_entries()
        .map(|(key, value)| {
            let value = if key.units().iter().copied().eq("tools".encode_utf16()) {
                format!("[{}]", visible.join(","))
            } else {
                doc.stringify_node(*value)
            };
            format!("{}:{value}", key.stringify())
        })
        .collect::<Vec<_>>();
    format!("{{{}}}", members.join(",")).len() + repeated
}
fn has_content_block(doc: &JsDocument, types: &[&str]) -> bool {
    let mut pending: Vec<_> = array(doc, property(doc, doc.root(), "messages"))
        .unwrap_or_default()
        .iter()
        .filter_map(|n| property(doc, *n, "content"))
        .collect();
    while let Some(content) = pending.pop() {
        let Some(blocks) = array(doc, Some(content)) else {
            continue;
        };
        for &block in blocks {
            if types
                .iter()
                .any(|kind| is(doc, property(doc, block, "type"), kind))
            {
                return true;
            }
            if is(doc, property(doc, block, "type"), "tool_result")
                && let Some(child) = property(doc, block, "content")
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
    pub passthrough: Option<Value>,
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
    decision: Value,
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
        let turns = TurnState::new(1000, config.turn_ttl_ms);
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
            let mut decision = json!({"model":requested,"source":"passthrough","reason":if start.options.request_class == "auxiliary" { "internal_request" } else { "auto_mode_safeguards" }, "evaluation_latency_ms":0});
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
                    decision["continuity_state"] = json!("capacity_exhausted");
                }
            }
            start.passthrough = Some(decision);
            return start;
        }
        start.has_system_message =
            array(doc, doc.get(doc.root(), "messages")).is_some_and(|messages| {
                messages
                    .iter()
                    .any(|&message| is(doc, doc.get(message, "role"), "system"))
            });
        start.unknown_model = rank(requested) < 0
            && !["haiku", "sonnet", "opus"]
                .iter()
                .any(|tier| self.tier_model_exact(tier) == requested_identity(doc));
        start.model_specific_thinking = document_truthy(doc, doc.get(doc.root(), "thinking"))
            && !["disabled", "adaptive"].iter().any(|kind| {
                is(
                    doc,
                    doc.get(doc.root(), "thinking")
                        .and_then(|node| doc.get(node, "type")),
                    kind,
                )
            });
        start.model_specific_features = start.model_specific_thinking
            || ["context_management", "speed", "container", "mcp_servers"]
                .iter()
                .any(|key| document_truthy(doc, doc.get(doc.root(), key)))
            || array(doc, doc.get(doc.root(), "tools")).is_some_and(|tools| {
                tools.iter().any(|&tool| {
                    document_truthy(doc, doc.get(tool, "type"))
                        && !is(doc, doc.get(tool, "type"), "custom")
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
            && (context_size_bytes(doc, &self.config.models.haiku) > 150000
                || start.has_attachments)
        {
            start.early_count_model = Some(self.config.models.haiku.clone());
        }
        start
    }

    pub fn classified(
        &mut self,
        doc: &JsDocument,
        start: RouteStart,
        decision: Value,
        now_ms: u64,
    ) -> RoutePending {
        let requested_exact = requested_identity(doc);
        let requested_display = requested_exact.to_well_formed();
        let requested = requested_display.as_str();
        let mut model_exact = self.tier_model_exact(text(&decision, "tier"));
        let mut model = model_exact.to_well_formed();
        let mut reason = text(&decision, "reason").to_owned();
        if start.auto_mode && text(&decision, "tier") == "haiku" {
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
                    .find(|node| is(doc, doc.get(**node, "role"), "user"))
            })
            .and_then(|message| array(doc, doc.get(*message, "content")))
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|block| is(doc, doc.get(**block, "type"), "tool_result"))
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
            && !is(
                doc,
                doc.get(doc.root(), "thinking")
                    .and_then(|node| doc.get(node, "type")),
                "enabled",
            )
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
        } else if text(&decision, "source") == "fallback" && rank(requested) >= 1 {
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
        } else if needs_sonnet_document(doc) && text(&decision, "tier") == "haiku" {
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
        let large_context = context_size_bytes(doc, &model) > 150000 || start.has_attachments;
        let count_model = (large_context && !start.capacity_locked && can_upgrade_context(&model))
            .then(|| model.clone());
        RoutePending {
            count_model,
            start,
            decision,
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
        mut pending: RoutePending,
        count: Option<u64>,
        now_ms: u64,
    ) -> RouteDecision {
        let requested_exact = requested_identity(doc);
        let requested_display = requested_exact.to_well_formed();
        let requested = requested_display.as_str();
        if pending.count_model.is_some() {
            if let Some(count) = count.filter(|n| *n <= crate::config::MAX_SAFE_INTEGER) {
                pending.large_context = count > 190000;
                pending.decision["context_check"] = json!(if pending.large_context {
                    "over_budget"
                } else {
                    "within_budget"
                });
                pending.decision["counted_input_tokens"] = json!(count);
            } else {
                pending.decision["context_check"] = json!("count_unavailable");
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
                if pin.value["confirmed"] == true {
                    "confirmed"
                } else {
                    "selected"
                }
            })
            .or_else(|| pending.turn.continuation.then_some("unknown"));
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
            continuity = Some("capacity_exhausted");
        }
        pending.decision["model"] = json!(pending.model);
        pending.decision["reason"] = json!(pending.reason);
        if compatibility["compatible"] != true {
            pending.decision["compatibility_reason"] = compatibility["reason"].clone();
        }
        if let Some(continuity) = continuity {
            pending.decision["continuity_state"] = json!(continuity);
        }
        RouteDecision {
            decision: pending.decision,
            model: pending.model_exact,
        }
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
    fn deferred_schemas_count_only_when_referenced_and_count_repeated_expansion() {
        let mut body = json!({"model":"claude-haiku-4-5","tools":[{"name":"Loaded","input_schema":{"type":"object"}},{"name":"Deferred","defer_loading":true,"input_schema":{"type":"object"},"description":"schema ".repeat(30000)}],"messages":[{"role":"user","content":"Hi"}]});
        let original = document(body.clone());
        assert!(context_size_bytes(&original, "claude-haiku-4-5") < 1000);
        body["messages"][0]["content"] = json!([{"type":"tool_reference","tool_name":"Deferred"},{"type":"tool_reference","tool_name":"Deferred"}]);
        let referenced = document(body);
        assert!(context_size_bytes(&referenced, "claude-haiku-4-5") > referenced.stringify().len());
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
