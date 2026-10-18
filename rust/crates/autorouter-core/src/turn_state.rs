//! Active execution ownership, independent of the disposable classifier cache.
use crate::js_json::{JsDocument, JsNode, JsString};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

/// Exact consumed identities accompany an explicitly display-only metadata map.
#[derive(Clone)]
pub struct Pin {
    pub value: Value,
    pub model: JsString,
    pub requested_model: JsString,
    pub tools: Vec<ToolIdentity>,
    confirmed: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolIdentity {
    pub id: JsString,
    pub model: JsString,
}
#[derive(Clone)]
pub struct ContinuationEvidence {
    pub model: JsString,
    pub tools: Vec<ToolIdentity>,
}
#[derive(Clone)]
pub enum ToolOwner {
    Ambiguous,
    Owned { key: String, pin: Pin },
}
impl Pin {
    pub fn new(model: JsString, requested_model: JsString) -> Self {
        Self {
            value: json!({"model":model.to_well_formed(),"requestedModel":requested_model.to_well_formed()}),
            model,
            requested_model,
            tools: Vec::new(),
            confirmed: false,
        }
    }
    fn from_value(value: Value) -> Self {
        let model = value
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("")
            .into();
        let requested_model = value
            .get("requestedModel")
            .and_then(Value::as_str)
            .unwrap_or("")
            .into();
        let tools = value
            .get("toolModels")
            .and_then(Value::as_array)
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|tool| {
                        Some(ToolIdentity {
                            id: tool.get("id")?.as_str()?.into(),
                            model: tool.get("model")?.as_str()?.into(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            value,
            model,
            requested_model,
            tools,
            confirmed: false,
        }
    }

    /// Only a committed, validated completion can establish confirmation.
    pub fn is_confirmed(&self) -> bool {
        self.confirmed
    }
}

struct Record {
    keys: Vec<String>,
    scope: String,
    active: bool,
    pending: usize,
    sequence: u64,
    created_sequence: u64,
    expires: Option<u64>,
    pin: Option<Pin>,
    metadata: Option<JsDocument>,
    retains_tools: bool,
}
struct Attempt {
    record: u64,
    pin: Pin,
    sequence: u64,
    exact: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum CommitKind {
    SelectedValue,
    SelectedExact,
    Confirmed { exact: bool },
}

#[derive(Default)]
pub struct Selection<'a> {
    pub scope: &'a str,
    pub request_id: Option<&'a str>,
    pub sequence: u64,
}

pub struct TurnState {
    limit: usize,
    idle_ttl_ms: u64,
    next_record: u64,
    records: BTreeMap<u64, Record>,
    aliases: HashMap<String, u64>,
    attempts: HashMap<String, Attempt>,
}

impl Default for TurnState {
    fn default() -> Self {
        Self::new(1000, 30 * 60 * 1000)
    }
}

impl TurnState {
    pub fn new(limit: usize, idle_ttl_ms: u64) -> Self {
        Self {
            limit,
            idle_ttl_ms,
            next_record: 0,
            records: BTreeMap::new(),
            aliases: HashMap::new(),
            attempts: HashMap::new(),
        }
    }

    pub fn record_count(&self) -> usize {
        self.records.len()
    }
    pub fn attempt_count(&self) -> usize {
        self.attempts.len()
    }
    pub fn alias_count(&self) -> usize {
        self.aliases.len()
    }

    fn remove(&mut self, id: u64) {
        if let Some(record) = self.records.remove(&id) {
            for key in record.keys {
                if self.aliases.get(&key) == Some(&id) {
                    self.aliases.remove(&key);
                }
            }
        }
    }

    pub fn get(&mut self, key: &str, now_ms: u64) -> Option<&Value> {
        self.get_exact(key, now_ms).map(|pin| &pin.value)
    }
    pub fn get_exact(&mut self, key: &str, now_ms: u64) -> Option<&Pin> {
        let &id = self.aliases.get(key)?;
        let record = self.records.get(&id)?;
        if !record.active
            && record.pending == 0
            && record.expires.is_some_and(|expires| expires <= now_ms)
        {
            self.remove(id);
            return None;
        }
        if self.ambiguous(key) {
            return None;
        }
        self.records.get(&id)?.pin.as_ref()
    }

    /// Exact committed metadata, including spread-generated surrogate units.
    /// Applies the same expiry and ambiguous-alias checks as `get_exact`.
    pub fn get_metadata_exact(&mut self, key: &str, now_ms: u64) -> Option<&JsDocument> {
        self.get_exact(key, now_ms)?;
        self.records.get(self.aliases.get(key)?)?.metadata.as_ref()
    }

    pub fn ambiguous(&self, key: &str) -> bool {
        let Some(record) = self.aliases.get(key).and_then(|id| self.records.get(id)) else {
            return false;
        };
        record.keys.first().is_none_or(|first| first != key)
            && self
                .records
                .values()
                .filter(|other| {
                    other.active
                        && other.pin.is_some()
                        && other.scope == record.scope
                        && other.keys.iter().any(|k| k == key)
                })
                .take(2)
                .count()
                > 1
    }

    pub fn tool_owner(&self, scope: &str, ids: &[String]) -> Option<Value> {
        match self.tool_owner_exact(
            scope,
            &ids.iter().cloned().map(JsString::from).collect::<Vec<_>>(),
        )? {
            ToolOwner::Ambiguous => Some(json!({"ambiguous":true})),
            ToolOwner::Owned { key, pin } => Some(json!({"key":key,"pin":pin.value})),
        }
    }
    pub fn tool_owner_exact(&self, scope: &str, ids: &[JsString]) -> Option<ToolOwner> {
        if ids.is_empty() {
            return None;
        }
        let mut matches = self.records.values().filter(|record| {
            record.scope == scope
                && record.pin.as_ref().is_some_and(|pin| {
                    pin.is_confirmed()
                        && ids
                            .iter()
                            .all(|id| pin.tools.iter().any(|tool| &tool.id == id))
                })
        });
        let first = matches.next()?;
        if matches.next().is_some() {
            return Some(ToolOwner::Ambiguous);
        }
        Some(ToolOwner::Owned {
            key: first.keys.first()?.clone(),
            pin: first.pin.clone()?,
        })
    }

    pub fn select(
        &mut self,
        keys: &[String],
        pin: Value,
        selection: Selection<'_>,
        now_ms: u64,
    ) -> bool {
        self.select_pin(
            keys,
            Pin::from_value(pin),
            selection,
            now_ms,
            CommitKind::SelectedValue,
        )
    }
    pub fn select_exact(
        &mut self,
        keys: &[String],
        pin: Pin,
        selection: Selection<'_>,
        now_ms: u64,
    ) -> bool {
        self.select_pin(keys, pin, selection, now_ms, CommitKind::SelectedExact)
    }

    fn select_pin(
        &mut self,
        keys: &[String],
        pin: Pin,
        selection: Selection<'_>,
        now_ms: u64,
        kind: CommitKind,
    ) -> bool {
        let Some(primary) = keys.first() else {
            return false;
        };
        let request_id = selection.request_id.filter(|id| !id.is_empty());
        // Admission is checked before mutating sequences or installing aliases.
        if request_id
            .is_some_and(|id| self.attempts.len() >= self.limit || self.attempts.contains_key(id))
        {
            return false;
        }
        let id = match self.aliases.get(primary).copied() {
            Some(id) => id,
            None => {
                // Iteration order matters when pressure retires the oldest idle
                // record. BTreeMap IDs preserve the source Set insertion order.
                let candidates: Vec<_> = self.records.keys().copied().collect();
                for old in candidates {
                    let record = &self.records[&old];
                    if !record.active
                        && record.pending == 0
                        && (record.expires.is_some_and(|expires| expires <= now_ms)
                            || self.records.len() >= self.limit)
                    {
                        self.remove(old);
                    }
                }
                if self.records.len() >= self.limit {
                    return false;
                }
                let id = self.next_record;
                self.next_record = self
                    .next_record
                    .checked_add(1)
                    .expect("task identity exhausted");
                self.records.insert(
                    id,
                    Record {
                        keys: Vec::new(),
                        scope: selection.scope.to_owned(),
                        active: true,
                        pending: 0,
                        sequence: selection.sequence,
                        created_sequence: selection.sequence,
                        expires: None,
                        pin: None,
                        metadata: None,
                        retains_tools: false,
                    },
                );
                id
            }
        };
        let record = self.records.get_mut(&id).expect("admitted record");
        record.sequence = record.sequence.max(selection.sequence);
        for key in keys {
            if record.keys.contains(key) {
                continue;
            }
            record.keys.push(key.clone());
            if record.keys.len() == 1 {
                self.aliases.insert(key.clone(), id);
            }
            if record.keys.len() > 8 {
                let expired = record.keys.remove(1);
                if self.aliases.get(&expired) == Some(&id) {
                    self.aliases.remove(&expired);
                }
            }
        }
        if let Some(request_id) = request_id {
            record.pending += 1;
            self.attempts.insert(
                request_id.to_owned(),
                Attempt {
                    record: id,
                    pin,
                    sequence: selection.sequence,
                    exact: kind == CommitKind::SelectedExact,
                },
            );
        } else {
            self.commit(id, pin, selection.sequence, now_ms, kind);
        }
        true
    }

    fn commit(&mut self, id: u64, mut pin: Pin, sequence: u64, now_ms: u64, kind: CommitKind) {
        let Some(record) = self.records.get(&id) else {
            return;
        };
        if sequence != record.sequence {
            return;
        }
        let keys = record.keys.clone();
        let scope = record.scope.clone();
        let created = record.created_sequence;
        let mut metadata = JsDocument::from_serde_value(&pin.value).into_object_spread();
        let retains_tools = if kind == CommitKind::SelectedValue {
            raw_tools_retain(&metadata)
        } else {
            !pin.tools.is_empty()
        };
        pin.value = spread_observation(pin.value);
        pin.confirmed = matches!(kind, CommitKind::Confirmed { .. });
        if kind != CommitKind::SelectedValue {
            pin.value["model"] = json!(pin.model.to_well_formed());
            metadata
                .set_root_field_json("model", pin.model.stringify().as_bytes())
                .expect("exact model JSON");
            if matches!(
                kind,
                CommitKind::SelectedExact | CommitKind::Confirmed { exact: true }
            ) {
                pin.value["requestedModel"] = json!(pin.requested_model.to_well_formed());
                metadata
                    .set_root_field_json(
                        "requestedModel",
                        pin.requested_model.stringify().as_bytes(),
                    )
                    .expect("exact requested model JSON");
            }
        }
        pin.value["confirmed"] = json!(pin.confirmed);
        metadata
            .set_root_field_json("confirmed", if pin.confirmed { b"true" } else { b"false" })
            .expect("confirmation JSON");
        if kind != CommitKind::SelectedValue
            && (pin.confirmed || !pin.tools.is_empty() || pin.value.get("toolModels").is_some())
        {
            pin.value["toolModels"] = json!(pin.tools.iter().map(|tool| json!({"id":tool.id.to_well_formed(),"model":tool.model.to_well_formed()})).collect::<Vec<_>>());
            let tools = format!(
                "[{}]",
                pin.tools
                    .iter()
                    .map(|tool| format!(
                        "{{\"id\":{},\"model\":{}}}",
                        tool.id.stringify(),
                        tool.model.stringify()
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            metadata
                .set_root_field_json("toolModels", tools.as_bytes())
                .expect("exact tools JSON");
        }
        let record = self.records.get_mut(&id).unwrap();
        record.pin = Some(pin);
        record.metadata = Some(metadata);
        record.retains_tools = retains_tools;
        for key in keys {
            let current = self.aliases.get(&key).and_then(|id| self.records.get(id));
            if current
                .is_none_or(|current| current.pin.is_none() || current.created_sequence <= created)
                || self.aliases.get(&key) == Some(&id)
            {
                self.aliases.insert(key, id);
            }
        }
        let newer_exists = self.records.iter().any(|(&other_id, other)| {
            other_id != id
                && other.scope == scope
                && other.pin.is_some()
                && other.created_sequence > created
        });
        let record = self.records.get_mut(&id).unwrap();
        record.active = record.retains_tools || !newer_exists;
        if !record.active {
            record.expires = Some(now_ms.saturating_add(self.idle_ttl_ms));
        }
        for (&other_id, old) in &mut self.records {
            if other_id != id
                && old.scope == scope
                && old.created_sequence < created
                && old.pending == 0
                && !old.retains_tools
            {
                old.active = false;
                old.expires = Some(now_ms.saturating_add(self.idle_ttl_ms));
            }
        }
    }

    /// Evidence is accepted only after the caller proves clean protocol EOF and
    /// downstream delivery. Failure/cancellation passes None and releases staging.
    pub fn complete(&mut self, request_id: &str, evidence: Option<&Value>, now_ms: u64) -> bool {
        let exact = evidence.and_then(|evidence| {
            let model: JsString = evidence.get("continuation_model")?.as_str()?.into();
            let tools = match evidence.get("tool_uses") {
                None | Some(Value::Null) => Vec::new(),
                Some(value) => value
                    .as_array()?
                    .iter()
                    .map(|tool| {
                        Some(ToolIdentity {
                            id: tool.get("id")?.as_str()?.into(),
                            model: tool.get("model")?.as_str()?.into(),
                        })
                    })
                    .collect::<Option<Vec<_>>>()?,
            };
            Some(ContinuationEvidence { model, tools })
        });
        self.complete_exact(request_id, exact.as_ref(), now_ms)
    }
    pub fn complete_exact(
        &mut self,
        request_id: &str,
        evidence: Option<&ContinuationEvidence>,
        now_ms: u64,
    ) -> bool {
        let Some(attempt) = self.attempts.remove(request_id) else {
            return false;
        };
        let id = attempt.record;
        let record = self
            .records
            .get_mut(&id)
            .expect("pending attempt retains record");
        record.pending -= 1;
        if let Some(evidence) = evidence.filter(|evidence| {
            !evidence.model.units().is_empty() && attempt.sequence == record.sequence
        }) && evidence.tools.len() <= 1000
            && evidence.tools.iter().all(|tool| {
                !tool.id.units().is_empty()
                    && tool.id.units().len() <= 256
                    && tool.model == evidence.model
            })
        {
            let mut pin = attempt.pin;
            pin.model = evidence.model.clone();
            pin.tools = evidence.tools.clone();
            self.commit(
                id,
                pin,
                attempt.sequence,
                now_ms,
                CommitKind::Confirmed {
                    exact: attempt.exact,
                },
            );
            return true;
        }
        let record = &self.records[&id];
        if record.pin.is_none() && record.pending == 0 {
            self.remove(id);
        } else if record.pending == 0
            && !record.retains_tools
            && self.records.iter().any(|(&other_id, other)| {
                other_id != id
                    && other.scope == record.scope
                    && other.pin.is_some()
                    && other.created_sequence > record.created_sequence
            })
        {
            let record = self.records.get_mut(&id).unwrap();
            record.active = false;
            record.expires = Some(now_ms.saturating_add(self.idle_ttl_ms));
        }
        false
    }
}

fn raw_tools_retain(metadata: &JsDocument) -> bool {
    let Some(tools) = metadata.get(metadata.root(), "toolModels") else {
        return false;
    };
    match metadata.node(tools) {
        Some(JsNode::Array(values)) => !values.is_empty(),
        Some(JsNode::String(value)) => !value.units().is_empty(),
        Some(JsNode::Object(_)) => metadata.is_truthy(metadata.get(tools, "length")),
        _ => false,
    }
}

// Preserve the existing Value observation boundary and opaque object members.
// Exact surrogate units produced by string spread live in Record.metadata.
fn spread_observation(value: Value) -> Value {
    match value {
        Value::Object(_) => value,
        Value::Array(values) => Value::Object(
            values
                .into_iter()
                .enumerate()
                .map(|(i, value)| (i.to_string(), value))
                .collect(),
        ),
        Value::String(value) => Value::Object(
            value
                .encode_utf16()
                .enumerate()
                .map(|(i, unit)| (i.to_string(), json!(String::from_utf16_lossy(&[unit]))))
                .collect(),
        ),
        _ => json!({}),
    }
}

/// Deterministic, development-only state-machine adapter. The product uses the
/// typed operations directly; no clocks or I/O are hidden inside TurnState.
pub fn run_fixture(input: &Value) -> Result<Value, String> {
    let mut state = TurnState::new(
        input.get("limit").and_then(Value::as_u64).unwrap_or(1000) as usize,
        input
            .get("idle_ttl_ms")
            .and_then(Value::as_u64)
            .unwrap_or(30 * 60 * 1000),
    );
    let mut now = input.get("now").and_then(Value::as_u64).unwrap_or(0);
    let mut results = Vec::new();
    for step in input
        .get("steps")
        .and_then(Value::as_array)
        .ok_or("turn_state requires steps")?
    {
        let result = match step
            .get("action")
            .and_then(Value::as_str)
            .ok_or("turn_state step requires action")?
        {
            "now" => {
                now = step
                    .get("value")
                    .and_then(Value::as_u64)
                    .ok_or("now requires value")?;
                Value::Null
            }
            "select" => {
                let keys = strings(step.get("keys").ok_or("select requires keys")?)?;
                json!(state.select(
                    &keys,
                    step.get("pin").cloned().ok_or("select requires pin")?,
                    Selection {
                        scope: step.get("scope").and_then(Value::as_str).unwrap_or(""),
                        request_id: step.get("request_id").and_then(Value::as_str),
                        sequence: step.get("sequence").and_then(Value::as_u64).unwrap_or(0),
                    },
                    now
                ))
            }
            "get" => state
                .get(
                    step.get("key")
                        .and_then(Value::as_str)
                        .ok_or("get requires key")?,
                    now,
                )
                .cloned()
                .unwrap_or(Value::Null),
            "ambiguous" => json!(
                state.ambiguous(
                    step.get("key")
                        .and_then(Value::as_str)
                        .ok_or("ambiguous requires key")?
                )
            ),
            "tool_owner" => state
                .tool_owner(
                    step.get("scope").and_then(Value::as_str).unwrap_or(""),
                    &strings(step.get("ids").ok_or("tool_owner requires ids")?)?,
                )
                .unwrap_or(Value::Null),
            "complete" => json!(
                state.complete(
                    step.get("request_id")
                        .and_then(Value::as_str)
                        .ok_or("complete requires request_id")?,
                    step.get("evidence"),
                    now
                )
            ),
            "counts" => {
                json!({"records":state.record_count(), "attempts":state.attempt_count(), "aliases":state.alias_count()})
            }
            _ => return Err("unknown turn_state action".to_owned()),
        };
        results.push(result);
    }
    Ok(json!(results))
}

fn strings(value: &Value) -> Result<Vec<String>, String> {
    value
        .as_array()
        .ok_or("expected string array")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| "expected string array".to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_metadata_retains_spread_utf16_units_and_committed_identities() {
        let mut state = TurnState::default();
        assert!(state.select(
            &["string".into()],
            json!("A😀e\u{301}"),
            Selection::default(),
            0
        ));
        assert_eq!(
            state.get_metadata_exact("string", 0).unwrap().stringify(),
            r#"{"0":"A","1":"\ud83d","2":"\ude00","3":"e","4":"́","confirmed":false}"#
        );
        assert_eq!(state.get("string", 0).unwrap()["1"], "\u{fffd}");
        let model = JsString::from_utf16(vec![0xd800]);
        let id = JsString::from_utf16(vec![0xd801]);
        assert!(state.select_exact(
            &["exact".into()],
            Pin::new(model.clone(), model.clone()),
            Selection {
                scope: "s",
                request_id: Some("done"),
                sequence: 1,
            },
            0
        ));
        assert!(state.complete_exact(
            "done",
            Some(&ContinuationEvidence {
                model: model.clone(),
                tools: vec![ToolIdentity {
                    id: id.clone(),
                    model: model.clone()
                }],
            }),
            0
        ));
        assert!(state.get_exact("exact", 0).unwrap().is_confirmed());
        let metadata = state.get_metadata_exact("exact", 0).unwrap();
        assert_eq!(
            metadata.string(metadata.get(metadata.root(), "model").unwrap()),
            Some(&model)
        );
        assert_eq!(
            metadata.string(metadata.get(metadata.root(), "requestedModel").unwrap()),
            Some(&model)
        );
        assert!(metadata.stringify().contains(r#""id":"\ud801""#));
        assert!(matches!(
            state.tool_owner_exact("s", &[id]),
            Some(ToolOwner::Owned { .. })
        ));
        assert!(state.tool_owner("s", &["\u{fffd}".into()]).is_none());
        assert!(state.get_metadata_exact("absent", 0).is_none());
    }

    #[test]
    fn exact_metadata_obeys_alias_ambiguity_and_retired_expiry() {
        let mut state = TurnState::new(1000, 10);
        assert!(stage(&mut state, &["old", "shared"], "opus", "old", 1));
        assert!(state.complete(
            "old",
            Some(&done("opus", json!([{"id":"tool", "model":"opus"}]))),
            0
        ));
        assert!(stage(&mut state, &["new", "shared"], "haiku", "new", 2));
        assert!(state.complete("new", Some(&done("haiku", json!([]))), 0));
        assert!(state.get_metadata_exact("shared", 0).is_none());
        assert!(state.get_metadata_exact("old", 10).is_some());
        assert!(stage(&mut state, &["old"], "opus", "released", 3));
        assert!(state.complete("released", Some(&done("opus", json!([]))), 10));
        assert!(state.get_metadata_exact("old", 19).is_some());
        assert!(state.get_metadata_exact("old", 20).is_none());
        assert!(state.get_metadata_exact("shared", 20).is_some());
        assert_eq!((state.record_count(), state.alias_count()), (1, 2));
    }

    #[test]
    fn failed_value_attempts_never_replace_confirmed_metadata_or_publish_aliases() {
        let mut state = TurnState::default();
        assert!(stage(&mut state, &["old"], "opus", "initial", 1));
        assert!(state.complete(
            "initial",
            Some(&done("opus", json!([{"id":"tool", "model":"opus"}]))),
            0
        ));
        let original = state.get_metadata_exact("old", 0).unwrap().stringify();
        let mut sequence = 1;
        for input in [
            Value::Null,
            json!(false),
            json!(42),
            json!("x"),
            json!([1]),
            json!({"confirmed":true}),
        ] {
            for evidence in [None, Some(done("opus", json!([null])))] {
                sequence += 1;
                for (key, retained) in [("old", true), ("fresh", false)] {
                    assert!(state.select(
                        &[key.into(), "failed-alias".into()],
                        input.clone(),
                        Selection {
                            scope: "s",
                            request_id: Some("retry"),
                            sequence,
                        },
                        1_800_000
                    ));
                    assert!(state.get("failed-alias", 1_800_000).is_none());
                    assert!(!state.complete("retry", evidence.as_ref(), 1_800_000));
                    assert_eq!(state.get(key, 3_600_000).is_some(), retained);
                    assert!(state.get("failed-alias", 3_600_000).is_none());
                    assert!(state.get_exact("old", 3_600_000).unwrap().is_confirmed());
                    assert_eq!(
                        state
                            .get_metadata_exact("old", 3_600_000)
                            .unwrap()
                            .stringify(),
                        original
                    );
                    assert!(state.tool_owner("s", &["tool".into()]).is_some());
                    assert_eq!((state.record_count(), state.attempt_count()), (1, 0));
                }
            }
        }
    }

    #[test]
    fn committed_metadata_preserves_deep_opaque_value_members() {
        let mut opaque = json!({"leaf":"retained"});
        for _ in 0..96 {
            opaque = json!([opaque]);
        }
        let mut state = TurnState::default();
        assert!(state.select(
            &["deep".into()],
            json!({"opaque":opaque.clone()}),
            Selection::default(),
            0
        ));
        assert_eq!(state.get("deep", 0).unwrap()["opaque"], opaque);
        let metadata = state.get_metadata_exact("deep", 0).unwrap();
        let node = metadata.get(metadata.root(), "opaque").unwrap();
        assert_eq!(
            metadata.stringify_node(node),
            JsDocument::from_serde_value(&opaque).stringify()
        );
    }

    #[test]
    fn selected_value_pins_spread_every_json_kind_without_confirmation() {
        for (input, expected) in [
            (Value::Null, json!({"confirmed": false})),
            (json!(true), json!({"confirmed": false})),
            (json!(42), json!({"confirmed": false})),
            (
                json!(["a", {"opaque": true}]),
                json!({"0": "a", "1": {"opaque": true}, "confirmed": false}),
            ),
            (json!("aé"), json!({"0": "a", "1": "é", "confirmed": false})),
            (
                json!({"confirmed": true, "opaque": [1]}),
                json!({"confirmed": false, "opaque": [1]}),
            ),
        ] {
            let mut state = TurnState::default();
            assert!(state.select(&["task".into()], input, Selection::default(), 0));
            assert_eq!(state.get("task", 0), Some(&expected));
            assert!(state.tool_owner("", &["tool".into()]).is_none());
        }
    }

    #[test]
    fn completion_of_primitive_pin_retains_tools_until_they_are_released() {
        for input in [
            Value::Null,
            json!(false),
            json!(42),
            json!("a"),
            json!([1]),
            json!({}),
        ] {
            let mut state = TurnState::new(2, 1_800_000);
            assert!(state.select(
                &["old".into(), "alias".into()],
                input,
                Selection {
                    scope: "s",
                    request_id: Some("initial"),
                    sequence: 1,
                },
                0
            ));
            assert!(state.complete(
                "initial",
                Some(&done("opus", json!([{"id":"tool", "model":"opus"}]))),
                0
            ));
            assert!(stage(&mut state, &["new"], "haiku", "new", 2));
            assert!(state.complete("new", Some(&done("haiku", json!([]))), 0));
            assert_eq!(state.get("alias", 1_800_000).unwrap()["model"], "opus");
            assert_eq!(
                state.tool_owner("s", &["tool".into()]).unwrap()["key"],
                "old"
            );
            assert!(state.tool_owner("other", &["tool".into()]).is_none());
            assert!(!stage(&mut state, &["pressure"], "sonnet", "pressure", 3));
            assert!(stage(&mut state, &["old"], "sonnet", "failed-retry", 4));
            assert!(!state.complete("failed-retry", None, 1_800_000));
            assert_eq!(state.get("old", 3_600_000).unwrap()["model"], "opus");
            assert!(stage(&mut state, &["old"], "sonnet", "release", 5));
            assert!(state.complete("release", Some(&done("opus", json!([]))), 3_600_000));
            assert!(state.tool_owner("s", &["tool".into()]).is_none());
            assert!(state.get("old", 5_399_999).is_some());
            assert!(state.get("old", 5_400_000).is_none());
            assert_eq!(state.get("new", 5_400_000).unwrap()["model"], "haiku");
            assert_eq!(
                (
                    state.record_count(),
                    state.attempt_count(),
                    state.alias_count()
                ),
                (1, 0, 1)
            );
        }
    }

    #[test]
    fn value_selection_retention_preserves_raw_tool_length_truthiness() {
        for (tools, retains) in [
            (json!([null]), true),
            (json!("x"), true),
            (json!({"length": 1}), true),
            (json!({"length": []}), true),
            (json!([]), false),
            (json!(""), false),
            (json!({"length": 0}), false),
            (json!({"length": null}), false),
        ] {
            let mut state = TurnState::new(1000, 10);
            assert!(state.select(
                &["old".into()],
                json!({"toolModels": tools, "confirmed": true}),
                Selection {
                    scope: "s",
                    sequence: 1,
                    ..Default::default()
                },
                0
            ));
            assert!(stage(&mut state, &["new"], "haiku", "new", 2));
            assert!(state.complete("new", Some(&done("haiku", json!([]))), 0));
            assert_eq!(state.get("old", 10).is_some(), retains);
            assert!(state.tool_owner("s", &["tool".into()]).is_none());
        }
    }

    #[test]
    fn exact_selection_uses_typed_identities_and_tools_over_display_metadata() {
        let mut state = TurnState::new(1000, 10);
        let model = JsString::from_utf16(vec![0xd800]);
        let mut pin = Pin::new(model.clone(), "requested".into());
        pin.value = json!({"model": "forged", "confirmed": true, "toolModels": []});
        pin.tools.push(ToolIdentity {
            id: "pending".into(),
            model: model.clone(),
        });
        assert!(state.select_exact(
            &["old".into()],
            pin,
            Selection {
                scope: "s",
                sequence: 1,
                ..Default::default()
            },
            0
        ));
        assert!(stage(&mut state, &["new"], "haiku", "new", 2));
        assert!(state.complete("new", Some(&done("haiku", json!([]))), 0));
        let pin = state
            .get_exact("old", 10)
            .expect("exact tools retain the task");
        assert_eq!(pin.model, model);
        assert_eq!(pin.value["model"], "\u{fffd}");
        assert_eq!(pin.value["confirmed"], false);
        assert!(state.tool_owner_exact("s", &["pending".into()]).is_none());
    }

    #[test]
    fn exact_tool_and_model_ownership_does_not_collapse_repaired_unicode() {
        let mut state = TurnState::default();
        for (index, unit) in [0xd800, 0xd801].into_iter().enumerate() {
            let key = index.to_string();
            let model = JsString::from_utf16(vec![unit]);
            let tool = JsString::from_utf16(vec![unit]);
            assert!(state.select_exact(
                std::slice::from_ref(&key),
                Pin::new(model.clone(), model.clone()),
                Selection {
                    scope: "s",
                    request_id: Some(&key),
                    sequence: index as u64
                },
                0
            ));
            let evidence = ContinuationEvidence {
                model: model.clone(),
                tools: vec![ToolIdentity {
                    id: tool.clone(),
                    model: model.clone(),
                }],
            };
            assert!(state.complete_exact(&key, Some(&evidence), 0));
            assert_eq!(state.get_exact(&key, 0).unwrap().model, model);
            match state.tool_owner_exact("s", &[tool]) {
                Some(ToolOwner::Owned { pin, .. }) => assert_eq!(pin.model, model),
                _ => panic!("exact identity must have one owner"),
            }
        }
        assert!(state.tool_owner("s", &["\u{fffd}".into()]).is_none());
        assert!(
            state
                .tool_owner_exact("other", &[JsString::from_utf16(vec![0xd800])])
                .is_none()
        );
        let key = "mismatch".to_owned();
        assert!(state.select_exact(
            std::slice::from_ref(&key),
            Pin::new("selected".into(), "requested".into()),
            Selection {
                scope: "s",
                request_id: Some(&key),
                sequence: 3
            },
            0
        ));
        assert!(!state.complete_exact(
            &key,
            Some(&ContinuationEvidence {
                model: JsString::from_utf16(vec![0xd800]),
                tools: vec![ToolIdentity {
                    id: "tool".into(),
                    model: JsString::from_utf16(vec![0xd801])
                }]
            }),
            0
        ));
        assert!(state.get_exact(&key, 0).is_none());
    }
    fn pin(model: &str) -> Value {
        json!({"model":model, "requestedModel":"haiku"})
    }
    fn done(model: &str, tools: Value) -> Value {
        json!({"continuation_model":model,"tool_uses":tools})
    }
    fn stage(state: &mut TurnState, keys: &[&str], model: &str, id: &str, sequence: u64) -> bool {
        state.select(
            &keys.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            pin(model),
            Selection {
                scope: "s",
                request_id: Some(id),
                sequence,
            },
            0,
        )
    }

    #[test]
    fn cancellation_and_supersession_cannot_replace_confirmation() {
        let mut state = TurnState::default();
        stage(&mut state, &["one"], "opus", "initial", 1);
        assert!(state.get("one", 0).is_none());
        assert!(state.complete("initial", Some(&done("opus", json!([]))), 0));
        stage(&mut state, &["one"], "haiku", "old", 2);
        stage(&mut state, &["one"], "sonnet", "new", 3);
        assert!(!state.complete("old", Some(&done("haiku", json!([]))), 0));
        assert!(!state.complete("new", None, 0));
        assert_eq!(state.get("one", 0).unwrap()["model"], "opus");
        assert_eq!(state.attempt_count(), 0);
    }

    #[test]
    fn active_tools_survive_ttl_pressure_and_rejected_admission() {
        let mut state = TurnState::new(1, 10);
        stage(&mut state, &["one", "alias"], "opus", "initial", 1);
        state.complete(
            "initial",
            Some(&done("opus", json!([{"id":"tool","model":"opus"}]))),
            0,
        );
        assert!(stage(&mut state, &["one"], "sonnet", "accepted", 2));
        assert!(!stage(
            &mut state,
            &["one", "rejected-alias"],
            "haiku",
            "rejected",
            3
        ));
        assert_eq!(state.get("alias", 1_000_000).unwrap()["model"], "opus");
        assert!(state.complete("accepted", Some(&done("sonnet", json!([]))), 1_000_000));
        assert!(state.get("rejected-alias", 1_000_000).is_none());
    }

    #[test]
    fn same_content_is_ambiguous_but_tool_ownership_is_scoped() {
        let mut state = TurnState::default();
        for (id, model, seq) in [("a", "opus", 1), ("b", "haiku", 2)] {
            stage(&mut state, &[id, "same"], model, id, seq);
            state.complete(
                id,
                Some(&done(
                    model,
                    json!([{"id":format!("tool-{id}"),"model":model}]),
                )),
                0,
            );
        }
        assert!(state.get("same", 0).is_none());
        assert_eq!(
            state.tool_owner("s", &["tool-a".into()]).unwrap()["pin"]["model"],
            "opus"
        );
        assert!(state.tool_owner("other", &["tool-a".into()]).is_none());
    }

    #[test]
    fn old_late_tool_completion_cannot_retire_newer_task() {
        let mut state = TurnState::new(1000, 10);
        stage(&mut state, &["old"], "opus", "old", 1);
        state.complete(
            "old",
            Some(&done("opus", json!([{"id":"slow","model":"opus"}]))),
            0,
        );
        stage(&mut state, &["new"], "haiku", "new", 2);
        state.complete("new", Some(&done("haiku", json!([]))), 0);
        stage(&mut state, &["old"], "opus", "tool-result", 3);
        state.complete("tool-result", Some(&done("opus", json!([]))), 0);
        assert!(state.get("old", 11).is_none());
        assert_eq!(state.get("new", 11).unwrap()["model"], "haiku");
    }

    #[test]
    fn failed_same_content_attempt_retains_prior_alias() {
        let mut state = TurnState::default();
        stage(&mut state, &["a", "content"], "opus", "a", 1);
        state.complete(
            "a",
            Some(&done("opus", json!([{"id":"tool","model":"opus"}]))),
            0,
        );
        stage(&mut state, &["b", "content"], "haiku", "b", 2);
        state.complete("b", None, 0);
        assert_eq!(state.get("content", 0).unwrap()["model"], "opus");
        for tools in [
            json!({}),
            json!([null]),
            json!([{"id":"wrong","model":"sonnet"}]),
        ] {
            stage(&mut state, &["c"], "opus", "c", 3);
            assert!(!state.complete("c", Some(&done("opus", tools)), 0));
            assert!(state.get("c", 0).is_none());
        }
    }

    #[test]
    fn aliases_are_bounded_and_unexecuted_selections_are_unconfirmed() {
        let mut state = TurnState::default();
        for i in 0..100 {
            assert!(state.select(
                &["prompt".into(), format!("discovery-{i}")],
                pin("opus"),
                Selection {
                    sequence: i,
                    ..Default::default()
                },
                0
            ));
        }
        assert_eq!(state.record_count(), 1);
        assert_eq!(state.alias_count(), 8);
        assert_eq!(state.get("prompt", 0).unwrap()["model"], "opus");
        assert_eq!(state.get("prompt", 0).unwrap()["confirmed"], false);
        assert_eq!(state.get("discovery-99", 0).unwrap()["model"], "opus");
    }

    #[test]
    fn failed_old_retry_releases_retired_task_after_newer_task_commits() {
        let mut state = TurnState::new(1000, 10);
        stage(&mut state, &["old"], "opus", "initial", 1);
        state.complete("initial", Some(&done("opus", json!([]))), 0);
        stage(&mut state, &["old"], "opus", "retry", 2);
        stage(&mut state, &["new"], "haiku", "new", 3);
        state.complete("new", Some(&done("haiku", json!([]))), 0);
        state.complete("retry", None, 0);
        assert!(state.get("old", 11).is_none());
        assert_eq!(state.get("new", 11).unwrap()["model"], "haiku");
    }

    #[test]
    fn different_prompt_identity_cannot_replace_pending_same_content_tools() {
        let mut state = TurnState::default();
        stage(
            &mut state,
            &["prompt-one", "same-content"],
            "opus",
            "one",
            1,
        );
        state.complete(
            "one",
            Some(&done("opus", json!([{"id":"pending","model":"opus"}]))),
            0,
        );
        stage(
            &mut state,
            &["prompt-two", "same-content"],
            "haiku",
            "two",
            2,
        );
        state.complete("two", Some(&done("haiku", json!([]))), 0);
        assert_eq!(state.get("prompt-one", 0).unwrap()["model"], "opus");
        assert_eq!(state.get("prompt-two", 0).unwrap()["model"], "haiku");
        assert!(state.get("same-content", 0).is_none());
        assert_eq!(
            state.tool_owner("s", &["pending".into()]).unwrap()["pin"]["model"],
            "opus"
        );
    }
}
