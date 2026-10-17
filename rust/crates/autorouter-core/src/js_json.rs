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

/// Infallible append-only JSON byte destination inside the core crate.
/// Implementations must consume every UTF-8 fragment in order. `reserve` is
/// only an allocation hint; bounded streaming destinations may ignore it.
pub(crate) trait JsonWrite {
    fn reserve(&mut self, _additional: usize) {}

    fn push_str(&mut self, value: &str);

    fn push(&mut self, value: char) {
        let mut encoded = [0_u8; 4];
        self.push_str(value.encode_utf8(&mut encoded));
    }
}

impl JsonWrite for String {
    fn reserve(&mut self, additional: usize) {
        String::reserve(self, additional);
    }

    fn push_str(&mut self, value: &str) {
        String::push_str(self, value);
    }

    fn push(&mut self, value: char) {
        String::push(self, value);
    }
}

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
        self.write_json(&mut output);
        output
    }

    /// Append this string's exact JSON representation without a temporary
    /// string. The same scalar/UTF-16 writer serves `stringify` and fingerprints.
    pub(crate) fn write_json<W: JsonWrite + ?Sized>(&self, output: &mut W) {
        quote(self, output);
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

enum ObjectOrder {
    Natural(std::ops::Range<usize>),
    Indexed(std::vec::IntoIter<usize>),
}

impl Iterator for ObjectOrder {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Natural(order) => order.next(),
            Self::Indexed(order) => order.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Natural(order) => order.size_hint(),
            Self::Indexed(order) => order.size_hint(),
        }
    }
}

impl DoubleEndedIterator for ObjectOrder {
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            Self::Natural(order) => order.next_back(),
            Self::Indexed(order) => order.next_back(),
        }
    }
}

impl ExactSizeIterator for ObjectOrder {}

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
        self.ordered().map(|index| &self.entries[index])
    }
    fn insert(&mut self, key: JsString, value: NodeId) {
        if let Some(&index) = self.positions.get(&key) {
            self.entries[index].1 = value;
        } else {
            self.positions.insert(key.clone(), self.entries.len());
            self.entries.push((key, value));
        }
    }
    fn ordered(&self) -> ObjectOrder {
        // Ordinary string keys already have their required insertion order.
        // Borrow positions without allocating vectors for common JSON objects.
        // Numeric-key objects retain the complete sorting path below.
        if self
            .entries
            .iter()
            .all(|(key, _)| key.array_index().is_none())
        {
            return ObjectOrder::Natural(0..self.entries.len());
        }
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
        ObjectOrder::Indexed(
            indexed
                .into_iter()
                .map(|(_, index)| index)
                .chain(ordinary)
                .collect::<Vec<_>>()
                .into_iter(),
        )
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

fn quote<W: JsonWrite + ?Sized>(value: &JsString, output: &mut W) {
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
                write_unicode_escape(unit, output);
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
                write_unicode_escape(unit, output);
            }
            _ => output.push(char::from_u32(u32::from(unit)).expect("non-surrogate unit")),
        }
    }
    output.push('"');
}

fn write_unicode_escape<W: JsonWrite + ?Sized>(unit: u16, output: &mut W) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = [
        b'\\',
        b'u',
        HEX[usize::from((unit >> 12) & 15)],
        HEX[usize::from((unit >> 8) & 15)],
        HEX[usize::from((unit >> 4) & 15)],
        HEX[usize::from(unit & 15)],
    ];
    output.push_str(std::str::from_utf8(&bytes).expect("ASCII JSON escape"));
}

fn needs_json_escape(value: &str) -> bool {
    value
        .bytes()
        .any(|byte| matches!(byte, b'"' | b'\\' | 0..=31))
}

fn quote_scalar<W: JsonWrite + ?Sized>(value: &str, output: &mut W) {
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
                output.push_str(&value[start..index]);
                write_unicode_escape(u16::from(byte), output);
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

    /// Own only the selected reachable nodes, preserving source values rather
    /// than JSON.stringify's nonfinite-to-null conversion. No environment-sized
    /// clone, recursive traversal or inherited length memo participates.
    pub(crate) fn compact_subtree(&self, root: NodeId) -> Option<Self> {
        let mut remap = HashMap::new();
        let mut reachable = Vec::new();
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            if remap.insert(node, 0).is_some() {
                continue;
            }
            let value = self.nodes.get(node)?;
            reachable.push(node);
            match value {
                JsNode::Array(children) => pending.extend(children.iter().copied()),
                JsNode::Object(object) => {
                    pending.extend(object.entries().iter().map(|(_, child)| *child));
                }
                _ => {}
            }
        }
        reachable.sort_unstable();
        for (index, node) in reachable.iter().enumerate() {
            remap.insert(*node, index);
        }
        let mut nodes = Vec::with_capacity(reachable.len());
        for node in reachable {
            let mut value = self.nodes[node].clone();
            match &mut value {
                JsNode::Array(children) => {
                    for child in children {
                        *child = *remap.get(child)?;
                    }
                }
                JsNode::Object(object) => {
                    for (_, child) in &mut object.entries {
                        *child = *remap.get(child)?;
                    }
                }
                _ => {}
            }
            nodes.push(value);
        }
        Some(Self {
            nodes,
            root: *remap.get(&root)?,
            serialized_len: OnceLock::new(),
        })
    }

    /// Import a scalar JSON value directly into the arena. Numbers become JS
    /// doubles; object insertion order survives without serializing, reparsing
    /// or recursively cloning an arbitrarily nested serde Value.
    pub(crate) fn from_serde_value(value: &serde_json::Value) -> Self {
        use serde_json::Value;
        let mut nodes = vec![JsNode::Null];
        let mut pending = vec![(0, value)];
        while let Some((index, value)) = pending.pop() {
            let node =
                match value {
                    Value::Null => JsNode::Null,
                    Value::Bool(value) => JsNode::Bool(*value),
                    Value::Number(value) => JsNode::Number(value.as_f64().unwrap_or_else(|| {
                        value.to_string().parse().expect("JSON number spelling")
                    })),
                    Value::String(value) => JsNode::String(JsString::from_scalar(value)),
                    Value::Array(values) => {
                        let mut children = Vec::with_capacity(values.len());
                        for value in values {
                            let child = nodes.len();
                            nodes.push(JsNode::Null);
                            children.push(child);
                            pending.push((child, value));
                        }
                        JsNode::Array(children)
                    }
                    Value::Object(values) => {
                        let mut object = JsObject::default();
                        for (key, value) in values {
                            let child = nodes.len();
                            nodes.push(JsNode::Null);
                            object.insert(JsString::from_scalar(key), child);
                            pending.push((child, value));
                        }
                        JsNode::Object(object)
                    }
                };
            nodes[index] = node;
        }
        Self {
            nodes,
            root: 0,
            serialized_len: OnceLock::new(),
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
        self.write_node(root, &mut output);
        output
    }

    /// Append the authoritative JSON representation of one node. Iterative
    /// traversal, exact number/string semantics and property order match
    /// `stringify_node`; no lossy observation projection participates.
    pub(crate) fn write_node<W: JsonWrite + ?Sized>(&self, root: NodeId, output: &mut W) {
        let mut stack = vec![WriteItem::Node(root)];
        let mut number = ryu_js::Buffer::new();
        while let Some(item) = stack.pop() {
            match item {
                WriteItem::Key(key) => key.write_json(output),
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
                    JsNode::String(value) => value.write_json(output),
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
                        for (index, position) in value.ordered().enumerate().rev() {
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
    fn compact_subtree_owns_only_reachable_source_values_and_a_fresh_memo() {
        let source = JsDocument::parse(br#"{"outside":"unrelated","credential":{"2":-0,"1":1e400,"utf":"\ud800","dup":"stale","dup":{"\udfff":[-1e400,true,null]}}}"#).unwrap();
        let before = source.stringify();
        exact_memo_limits(&source);
        let original_memo = source.serialized_len.get().copied();
        assert!(source.compact_subtree(usize::MAX).is_none());
        let credential = source.get(source.root(), "credential").unwrap();
        let mut owned = source.compact_subtree(credential).unwrap();
        assert_eq!(owned.nodes.len(), 9);
        assert!(owned.serialized_len.get().is_none());
        let expected = r#"{"1":null,"2":0,"utf":"\ud800","dup":{"\udfff":[null,true,null]}}"#;
        assert_eq!(owned.stringify(), expected);
        let overflow = owned.get(owned.root(), "1").unwrap();
        assert!(
            matches!(owned.node(overflow), Some(JsNode::Number(value)) if *value == f64::INFINITY)
        );
        let zero = owned.get(owned.root(), "2").unwrap();
        assert!(
            matches!(owned.node(zero), Some(JsNode::Number(value)) if *value == 0.0 && value.is_sign_negative())
        );
        let utf = owned.get(owned.root(), "utf").unwrap();
        assert_eq!(owned.string(utf).unwrap().units(), &[0xd800]);
        let dup = owned.get(owned.root(), "dup").unwrap();
        let JsNode::Object(object) = owned.node(dup).unwrap() else {
            panic!("owned object");
        };
        let (key, array) = &object.entries()[0];
        assert_eq!(key.units(), &[0xdfff]);
        let JsNode::Array(children) = owned.node(*array).unwrap() else {
            panic!("owned array");
        };
        assert!(
            matches!(owned.node(children[0]), Some(JsNode::Number(value)) if *value == f64::NEG_INFINITY)
        );
        exact_memo_limits(&owned);
        owned
            .set_root_field_json("utf", br#""replacement""#)
            .unwrap();
        assert!(owned.serialized_len.get().is_none());
        assert_eq!(source.stringify(), before);
        assert_eq!(source.serialized_len.get().copied(), original_memo);
        drop(source);
        assert!(
            matches!(owned.node(overflow), Some(JsNode::Number(value)) if *value == f64::INFINITY)
        );
        assert_eq!(
            owned
                .string(owned.get(owned.root(), "utf").unwrap())
                .unwrap()
                .to_scalar()
                .as_deref(),
            Some("replacement")
        );
    }

    #[test]
    fn serde_value_import_keeps_order_and_javascript_number_interpretation() {
        let input: serde_json::Value = serde_json::from_str(r#"{"2":-0.0,"1":9007199254740993,"z":[true,false,null,"é😀"],"a":18446744073709551615}"#).unwrap();
        let before = serde_json::to_string(&input).unwrap();
        let document = JsDocument::from_serde_value(&input);
        assert!(document.serialized_len.get().is_none());
        let JsNode::Object(object) = document.node(document.root()).unwrap() else {
            panic!("imported object");
        };
        assert_eq!(
            object
                .entries()
                .iter()
                .map(|(key, _)| key.to_scalar().unwrap())
                .collect::<Vec<_>>(),
            ["2", "1", "z", "a"]
        );
        assert_eq!(
            document.stringify(),
            r#"{"1":9007199254740992,"2":0,"z":[true,false,null,"é😀"],"a":18446744073709552000}"#
        );
        let zero = document.get(document.root(), "2").unwrap();
        assert!(
            matches!(document.node(zero), Some(JsNode::Number(value)) if *value == 0.0 && value.is_sign_negative())
        );
        assert_eq!(serde_json::to_string(&input).unwrap(), before);
        exact_memo_limits(&document);
    }

    #[test]
    fn source_subtree_and_serde_import_walk_deep_data_without_recursive_clones() {
        // The fixture guard also dismantles iteratively during assertion unwind.
        // A recursive Value drop must not obscure a conversion failure.
        struct IterativeValue(serde_json::Value);
        impl Drop for IterativeValue {
            fn drop(&mut self) {
                let mut pending = vec![std::mem::take(&mut self.0)];
                while let Some(value) = pending.pop() {
                    match value {
                        serde_json::Value::Array(mut children) => pending.append(&mut children),
                        serde_json::Value::Object(children) => {
                            pending.extend(children.into_iter().map(|(_, value)| value));
                        }
                        _ => {}
                    }
                }
            }
        }
        let depth = 20_000;
        let mut input = IterativeValue(serde_json::Value::String("leaf".into()));
        for _ in 0..depth {
            input.0 = serde_json::Value::Array(vec![std::mem::take(&mut input.0)]);
        }
        let document = JsDocument::from_serde_value(&input.0);
        drop(input);
        assert_eq!(document.nodes.len(), depth + 1);
        let expected = format!("{}\"leaf\"{}", "[".repeat(depth), "]".repeat(depth));
        assert_eq!(document.stringify(), expected);
        exact_memo_limits(&document);
        let owned = document.compact_subtree(document.root()).unwrap();
        assert_eq!(owned.nodes.len(), depth + 1);
        assert!(owned.serialized_len.get().is_none());
        drop(document);
        assert_eq!(owned.stringify(), expected);
        assert_eq!(owned.serialized_len_up_to(expected.len() - 1), None);
        assert_eq!(
            owned.serialized_len_up_to(expected.len()),
            Some(expected.len())
        );
    }

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

    // Retained pre-optimization algorithm: independent of the iterator's
    // natural-order fast path, and used only by these differential controls.
    fn previous_object_order(object: &JsObject) -> Vec<usize> {
        let mut indexed = Vec::new();
        let mut ordinary = Vec::new();
        for (index, (key, _)) in object.entries.iter().enumerate() {
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

    #[test]
    fn object_order_matches_previous_algorithm_and_exact_iterator_contract() {
        let mut keys: Vec<_> = [
            "a",
            "z",
            "__proto__",
            "01",
            "4294967295",
            "é",
            "0",
            "2",
            "10",
            "4294967294",
            "1e0",
            "-0",
            "０",
        ]
        .into_iter()
        .map(JsString::from_scalar)
        .collect();
        keys.push(JsString::from_utf16(vec![0xd800]));
        for width in 0..=keys.len() {
            for offset in 0..keys.len() {
                for reverse in [false, true] {
                    let mut object = JsObject::default();
                    for step in 0..width {
                        let position = (offset
                            + if reverse { keys.len() - 1 - step } else { step })
                            % keys.len();
                        object.insert(keys[position].clone(), step);
                        if step % 3 == 0 {
                            object.insert(keys[position].clone(), step + 100);
                        }
                    }
                    let previous = previous_object_order(&object);
                    assert_eq!(object.ordered().collect::<Vec<_>>(), previous);
                    assert_eq!(
                        object.ordered().rev().collect::<Vec<_>>(),
                        previous.iter().copied().rev().collect::<Vec<_>>()
                    );
                    let natural = object
                        .entries
                        .iter()
                        .all(|(key, _)| key.array_index().is_none());
                    assert_eq!(matches!(object.ordered(), ObjectOrder::Natural(_)), natural);
                    let mut expected = std::collections::VecDeque::from(previous.clone());
                    let mut actual = object.ordered();
                    let mut step = 0;
                    while !expected.is_empty() {
                        assert_eq!(actual.len(), expected.len());
                        assert_eq!(actual.size_hint(), (expected.len(), Some(expected.len())));
                        if step % 3 == 0 {
                            assert_eq!(actual.next_back(), expected.pop_back());
                        } else {
                            assert_eq!(actual.next(), expected.pop_front());
                        }
                        step += 1;
                    }
                    assert_eq!(actual.len(), 0);
                    assert_eq!(actual.size_hint(), (0, Some(0)));
                    assert_eq!(actual.next(), None);
                    assert_eq!(actual.next_back(), None);
                    let ordered_keys: Vec<_> = object
                        .ordered_entries()
                        .map(|(key, _)| key.clone())
                        .collect();
                    assert_eq!(
                        ordered_keys,
                        previous
                            .iter()
                            .map(|&index| object.entries[index].0.clone())
                            .collect::<Vec<_>>()
                    );
                }
            }
        }
    }

    #[test]
    fn object_order_frozen_node_property_and_nested_json_outputs_match() {
        // Actual pinned Node 22.14.0 JSON.stringify(JSON.parse(input)) results.
        // No runtime Node process, reordering oracle or lossy projection is used.
        let cases = [
            (r###"{}"###, r###"{}"###),
            (r###"{"only":"one"}"###, r###"{"only":"one"}"###),
            (
                r###"{"z":"first","a":"second","__proto__":"ordinary","constructor":"still ordinary"}"###,
                r###"{"z":"first","a":"second","__proto__":"ordinary","constructor":"still ordinary"}"###,
            ),
            (
                r###"{"b":"old","4294967294":"maxindex","01":"leading","1":"one","0":"zero","4294967295":"notindex","-0":"minus","1e0":"exponent","000":"zeros","2":"two","b":"new"}"###,
                r###"{"0":"zero","1":"one","2":"two","4294967294":"maxindex","b":"new","01":"leading","4294967295":"notindex","-0":"minus","1e0":"exponent","000":"zeros"}"###,
            ),
            (
                r###"{"b":"first","\u0031":"first index","1":"last index","a":"last"}"###,
                r###"{"1":"last index","b":"first","a":"last"}"###,
            ),
            (
                r###"{"\ud800":"high","\udc00":"low","é":"accent","\ud83d\ude00":"pair","\u0000":"nul","０":"wide zero","0":"ascii zero"}"###,
                r###"{"0":"ascii zero","\ud800":"high","\udc00":"low","é":"accent","😀":"pair","\u0000":"nul","０":"wide zero"}"###,
            ),
            (
                r###"{"10":"ten","2":"two","1":"one","0":"zero","4294967296":"overflow","9007199254740991":"unsafe index","-1":"negative","1.0":"decimal"}"###,
                r###"{"0":"zero","1":"one","2":"two","10":"ten","4294967296":"overflow","9007199254740991":"unsafe index","-1":"negative","1.0":"decimal"}"###,
            ),
            (
                r###"{"tools":[{"name":"Synthetic","input_schema":{"type":"object","properties":{"z":{},"2":{"9":"nine","0":"zero","b":"letter"},"a":{}}}}],"messages":[{"role":"user","content":[{"type":"text","text":"Synthetic \\ and \" and \u0000"}]}]}"###,
                r###"{"tools":[{"name":"Synthetic","input_schema":{"type":"object","properties":{"2":{"0":"zero","9":"nine","b":"letter"},"z":{},"a":{}}}}],"messages":[{"role":"user","content":[{"type":"text","text":"Synthetic \\ and \" and \u0000"}]}]}"###,
            ),
            (
                r###"{"a":"old","2":"first","a":"new","02":"ordinary","2":"replaced","__proto__":{"9":"nine","1":"one","z":"last"}}"###,
                r###"{"2":"replaced","a":"new","02":"ordinary","__proto__":{"1":"one","9":"nine","z":"last"}}"###,
            ),
        ];
        for (input, expected) in cases {
            let document = JsDocument::parse(input.as_bytes()).unwrap();
            assert_eq!(document.stringify(), expected);
            let JsNode::Object(object) = document.node(document.root()).unwrap() else {
                panic!("object fixture")
            };
            assert_eq!(
                object.ordered().collect::<Vec<_>>(),
                previous_object_order(object)
            );
        }
    }

    #[test]
    fn object_order_setters_keep_positions_and_enter_indexed_fallback() {
        let mut document = JsDocument::parse(
            br#"{"later":"first","earlier":"second","outer":{"b":"bee","a":"aye"}}"#,
        )
        .unwrap();
        let steps = [
            (None, "later", br#""updated""#.as_slice()),
            (None, "9", br#""nine""#.as_slice()),
            (None, "0", br#""zero""#.as_slice()),
            (None, "9", br#""replaced""#.as_slice()),
            (Some("outer"), "2", br#""two""#.as_slice()),
        ];
        // Actual Node operations replace an existing key, append numeric keys,
        // replace a numeric key, then mutate a nested ordinary-key object.
        let expected = [
            r###"{"later":"updated","earlier":"second","outer":{"b":"bee","a":"aye"}}"###,
            r###"{"9":"nine","later":"updated","earlier":"second","outer":{"b":"bee","a":"aye"}}"###,
            r###"{"0":"zero","9":"nine","later":"updated","earlier":"second","outer":{"b":"bee","a":"aye"}}"###,
            r###"{"0":"zero","9":"replaced","later":"updated","earlier":"second","outer":{"b":"bee","a":"aye"}}"###,
            r###"{"0":"zero","9":"replaced","later":"updated","earlier":"second","outer":{"2":"two","b":"bee","a":"aye"}}"###,
        ];
        for (index, ((parent, key, value), expected)) in steps.into_iter().zip(expected).enumerate()
        {
            let parent = parent.map_or(document.root(), |name| {
                document.get(document.root(), name).unwrap()
            });
            document.set_field_json(parent, key, value).unwrap();
            assert_eq!(document.stringify(), expected);
            assert_eq!(
                serde_json::to_string(&document.to_serde_observation_lossy()).unwrap(),
                expected
            );
            let JsNode::Object(object) = document.node(parent).unwrap() else {
                panic!("object fixture")
            };
            assert_eq!(
                object.ordered().collect::<Vec<_>>(),
                previous_object_order(object)
            );
            assert_eq!(
                matches!(object.ordered(), ObjectOrder::Natural(_)),
                index == 0
            );
        }
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

#[cfg(test)]
mod writer_tests {
    use super::*;

    // Uses only the default reserve/push implementations: no String facade or
    // serde projection can silently substitute for the generic writer path.
    #[derive(Default)]
    struct ByteSink(Vec<u8>);
    impl JsonWrite for ByteSink {
        fn push_str(&mut self, value: &str) {
            self.0.extend_from_slice(value.as_bytes());
        }
    }

    #[test]
    fn string_and_fragment_writers_match_literal_json_and_append_without_reset() {
        // Fixed expectations include the existing frozen Node ordering and
        // surrogate cases; fresh parser/writer outputs are never their oracle.
        let cases = [
            (r#"null"#, r#"null"#),
            (
                r#"[true,false,null,-0,1e400,-1e400,1e21,1e-7]"#,
                r#"[true,false,null,0,null,null,1e+21,1e-7]"#,
            ),
            (
                r#"{"z":1,"2":2,"1":1,"z":3,"01":4,"4294967295":5}"#,
                r#"{"1":1,"2":2,"z":3,"01":4,"4294967295":5}"#,
            ),
            (
                r#"{"\ud800":"\udc00","pair":"\ud83e\udd80","n":9007199254740993}"#,
                r#"{"\ud800":"\udc00","pair":"🦀","n":9007199254740992}"#,
            ),
            (
                r#"{"__proto__":{"9":"nine","1":"one"},"escapes":"\u0000\u001f\b\t\n\f\r\"\\\/"}"#,
                r#"{"__proto__":{"1":"one","9":"nine"},"escapes":"\u0000\u001f\b\t\n\f\r\"\\/"}"#,
            ),
        ];
        for (input, expected) in cases {
            let document = JsDocument::parse(input.as_bytes()).unwrap();
            assert_eq!(document.stringify(), expected);
            let mut string = String::from("prefix|");
            document.write_node(document.root(), &mut string);
            assert_eq!(string, format!("prefix|{expected}"));

            let mut sink = ByteSink(b"prefix|".to_vec());
            let writer: &mut dyn JsonWrite = &mut sink;
            document.write_node(document.root(), writer);
            writer.push('|');
            JsString::from_scalar("after").write_json(writer);
            assert_eq!(sink.0, format!("prefix|{expected}|\"after\"").as_bytes());
        }
    }

    #[test]
    fn scalar_and_utf16_string_writers_keep_control_and_surrogate_escapes() {
        for (units, expected) in [
            (vec![0xd800], r#""\ud800""#),
            (vec![0xdc00], r#""\udc00""#),
            (vec![0xdabc, 0x61, 0xdfff], r#""\udabca\udfff""#),
            (vec![0xd83e, 0xdd80], "\"🦀\""),
            (vec![0xd800, 0xdc00], "\"𐀀\""),
            (
                vec![0, 0x1f, 8, 9, 10, 12, 13, 34, 92],
                r#""\u0000\u001f\b\t\n\f\r\"\\""#,
            ),
        ] {
            let value = JsString::from_utf16(units);
            let mut sink = ByteSink::default();
            value.write_json(&mut sink);
            assert_eq!(sink.0, expected.as_bytes());
            assert_eq!(value.stringify(), expected);
        }
        for text in ["", "plain", "é水🦀", "\0\u{1f}\n\t\r\"\\🦀\u{2028}\u{2029}"] {
            let scalar = JsString::from_scalar(text);
            let utf16 = JsString::from_utf16(text.encode_utf16().collect());
            let mut left = ByteSink::default();
            let mut right = ByteSink::default();
            scalar.write_json(&mut left);
            utf16.write_json(&mut right);
            assert_eq!(left.0, right.0);
        }
    }

    #[test]
    fn appending_to_reserved_string_preserves_existing_storage_and_prefix() {
        let value = JsString::from_scalar(&"ordinary é水🦀 ".repeat(100));
        let mut output = String::with_capacity(16 * 1024);
        output.push_str("retained|");
        let pointer = output.as_ptr();
        let capacity = output.capacity();
        value.write_json(&mut output);
        assert_eq!(output, format!("retained|{}", value.stringify()));
        assert_eq!(output.capacity(), capacity);
        assert_eq!(output.as_ptr(), pointer);
    }

    #[test]
    fn unescaped_scalar_keeps_bulk_writes_without_populating_utf16_cache() {
        #[derive(Default)]
        struct CountSink {
            bytes: usize,
            largest_fragment: usize,
        }
        impl JsonWrite for CountSink {
            fn push_str(&mut self, value: &str) {
                self.bytes += value.len();
                self.largest_fragment = self.largest_fragment.max(value.len());
            }
        }
        let text = "ordinary é水🦀 ".repeat(1024);
        let value = JsString::from_scalar(&text);
        assert!(value.0.units.get().is_none());
        let mut sink = CountSink::default();
        value.write_json(&mut sink);
        assert_eq!(sink.bytes, text.len() + 2);
        assert_eq!(sink.largest_fragment, text.len());
        assert!(value.0.units.get().is_none());
    }

    #[test]
    fn subtree_and_mutated_document_writes_keep_authoritative_order_and_values() {
        let mut document = JsDocument::parse(
            br#"{"before":true,"nested":{"9":"nine","2":"two","x":"\ud800"},"after":[-0,1e400]}"#,
        )
        .unwrap();
        let nested = document.get(document.root(), "nested").unwrap();
        let mut output = ByteSink::default();
        document.write_node(nested, &mut output);
        assert_eq!(output.0, br#"{"2":"two","9":"nine","x":"\ud800"}"#);
        document.set_root_field_json("before", b"false").unwrap();
        let mut updated = ByteSink::default();
        document.write_node(document.root(), &mut updated);
        assert_eq!(
            updated.0,
            br#"{"before":false,"nested":{"2":"two","9":"nine","x":"\ud800"},"after":[0,null]}"#
        );
    }

    #[test]
    fn fragment_writer_keeps_iterative_deep_data_traversal() {
        let input = format!("{}0{}", "[".repeat(20_000), "]".repeat(20_000));
        let document = JsDocument::parse(input.as_bytes()).unwrap();
        let mut sink = ByteSink::default();
        document.write_node(document.root(), &mut sink);
        assert_eq!(sink.0, input.as_bytes());
    }
}
