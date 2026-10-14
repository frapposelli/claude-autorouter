//! JSON.parse/JSON.stringify-compatible data, without recursive parser stacks.
//!
//! JavaScript strings are UTF-16 sequences, not necessarily Unicode scalar
//! strings. Provider JSON must retain lone surrogate escapes and IEEE-754
//! number semantics, including overflow, duplicate keys and integer-key order.
//! An arena also makes dropping deeply nested opaque input non-recursive.
//!
//! This document is the authoritative wire representation. The explicitly
//! lossy observation projection is never suitable for forwarding requests.
use std::borrow::Borrow;
use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};

pub type NodeId = usize;

#[derive(Clone)]
pub struct JsString(Arc<StringData>);

// Both representations describe immutable code units. Populating the lazy
// UTF-16 cache never changes equality or hashing, including for map keys.
struct StringData {
    scalar: Option<String>,
    units: OnceLock<Vec<u16>>,
    needs_escape: bool,
}

impl JsString {
    pub fn from_utf16(units: Vec<u16>) -> Self {
        let scalar = String::from_utf16(&units).ok();
        let needs_escape = scalar.as_deref().is_some_and(needs_json_escape);
        Self(Arc::new(StringData {
            scalar,
            units: OnceLock::from(units),
            needs_escape,
        }))
    }
    pub fn from_scalar(value: &str) -> Self {
        Self::from(value.to_owned())
    }
    fn from_unescaped_scalar(value: &str) -> Self {
        Self(Arc::new(StringData {
            scalar: Some(value.to_owned()),
            units: OnceLock::new(),
            needs_escape: false,
        }))
    }
    pub fn units(&self) -> &[u16] {
        self.0.units.get_or_init(|| {
            self.0
                .scalar
                .as_deref()
                .expect("scalar or UTF-16 representation")
                .encode_utf16()
                .collect()
        })
    }
    pub fn to_scalar(&self) -> Option<String> {
        self.0.scalar.clone()
    }
    pub fn to_well_formed(&self) -> String {
        self.0
            .scalar
            .clone()
            .unwrap_or_else(|| String::from_utf16_lossy(self.units()))
    }
    pub fn stringify(&self) -> String {
        let mut output = String::new();
        quote(self, &mut output);
        output
    }

    /// Exact JSON byte charge without allocating output or a lazy UTF-16 cache.
    /// Stops when the caller's byte limit cannot admit the complete string.
    pub fn serialized_len_up_to(&self, limit: usize) -> Option<usize> {
        let mut length = 2usize;
        if length > limit {
            return None;
        }
        let mut add = |bytes: usize| {
            length = length.checked_add(bytes)?;
            (length <= limit).then_some(())
        };
        if let Some(scalar) = self.0.scalar.as_deref() {
            // Escaping can only increase the byte count, so large scalar input
            // is rejected without scanning or materializing its code units.
            if scalar.len() > limit - 2 {
                return None;
            }
            if !self.0.needs_escape {
                return scalar.len().checked_add(2);
            }
            for byte in scalar.bytes() {
                add(match byte {
                    8 | 9 | 10 | 12 | 13 | 34 | 92 => 2,
                    0..=31 => 6,
                    _ => 1,
                })?;
            }
        } else {
            let units = self.0.units.get().expect("UTF-16 storage already exists");
            let mut index = 0;
            while index < units.len() {
                let unit = units[index];
                index += 1;
                let bytes = match unit {
                    8 | 9 | 10 | 12 | 13 | 34 | 92 => 2,
                    0..=31 => 6,
                    0xd800..=0xdbff
                        if units
                            .get(index)
                            .is_some_and(|next| (0xdc00..=0xdfff).contains(next)) =>
                    {
                        index += 1;
                        4
                    }
                    0xd800..=0xdfff => 6,
                    _ => char::from_u32(u32::from(unit))
                        .expect("non-surrogate unit")
                        .len_utf8(),
                };
                add(bytes)?;
            }
        }
        Some(length)
    }

    fn array_index(&self) -> Option<u32> {
        // Every array-index key is ASCII, so scalar storage needs no UTF-16
        // allocation merely to establish property enumeration order.
        let bytes = self.0.scalar.as_deref()?.as_bytes();
        if bytes.is_empty() || bytes.len() > 10 || (bytes.len() > 1 && bytes[0] == b'0') {
            return None;
        }
        let mut number = 0_u32;
        for &byte in bytes {
            if !byte.is_ascii_digit() {
                return None;
            }
            number = number
                .checked_mul(10)?
                .checked_add(u32::from(byte - b'0'))?;
        }
        (number != u32::MAX).then_some(number)
    }
}

impl PartialEq for JsString {
    fn eq(&self, other: &Self) -> bool {
        if Arc::ptr_eq(&self.0, &other.0) {
            return true;
        }
        match (&self.0.scalar, &other.0.scalar) {
            (Some(left), Some(right)) => left == right,
            (None, None) => self.units() == other.units(),
            _ => false,
        }
    }
}
impl Eq for JsString {}
impl Hash for JsString {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.units().hash(state);
    }
}
impl Borrow<[u16]> for JsString {
    fn borrow(&self) -> &[u16] {
        self.units()
    }
}

impl fmt::Debug for JsString {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        let length = self.0.units.get().map_or_else(
            || {
                self.0
                    .scalar
                    .as_deref()
                    .expect("scalar string")
                    .encode_utf16()
                    .count()
            },
            Vec::len,
        );
        write!(output, "JsString(<{length} UTF-16 units>)")
    }
}

impl From<&str> for JsString {
    fn from(value: &str) -> Self {
        Self::from_scalar(value)
    }
}
impl From<String> for JsString {
    fn from(value: String) -> Self {
        let needs_escape = needs_json_escape(&value);
        Self(Arc::new(StringData {
            scalar: Some(value),
            units: OnceLock::new(),
            needs_escape,
        }))
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
        let units: Vec<u16> = key.encode_utf16().collect();
        self.positions
            .get(units.as_slice())
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
    serialized_len: OnceLock<usize>,
}

#[cfg(test)]
thread_local! {
    static LENGTH_TRAVERSALS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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
        let start = self.offset;
        let special = self.text.as_bytes()[start..]
            .iter()
            .position(|byte| matches!(byte, b'"' | b'\\' | 0..=31))
            .ok_or(JsonError)?;
        self.offset += special;
        if self.byte() == Some(b'"') {
            let value = JsString::from_unescaped_scalar(&self.text[start..self.offset]);
            self.offset += 1;
            return Ok(value);
        }
        let mut units: Vec<u16> = self.text[start..self.offset].encode_utf16().collect();
        loop {
            match self.byte().ok_or(JsonError)? {
                b'"' => {
                    self.offset += 1;
                    return Ok(JsString::from_utf16(units));
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
            serialized_len: OnceLock::new(),
        })
    }
}

fn quote(value: &JsString, output: &mut String) {
    if let Some(scalar) = value.0.scalar.as_deref() {
        if value.0.needs_escape {
            quote_scalar(scalar, output);
        } else {
            output.reserve(scalar.len().saturating_add(2));
            output.push('"');
            output.push_str(scalar);
            output.push('"');
        }
        return;
    }
    let units = value.units();
    output.reserve(units.len().saturating_add(2));
    output.push('"');
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
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
                if units
                    .get(index)
                    .is_some_and(|next| (0xdc00..=0xdfff).contains(next)) =>
            {
                let low = units[index];
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

fn needs_json_escape(value: &str) -> bool {
    value
        .bytes()
        .any(|byte| matches!(byte, b'"' | b'\\' | 0..=31))
}

fn quote_scalar(value: &str, output: &mut String) {
    output.reserve(value.len().saturating_add(2));
    output.push('"');
    let mut start = 0;
    for (index, byte) in value.bytes().enumerate() {
        let escape = match byte {
            8 => "\\b",
            9 => "\\t",
            10 => "\\n",
            12 => "\\f",
            13 => "\\r",
            34 => "\\\"",
            92 => "\\\\",
            0..=31 => {
                use std::fmt::Write;
                output.push_str(&value[start..index]);
                write!(output, "\\u{byte:04x}").expect("string writing");
                start = index + 1;
                continue;
            }
            _ => continue,
        };
        // ASCII escapes always lie on UTF-8 boundaries. Copy entire ordinary
        // spans, including multi-byte scalars, without decoding each byte.
        output.push_str(&value[start..index]);
        output.push_str(escape);
        start = index + 1;
    }
    output.push_str(&value[start..]);
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

    /// Exact serialized byte charge with early limit rejection. Traversal uses
    /// one cursor per nesting level, never a stack of every opaque child, and
    /// does not allocate output, object-order indexes or UTF-16 string caches.
    /// Only successful complete lengths are memoized; every call checks its
    /// own limit. Clones own independent cells, and setters invalidate first.
    pub fn serialized_len_up_to(&self, limit: usize) -> Option<usize> {
        if let Some(&length) = self.serialized_len.get() {
            return (length <= limit).then_some(length);
        }
        #[cfg(test)]
        LENGTH_TRAVERSALS.with(|count| count.set(count.get() + 1));
        enum Cursor<'a> {
            Node(NodeId),
            Array(&'a [NodeId], usize),
            Object(&'a [(JsString, NodeId)], usize),
        }
        let mut stack = vec![Cursor::Node(self.root)];
        let mut length = 0usize;
        let mut number = ryu_js::Buffer::new();
        while let Some(cursor) = stack.pop() {
            let remaining = limit.checked_sub(length)?;
            let bytes = match cursor {
                Cursor::Node(node) => match self.nodes.get(node)? {
                    JsNode::Null => 4,
                    JsNode::Bool(value) => {
                        if *value {
                            4
                        } else {
                            5
                        }
                    }
                    JsNode::Number(value) => {
                        if !value.is_finite() {
                            4
                        } else if *value == 0.0 {
                            1
                        } else {
                            number.format_finite(*value).len()
                        }
                    }
                    JsNode::String(value) => value.serialized_len_up_to(remaining)?,
                    JsNode::Array(children) => {
                        stack.push(Cursor::Array(children, 0));
                        2
                    }
                    JsNode::Object(object) => {
                        stack.push(Cursor::Object(object.entries(), 0));
                        2
                    }
                },
                Cursor::Array(children, index) => {
                    if let Some(child) = children.get(index) {
                        stack.push(Cursor::Array(children, index + 1));
                        stack.push(Cursor::Node(*child));
                        usize::from(index != 0)
                    } else {
                        0
                    }
                }
                Cursor::Object(entries, index) => {
                    if let Some((key, child)) = entries.get(index) {
                        stack.push(Cursor::Object(entries, index + 1));
                        stack.push(Cursor::Node(*child));
                        key.serialized_len_up_to(remaining)?
                            .checked_add(1 + usize::from(index != 0))?
                    } else {
                        0
                    }
                }
            };
            length = length.checked_add(bytes)?;
            if length > limit {
                return None;
            }
        }
        // Computing before set avoids a full traversal inside get_or_init.
        // Concurrent cold readers may compute independently; only success is stored.
        let _ = self.serialized_len.set(length);
        Some(length)
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
        // Exclusive mutation owns invalidation before any arena push, including
        // a failed remap that might leave unreachable appended nodes.
        self.serialized_len.take();
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

    #[test]
    fn observer_byte_charge_matches_exact_unicode_numbers_and_duplicate_keys() {
        for input in [
            r#"null"#,
            r#"[true,false,-0,1e400,1e-300,123456789012345678901]"#,
            r#"{"2":"二","1":"😀","b":"é","b":"\ud800","q\"":"\u0000\b\t\n\f\r\\"}"#,
            r#"["\ud800\ud83d\ude00\udc00","\u2028\u2029"]"#,
        ] {
            let document = JsDocument::parse(input.as_bytes()).unwrap();
            let expected = document.stringify().len();
            assert_eq!(document.serialized_len_up_to(expected), Some(expected));
            assert_eq!(document.serialized_len_up_to(expected + 1), Some(expected));
            assert_eq!(document.serialized_len_up_to(expected - 1), None);
        }
    }

    #[test]
    fn observer_byte_charge_rejects_large_scalars_without_utf16_materialization() {
        let input = format!("\"{}\"", "a".repeat(128 * 1024));
        let document = JsDocument::parse(input.as_bytes()).unwrap();
        let string = document.string(document.root()).unwrap();
        assert!(string.0.units.get().is_none());
        assert_eq!(document.serialized_len_up_to(64 * 1024), None);
        assert!(string.0.units.get().is_none());
        assert_eq!(
            document.serialized_len_up_to(input.len()),
            Some(input.len())
        );
        assert!(string.0.units.get().is_none());
    }

    #[test]
    fn observer_byte_charge_walks_deep_and_wide_data_with_bounded_cursors() {
        let deep = format!("{}null{}", "[".repeat(10_000), "]".repeat(10_000));
        let document = JsDocument::parse(deep.as_bytes()).unwrap();
        assert_eq!(document.serialized_len_up_to(deep.len()), Some(deep.len()));
        assert_eq!(document.serialized_len_up_to(1024), None);
        let wide = format!("[{}]", vec!["true"; 10_000].join(","));
        let document = JsDocument::parse(wide.as_bytes()).unwrap();
        assert_eq!(document.serialized_len_up_to(wide.len()), Some(wide.len()));
        assert_eq!(document.serialized_len_up_to(1024), None);
    }

    fn exact_memo_limits(document: &JsDocument) {
        let exact = document.stringify().len();
        assert_eq!(document.serialized_len_up_to(usize::MAX), Some(exact));
        assert_eq!(document.serialized_len.get(), Some(&exact));
        for limit in [0, exact - 1, exact, exact + 1, usize::MAX] {
            assert_eq!(
                document.serialized_len_up_to(limit),
                (exact <= limit).then_some(exact)
            );
        }
    }

    #[test]
    fn length_memo_keeps_failures_out_and_checks_every_current_limit() {
        LENGTH_TRAVERSALS.with(|count| count.set(0));
        let input = format!("\"{}\"", "x".repeat(1024));
        let document = JsDocument::parse(input.as_bytes()).unwrap();
        assert_eq!(document.serialized_len_up_to(32), None);
        assert!(document.serialized_len.get().is_none());
        assert_eq!(LENGTH_TRAVERSALS.with(std::cell::Cell::get), 1);
        exact_memo_limits(&document);
        assert_eq!(LENGTH_TRAVERSALS.with(std::cell::Cell::get), 2);
        assert_eq!(document.stringify(), input);
        assert!(
            document
                .string(document.root())
                .unwrap()
                .0
                .units
                .get()
                .is_none()
        );
    }

    #[test]
    fn length_memo_clones_and_public_mutations_have_independent_identity() {
        let mut original = JsDocument::parse(br#"{"model":"claude-haiku-4-5","nested":{"text":"\ud800","flag":false},"2":-0,"1":1e400,"2":"last"}"#).unwrap();
        let mut cold_clone = original.clone();
        assert!(cold_clone.serialized_len.get().is_none());
        exact_memo_limits(&original);
        assert!(cold_clone.serialized_len.get().is_none());
        let wire = original.stringify();
        let original_memo = original.serialized_len.get().copied();
        let mut warm_clone = original.clone();
        assert_eq!(
            warm_clone.serialized_len.get(),
            original.serialized_len.get()
        );
        let model = original.get(original.root(), "model").unwrap();
        for (parent, value) in [
            (model, b"true".as_slice()),
            (usize::MAX, b"true".as_slice()),
            (original.root(), b"[".as_slice()),
        ] {
            let memo = original.serialized_len.get().copied();
            assert_eq!(
                original.set_field_json(parent, "new", value),
                Err(JsonError)
            );
            assert_eq!(original.serialized_len.get().copied(), memo);
            assert_eq!(original.stringify(), wire);
        }
        cold_clone
            .set_root_field_json("model", br#""claude-sonnet-5-5""#)
            .unwrap();
        assert!(cold_clone.serialized_len.get().is_none());
        exact_memo_limits(&cold_clone);
        assert_eq!(original.serialized_len.get().copied(), original_memo);
        warm_clone
            .set_root_field_json("opaque", br#"{"extra":"\u00e9\u0000"}"#)
            .unwrap();
        assert!(warm_clone.serialized_len.get().is_none());
        exact_memo_limits(&warm_clone);
        assert_eq!(original.serialized_len.get().copied(), original_memo);
        let warm_memo = warm_clone.serialized_len.get().copied();
        let warm_wire = warm_clone.stringify();
        let nested = original.get(original.root(), "nested").unwrap();
        original
            .set_field_json(nested, "text", br#""longer\udc00""#)
            .unwrap();
        assert!(original.serialized_len.get().is_none());
        exact_memo_limits(&original);
        original.set_root_field_json("2", b"false").unwrap();
        assert!(original.serialized_len.get().is_none());
        exact_memo_limits(&original);
        assert_eq!(warm_clone.stringify(), warm_wire);
        assert_eq!(warm_clone.serialized_len.get().copied(), warm_memo);
        assert_ne!(original.stringify(), wire);
        assert_ne!(cold_clone.stringify(), wire);
        exact_memo_limits(&cold_clone);
        exact_memo_limits(&warm_clone);
    }

    #[test]
    fn length_memo_never_reuses_a_model_sensitive_context_charge() {
        LENGTH_TRAVERSALS.with(|count| count.set(0));
        let mut document = JsDocument::parse(br#"{"model":"claude-haiku-4-5","tools":[{"name":"L","input_schema":{"type":"object"}},{"name":"D","defer_loading":true,"description":"unused synthetic schema","input_schema":{"type":"object"}}],"messages":[{"role":"user","content":"synthetic"}]}"#).unwrap();
        let full = document.stringify().len() as u64;
        let mut visible = None;
        for model in ["claude-haiku-4-5", "claude-sonnet-5-5", "claude-opus-5-5"] {
            let charge = crate::router::context_size_bytes(&document, model)
                .unwrap()
                .exact_safe_bytes()
                .unwrap();
            assert!(charge < full);
            assert_eq!(*visible.get_or_insert(charge), charge);
        }
        assert_eq!(
            crate::router::context_size_bytes(&document, "unknown-custom-model")
                .unwrap()
                .exact_safe_bytes(),
            Some(full)
        );
        assert_eq!(LENGTH_TRAVERSALS.with(std::cell::Cell::get), 1);
        let tools = document.get(document.root(), "tools").unwrap();
        let Some(JsNode::Array(items)) = document.node(tools) else {
            panic!("synthetic tools");
        };
        let deferred = items[1];
        document
            .set_field_json(deferred, "defer_loading", b"false")
            .unwrap();
        assert!(document.serialized_len.get().is_none());
        let changed = document.stringify().len() as u64;
        assert_eq!(
            crate::router::context_size_bytes(&document, "claude-haiku-4-5")
                .unwrap()
                .exact_safe_bytes(),
            Some(changed)
        );
        assert_eq!(LENGTH_TRAVERSALS.with(std::cell::Cell::get), 2);
    }

    #[test]
    fn length_memo_shared_cold_readers_remain_send_sync_and_limit_correct() {
        fn require_send_sync<T: Send + Sync>() {}
        require_send_sync::<JsDocument>();
        let input = format!("\"{}\"", "x".repeat(128 * 1024));
        let document = Arc::new(JsDocument::parse(input.as_bytes()).unwrap());
        let exact = input.len();
        let limits = [0, 32, exact - 1, exact, exact + 1, usize::MAX];
        let barrier = Arc::new(std::sync::Barrier::new(limits.len()));
        let workers: Vec<_> = limits
            .into_iter()
            .map(|limit| {
                let document = document.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    assert_eq!(
                        document.serialized_len_up_to(limit),
                        (exact <= limit).then_some(exact)
                    );
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(document.serialized_len.get(), Some(&exact));
        assert_eq!(document.stringify(), input);
        exact_memo_limits(&document);
        // Concurrent misses may each traverse; there is no single-flight claim.
    }

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
    fn string_identity_and_quoting_agree_across_scalar_and_utf16_inputs() {
        for text in ["", "ordinary text", "é水🦀", "\0\u{1f}\n\t\r\"\\🦀\u{2028}"] {
            let scalar = JsString::from_scalar(text);
            let utf16 = JsString::from_utf16(text.encode_utf16().collect());
            assert_eq!(scalar, utf16);
            #[allow(
                clippy::mutable_key_type,
                reason = "JsString only caches immutable code units"
            )]
            let mut identities = std::collections::HashSet::new();
            identities.insert(scalar.clone());
            assert!(identities.contains(&utf16));
            assert_eq!(scalar.stringify(), utf16.stringify());
            let parsed = JsDocument::parse(scalar.stringify().as_bytes()).unwrap();
            assert_eq!(parsed.string(parsed.root()).unwrap(), &utf16);
            assert_eq!(scalar.units(), utf16.units());
        }
        assert_ne!(JsString::from_utf16(vec![0xd800]), JsString::from("�"));
        assert_eq!(
            normalized(r#"{"é🦀":1,"\u00e9\ud83e\udd80":2,"\ud800":3,"�":4}"#),
            r#"{"é🦀":2,"\ud800":3,"�":4}"#
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
