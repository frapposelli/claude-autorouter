//! Privacy-preserving evaluator context and direct-human diagnostic excerpts.
//! Classifier budgets count serialized UTF-16 units, as the original JS API
//! does. Prompt logs instead count Unicode scalar values and are well formed.
use crate::js_json::{JsDocument, JsNode, JsString, NodeId};
use crate::redaction::redact_sensitive;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};

const MARGIN: usize = 4096;
const MARKER: &str = "\n[... omitted ...]\n";

fn space(unit: u16) -> bool {
    matches!(unit, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a
        | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}
fn trim(units: &[u16]) -> &[u16] {
    let start = units.iter().position(|u| !space(*u)).unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|u| !space(*u))
        .map_or(start, |i| i + 1);
    &units[start..end]
}
fn empty() -> JsString {
    JsString::from_utf16(Vec::new())
}
fn scalar(value: &str) -> JsString {
    JsString::from_scalar(value)
}
fn is(doc: &JsDocument, node: Option<NodeId>, text: &str) -> bool {
    node.and_then(|n| doc.string(n))
        .is_some_and(|v| v.units().iter().copied().eq(text.encode_utf16()))
}
fn property(doc: &JsDocument, node: NodeId, key: &str) -> Option<NodeId> {
    doc.get(node, key)
}
fn array(doc: &JsDocument, node: Option<NodeId>) -> Option<&[NodeId]> {
    match node.and_then(|n| doc.node(n)) {
        Some(JsNode::Array(values)) => Some(values),
        _ => None,
    }
}
fn truthy(doc: &JsDocument, node: Option<NodeId>) -> bool {
    match node.and_then(|n| doc.node(n)) {
        None | Some(JsNode::Null | JsNode::Bool(false)) => false,
        Some(JsNode::Number(n)) => *n != 0.0 && !n.is_nan(),
        Some(JsNode::String(s)) => !s.units().is_empty(),
        _ => true,
    }
}
fn string_coerce(doc: &JsDocument, node: Option<NodeId>) -> JsString {
    match node.and_then(|n| doc.node(n)) {
        None | Some(JsNode::Null) => empty(),
        Some(JsNode::String(s)) => s.clone(),
        Some(JsNode::Bool(v)) => scalar(if *v { "true" } else { "false" }),
        Some(JsNode::Number(v)) => {
            if v.is_nan() {
                scalar("NaN")
            } else if *v == f64::INFINITY {
                scalar("Infinity")
            } else if *v == f64::NEG_INFINITY {
                scalar("-Infinity")
            } else if *v == 0.0 {
                scalar("0")
            } else {
                scalar(ryu_js::Buffer::new().format_finite(*v))
            }
        }
        Some(JsNode::Object(_)) => scalar("[object Object]"),
        Some(JsNode::Array(values)) => {
            let mut result = Vec::new();
            for (i, child) in values.iter().enumerate() {
                if i > 0 {
                    result.push(b',' as u16);
                }
                result.extend_from_slice(string_coerce(doc, Some(*child)).units());
            }
            JsString::from_utf16(result)
        }
    }
}

fn reminder(value: &JsString) -> bool {
    let mut remaining = trim(value.units());
    let mut found = false;
    while !remaining.is_empty() {
        let Some(tag) = ["system-reminder", "available-deferred-tools"]
            .into_iter()
            .find(|tag| {
                remaining.starts_with(&format!("<{tag}>").encode_utf16().collect::<Vec<_>>())
            })
        else {
            return false;
        };
        let opening = tag.len() + 2;
        let closing: Vec<u16> = format!("</{tag}>").encode_utf16().collect();
        let Some(end) = remaining[opening..]
            .windows(closing.len())
            .position(|v| v == closing)
        else {
            return false;
        };
        remaining = &remaining[opening + end + closing.len()..];
        remaining = &remaining[remaining
            .iter()
            .position(|u| !space(*u))
            .unwrap_or(remaining.len())..];
        found = true;
    }
    found
}

// Explicit work stack avoids recursive traversal of nested tool results.
fn content_text(doc: &JsDocument, node: Option<NodeId>, omit_reminders: bool) -> JsString {
    enum Work {
        Content(Option<NodeId>, bool),
        Block(NodeId),
        Text(JsString),
    }
    let mut work = vec![Work::Content(node, omit_reminders)];
    let mut output = Vec::new();
    while let Some(item) = work.pop() {
        match item {
            Work::Text(text) => output.extend_from_slice(text.units()),
            Work::Content(node, omit) => {
                if let Some(text) = node.and_then(|n| doc.string(n)) {
                    output.extend_from_slice(text.units());
                } else if let Some(blocks) = array(doc, node) {
                    let mut items = Vec::new();
                    for block in blocks {
                        if !matches!(doc.node(*block), Some(JsNode::Object(_) | JsNode::Array(_))) {
                            continue;
                        }
                        if is(doc, property(doc, *block, "type"), "text") {
                            let text = string_coerce(doc, property(doc, *block, "text"));
                            if omit && reminder(&text) {
                                continue;
                            }
                        }
                        if !items.is_empty() {
                            items.push(Work::Text(scalar("\n")));
                        }
                        items.push(Work::Block(*block));
                    }
                    work.extend(items.into_iter().rev());
                }
            }
            Work::Block(block) => {
                let kind = property(doc, block, "type");
                if is(doc, kind, "text") {
                    output.extend_from_slice(
                        string_coerce(doc, property(doc, block, "text")).units(),
                    );
                } else if is(doc, kind, "tool_result") {
                    output.extend(
                        format!(
                            "[tool result{}] ",
                            if truthy(doc, property(doc, block, "is_error")) {
                                " ERROR"
                            } else {
                                ""
                            }
                        )
                        .encode_utf16(),
                    );
                    work.push(Work::Content(property(doc, block, "content"), false));
                } else if is(doc, kind, "tool_use") {
                    output.extend("[tool call: ".encode_utf16());
                    output.extend(
                        string_coerce(doc, property(doc, block, "name"))
                            .units()
                            .iter()
                            .take(100),
                    );
                    output.extend("]".encode_utf16());
                } else if let Some(kind) = ["image", "document", "thinking", "redacted_thinking"]
                    .into_iter()
                    .find(|k| is(doc, kind, k))
                {
                    output.extend(format!("[{kind} omitted]").encode_utf16());
                } else {
                    output.extend("[non-text content omitted]".encode_utf16());
                }
            }
        }
    }
    JsString::from_utf16(output)
}
fn human_task(doc: &JsDocument, message: NodeId) -> JsString {
    if !is(doc, property(doc, message, "role"), "user") {
        return empty();
    }
    let content = property(doc, message, "content");
    if array(doc, content).is_some_and(|blocks| {
        blocks
            .iter()
            .any(|b| is(doc, property(doc, *b, "type"), "tool_result"))
    }) {
        return empty();
    }
    content_text(doc, content, true)
}
fn standalone(doc: &JsDocument, content: Option<NodeId>) -> Option<&JsString> {
    if let Some(text) = content.and_then(|n| doc.string(n)) {
        return Some(text);
    }
    let blocks = array(doc, content)?;
    if blocks.len() != 1 || !is(doc, property(doc, blocks[0], "type"), "text") {
        return None;
    }
    property(doc, blocks[0], "text").and_then(|n| doc.string(n))
}

pub fn goal_feedback_indexes_document(
    doc: &JsDocument,
    messages: Option<NodeId>,
) -> BTreeSet<usize> {
    let mut indexes = BTreeSet::new();
    let Some(messages) = array(doc, messages) else {
        return indexes;
    };
    let mut condition: Option<JsString> = None;
    let mut short: Option<JsString> = None;
    let mut saw_full = false;
    for (index, &message) in messages.iter().enumerate() {
        if !is(doc, property(doc, message, "role"), "user") {
            continue;
        }
        let task = human_task(doc, message);
        if let Some(value) = goal_command(task.units()) {
            let value = trim(value);
            if value.is_empty() {
                continue;
            }
            let word = String::from_utf16_lossy(value);
            condition = (value.len() <= 4000
                && !["clear", "stop", "off", "reset", "none", "cancel"]
                    .iter()
                    .any(|v| word.eq_ignore_ascii_case(v)))
            .then(|| JsString::from_utf16(value.to_vec()));
            short = condition
                .as_ref()
                .filter(|v| v.units().len() > 500)
                .map(|v| {
                    let mut end = 500;
                    if (0xd800..=0xdbff).contains(&v.units()[end - 1]) {
                        end -= 1;
                    }
                    let mut prefix = v.units()[..end].to_vec();
                    prefix.extend(format!("… [+{} chars]", v.units().len() - end).encode_utf16());
                    JsString::from_utf16(prefix)
                });
            saw_full = false;
            continue;
        }
        let Some(condition) = condition.as_ref() else {
            continue;
        };
        if index == 0 || !is(doc, property(doc, messages[index - 1], "role"), "assistant") {
            continue;
        }
        let Some(text) = standalone(doc, property(doc, message, "content")) else {
            continue;
        };
        let matches = |label: &JsString| {
            let mut prefix: Vec<u16> = "Stop hook feedback:\n[".encode_utf16().collect();
            prefix.extend_from_slice(label.units());
            prefix.extend("]: ".encode_utf16());
            text.units().starts_with(&prefix) && !trim(&text.units()[prefix.len()..]).is_empty()
        };
        if matches(condition) {
            indexes.insert(index);
            saw_full = true;
        } else if saw_full && short.as_ref().is_some_and(matches) {
            indexes.insert(index);
        }
    }
    indexes
}

fn goal_command(mut units: &[u16]) -> Option<&[u16]> {
    for prefix in [
        "<command-name>/goal</command-name>",
        "<command-message>goal</command-message>",
        "<command-args>",
    ] {
        units = &units[units.iter().position(|u| !space(*u)).unwrap_or(units.len())..];
        let prefix: Vec<u16> = prefix.encode_utf16().collect();
        units = units.strip_prefix(prefix.as_slice())?;
    }
    let closing: Vec<u16> = "</command-args>".encode_utf16().collect();
    for end in 0..=units.len().saturating_sub(closing.len()) {
        if units.get(end..end + closing.len()) == Some(closing.as_slice())
            && units.get(end + closing.len()).is_none_or(|u| space(*u))
        {
            return Some(&units[..end]);
        }
    }
    None
}

fn excerpt(text: &JsString, length: usize) -> JsString {
    if text.units().len() <= length {
        return text.clone();
    }
    let marker = scalar(MARKER);
    let insert = length > marker.units().len() + 2;
    let retained = length.saturating_sub(if insert { marker.units().len() } else { 0 });
    let head = retained.div_ceil(2);
    let tail = retained - head;
    let mut out = text.units()[..head].to_vec();
    if insert {
        out.extend_from_slice(marker.units());
    }
    if tail > 0 {
        out.extend_from_slice(&text.units()[text.units().len() - tail..]);
    }
    JsString::from_utf16(out)
}
fn fit_text(text: &JsString, budget: usize) -> JsString {
    if budget == 0 || text.units().is_empty() {
        return empty();
    }
    let mut low = 0;
    let mut high = text.units().len().min(budget);
    let mut fitted = empty();
    while low <= high {
        let length = (low + high) / 2;
        let candidate = excerpt(text, length);
        if candidate.stringify().encode_utf16().count() - 2 <= budget {
            fitted = candidate;
            low = length + 1;
        } else if length == 0 {
            break;
        } else {
            high = length - 1;
        }
    }
    fitted
}

// Redaction patterns recognize only ASCII words and the explicit JS whitespace
// set. An unused, non-whitespace BMP scalar is therefore equivalent to an
// unpaired surrogate for those patterns and retains its one-unit length.
fn redact_units(text: &JsString) -> JsString {
    if let Some(text) = text.to_scalar() {
        return scalar(&redact_sensitive(&text));
    }
    let mut used: HashSet<u16> = text.units().iter().copied().collect();
    let mut substitutions = HashMap::<u16, u16>::new();
    let mut reverse = HashMap::<u16, u16>::new();
    let mut candidates = (0xe000..=0xf8ff)
        .chain(0x0080..=0xffff)
        .filter(|u| !space(*u) && !(0xd800..=0xdfff).contains(u));
    let mut mapped = Vec::new();
    for decoded in char::decode_utf16(text.units().iter().copied()) {
        match decoded {
            Ok(ch) => mapped.extend(ch.encode_utf16(&mut [0; 2]).iter().copied()),
            Err(error) => {
                let surrogate = error.unpaired_surrogate();
                let replacement = *substitutions.entry(surrogate).or_insert_with(|| {
                    let replacement = candidates
                        .find(|u| !used.contains(u))
                        .expect("bounded classifier text has an unused BMP scalar");
                    used.insert(replacement);
                    reverse.insert(replacement, surrogate);
                    replacement
                });
                mapped.push(replacement);
            }
        }
    }
    let redacted = redact_sensitive(&String::from_utf16(&mapped).expect("mapped UTF-16 is scalar"));
    JsString::from_utf16(
        redacted
            .encode_utf16()
            .map(|u| reverse.get(&u).copied().unwrap_or(u))
            .collect(),
    )
}
fn classifier_text(text: &JsString, limit: usize) -> JsString {
    let span = limit.saturating_add(MARGIN);
    let text = if text.units().len() > span.saturating_mul(2) {
        let mut units = text.units()[..span].to_vec();
        units.extend(MARKER.encode_utf16());
        units.extend_from_slice(&text.units()[text.units().len() - span..]);
        JsString::from_utf16(units)
    } else {
        text.clone()
    };
    redact_units(&text)
}

#[derive(Clone, Debug)]
pub struct PromptState {
    pub system: JsString,
    pub original_task: JsString,
    pub current_task: JsString,
    pub recent_messages: Vec<(Option<String>, JsString)>,
    pub message_count: usize,
    pub tool_count_json: String,
}
impl PromptState {
    pub fn stringify(&self) -> String {
        let messages: Vec<String> = self
            .recent_messages
            .iter()
            .map(|(role, content)| {
                format!(
                    "{{{}{}}}",
                    role.as_ref()
                        .map(|role| format!("\"role\":{role},"))
                        .unwrap_or_default(),
                    format_args!("\"content\":{}", content.stringify())
                )
            })
            .collect();
        format!(
            "{{\"system\":{},\"original_task\":{},\"current_task\":{},\"recent_messages\":[{}],\"message_count\":{},\"tool_count\":{},\"context_is_excerpt\":true}}",
            self.system.stringify(),
            self.original_task.stringify(),
            self.current_task.stringify(),
            messages.join(","),
            self.message_count,
            self.tool_count_json
        )
    }
    pub fn observation(&self) -> Value {
        JsDocument::parse(self.stringify().as_bytes())
            .expect("state JSON")
            .to_serde_observation_lossy()
    }
    fn remaining(&self, limit: usize) -> usize {
        limit.saturating_sub(self.stringify().encode_utf16().count())
    }
}
pub fn build_state_document(doc: &JsDocument, limit: usize) -> PromptState {
    build_state_inner(doc, limit, true)
}

/// Ollama omits executor background before budgeting, then also bounds the
/// serialized UTF-8 bytes. Keep the original 200-character floor behavior.
pub fn build_ollama_state_document(doc: &JsDocument, limit: usize) -> PromptState {
    let mut budget = limit;
    let mut state = build_state_inner(doc, budget, false);
    while state.stringify().len() > limit && budget > 200 {
        budget = ((budget as f64 * limit as f64 / state.stringify().len() as f64).floor() as usize)
            .saturating_sub(1)
            .max(200);
        state = build_state_inner(doc, budget, false);
    }
    state
}

fn build_state_inner(doc: &JsDocument, limit: usize, include_system: bool) -> PromptState {
    let root = doc.root();
    let messages_node = property(doc, root, "messages");
    let messages = array(doc, messages_node).unwrap_or_default();
    let feedback = goal_feedback_indexes_document(doc, messages_node);
    let mut first_task = empty();
    let mut current_task = empty();
    let mut current_index = None;
    for (index, &message) in messages.iter().enumerate() {
        if feedback.contains(&index) {
            continue;
        }
        let task = human_task(doc, message);
        if trim(task.units()).is_empty() {
            continue;
        }
        if first_task.units().is_empty() {
            first_task = task.clone();
        }
        current_task = task;
        current_index = Some(index);
    }
    let tools = property(doc, root, "tools");
    let tool_count_json = match tools.and_then(|n| doc.node(n)) {
        Some(JsNode::Array(values)) => values.len().to_string(),
        Some(JsNode::String(value)) => value.units().len().to_string(),
        Some(JsNode::Object(_)) => tools
            .and_then(|n| property(doc, n, "length"))
            .filter(|n| !matches!(doc.node(*n), Some(JsNode::Null)))
            .map(|n| doc.stringify_node(n))
            .unwrap_or_else(|| "0".into()),
        _ => "0".into(),
    };
    let mut state = PromptState {
        system: empty(),
        original_task: empty(),
        current_task: empty(),
        recent_messages: Vec::new(),
        message_count: messages.len(),
        tool_count_json,
    };
    state.current_task = fit_text(
        &classifier_text(&current_task, limit),
        state
            .remaining(limit)
            .min((limit as f64 * 0.55).floor() as usize),
    );
    state.original_task = fit_text(
        &classifier_text(&first_task, limit),
        2000.min((state.remaining(limit) as f64 * 0.3).floor() as usize),
    );
    state.system = fit_text(
        &classifier_text(
            &content_text(
                doc,
                include_system
                    .then(|| property(doc, root, "system"))
                    .flatten(),
                true,
            ),
            limit,
        ),
        1000.min((state.remaining(limit) as f64 * 0.3).floor() as usize),
    );
    for (index, &message) in messages.iter().enumerate().rev() {
        if state.recent_messages.len() >= 8 {
            break;
        }
        if current_index == Some(index) {
            continue;
        }
        let text = content_text(doc, property(doc, message, "content"), true);
        if text.units().is_empty() {
            continue;
        }
        let role = property(doc, message, "role").map(|node| doc.stringify_node(node));
        let overhead = format!(
            "{{{}\"content\":\"\"}}",
            role.as_ref()
                .map(|r| format!("\"role\":{r},"))
                .unwrap_or_default()
        )
        .encode_utf16()
        .count()
            + usize::from(!state.recent_messages.is_empty());
        let budget = 3000.min(state.remaining(limit).saturating_sub(overhead));
        if budget < 1 {
            break;
        }
        state
            .recent_messages
            .insert(0, (role, fit_text(&classifier_text(&text, limit), budget)));
    }
    state
}

pub fn prompt_excerpt_document(doc: &JsDocument, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let messages_node = property(doc, doc.root(), "messages");
    let Some(messages) = array(doc, messages_node) else {
        return String::new();
    };
    let mut feedback = None;
    for (index, &message) in messages.iter().enumerate().rev() {
        if !is(doc, property(doc, message, "role"), "user") {
            continue;
        }
        let content = property(doc, message, "content");
        if array(doc, content).is_some_and(|blocks| {
            blocks
                .iter()
                .any(|b| is(doc, property(doc, *b, "type"), "tool_result"))
        }) {
            continue;
        }
        if standalone(doc, content).is_some_and(|s| {
            s.units()
                .starts_with(&"Stop hook feedback:\n[".encode_utf16().collect::<Vec<_>>())
        }) && feedback
            .get_or_insert_with(|| goal_feedback_indexes_document(doc, messages_node))
            .contains(&index)
        {
            continue;
        }
        let mut characters = Vec::new();
        let mut non_text = false;
        let window = max_chars.saturating_add(MARGIN);
        let blocks: Vec<Option<&JsString>> = if let Some(text) = content.and_then(|n| doc.string(n))
        {
            vec![Some(text)]
        } else {
            array(doc, content)
                .unwrap_or_default()
                .iter()
                .map(|b| {
                    if is(doc, property(doc, *b, "type"), "text") {
                        property(doc, *b, "text").and_then(|n| doc.string(n))
                    } else {
                        None
                    }
                })
                .collect()
        };
        for block in blocks {
            let Some(value) = block else {
                non_text = true;
                continue;
            };
            if trim(value.units()).is_empty() || reminder(value) {
                continue;
            }
            if !characters.is_empty() {
                characters.push('\n');
            }
            for character in char::decode_utf16(value.units().iter().copied()) {
                if characters.len() >= window {
                    break;
                }
                characters.push(character.unwrap_or('\u{fffd}'));
            }
            if characters.len() >= window {
                break;
            }
        }
        if !characters.is_empty() {
            return redact_sensitive(&characters.into_iter().collect::<String>())
                .chars()
                .take(max_chars)
                .collect();
        }
        if non_text {
            return String::new();
        }
    }
    String::new()
}

fn document(value: &Value) -> JsDocument {
    JsDocument::parse(value.to_string().as_bytes()).expect("serde JSON")
}
pub fn build_state(body: &Value, limit: usize) -> Value {
    build_state_document(&document(body), limit).observation()
}
pub fn prompt_excerpt(body: &Value, max_chars: usize) -> String {
    prompt_excerpt_document(&document(body), max_chars)
}
pub fn goal_feedback_indexes(messages: &Value) -> BTreeSet<usize> {
    let doc = document(messages);
    goal_feedback_indexes_document(&doc, Some(doc.root()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn goal(condition: &str) -> Value {
        json!({"role":"user","content":format!("<command-name>/goal</command-name>\n<command-message>goal</command-message>\n<command-args>{condition}</command-args>")})
    }
    fn feedback(condition: &str) -> Value {
        json!({"role":"user","content":format!("Stop hook feedback:\n[{condition}]: Still missing.")})
    }
    #[test]
    fn goal_feedback_preserves_steering_and_requires_full_label_first() {
        let condition = format!("{}😀tail", "x".repeat(499));
        let short = format!("{}… [+6 chars]", "x".repeat(499));
        let assistant = json!({"role":"assistant","content":"Working"});
        let messages = json!([goal(&condition), assistant, feedback(&short), assistant, feedback(&condition), {"role":"user","content":"Use alternate tests"}, assistant, feedback(&short)]);
        assert_eq!(goal_feedback_indexes(&messages), BTreeSet::from([4, 7]));
        assert_eq!(
            build_state(&json!({"messages":messages}), 12000)["current_task"],
            "Use alternate tests"
        );
    }
    #[test]
    fn diagnostics_omit_payloads_and_stop_on_new_attachment_task() {
        let mut body = json!({"messages":[{"role":"user","content":[{"type":"text","text":"<system-reminder>Private setup</system-reminder>"},{"type":"text","text":"Explain 😀diagram"},{"type":"thinking","thinking":"CANARY"}]}]});
        assert_eq!(prompt_excerpt(&body, 9), "Explain 😀");
        body["messages"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"user","content":[{"type":"image","source":{"data":"CANARY"}}]}));
        assert_eq!(prompt_excerpt(&body, 500), "");
        assert!(!build_state(&body, 12000).to_string().contains("CANARY"));
    }
    #[test]
    fn escaped_budget_keeps_task_ends_and_tool_error() {
        let body = json!({"system":"\u{0000}".repeat(1000),"messages":[{"role":"user","content":format!("FIRST {} LAST", "\u{0000}\"\\".repeat(1000))},{"role":"user","content":[{"type":"tool_result","is_error":true,"content":format!("START {} FAILURE", "result ".repeat(8000))}]}]});
        for limit in [2000, 12000] {
            let state = build_state_document(&document(&body), limit);
            assert!(state.stringify().encode_utf16().count() <= limit);
            assert!(state.current_task.to_well_formed().starts_with("FIRST"));
            assert!(state.current_task.to_well_formed().ends_with("LAST"));
            assert!(
                state.recent_messages[0]
                    .1
                    .to_well_formed()
                    .ends_with("FAILURE")
            );
        }
    }
    #[test]
    fn utf16_is_retained_for_classifier_and_repaired_for_logs() {
        let doc = JsDocument::parse(br#"{"messages":[{"role":"user","content":"\ud800A\udc00"}]}"#)
            .unwrap();
        assert_eq!(prompt_excerpt_document(&doc, 500), "\u{fffd}A\u{fffd}");
        assert!(
            build_state_document(&doc, 12000)
                .stringify()
                .contains("\\ud800A\\udc00")
        );
    }
    #[test]
    fn redaction_precedes_excerpt_boundary() {
        let body = json!({"messages":[{"role":"user","content":"Explain api_key=synthetic-secret-token-123456 and continue"}]});
        assert_eq!(prompt_excerpt(&body, 25), "Explain api_key=[REDACTED");
        assert!(
            !build_state(&body, 12000)
                .to_string()
                .contains("synthetic-secret")
        );
    }
}
