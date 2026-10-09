//! Bounded observation of provider bytes. Observation is not delivery evidence.
//!
//! The body adapter preserves the original `Bytes` frames and only records safe
//! model, tool, usage and terminal metadata. Combine clean completion evidence
//! with `transport_completion::Delivery::Flushed` before committing continuity.

use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use autorouter_core::js_json::{JsDocument, JsNode, JsString, NodeId};
use bytes::Bytes;
use hyper::body::{Body, Frame, SizeHint};
use serde::Serialize;
use serde_json::{Map, Value, json};

pub const DEFAULT_BUFFER_BYTES: usize = 64 * 1024;
pub const MAX_TRACKED_BLOCKS: usize = 256;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
const TOKEN_FIELDS: &[&str] = &[
    "input_tokens",
    "output_tokens",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
];
const CACHE_FIELDS: &[&str] = &["ephemeral_5m_input_tokens", "ephemeral_1h_input_tokens"];
const STOP_REASONS: &[&str] = &[
    "end_turn",
    "max_tokens",
    "stop_sequence",
    "tool_use",
    "pause_turn",
    "refusal",
    "model_context_window_exceeded",
];
const ERROR_TYPES: &[&str] = &[
    "invalid_request_error",
    "authentication_error",
    "permission_error",
    "not_found_error",
    "request_too_large",
    "rate_limit_error",
    "api_error",
    "overloaded_error",
    "billing_error",
    "timeout_error",
];

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ToolUse {
    pub id: String,
    pub model: JsString,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CompletionEvidence {
    pub model: JsString,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation_model: Option<JsString>,
    pub stop_reason: String,
    pub tool_uses: Vec<ToolUse>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub enum Observation {
    Model {
        model: JsString,
    },
    Error {
        error_type: &'static str,
    },
    Usage {
        usage: Map<String, Value>,
    },
    Execution {
        model: JsString,
        source: &'static str,
    },
    Complete(CompletionEvidence),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidBufferLimit;

#[derive(Clone, Copy, Eq, PartialEq)]
enum Mode {
    Sse,
    Json,
    Other,
}

fn safe_integer(value: Option<&Value>) -> Option<u64> {
    let number = value?.as_f64()?;
    ((0.0..=MAX_SAFE_INTEGER).contains(&number) && number.fract() == 0.0).then_some(number as u64)
}

// Only observer metadata is materialized; opaque request/tool payloads are
// excluded and the bounded depth covers every consumed protocol path. Exact
// UTF16 strings and IEEE numbers remain authoritative in this semantic view.
#[derive(Clone)]
enum Metadata {
    Null,
    Bool(bool),
    Number(f64),
    String(JsString, Option<String>),
    Array(Vec<Metadata>),
    Object(HashMap<String, Metadata>),
    Opaque,
}
impl Metadata {
    fn from_document(doc: &JsDocument) -> Self {
        Self::from_node(doc, doc.root(), 0)
    }
    fn from_node(doc: &JsDocument, node: NodeId, depth: usize) -> Self {
        match doc.node(node) {
            Some(JsNode::Null) => Self::Null,
            Some(JsNode::Bool(value)) => Self::Bool(*value),
            Some(JsNode::Number(value)) => Self::Number(*value),
            Some(JsNode::String(value)) => Self::String(value.clone(), value.to_scalar()),
            Some(JsNode::Array(values)) if depth < 8 => Self::Array(
                values
                    .iter()
                    .map(|node| Self::from_node(doc, *node, depth + 1))
                    .collect(),
            ),
            Some(JsNode::Object(value)) if depth < 8 => Self::Object(
                value
                    .entries()
                    .iter()
                    .filter_map(|(key, node)| {
                        let key = key.to_scalar()?;
                        const FIELDS: &[&str] = &[
                            "type",
                            "model",
                            "message",
                            "usage",
                            "iterations",
                            "to",
                            "content",
                            "content_block",
                            "index",
                            "delta",
                            "stop_reason",
                            "error",
                            "id",
                            "input_tokens",
                            "output_tokens",
                            "cache_creation_input_tokens",
                            "cache_read_input_tokens",
                            "cache_creation",
                            "ephemeral_5m_input_tokens",
                            "ephemeral_1h_input_tokens",
                            "speed",
                            "inference_geo",
                            "service_tier",
                        ];
                        FIELDS
                            .contains(&key.as_str())
                            .then(|| (key, Self::from_node(doc, *node, depth + 1)))
                    })
                    .collect(),
            ),
            _ => Self::Opaque,
        }
    }
    fn get(&self, key: &str) -> Option<&Self> {
        self.as_object()?.get(key)
    }
    fn as_object(&self) -> Option<&HashMap<String, Self>> {
        if let Self::Object(value) = self {
            Some(value)
        } else {
            None
        }
    }
    fn as_array(&self) -> Option<&Vec<Self>> {
        if let Self::Array(value) = self {
            Some(value)
        } else {
            None
        }
    }
    fn as_str(&self) -> Option<&str> {
        if let Self::String(_, value) = self {
            value.as_deref()
        } else {
            None
        }
    }
    fn as_js_str(&self) -> Option<&JsString> {
        if let Self::String(value, _) = self {
            Some(value)
        } else {
            None
        }
    }
    fn as_f64(&self) -> Option<f64> {
        if let Self::Number(value) = self {
            Some(*value)
        } else {
            None
        }
    }
    fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
    fn is_string(&self) -> bool {
        matches!(self, Self::String(..))
    }
    fn is_object(&self) -> bool {
        matches!(self, Self::Object(..))
    }
}
fn metadata_integer(value: Option<&Metadata>) -> Option<u64> {
    let number = value?.as_f64()?;
    ((0.0..=MAX_SAFE_INTEGER).contains(&number) && number.fract() == 0.0).then_some(number as u64)
}
fn valid_model(value: Option<&Metadata>) -> Option<&JsString> {
    value?.as_js_str().filter(|value| {
        !value.units().is_empty()
            && value.units().len() <= 256
            && !value
                .units()
                .iter()
                .any(|unit| *unit <= 0x1f || *unit == 0x7f)
    })
}
fn valid_tool_id(value: Option<&Metadata>) -> Option<&str> {
    value?.as_str().filter(|value| {
        !value.is_empty()
            && value.len() <= 256
            && value
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'-')
    })
}
fn js_equal(left: Option<&Metadata>, right: Option<&Metadata>) -> bool {
    match (left, right) {
        (None, None) | (Some(Metadata::Null), Some(Metadata::Null)) => true,
        (Some(Metadata::Bool(a)), Some(Metadata::Bool(b))) => a == b,
        (Some(Metadata::String(a, _)), Some(Metadata::String(b, _))) => a == b,
        (Some(Metadata::Number(a)), Some(Metadata::Number(b))) => a == b,
        _ => false,
    }
}

fn js_space(ch: char) -> bool {
    matches!(ch, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}'
        | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

fn line_end(ch: char) -> bool {
    matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn known_oversized_event(prefix: &str, allowed: &[&str]) -> bool {
    let mut line_start = true;
    for (index, ch) in prefix.char_indices() {
        if line_start && let Some(tail) = prefix[index..].strip_prefix("event:") {
            let tail = tail.trim_start_matches(js_space);
            if allowed.iter().any(|kind| {
                tail.strip_prefix(kind).is_some_and(|rest| {
                    rest.is_empty() || rest.chars().next().is_some_and(line_end)
                })
            }) {
                return true;
            }
        }
        line_start = line_end(ch);
    }
    false
}

/// No event history is retained. The caller supplies a bounded, nonblocking
/// metadata sink; panics in the sink cannot interrupt provider forwarding.
pub struct ResponseObserver {
    sink: Box<dyn FnMut(Observation) + Send>,
    mode: Mode,
    active: bool,
    limit: usize,
    buffer: Vec<u8>,
    reported_model: Option<JsString>,
    discarding: bool,
    line_bytes: u8,
    previous_byte: Option<u8>,
    usage: Map<String, Value>,
    invalid_usage: bool,
    started: bool,
    started_model: Option<Metadata>,
    completed: bool,
    final_delta: bool,
    delta_output_known: bool,
    usage_reported: bool,
    serving_model: Option<JsString>,
    stop_reason: Option<String>,
    invalid_execution: bool,
    ambiguous_execution: bool,
    open_blocks: HashMap<u64, Option<ToolUse>>,
    tool_uses: Vec<ToolUse>,
}

impl ResponseObserver {
    pub fn new(
        content_type: &str,
        max_buffer_bytes: usize,
        sink: impl FnMut(Observation) + Send + 'static,
    ) -> Result<Self, InvalidBufferLimit> {
        if max_buffer_bytes == 0 || max_buffer_bytes as f64 > MAX_SAFE_INTEGER {
            return Err(InvalidBufferLimit);
        }
        let media_type = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim_matches(js_space)
            .to_lowercase();
        let mode = if media_type == "text/event-stream" {
            Mode::Sse
        } else if media_type == "application/json" || media_type.ends_with("+json") {
            Mode::Json
        } else {
            Mode::Other
        };
        Ok(Self {
            sink: Box::new(sink),
            mode,
            active: mode != Mode::Other,
            limit: max_buffer_bytes,
            buffer: Vec::new(),
            reported_model: None,
            discarding: false,
            line_bytes: 0,
            previous_byte: None,
            usage: Map::new(),
            invalid_usage: false,
            started: false,
            started_model: None,
            completed: false,
            final_delta: false,
            delta_output_known: false,
            usage_reported: false,
            serving_model: None,
            stop_reason: None,
            invalid_execution: false,
            ambiguous_execution: false,
            open_blocks: HashMap::new(),
            tool_uses: Vec::new(),
        })
    }

    fn emit(&mut self, event: Observation) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.sink)(event)));
    }

    fn stop(&mut self) {
        self.active = false;
        self.buffer = Vec::new();
    }

    fn unsupported_pricing(&mut self) {
        self.usage
            .insert("pricing_unsupported".into(), Value::Bool(true));
    }

    fn report(&mut self, value: Option<&Metadata>, source: &'static str) {
        let Some(model) = valid_model(value) else {
            self.invalid_execution = true;
            return;
        };
        let model = model.to_owned();
        self.serving_model = Some(model.clone());
        if self.reported_model.as_ref() == Some(&model) {
            return;
        }
        self.reported_model = Some(model.clone());
        self.emit(Observation::Model {
            model: model.clone(),
        });
        self.emit(Observation::Execution { model, source });
    }

    fn observe_iterations(&mut self, value: Option<&Metadata>) {
        let Some(iterations) = value
            .and_then(|value| value.get("iterations"))
            .and_then(Metadata::as_array)
        else {
            return;
        };
        let Some(fallback) = iterations
            .iter()
            .rev()
            .find(|value| value.get("type").and_then(Metadata::as_str) == Some("fallback_message"))
        else {
            return;
        };
        let Some(model) = valid_model(fallback.get("model")) else {
            self.ambiguous_execution = true;
            return;
        };
        if Some(model) != self.serving_model.as_ref()
            && (!self.tool_uses.is_empty() || !self.open_blocks.is_empty())
        {
            self.ambiguous_execution = true;
        }
        self.report(fallback.get("model"), "usage_iterations");
    }

    fn observe_fallback(&mut self, block: &Metadata) {
        self.unsupported_pricing();
        let model = block.get("to").and_then(|value| value.get("model"));
        if valid_model(model).is_none() {
            self.invalid_execution = true;
            return;
        }
        if !self.open_blocks.is_empty() {
            self.invalid_execution = true;
        }
        self.tool_uses.clear();
        self.report(model, "fallback");
    }

    fn open_block(&mut self, index: Option<&Metadata>, block: Option<&Metadata>) {
        let index = metadata_integer(index);
        let block_type = block
            .and_then(|value| value.get("type"))
            .and_then(Metadata::as_str);
        if !self.started
            || self.completed
            || self.final_delta
            || index.is_none()
            || block_type.is_none()
            || self.open_blocks.contains_key(&index.unwrap_or_default())
            || self.open_blocks.len() >= MAX_TRACKED_BLOCKS
        {
            self.invalid_execution = true;
            return;
        }
        let block = block.expect("typed block");
        if block_type == Some("fallback") {
            self.observe_fallback(block);
        }
        let mut tool = None;
        if block_type == Some("tool_use") {
            if let (Some(id), Some(model)) =
                (valid_tool_id(block.get("id")), self.serving_model.as_ref())
            {
                if self.tool_uses.len() >= MAX_TRACKED_BLOCKS {
                    self.invalid_execution = true;
                } else {
                    tool = Some(ToolUse {
                        id: id.to_owned(),
                        model: model.clone(),
                    });
                }
            } else {
                self.invalid_execution = true;
            }
        }
        self.open_blocks.insert(index.expect("valid index"), tool);
    }

    fn close_block(&mut self, index: Option<&Metadata>) {
        let Some(block) = metadata_integer(index).and_then(|index| self.open_blocks.remove(&index))
        else {
            self.invalid_execution = true;
            return;
        };
        if let Some(tool) = block {
            if self.tool_uses.iter().any(|other| other.id == tool.id)
                || self.tool_uses.len() >= MAX_TRACKED_BLOCKS
            {
                self.invalid_execution = true;
            } else {
                self.tool_uses.push(tool);
            }
        }
    }

    fn report_completion(&mut self) {
        if !self.started || self.invalid_execution || !self.open_blocks.is_empty() {
            return;
        }
        let (Some(model), Some(stop_reason)) =
            (self.serving_model.clone(), self.stop_reason.as_deref())
        else {
            return;
        };
        let stop_reason = if STOP_REASONS.contains(&stop_reason) {
            stop_reason
        } else {
            "unknown"
        };
        let actionable = if stop_reason == "tool_use" && !self.ambiguous_execution {
            self.tool_uses.clone()
        } else {
            Vec::new()
        };
        let tool_evidence = stop_reason != "tool_use"
            || (!actionable.is_empty() && actionable.iter().all(|tool| tool.model == model));
        let continuation = stop_reason != "refusal"
            && stop_reason != "unknown"
            && !self.ambiguous_execution
            && tool_evidence;
        self.emit(Observation::Complete(CompletionEvidence {
            continuation_model: continuation.then(|| model.clone()),
            model,
            stop_reason: stop_reason.to_owned(),
            tool_uses: actionable,
        }));
    }

    fn update_usage(&mut self, value: Option<&Metadata>) {
        let Some(value) = value else {
            return;
        };
        let Some(value) = value.as_object() else {
            self.invalid_usage = true;
            return;
        };
        if let Some(iterations) = value.get("iterations").filter(|value| !value.is_null()) {
            let valid = iterations.as_array().is_some_and(|iterations| {
                iterations.iter().all(|iteration| {
                    iteration.is_object()
                        && iteration.get("type").and_then(Metadata::as_str) == Some("message")
                        && iteration.get("model").is_none_or(|model| {
                            model.is_string() && js_equal(Some(model), self.started_model.as_ref())
                        })
                        && TOKEN_FIELDS.iter().all(|field| {
                            iteration
                                .get(field)
                                .is_none_or(|value| metadata_integer(Some(value)).is_some())
                        })
                })
            });
            if !valid {
                self.unsupported_pricing();
            }
        }
        for field in TOKEN_FIELDS {
            if let Some(value) = value.get(*field) {
                if let Some(count) = metadata_integer(Some(value)) {
                    self.usage.insert((*field).into(), json!(count));
                } else {
                    self.invalid_usage = true;
                }
            }
        }
        if let Some(cache) = value.get("cache_creation").filter(|value| !value.is_null()) {
            if !cache.is_object() {
                self.invalid_usage = true;
            } else {
                for field in CACHE_FIELDS {
                    if let Some(value) = cache.get(field) {
                        if let Some(count) = metadata_integer(Some(value)) {
                            self.usage
                                .entry("cache_creation")
                                .or_insert_with(|| json!({}))
                                .as_object_mut()
                                .expect("cache object")
                                .insert((*field).into(), json!(count));
                        } else {
                            self.invalid_usage = true;
                        }
                    }
                }
            }
        }
        for (field, allowed) in [
            ("speed", &["standard", "fast"][..]),
            ("inference_geo", &["global", "us", "not_available"][..]),
            (
                "service_tier",
                &["standard", "priority", "batch", "flex"][..],
            ),
        ] {
            if let Some(value) = value.get(field) {
                let safe = if field == "inference_geo" && value.is_null() {
                    "not_available"
                } else {
                    value
                        .as_str()
                        .filter(|text| allowed.contains(text))
                        .unwrap_or("unknown")
                };
                self.usage.insert(field.into(), json!(safe));
            }
        }
    }

    fn report_usage(&mut self) {
        if self.usage_reported
            || self.invalid_usage
            || safe_integer(self.usage.get("input_tokens")).is_none()
            || safe_integer(self.usage.get("output_tokens")).is_none()
        {
            return;
        }
        self.usage_reported = true;
        self.emit(Observation::Usage {
            usage: self.usage.clone(),
        });
    }

    fn report_error(&mut self, kind: Option<&Metadata>) {
        self.stop();
        let error_type = kind
            .and_then(Metadata::as_str)
            .and_then(|kind| ERROR_TYPES.iter().copied().find(|allowed| *allowed == kind))
            .unwrap_or("unknown_error");
        self.emit(Observation::Error { error_type });
    }

    fn parse_frame(&mut self) {
        let text = String::from_utf8_lossy(&self.buffer);
        let mut data = Vec::new();
        let mut event = None;
        for line in text
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
        {
            if let Some(value) = line.strip_prefix("data:") {
                data.push(value.strip_prefix(' ').unwrap_or(value));
            } else if line == "data" {
                data.push("");
            } else if let Some(value) = line.strip_prefix("event:") {
                event = Some(value.trim_matches(js_space));
            }
        }
        let parsed = autorouter_core::js_json::JsDocument::parse(data.join("\n").as_bytes())
            .map(|document| Metadata::from_document(&document));
        let event = event.map(str::to_owned);
        let has_data = !data.is_empty();
        self.buffer.clear();
        let Ok(payload) = parsed else {
            if has_data {
                self.invalid_usage = true;
                self.invalid_execution = true;
            }
            return;
        };
        let is = |kind| {
            payload.get("type").and_then(Metadata::as_str) == Some(kind)
                || event.as_deref() == Some(kind)
        };
        if is("error") {
            self.report_error(payload.get("error").and_then(|error| error.get("type")));
        } else if is("message_start") {
            let message = payload.get("message");
            let model = message.and_then(|message| message.get("model"));
            let usage = message.and_then(|message| message.get("usage"));
            if !self.started {
                self.started = true;
                self.started_model = model.cloned();
                self.report(model, "message_start");
                self.update_usage(usage);
                self.observe_iterations(usage);
            } else {
                if !js_equal(model, self.started_model.as_ref()) {
                    self.unsupported_pricing();
                }
                self.invalid_execution = true;
            }
        } else if is("message_delta") {
            if self.started && !self.completed {
                let usage = payload.get("usage");
                self.update_usage(usage);
                self.observe_iterations(usage);
                self.delta_output_known =
                    metadata_integer(usage.and_then(|value| value.get("output_tokens"))).is_some();
                let stop = payload
                    .get("delta")
                    .and_then(|value| value.get("stop_reason"))
                    .and_then(Metadata::as_str);
                self.final_delta = stop.is_some_and(|text| !text.is_empty());
                if self.final_delta && !self.open_blocks.is_empty() {
                    self.invalid_execution = true;
                }
                self.stop_reason = stop.filter(|text| !text.is_empty()).map(str::to_owned);
                if stop == Some("refusal") {
                    self.unsupported_pricing();
                }
            }
        } else if is("message_stop") {
            self.completed = self.started;
        } else if is("content_block_start") {
            let block = payload.get("content_block");
            if block
                .and_then(|block| block.get("type"))
                .and_then(Metadata::as_str)
                == Some("fallback")
            {
                self.unsupported_pricing();
            }
            self.open_block(payload.get("index"), block);
        } else if is("content_block_stop") {
            self.close_block(payload.get("index"));
        }
    }

    fn oversized_frame(&mut self) {
        let prefix = String::from_utf8_lossy(&self.buffer);
        // Equivalent to the existing anchored multiline event envelope checks.
        // Only known content/ping frames can be skipped without losing evidence.
        if !known_oversized_event(
            &prefix,
            &["content_block_delta", "content_block_stop", "ping"],
        ) {
            self.invalid_usage = true;
        }
        if !known_oversized_event(&prefix, &["content_block_delta", "ping"]) {
            self.invalid_execution = true;
        }
        self.discarding = true;
        self.buffer.clear();
    }

    /// Observe a borrowed chunk without modifying or retaining its payload.
    pub fn push(&mut self, chunk: &[u8]) {
        if !self.active {
            return;
        }
        if self.buffer.capacity() == 0 && self.buffer.try_reserve_exact(self.limit).is_err() {
            self.stop();
            return;
        }
        if self.mode == Mode::Json {
            if chunk.len() > self.limit - self.buffer.len() {
                self.stop();
                return;
            }
            self.buffer.extend_from_slice(chunk);
            return;
        }
        for &byte in chunk {
            if !self.discarding {
                if self.buffer.len() == self.limit {
                    self.oversized_frame();
                } else {
                    self.buffer.push(byte);
                }
            }
            let end = byte == b'\n'
                && (self.line_bytes == 0
                    || (self.line_bytes == 1 && self.previous_byte == Some(b'\r')));
            self.line_bytes = if byte == b'\n' {
                0
            } else {
                (self.line_bytes + 1).min(2)
            };
            self.previous_byte = Some(byte);
            if end {
                if self.discarding {
                    self.discarding = false;
                    self.buffer.clear();
                } else {
                    self.parse_frame();
                }
                if !self.active {
                    return;
                }
            }
        }
    }

    fn finish_json(&mut self, payload: &Metadata) {
        if payload.get("type").and_then(Metadata::as_str) == Some("error") {
            self.report_error(payload.get("error").and_then(|error| error.get("type")));
            return;
        }
        self.started = true;
        self.started_model = payload.get("model").cloned();
        self.report(payload.get("model"), "message");
        self.update_usage(payload.get("usage"));
        if let Some(content) = payload.get("content").and_then(Metadata::as_array) {
            let mut final_fallback_model = None;
            for block in content {
                let kind = block.get("type").and_then(Metadata::as_str);
                if kind.is_none() {
                    self.invalid_execution = true;
                }
                if kind == Some("fallback") {
                    self.unsupported_pricing();
                    self.tool_uses.clear();
                    if let Some(model) =
                        valid_model(block.get("to").and_then(|value| value.get("model")))
                    {
                        final_fallback_model = Some(model);
                    } else {
                        self.ambiguous_execution = true;
                    }
                } else if kind == Some("tool_use") {
                    if let Some(id) = valid_tool_id(block.get("id")) {
                        if self.tool_uses.len() >= MAX_TRACKED_BLOCKS
                            || self.tool_uses.iter().any(|tool| tool.id == id)
                        {
                            self.invalid_execution = true;
                        } else {
                            self.tool_uses.push(ToolUse {
                                id: id.to_owned(),
                                model: self
                                    .serving_model
                                    .clone()
                                    .unwrap_or_else(|| JsString::from_scalar("")),
                            });
                        }
                    } else {
                        self.invalid_execution = true;
                    }
                }
            }
            if final_fallback_model.is_some()
                && final_fallback_model != payload.get("model").and_then(Metadata::as_js_str)
            {
                self.ambiguous_execution = true;
            }
        } else {
            self.invalid_execution = true;
        }
        self.observe_iterations(payload.get("usage"));
        self.stop_reason = payload
            .get("stop_reason")
            .and_then(Metadata::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned);
        if self.stop_reason.as_deref() == Some("refusal") {
            self.unsupported_pricing();
        }
        self.report_usage();
        self.report_completion();
    }

    /// Invoke only at clean upstream body EOF, never at a final SSE delta alone.
    pub fn finish(&mut self) {
        if self.active && self.mode == Mode::Json && !self.buffer.is_empty() {
            if let Ok(document) = autorouter_core::js_json::JsDocument::parse(&self.buffer) {
                let payload = Metadata::from_document(&document);
                self.finish_json(&payload);
            }
        } else if self.active
            && self.mode == Mode::Sse
            && self.buffer.is_empty()
            && !self.discarding
            && (self.completed || self.final_delta)
        {
            if self.delta_output_known {
                self.report_usage();
            }
            self.report_completion();
        }
        self.stop();
    }

    pub fn destroy(&mut self) {
        self.stop();
    }
}

/// A demand-driven body adapter. Wrap this with `CompletionRegistry::track`:
/// provider evidence arrives first; successful downstream flush arrives later.
pub struct ObservedBody<B> {
    inner: Pin<Box<B>>,
    observer: ResponseObserver,
    finished: bool,
}

impl<B> ObservedBody<B> {
    pub fn new(inner: B, observer: ResponseObserver) -> Self {
        Self {
            inner: Box::pin(inner),
            observer,
            finished: false,
        }
    }
}

impl<B: Body<Data = Bytes>> Body for ObservedBody<B> {
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, B::Error>>> {
        let this = self.get_mut();
        match this.inner.as_mut().poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.observer.push(data);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(None) => {
                this.finished = true;
                this.observer.finish();
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(error))) => {
                this.finished = true;
                this.observer.destroy();
                Poll::Ready(Some(Err(error)))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.finished
    }
    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}

impl<B> Drop for ObservedBody<B> {
    fn drop(&mut self) {
        if !self.finished {
            self.observer.destroy();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::{BodyExt, Full};
    use std::sync::{Arc, Mutex};

    type Events = Arc<Mutex<Vec<Observation>>>;

    fn observer(content_type: &str, limit: usize) -> (ResponseObserver, Events) {
        let events = Events::default();
        let target = events.clone();
        let observer = ResponseObserver::new(content_type, limit, move |event| {
            target.lock().unwrap().push(event)
        })
        .unwrap();
        (observer, events)
    }

    fn frame(value: Value) -> Vec<u8> {
        format!(
            "event: {}\ndata: {value}\n\n",
            value["type"].as_str().unwrap()
        )
        .into_bytes()
    }

    fn start(model: &str) -> Vec<u8> {
        frame(
            json!({"type":"message_start","message":{"model":model,"usage":{"input_tokens":100,"output_tokens":0}}}),
        )
    }

    fn end(reason: &str) -> Vec<u8> {
        frame(
            json!({"type":"message_delta","delta":{"stop_reason":reason},"usage":{"output_tokens":12}}),
        )
    }

    fn open(index: u64, block: Value) -> Vec<u8> {
        frame(json!({"type":"content_block_start","index":index,"content_block":block}))
    }

    fn close(index: u64) -> Vec<u8> {
        frame(json!({"type":"content_block_stop","index":index}))
    }

    fn observed(chunks: &[Vec<u8>], limit: usize) -> Vec<Observation> {
        let (mut observer, events) = observer("text/event-stream", limit);
        for chunk in chunks {
            observer.push(chunk);
        }
        observer.finish();
        events.lock().unwrap().clone()
    }

    fn completion(events: &[Observation]) -> Option<&CompletionEvidence> {
        events.iter().find_map(|event| match event {
            Observation::Complete(value) => Some(value),
            _ => None,
        })
    }

    fn usage(events: &[Observation]) -> Option<&Map<String, Value>> {
        events.iter().find_map(|event| match event {
            Observation::Usage { usage } => Some(usage),
            _ => None,
        })
    }

    #[test]
    fn consumed_utf16_models_remain_distinct_in_completion_and_fallback_checks() {
        let (mut observer, events) = observer("application/json", DEFAULT_BUFFER_BYTES);
        observer.push(br#"{"model":"vendor-\ud800","content":[{"type":"tool_use","id":"tool_1"}],"stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":2}}"#);
        observer.finish();
        let events = events.lock().unwrap();
        let evidence = completion(&events).unwrap();
        let expected = JsString::from_utf16("vendor-".encode_utf16().chain([0xd800]).collect());
        assert_eq!(evidence.model, expected);
        assert_eq!(evidence.continuation_model.as_ref(), Some(&expected));
        assert_eq!(evidence.tool_uses[0].model, expected);
        assert!(
            serde_json::to_value(evidence).is_err(),
            "scalar JSON must not repair identity"
        );
        drop(events);

        let (mut observer, events) = self::observer("application/json", DEFAULT_BUFFER_BYTES);
        observer.push(br#"{"model":"vendor-\ud800","content":[{"type":"fallback","to":{"model":"vendor-\ud801"}}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":2}}"#);
        observer.finish();
        assert!(
            completion(&events.lock().unwrap())
                .unwrap()
                .continuation_model
                .is_none()
        );
    }

    #[test]
    fn clean_eof_is_required_after_final_delta_and_observation_is_immediate() {
        let (mut observer, events) =
            observer(" TEXT/EVENT-STREAM ; charset=utf-8", DEFAULT_BUFFER_BYTES);
        observer.push(&start("opus"));
        assert_eq!(events.lock().unwrap().len(), 2);
        observer.push(&end("end_turn"));
        assert!(completion(&events.lock().unwrap()).is_none());
        observer.finish();
        let locked = events.lock().unwrap();
        assert_eq!(
            completion(&locked)
                .unwrap()
                .continuation_model
                .as_ref()
                .and_then(JsString::to_scalar)
                .as_deref(),
            Some("opus")
        );
        assert_eq!(usage(&locked).unwrap()["output_tokens"], 12);
        drop(locked);
        observer.finish();
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| matches!(event, Observation::Complete(_)))
                .count(),
            1
        );
    }

    #[test]
    fn every_chunk_boundary_preserves_unicode_crlf_comments_and_multiline_data() {
        let text = format!(
            ": keepalive\r\nevent: ping\r\ndata: {{}}\r\n\r\n{}event: content_block_delta\r\ndata: {{\"type\":\"content_block_delta\",\r\ndata: \"delta\":{{\"text\":\"😀é終\"}}}}\r\n\r\n{}",
            String::from_utf8(start("opus"))
                .unwrap()
                .replace('\n', "\r\n"),
            String::from_utf8(end("end_turn"))
                .unwrap()
                .replace('\n', "\r\n")
        );
        let bytes = text.as_bytes();
        let expected = observed(&[bytes.to_vec()], DEFAULT_BUFFER_BYTES);
        assert!(completion(&expected).is_some());
        for split in 0..=bytes.len() {
            assert_eq!(
                observed(
                    &[bytes[..split].to_vec(), bytes[split..].to_vec()],
                    DEFAULT_BUFFER_BYTES
                ),
                expected,
                "split {split}"
            );
        }
        assert_eq!(
            observed(
                &bytes.iter().map(|byte| vec![*byte]).collect::<Vec<_>>(),
                DEFAULT_BUFFER_BYTES
            ),
            expected
        );
    }

    #[test]
    fn many_ping_frames_do_not_consume_the_per_frame_budget() {
        let mut chunks = vec![start("sonnet")];
        for _ in 0..1000 {
            chunks.push(b": comment\nevent: ping\ndata: {}\n\n".to_vec());
        }
        chunks.push(end("end_turn"));
        let events = observed(&chunks, 256);
        assert!(completion(&events).is_some());
        assert!(usage(&events).is_some());
    }

    #[test]
    fn oversized_output_is_skipped_but_later_errors_remain_sanitized() {
        let huge = frame(json!({"type":"content_block_delta","delta":{"text":"x".repeat(4000)}}));
        let events = observed(&[start("opus"), huge.clone(), end("end_turn")], 256);
        assert!(completion(&events).is_some());
        assert!(usage(&events).is_some());
        let events = observed(
            &[
                start("opus"),
                huge,
                frame(
                    json!({"type":"error","error":{"type":"PRIVATE_SECRET","message":"PRIVATE_PAYLOAD"}}),
                ),
                end("end_turn"),
            ],
            256,
        );
        assert_eq!(
            events.last(),
            Some(&Observation::Error {
                error_type: "unknown_error"
            })
        );
        assert!(completion(&events).is_none());
        assert!(!serde_json::to_string(&events).unwrap().contains("PRIVATE"));
    }

    #[test]
    fn skipped_metadata_and_malformed_frames_never_invent_completion() {
        for corrupt in [
            frame(json!({"type":"message_delta","padding":"x".repeat(1000)})),
            b"event: content_block_delta\ndata: {broken}\n\n".to_vec(),
            open(
                0,
                json!({"type":"tool_use","id":"tool","name":"Read","padding":"x".repeat(1000)}),
            ),
        ] {
            let events = observed(&[start("opus"), corrupt, end("end_turn")], 256);
            assert!(completion(&events).is_none());
            assert!(usage(&events).is_none());
        }
        let events = observed(
            &[
                start("opus"),
                end("end_turn"),
                b"data: {unfinished".to_vec(),
            ],
            256,
        );
        assert!(completion(&events).is_none());
    }

    #[test]
    fn destruction_after_terminal_frame_cannot_publish_completion_or_usage() {
        let (mut observer, events) = observer("text/event-stream", 256);
        observer.push(&start("sonnet"));
        observer.push(&end("end_turn"));
        observer.destroy();
        observer.finish();
        assert!(completion(&events.lock().unwrap()).is_none());
        assert!(usage(&events.lock().unwrap()).is_none());
    }

    #[test]
    fn cumulative_usage_replaces_counts_and_retains_only_safe_metadata() {
        let events = observed(
            &[
                frame(
                    json!({"type":"message_start","message":{"model":"opus","usage":{
                "input_tokens":100.0,"output_tokens":0,"cache_creation_input_tokens":20,
                "cache_creation":{"ephemeral_5m_input_tokens":5,"ephemeral_1h_input_tokens":15},
                "speed":"fast","inference_geo":null,"service_tier":"priority","private":"SECRET"}}}),
                ),
                frame(json!({"type":"message_delta","delta":{},"usage":{"output_tokens":5}})),
                end("end_turn"),
            ],
            DEFAULT_BUFFER_BYTES,
        );
        let usage = usage(&events).unwrap();
        assert_eq!(usage["input_tokens"], 100);
        assert_eq!(usage["output_tokens"], 12);
        assert_eq!(usage["cache_creation"]["ephemeral_1h_input_tokens"], 15);
        assert_eq!(usage["inference_geo"], "not_available");
        assert!(!usage.contains_key("private"));
    }

    #[test]
    fn invalid_usage_does_not_erase_independent_execution_evidence() {
        for count in [
            json!(-1),
            json!(1.2),
            json!("12"),
            Value::Null,
            json!(9007199254740992_u64),
        ] {
            let events = observed(
                &[
                    start("opus"),
                    frame(
                        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":count}}),
                    ),
                ],
                1024,
            );
            assert!(usage(&events).is_none());
            assert!(completion(&events).is_some());
        }
    }

    #[test]
    fn fallback_boundaries_discard_earlier_tools_and_use_final_model() {
        let events = observed(
            &[
                start("opus"),
                open(0, json!({"type":"tool_use","id":"old","name":"Read"})),
                close(0),
                open(1, json!({"type":"fallback","to":{"model":"sonnet"}})),
                close(1),
                open(2, json!({"type":"tool_use","id":"middle","name":"Read"})),
                close(2),
                open(3, json!({"type":"fallback","to":{"model":"haiku"}})),
                close(3),
                open(4, json!({"type":"tool_use","id":"final","name":"Read"})),
                close(4),
                end("tool_use"),
            ],
            1024,
        );
        let evidence = completion(&events).unwrap();
        assert_eq!(
            evidence
                .continuation_model
                .as_ref()
                .and_then(JsString::to_scalar)
                .as_deref(),
            Some("haiku")
        );
        assert_eq!(
            evidence.tool_uses,
            [ToolUse {
                id: "final".into(),
                model: "haiku".into()
            }]
        );
        assert_eq!(usage(&events).unwrap()["pricing_unsupported"], true);
    }

    #[test]
    fn sticky_fallback_iterations_cannot_reassign_an_existing_tool() {
        let fallback = frame(
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{
            "output_tokens":12,"iterations":[{"type":"fallback_message","model":"sonnet"}]}}),
        );
        let events = observed(
            &[
                start("opus"),
                open(0, json!({"type":"tool_use","id":"old","name":"Read"})),
                close(0),
                fallback,
            ],
            1024,
        );
        let evidence = completion(&events).unwrap();
        assert_eq!(evidence.model, JsString::from_scalar("sonnet"));
        assert_eq!(evidence.continuation_model, None);
        assert!(evidence.tool_uses.is_empty());
        let events = observed(
            &[
                start("opus"),
                frame(
                    json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{
            "output_tokens":12,"iterations":[{"type":"fallback_message","model":"sonnet"}]}}),
                ),
            ],
            1024,
        );
        assert_eq!(
            completion(&events)
                .unwrap()
                .continuation_model
                .as_ref()
                .and_then(JsString::to_scalar)
                .as_deref(),
            Some("sonnet")
        );
    }

    #[test]
    fn open_duplicate_invalid_excessive_or_missing_client_tools_never_pin() {
        let cases = [
            vec![open(
                0,
                json!({"type":"tool_use","id":"open","name":"Read"}),
            )],
            vec![
                open(0, json!({"type":"tool_use","id":"bad id","name":"Read"})),
                close(0),
            ],
            vec![close(99)],
            vec![
                open(0, json!({"type":"tool_use","id":"dup","name":"Read"})),
                close(0),
                open(1, json!({"type":"tool_use","id":"dup","name":"Read"})),
                close(1),
            ],
            vec![
                open(
                    0,
                    json!({"type":"server_tool_use","id":"server","name":"search"}),
                ),
                close(0),
            ],
        ];
        for blocks in cases {
            let mut chunks = vec![start("opus")];
            chunks.extend(blocks);
            chunks.push(end("tool_use"));
            assert!(
                completion(&observed(&chunks, 1024))
                    .is_none_or(|evidence| evidence.continuation_model.is_none())
            );
        }
        let mut chunks = vec![start("opus")];
        for index in 0..257 {
            chunks.push(open(
                index,
                json!({"type":"tool_use","id":format!("t{index}"),"name":"Read"}),
            ));
            chunks.push(close(index));
        }
        chunks.push(end("tool_use"));
        assert!(completion(&observed(&chunks, 1024)).is_none());
    }

    #[test]
    fn unknown_or_refused_stops_never_pin_and_private_stop_names_are_sanitized() {
        for reason in ["refusal", "PRIVATE_NEW_STOP"] {
            let events = observed(&[start("opus"), end(reason)], 1024);
            let evidence = completion(&events).unwrap();
            assert!(evidence.continuation_model.is_none());
            assert_eq!(
                evidence.stop_reason,
                if reason == "refusal" {
                    "refusal"
                } else {
                    "unknown"
                }
            );
        }
    }

    #[test]
    fn json_final_boundary_and_nullable_cache_usage_preserve_native_behavior() {
        let (mut observer, events) = observer("application/vendor+json", 4096);
        let payload = json!({"model":"sonnet","content":[
            {"type":"tool_use","id":"old","name":"Read"},
            {"type":"fallback","to":{"model":"sonnet"}},
            {"type":"tool_use","id":"final","name":"Read"}],
            "stop_reason":"tool_use","usage":{"input_tokens":100,"output_tokens":12,"cache_creation":null}});
        let bytes = serde_json::to_vec(&payload).unwrap();
        for chunk in bytes.chunks(3) {
            observer.push(chunk);
        }
        observer.finish();
        let locked = events.lock().unwrap();
        let evidence = completion(&locked).unwrap();
        assert_eq!(
            evidence.tool_uses,
            [ToolUse {
                id: "final".into(),
                model: "sonnet".into()
            }]
        );
        assert_eq!(
            evidence
                .continuation_model
                .as_ref()
                .and_then(JsString::to_scalar)
                .as_deref(),
            Some("sonnet")
        );
        assert!(usage(&locked).is_some());
    }

    #[test]
    fn json_oversize_unknown_media_and_malformed_data_cannot_emit_metadata() {
        for (media, bytes, limit) in [
            (
                "application/octet-stream",
                br#"{"model":"opus"}"#.to_vec(),
                1024,
            ),
            (
                "application/json",
                br#"{"model":"opus","padding":"too long"}"#.to_vec(),
                8,
            ),
            ("application/json", b"not json".to_vec(), 1024),
        ] {
            let (mut observer, events) = observer(media, limit);
            observer.push(&bytes);
            observer.finish();
            assert!(events.lock().unwrap().is_empty());
        }
        assert!(ResponseObserver::new("application/json", 0, |_| {}).is_err());
    }

    #[test]
    fn iteration_accounting_is_cumulative_and_unknown_shapes_are_unsupported() {
        for (iterations, unsupported) in [
            (
                json!([{"type":"message","model":"opus","input_tokens":100}]),
                false,
            ),
            (json!([{"type":"message","model":"other"}]), true),
            (json!([{"type":"advisor"}]), true),
            (json!([{"type":"message","output_tokens":-1}]), true),
            (json!("private"), true),
        ] {
            let events = observed(
                &[
                    start("opus"),
                    frame(
                        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12,"iterations":iterations}}),
                    ),
                ],
                1024,
            );
            assert_eq!(
                usage(&events).unwrap().get("pricing_unsupported") == Some(&Value::Bool(true)),
                unsupported
            );
            assert_eq!(usage(&events).unwrap()["input_tokens"], 100);
        }
    }

    #[tokio::test]
    async fn body_adapter_retains_original_bytes_and_defers_finish_until_actual_eof() {
        let bytes = Bytes::from([start("opus"), end("end_turn")].concat());
        let pointer = bytes.as_ptr();
        let (observer, events) = observer("text/event-stream", 1024);
        let mut body = ObservedBody::new(Full::new(bytes.clone()), observer);
        let returned = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(returned.as_ptr(), pointer);
        assert_eq!(returned, bytes);
        assert!(completion(&events.lock().unwrap()).is_none());
        assert!(body.frame().await.is_none());
        assert!(completion(&events.lock().unwrap()).is_some());
    }

    #[tokio::test]
    async fn callback_panics_do_not_change_forwarded_bytes() {
        let bytes = Bytes::from([start("opus"), end("end_turn")].concat());
        let observer = ResponseObserver::new("text/event-stream", 1024, |_| {
            panic!("synthetic callback failure")
        })
        .unwrap();
        let body = ObservedBody::new(Full::new(bytes.clone()), observer);
        assert_eq!(body.collect().await.unwrap().to_bytes(), bytes);
    }
}
