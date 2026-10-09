//! JSON.parse/JSON.stringify-compatible data, without recursive parser stacks.
//!
//! JavaScript strings are UTF-16 sequences, not necessarily Unicode scalar
//! strings. Provider JSON must retain lone surrogate escapes and IEEE-754
//! number semantics, including overflow, duplicate keys and integer-key order.
//! An arena also makes dropping deeply nested opaque input non-recursive.
//!
//! This document is the authoritative wire representation. The explicitly
//! lossy observation projection is never suitable for forwarding requests.
use std::collections::HashMap;
use std::fmt;

pub type NodeId = usize;

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct JsString(Vec<u16>);

impl JsString {
    pub fn from_utf16(units: Vec<u16>) -> Self {
        Self(units)
    }
    pub fn from_scalar(value: &str) -> Self {
        Self(value.encode_utf16().collect())
    }
    pub fn units(&self) -> &[u16] {
        &self.0
    }
    pub fn to_scalar(&self) -> Option<String> {
        String::from_utf16(&self.0).ok()
    }
    pub fn to_well_formed(&self) -> String {
        String::from_utf16_lossy(&self.0)
    }
    pub fn stringify(&self) -> String {
        let mut output = String::new();
        quote(self, &mut output);
        output
    }

    fn array_index(&self) -> Option<u32> {
        if self.0.is_empty() || self.0.len() > 10 || (self.0.len() > 1 && self.0[0] == b'0' as u16)
        {
            return None;
        }
        let mut number = 0_u32;
        for &unit in &self.0 {
            if !(b'0' as u16..=b'9' as u16).contains(&unit) {
                return None;
            }
            number = number
                .checked_mul(10)?
                .checked_add(u32::from(unit - b'0' as u16))?;
        }
        (number != u32::MAX).then_some(number)
    }
}

impl fmt::Debug for JsString {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(output, "JsString(<{} UTF-16 units>)", self.0.len())
    }
}

impl From<&str> for JsString {
    fn from(value: &str) -> Self {
        Self::from_scalar(value)
    }
}
impl From<String> for JsString {
    fn from(value: String) -> Self {
        Self::from_scalar(&value)
    }
}
// serde's string model cannot represent unpaired UTF16. Refuse that boundary
// rather than silently collapse two distinct identities; exact wire users must
// use stringify(), and display-only callers explicitly use to_well_formed().
impl serde::Serialize for JsString {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let scalar = self.to_scalar().ok_or_else(|| {
            serde::ser::Error::custom("UTF16 string requires exact JSON serialization")
        })?;
        serializer.serialize_str(&scalar)
    }
}

#[derive(Clone, Default)]
pub struct JsObject {
    entries: Vec<(JsString, NodeId)>,
    positions: HashMap<JsString, usize>,
}

impl JsObject {
    pub fn get(&self, key: &str) -> Option<NodeId> {
        self.positions
            .get(&JsString::from_scalar(key))
            .map(|&index| self.entries[index].1)
    }
    pub fn entries(&self) -> &[(JsString, NodeId)] {
        &self.entries
    }
    /// Enumerable property order used by JSON.stringify: integer indices
    /// first, then the remaining keys in their original insertion order.
    pub fn ordered_entries(
        &self,
    ) -> impl DoubleEndedIterator<Item = &(JsString, NodeId)> + ExactSizeIterator {
        self.ordered().into_iter().map(|index| &self.entries[index])
    }
    fn insert(&mut self, key: JsString, value: NodeId) {
        if let Some(&index) = self.positions.get(&key) {
            self.entries[index].1 = value;
        } else {
            self.positions.insert(key.clone(), self.entries.len());
            self.entries.push((key, value));
        }
    }
    fn ordered(&self) -> Vec<usize> {
        let mut indexed = Vec::new();
        let mut ordinary = Vec::new();
        for (index, (key, _)) in self.entries.iter().enumerate() {
            if let Some(number) = key.array_index() {
                indexed.push((number, index));
            } else {
                ordinary.push(index);
            }
        }
        indexed.sort_unstable_by_key(|&(number, _)| number);
        indexed
            .into_iter()
            .map(|(_, index)| index)
            .chain(ordinary)
            .collect()
    }
}

#[derive(Clone)]
pub enum JsNode {
    Null,
    Bool(bool),
    Number(f64),
    String(JsString),
    Array(Vec<NodeId>),
    Object(JsObject),
}

#[derive(Clone)]
pub struct JsDocument {
    nodes: Vec<JsNode>,
    root: NodeId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JsonError;

impl fmt::Display for JsonError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("Invalid JSON")
    }
}
impl std::error::Error for JsonError {}

#[derive(Clone, Copy)]
enum Frame {
    ArrayValue { node: NodeId, allow_end: bool },
    ArrayAfter { node: NodeId },
    ObjectKey { node: NodeId, allow_end: bool },
    ObjectAfter { node: NodeId },
}

struct Parser<'a> {
    text: &'a str,
    offset: usize,
    nodes: Vec<JsNode>,
}

impl<'a> Parser<'a> {
    fn byte(&self) -> Option<u8> {
        self.text.as_bytes().get(self.offset).copied()
    }
    fn whitespace(&mut self) {
        while matches!(self.byte(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.offset += 1;
        }
    }
    fn take(&mut self, byte: u8) -> Result<(), JsonError> {
        self.whitespace();
        if self.byte() != Some(byte) {
            return Err(JsonError);
        }
        self.offset += 1;
        Ok(())
    }
    fn add(&mut self, node: JsNode) -> NodeId {
        let index = self.nodes.len();
        self.nodes.push(node);
        index
    }
    fn literal(&mut self, text: &str, node: JsNode) -> Result<NodeId, JsonError> {
        if !self.text[self.offset..].starts_with(text) {
            return Err(JsonError);
        }
        self.offset += text.len();
        Ok(self.add(node))
    }
    fn string(&mut self) -> Result<JsString, JsonError> {
        self.take(b'"')?;
        let mut units = Vec::new();
        loop {
            match self.byte().ok_or(JsonError)? {
                b'"' => {
                    self.offset += 1;
                    return Ok(JsString(units));
                }
                b'\\' => {
                    self.offset += 1;
                    let escaped = self.byte().ok_or(JsonError)?;
                    self.offset += 1;
                    units.push(match escaped {
                        b'"' | b'\\' | b'/' => u16::from(escaped),
                        b'b' => 8,
                        b'f' => 12,
                        b'n' => 10,
                        b'r' => 13,
                        b't' => 9,
                        b'u' => {
                            let mut value = 0_u16;
                            for _ in 0..4 {
                                let digit = match self.byte().ok_or(JsonError)? {
                                    ch @ b'0'..=b'9' => ch - b'0',
                                    ch @ b'a'..=b'f' => ch - b'a' + 10,
                                    ch @ b'A'..=b'F' => ch - b'A' + 10,
                                    _ => return Err(JsonError),
                                };
                                value = value * 16 + u16::from(digit);
                                self.offset += 1;
                            }
                            value
                        }
                        _ => return Err(JsonError),
                    });
                }
                0..=31 => return Err(JsonError),
                byte @ 0x20..=0x7f => {
                    // ASCII descriptions dominate large tool catalogs. Avoid
                    // decoding and re-encoding one Unicode scalar per byte.
                    units.push(u16::from(byte));
                    self.offset += 1;
                }
                _ => {
                    let ch = self.text[self.offset..].chars().next().ok_or(JsonError)?;
                    let mut encoded = [0; 2];
                    units.extend_from_slice(ch.encode_utf16(&mut encoded));
                    self.offset += ch.len_utf8();
                }
            }
        }
    }
    fn digits(&mut self) -> Result<(), JsonError> {
        let start = self.offset;
        while self.byte().is_some_and(|byte| byte.is_ascii_digit()) {
            self.offset += 1;
        }
        if self.offset == start {
            Err(JsonError)
        } else {
            Ok(())
        }
    }
    fn number(&mut self) -> Result<NodeId, JsonError> {
        let start = self.offset;
        if self.byte() == Some(b'-') {
            self.offset += 1;
        }
        match self.byte() {
            Some(b'0') => self.offset += 1,
            Some(b'1'..=b'9') => self.digits()?,
            _ => return Err(JsonError),
        }
        if self.byte() == Some(b'.') {
            self.offset += 1;
            self.digits()?;
        }
        if matches!(self.byte(), Some(b'e' | b'E')) {
            self.offset += 1;
            if matches!(self.byte(), Some(b'+' | b'-')) {
                self.offset += 1;
            }
            self.digits()?;
        }
        let number = self.text[start..self.offset]
            .parse::<f64>()
            .map_err(|_| JsonError)?;
        Ok(self.add(JsNode::Number(number)))
    }
    fn value(&mut self, stack: &mut Vec<Frame>) -> Result<NodeId, JsonError> {
        self.whitespace();
        match self.byte().ok_or(JsonError)? {
            b'n' => self.literal("null", JsNode::Null),
            b't' => self.literal("true", JsNode::Bool(true)),
            b'f' => self.literal("false", JsNode::Bool(false)),
            b'"' => {
                let value = self.string()?;
                Ok(self.add(JsNode::String(value)))
            }
            b'[' => {
                self.offset += 1;
                let node = self.add(JsNode::Array(Vec::new()));
                stack.push(Frame::ArrayValue {
                    node,
                    allow_end: true,
                });
                Ok(node)
            }
            b'{' => {
                self.offset += 1;
                let node = self.add(JsNode::Object(JsObject::default()));
                stack.push(Frame::ObjectKey {
                    node,
                    allow_end: true,
                });
                Ok(node)
            }
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(JsonError),
        }
    }
    fn parse(mut self) -> Result<JsDocument, JsonError> {
        let mut stack = Vec::new();
        let root = self.value(&mut stack)?;
        while let Some(frame) = stack.pop() {
            self.whitespace();
            match frame {
                Frame::ArrayValue { node, allow_end } => {
                    if allow_end && self.byte() == Some(b']') {
                        self.offset += 1;
                        continue;
                    }
                    stack.push(Frame::ArrayAfter { node });
                    let child = self.value(&mut stack)?;
                    let JsNode::Array(values) = &mut self.nodes[node] else {
                        unreachable!()
                    };
                    values.push(child);
                }
                Frame::ArrayAfter { node } => match self.byte() {
                    Some(b',') => {
                        self.offset += 1;
                        stack.push(Frame::ArrayValue {
                            node,
                            allow_end: false,
                        });
                    }
                    Some(b']') => self.offset += 1,
                    _ => return Err(JsonError),
                },
                Frame::ObjectKey { node, allow_end } => {
                    if allow_end && self.byte() == Some(b'}') {
                        self.offset += 1;
                        continue;
                    }
                    let key = self.string()?;
                    self.take(b':')?;
                    stack.push(Frame::ObjectAfter { node });
                    let child = self.value(&mut stack)?;
                    let JsNode::Object(object) = &mut self.nodes[node] else {
                        unreachable!()
                    };
                    object.insert(key, child);
                }
                Frame::ObjectAfter { node } => match self.byte() {
                    Some(b',') => {
                        self.offset += 1;
                        stack.push(Frame::ObjectKey {
                            node,
                            allow_end: false,
                        });
                    }
                    Some(b'}') => self.offset += 1,
                    _ => return Err(JsonError),
                },
            }
        }
        self.whitespace();
        if self.offset != self.text.len() {
            return Err(JsonError);
        }
        Ok(JsDocument {
            nodes: self.nodes,
            root,
        })
    }
}

fn quote(value: &JsString, output: &mut String) {
    output.reserve(value.0.len().saturating_add(2));
    output.push('"');
    let mut index = 0;
    while index < value.0.len() {
        let unit = value.0[index];
        index += 1;
        match unit {
            8 => output.push_str("\\b"),
            9 => output.push_str("\\t"),
            10 => output.push_str("\\n"),
            12 => output.push_str("\\f"),
            13 => output.push_str("\\r"),
            34 => output.push_str("\\\""),
            92 => output.push_str("\\\\"),
            32..=127 => output.push(char::from(unit as u8)),
            0..=31 => {
                use std::fmt::Write;
                write!(output, "\\u{unit:04x}").expect("string writing");
            }
            0xd800..=0xdbff
                if value
                    .0
                    .get(index)
                    .is_some_and(|next| (0xdc00..=0xdfff).contains(next)) =>
            {
                let low = value.0[index];
                index += 1;
                let scalar = 0x10000 + ((u32::from(unit) - 0xd800) << 10) + u32::from(low) - 0xdc00;
                output.push(char::from_u32(scalar).expect("surrogate pair"));
            }
            0xd800..=0xdfff => {
                use std::fmt::Write;
                write!(output, "\\u{unit:04x}").expect("string writing");
            }
            _ => output.push(char::from_u32(u32::from(unit)).expect("non-surrogate unit")),
        }
    }
    output.push('"');
}

enum WriteItem<'a> {
    Node(NodeId),
    Key(&'a JsString),
    Punctuation(char),
}

impl JsDocument {
    /// Mirrors Buffer.toString('utf8') followed by JSON.parse. The transport
    /// must enforce its existing byte cap before invoking this pure parser.
    pub fn parse(bytes: &[u8]) -> Result<Self, JsonError> {
        let text = String::from_utf8_lossy(bytes);
        Parser {
            text: &text,
            offset: 0,
            nodes: Vec::new(),
        }
        .parse()
    }
    pub fn root(&self) -> NodeId {
        self.root
    }
    pub fn node(&self, node: NodeId) -> Option<&JsNode> {
        self.nodes.get(node)
    }
    pub fn get(&self, node: NodeId, key: &str) -> Option<NodeId> {
        match self.nodes.get(node)? {
            JsNode::Object(value) => value.get(key),
            _ => None,
        }
    }
    pub fn string(&self, node: NodeId) -> Option<&JsString> {
        match self.nodes.get(node)? {
            JsNode::String(value) => Some(value),
            _ => None,
        }
    }
    pub fn stringify(&self) -> String {
        self.stringify_node(self.root)
    }

    /// Replace one explicitly approved top-level field, preserving all other
    /// UTF-16 strings, numeric semantics, unknown fields and key positions.
    pub fn set_root_field_json(&mut self, key: &str, bytes: &[u8]) -> Result<(), JsonError> {
        self.set_field_json(self.root, key, bytes)
    }

    /// Replace an approved field on a known object without projecting opaque
    /// surrounding settings or request extensions through scalar-only JSON.
    pub fn set_field_json(
        &mut self,
        parent: NodeId,
        key: &str,
        bytes: &[u8],
    ) -> Result<(), JsonError> {
        if !matches!(self.nodes.get(parent), Some(JsNode::Object(_))) {
            return Err(JsonError);
        }
        let value = Self::parse(bytes)?;
        let offset = self.nodes.len();
        let replacement = offset.checked_add(value.root).ok_or(JsonError)?;
        for mut node in value.nodes {
            match &mut node {
                JsNode::Array(children) => {
                    for child in children {
                        *child = child.checked_add(offset).ok_or(JsonError)?;
                    }
                }
                JsNode::Object(object) => {
                    for (_, child) in &mut object.entries {
                        *child = child.checked_add(offset).ok_or(JsonError)?;
                    }
                }
                _ => {}
            }
            self.nodes.push(node);
        }
        let JsNode::Object(object) = &mut self.nodes[parent] else {
            unreachable!()
        };
        object.insert(JsString::from_scalar(key), replacement);
        Ok(())
    }
    pub fn stringify_node(&self, root: NodeId) -> String {
        let mut output = String::new();
        let mut stack = vec![WriteItem::Node(root)];
        let mut number = ryu_js::Buffer::new();
        while let Some(item) = stack.pop() {
            match item {
                WriteItem::Key(key) => quote(key, &mut output),
                WriteItem::Punctuation(byte) => output.push(byte),
                WriteItem::Node(node) => match &self.nodes[node] {
                    JsNode::Null => output.push_str("null"),
                    JsNode::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
                    JsNode::Number(value) => {
                        if !value.is_finite() {
                            output.push_str("null");
                        } else if *value == 0.0 {
                            output.push('0');
                        } else {
                            output.push_str(number.format_finite(*value));
                        }
                    }
                    JsNode::String(value) => quote(value, &mut output),
                    JsNode::Array(values) => {
                        output.push('[');
                        stack.push(WriteItem::Punctuation(']'));
                        for (index, child) in values.iter().enumerate().rev() {
                            stack.push(WriteItem::Node(*child));
                            if index != 0 {
                                stack.push(WriteItem::Punctuation(','));
                            }
                        }
                    }
                    JsNode::Object(value) => {
                        output.push('{');
                        stack.push(WriteItem::Punctuation('}'));
                        for (index, position) in value.ordered().into_iter().enumerate().rev() {
                            let (key, child) = &value.entries[position];
                            stack.push(WriteItem::Node(*child));
                            stack.push(WriteItem::Punctuation(':'));
                            stack.push(WriteItem::Key(key));
                            if index != 0 {
                                stack.push(WriteItem::Punctuation(','));
                            }
                        }
                    }
                },
            }
        }
        output
    }

    /// Transitional metadata-only projection. Replaces invalid surrogate
    /// sequences with U+FFFD, non-finite numbers with null and data deeper than
    /// 64 levels with null. The depth cap bounds recursive serde Value drops.
    /// This preserves
    /// observation of ordinary metadata next to unusual opaque values, but is
    /// explicitly NOT parity for a consumed field containing those values.
    /// Never serialize this projection upstream or use it for request hashes.
    pub fn to_serde_observation_lossy(&self) -> serde_json::Value {
        let mut depths = vec![usize::MAX; self.nodes.len()];
        depths[self.root] = 0;
        for (index, node) in self.nodes.iter().enumerate() {
            if depths[index] == usize::MAX {
                continue;
            }
            let depth = depths[index] + 1;
            match node {
                JsNode::Array(children) => {
                    for child in children {
                        depths[*child] = depth;
                    }
                }
                JsNode::Object(object) => {
                    for (_, child) in object.entries() {
                        depths[*child] = depth;
                    }
                }
                _ => {}
            }
        }
        let mut values = vec![None; self.nodes.len()];
        for (index, node) in self.nodes.iter().enumerate().rev() {
            if depths[index] == usize::MAX || depths[index] > 64 {
                values[index] = Some(serde_json::Value::Null);
                continue;
            }
            let value = match node {
                JsNode::Null => serde_json::Value::Null,
                JsNode::Bool(value) => serde_json::Value::Bool(*value),
                JsNode::Number(value) => serde_json::Number::from_f64(*value)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null),
                JsNode::String(value) => serde_json::Value::String(value.to_well_formed()),
                JsNode::Array(children) => serde_json::Value::Array(
                    children
                        .iter()
                        .map(|child| {
                            values[*child]
                                .take()
                                .expect("child precedes parent projection")
                        })
                        .collect(),
                ),
                JsNode::Object(object) => {
                    let mut value = serde_json::Map::new();
                    for position in object.ordered() {
                        let (key, child) = &object.entries[position];
                        value.insert(
                            key.to_well_formed(),
                            values[*child]
                                .take()
                                .expect("child precedes parent projection"),
                        );
                    }
                    serde_json::Value::Object(value)
                }
            };
            values[index] = Some(value);
        }
        values[self.root].take().expect("document root")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalized(input: &str) -> String {
        JsDocument::parse(input.as_bytes()).unwrap().stringify()
    }

    #[test]
    fn integer_keys_duplicates_and_proto_names_match_javascript() {
        assert_eq!(
            normalized(r#"{"b":1,"2":2,"01":3,"0":4,"4294967295":5,"1":6,"b":7,"__proto__":8}"#),
            r#"{"0":4,"1":6,"2":2,"b":7,"01":3,"4294967295":5,"__proto__":8}"#
        );
    }

    #[test]
    fn lone_surrogates_and_unicode_pairs_are_preserved() {
        assert_eq!(
            normalized(r#"["\ud800","\udc00","\ud83d\ude00","é","\u0000"]"#),
            "[\"\\ud800\",\"\\udc00\",\"😀\",\"é\",\"\\u0000\"]"
        );
        let document = JsDocument::parse(br#""\ud800""#).unwrap();
        assert_eq!(document.string(document.root()).unwrap().units(), &[0xd800]);
        assert!(
            document
                .string(document.root())
                .unwrap()
                .to_scalar()
                .is_none()
        );
    }

    #[test]
    fn doubles_use_javascript_formatting_rounding_and_overflow() {
        assert_eq!(
            normalized("[-0,1e21,1e20,1e-6,1e-7,9007199254740993,1e400,-1e400,1e-400]"),
            "[0,1e+21,100000000000000000000,0.000001,1e-7,9007199254740992,null,null,0]"
        );
    }

    #[test]
    fn invalid_syntax_is_rejected_without_including_payload() {
        for input in [
            "",
            "[1,]",
            "{\"a\":1,}",
            "01",
            "-",
            "1.",
            "1e+",
            "true false",
            "NaN",
            "Infinity",
            "\u{feff}null",
            "\"\\x20\"",
            "\"a\nb\"",
        ] {
            assert!(
                JsDocument::parse(input.as_bytes()).is_err(),
                "accepted invalid fixture"
            );
        }
        assert_eq!(JsonError.to_string(), "Invalid JSON");
    }

    #[test]
    fn deep_opaque_data_parses_serializes_and_drops_without_recursive_stack() {
        let input = format!("{}0{}", "[".repeat(20000), "]".repeat(20000));
        assert_eq!(normalized(&input), input);
        drop(
            JsDocument::parse(input.as_bytes())
                .unwrap()
                .to_serde_observation_lossy(),
        );
    }

    #[test]
    fn invalid_utf8_matches_node_buffer_replacement() {
        assert_eq!(
            JsDocument::parse(&[b'"', 255, b'"']).unwrap().stringify(),
            "\"�\""
        );
    }

    #[test]
    fn lossy_projection_does_not_replace_the_authoritative_document() {
        let document =
            JsDocument::parse(br#"{"opaque":"\ud800","n":1e400,"model":"claude-sonnet-5"}"#)
                .unwrap();
        assert_eq!(
            document.to_serde_observation_lossy()["model"],
            "claude-sonnet-5"
        );
        assert_eq!(
            document.stringify(),
            r#"{"opaque":"\ud800","n":null,"model":"claude-sonnet-5"}"#
        );
    }

    #[test]
    fn approved_model_and_thinking_changes_preserve_opaque_utf16() {
        let mut document = JsDocument::parse(
            br#"{"model":"source","opaque":"\ud800","thinking":{"type":"disabled"},"messages":[]}"#,
        )
        .unwrap();
        document
            .set_root_field_json("model", br#""target""#)
            .unwrap();
        document
            .set_root_field_json("thinking", br#"{"type":"adaptive"}"#)
            .unwrap();
        assert_eq!(
            document.stringify(),
            r#"{"model":"target","opaque":"\ud800","thinking":{"type":"adaptive"},"messages":[]}"#
        );
    }
}
