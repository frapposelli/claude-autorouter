//! Bounded allowlisted telemetry. Body, credentials, raw errors and arbitrary
//! provider fields never cross this normalization boundary.

use serde_json::{Map, Value, json};

use crate::redaction::redact_sensitive;
use crate::request_validation::nonnegative_safe_integer;

pub const TELEMETRY_SCHEMA_VERSION: u64 = 2;
pub const UNPRICED_REASONS: &[&str] = &[
    "request_failed",
    "request_cancelled",
    "request_evicted",
    "unknown_baseline",
    "unknown_model",
    "missing_model",
    "missing_usage",
    "invalid_usage",
    "conflicting_usage",
    "mixed_models",
    "unsupported_pricing",
    "invalid_telemetry",
    "unknown_pricing_version",
    "unconfirmed_completion",
    "incomplete_usage",
    "missing_outcome",
];
const EVENTS: &[&str] = &[
    "request_start",
    "route",
    "upstream_response",
    "upstream_model",
    "upstream_usage",
    "upstream_error",
    "request_complete",
    "request_error",
    "request_cancelled",
];
const SOURCES: &[&str] = &["jev", "ollama", "cache", "fallback", "passthrough"];
const ERRORS: &[&str] = &[
    "invalid_request_error",
    "authentication_error",
    "billing_error",
    "permission_error",
    "not_found_error",
    "request_too_large",
    "rate_limit_error",
    "api_error",
    "overloaded_error",
    "timeout_error",
    "unknown_error",
    "http_error",
    "request_error",
];
const CLASSIFIER_ERRORS: &[&str] = &[
    "timeout",
    "http_error",
    "invalid_response",
    "network_error",
    "capacity_exhausted",
];

pub fn sanitize_identifier(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| {
        (1..=200).contains(&text.len())
            && text
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
    })
}

pub fn sanitize_model(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| {
        (1..=120).contains(&text.len())
            && text
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.:/-".contains(&c))
    })
}

fn code(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| {
        (1..=80).contains(&text.len())
            && text.as_bytes()[0].is_ascii_lowercase()
            && text
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
    })
}

fn absent(value: Option<&Value>) -> bool {
    value.is_none_or(|value| value.is_null() || value.as_str() == Some(""))
}

fn valid_status(value: &Value) -> bool {
    value
        .as_f64()
        .is_some_and(|value| value.fract() == 0.0 && (100.0..=599.0).contains(&value))
}

fn assign(row: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        row.insert(key.into(), value);
    }
}

fn allowed(row: &mut Map<String, Value>, entry: &Value, field: &str, values: &[&str]) {
    if let Some(value) = entry
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| values.contains(value))
    {
        row.insert(field.into(), value.into());
    }
}

pub fn normalize_pricing_context(value: Option<&Value>) -> Option<Value> {
    let value = value?;
    let Some(value) = value.as_object() else {
        return Some(json!({"pricing_unsupported":true}));
    };
    let mut row = Map::new();
    for (key, values) in [
        ("speed", &["standard", "fast"][..]),
        ("inference_geo", &["global", "us", "not_available"][..]),
        (
            "service_tier",
            &["auto", "standard_only", "standard", "priority", "batch"][..],
        ),
    ] {
        if let Some(value) = value.get(key) {
            if value.as_str().is_some_and(|value| values.contains(&value)) {
                row.insert(key.into(), value.clone());
            } else {
                row.insert("pricing_unsupported".into(), true.into());
            }
        }
    }
    for key in ["pricing_unsupported", "unsupported"] {
        if value
            .get(key)
            .is_some_and(|value| value.as_bool() != Some(false))
        {
            row.insert("pricing_unsupported".into(), true.into());
        }
    }
    Some(row.into())
}

pub fn normalize_usage_telemetry(value: Option<&Value>) -> Option<Value> {
    let value = value?;
    if !value.is_object() {
        return Some(json!({"pricing_unsupported":true}));
    }
    let mut row = normalize_pricing_context(Some(value))?.as_object()?.clone();
    for key in [
        "input_tokens",
        "output_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ] {
        if let Some(value) = value.get(key) {
            if nonnegative_safe_integer(value).is_some() {
                row.insert(key.into(), value.clone());
            } else {
                row.insert("pricing_unsupported".into(), true.into());
            }
        }
    }
    if let Some(creation) = value.get("cache_creation") {
        if creation.is_null()
            && value
                .get("cache_creation_input_tokens")
                .and_then(Value::as_f64)
                == Some(0.0)
        {
            // Provider emits null to denote no cache writes; preserve default.
        } else if let Some(creation) = creation.as_object() {
            let mut counts = Map::new();
            for key in ["ephemeral_5m_input_tokens", "ephemeral_1h_input_tokens"] {
                if let Some(value) = creation.get(key) {
                    if nonnegative_safe_integer(value).is_some() {
                        counts.insert(key.into(), value.clone());
                    } else {
                        row.insert("pricing_unsupported".into(), true.into());
                    }
                }
            }
            row.insert("cache_creation".into(), counts.into());
        } else {
            row.insert("pricing_unsupported".into(), true.into());
        }
    }
    Some(row.into())
}

fn reasons(value: Option<&Value>) -> Option<Value> {
    let value = value?.as_object()?;
    Some(
        UNPRICED_REASONS
            .iter()
            .filter_map(|key| {
                value
                    .get(*key)
                    .filter(|value| nonnegative_safe_integer(value).is_some())
                    .map(|value| ((*key).to_owned(), value.clone()))
            })
            .collect::<Map<_, _>>()
            .into(),
    )
}

fn version(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| {
        (1..=40).contains(&text.len())
            && text
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
    })
}

fn date_shape(value: &str) -> bool {
    value.len() == 10
        && value.as_bytes().iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
}

fn normalize_savings(value: Option<&Value>) -> Option<Value> {
    let value = value?;
    if !value.is_object() {
        return None;
    }
    let mut row = Map::new();
    assign(
        &mut row,
        "baseline_model",
        value
            .get("baseline_model")
            .and_then(sanitize_model)
            .map(Into::into),
    );
    for key in ["actual_usd", "baseline_usd", "saved_usd", "percent"] {
        if let Some(number) = value.get(key).and_then(Value::as_f64)
            && number.is_finite()
            && number.abs() <= 1e18
            && (!matches!(key, "actual_usd" | "baseline_usd") || number >= 0.0)
        {
            row.insert(key.into(), value[key].clone());
        }
    }
    for key in ["requests", "priced_requests", "unpriced_requests"] {
        if value
            .get(key)
            .is_some_and(|value| nonnegative_safe_integer(value).is_some())
        {
            row.insert(key.into(), value[key].clone());
        }
    }
    if value.get("partial").and_then(Value::as_bool) == Some(true) {
        row.insert("partial".into(), true.into());
    }
    assign(
        &mut row,
        "pricing_version",
        value
            .get("pricing_version")
            .and_then(version)
            .map(Into::into),
    );
    assign(
        &mut row,
        "pricing_date",
        value
            .get("pricing_date")
            .and_then(Value::as_str)
            .filter(|text| date_shape(text))
            .map(Into::into),
    );
    if value.get("pricing_source").and_then(Value::as_str)
        == Some("https://platform.claude.com/docs/en/about-claude/pricing")
    {
        row.insert("pricing_source".into(), value["pricing_source"].clone());
    }
    assign(
        &mut row,
        "unpriced_reasons",
        reasons(value.get("unpriced_reasons")),
    );
    Some(row.into())
}

// Preserve Date.parse's accepted ISO shape, including day-overflow dates such
// as February 30 and exactly 24:00:00.000. Timestamp generation belongs to the
// caller so this module remains deterministic and has no clock I/O.
pub fn valid_timestamp(value: &str) -> bool {
    if value.len() != 24 {
        return false;
    }
    for (index, c) in value.bytes().enumerate() {
        let expected = match index {
            4 | 7 => Some(b'-'),
            10 => Some(b'T'),
            13 | 16 => Some(b':'),
            19 => Some(b'.'),
            23 => Some(b'Z'),
            _ => None,
        };
        if expected.is_some_and(|expected| c != expected)
            || (expected.is_none() && !c.is_ascii_digit())
        {
            return false;
        }
    }
    let number = |range: std::ops::Range<usize>| value[range].parse::<u32>().unwrap_or(u32::MAX);
    let hour = number(11..13);
    (1..=12).contains(&number(5..7))
        && (1..=31).contains(&number(8..10))
        && hour <= 24
        && number(14..16) <= 59
        && number(17..19) <= 59
        && (hour < 24 || (number(14..16) == 0 && number(17..19) == 0 && number(20..23) == 0))
}

fn base(entry: &Value, event: &str, now: &str) -> Option<Map<String, Value>> {
    let request_id = sanitize_identifier(entry.get("request_id")?)?;
    let timestamp = entry
        .get("timestamp")
        .and_then(Value::as_str)
        .filter(|text| valid_timestamp(text))
        .unwrap_or(now);
    let mut row=json!({"schema_version":TELEMETRY_SCHEMA_VERSION,"event":event,"timestamp":timestamp,"request_id":request_id})
        .as_object()?.clone();
    for key in ["session_id", "agent_id", "prompt_id"] {
        if absent(entry.get(key)) {
            continue;
        }
        row.insert(key.into(), sanitize_identifier(entry.get(key)?)?.into());
    }
    if !absent(entry.get("request_class")) {
        row.insert(
            "request_class".into(),
            code(entry.get("request_class")?)?.into(),
        );
    }
    for key in [
        "requested_model",
        "selected_model",
        "confirmed_model",
        "model",
        "baseline_model",
    ] {
        assign(
            &mut row,
            key,
            entry.get(key).and_then(sanitize_model).map(Into::into),
        );
    }
    assign(
        &mut row,
        "pricing_version",
        entry
            .get("pricing_version")
            .and_then(version)
            .map(Into::into),
    );
    for key in ["reason", "compatibility_reason", "continuity_state"] {
        assign(&mut row, key, entry.get(key).and_then(code).map(Into::into));
    }
    allowed(&mut row, entry, "source", SOURCES);
    allowed(&mut row, entry, "evaluator", &["jev", "ollama"]);
    for key in ["tier", "classified_tier"] {
        allowed(&mut row, entry, key, &["haiku", "sonnet", "opus"]);
    }
    for key in [
        "latency_ms",
        "evaluation_latency_ms",
        "routing_latency_ms",
        "decision_latency_ms",
        "first_response_ms",
        "upstream_latency_ms",
        "total_latency_ms",
    ] {
        if entry
            .get(key)
            .and_then(Value::as_f64)
            .is_some_and(|n| n.is_finite() && n >= 0.0)
        {
            row.insert(key.into(), entry[key].clone());
        }
    }
    allowed(&mut row, entry, "classifier_error", CLASSIFIER_ERRORS);
    for key in ["classifier_status", "http_status"] {
        if entry.get(key).is_some_and(valid_status) {
            row.insert(key.into(), entry[key].clone());
        }
    }
    allowed(&mut row, entry, "error_type", ERRORS);
    allowed(
        &mut row,
        entry,
        "context_check",
        &["within_budget", "over_budget", "count_unavailable"],
    );
    if entry
        .get("counted_input_tokens")
        .is_some_and(|value| nonnegative_safe_integer(value).is_some())
    {
        row.insert(
            "counted_input_tokens".into(),
            entry["counted_input_tokens"].clone(),
        );
    }
    if let Some(transitions) = entry.get("model_transitions") {
        if let Some(transitions) = transitions.as_array() {
            let models: Vec<Value> = transitions
                .iter()
                .take(16)
                .filter_map(sanitize_model)
                .map(Into::into)
                .collect();
            if transitions.len() > 16 || models.len() != transitions.len() {
                row.insert("model_transitions_truncated".into(), true.into());
            }
            row.insert("model_transitions".into(), models.into());
        } else {
            row.insert("model_transitions_truncated".into(), true.into());
        }
    }
    if entry
        .get("model_transitions_truncated")
        .and_then(Value::as_bool)
        == Some(true)
    {
        row.insert("model_transitions_truncated".into(), true.into());
    }
    assign(
        &mut row,
        "usage",
        normalize_usage_telemetry(entry.get("usage")),
    );
    assign(
        &mut row,
        "pricing_context",
        normalize_pricing_context(entry.get("pricing_context")),
    );
    for key in ["usage_complete", "pricing_eligible", "completion_confirmed"] {
        if let Some(value) = entry.get(key).and_then(Value::as_bool) {
            row.insert(key.into(), value.into());
        }
    }
    allowed(&mut row, entry, "unpriced_reason", UNPRICED_REASONS);
    for key in ["savings", "savings_coverage"] {
        assign(&mut row, key, normalize_savings(entry.get(key)));
    }
    Some(row)
}

pub fn normalize_telemetry_event(entry: &Value, now: &str) -> Option<Value> {
    if !entry.is_object() {
        return None;
    }
    let event = match entry.get("event").and_then(Value::as_str)? {
        "error" => "request_error",
        "cancelled" => "request_cancelled",
        event => event,
    };
    if !EVENTS.contains(&event) {
        return None;
    }
    let mut row = base(entry, event, now)?;
    if entry.get("status").is_some_and(valid_status) {
        row.insert("status".into(), entry["status"].clone());
    }
    Some(row.into())
}

pub fn normalize_session_record(entry: &Value, include_prompts: bool, now: &str) -> Option<Value> {
    if !entry.is_object() {
        return None;
    }
    let event = entry.get("event").and_then(Value::as_str)?;
    if !matches!(event, "decision" | "outcome") {
        return None;
    }
    if entry
        .get("schema_version")
        .is_some_and(|value| !matches!(value.as_f64(), Some(1.0 | 2.0)))
    {
        return None;
    }
    let mut row = base(entry, event, now)?;
    if event == "decision" {
        if !row.contains_key("requested_model") || !row.contains_key("selected_model") {
            return None;
        }
        if include_prompts {
            let foreground = row
                .get("request_class")
                .is_none_or(|value| value.as_str() == Some("main"));
            let excerpt = if foreground {
                entry
                    .get("prompt_excerpt")
                    .and_then(Value::as_str)
                    .map(redact_sensitive)
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let mut chars = excerpt.chars();
            let prompt: String = chars.by_ref().take(500).collect();
            let truncated = chars.next().is_some()
                || (foreground
                    && entry.get("prompt_truncated").and_then(Value::as_bool) == Some(true));
            row.insert("prompt_excerpt".into(), prompt.into());
            row.insert("prompt_truncated".into(), truncated.into());
        }
    } else {
        let status = entry.get("status").and_then(Value::as_str)?;
        if !matches!(status, "completed" | "error" | "cancelled") {
            return None;
        }
        row.insert("status".into(), status.into());
    }
    Some(row.into())
}

/// Schema-specific view for the scalar telemetry normalizer. Invalid UTF-16
/// identifiers/models remain invalid under the ASCII allowlists; prompt text
/// is explicitly well-formed by the original contract. Non-finite numbers use
/// an invalid string sentinel instead of null, preserving explicit-invalid vs
/// absent identities. The unrestricted prompt excerpt is handled separately.
/// Opaque nesting is ignored beyond the deepest consumed schema field.
/// This view is never an event callback, diagnostic serialization or wire body.
pub fn normalization_projection(document: &crate::js_json::JsDocument) -> Value {
    use crate::js_json::{JsDocument, JsNode, NodeId};
    fn project(document: &JsDocument, node: NodeId, depth: usize) -> Value {
        if depth > 4 {
            return Value::Null;
        }
        match document.node(node) {
            Some(JsNode::Null) | None => Value::Null,
            Some(JsNode::Bool(value)) => Value::Bool(*value),
            Some(JsNode::Number(value)) => serde_json::Number::from_f64(*value)
                .map(Value::Number)
                .unwrap_or_else(|| Value::String("\0".into())),
            Some(JsNode::String(value)) => Value::String(value.to_well_formed()),
            Some(JsNode::Array(values)) => Value::Array(
                values
                    .iter()
                    .map(|node| project(document, *node, depth + 1))
                    .collect(),
            ),
            Some(JsNode::Object(object)) => Value::Object(
                object
                    .entries()
                    .iter()
                    .map(|(key, node)| (key.to_well_formed(), project(document, *node, depth + 1)))
                    .collect(),
            ),
        }
    }
    let mut value = project(document, document.root(), 0);
    if let Some(node) = document.get(document.root(), "prompt_excerpt")
        && !matches!(document.node(node), Some(JsNode::String(_)))
        && let Some(object) = value.as_object_mut()
    {
        object.insert("prompt_excerpt".into(), Value::Null);
    }
    value
}
pub fn normalize_telemetry_document(
    document: &crate::js_json::JsDocument,
    now: &str,
) -> Option<Value> {
    normalize_telemetry_event(&normalization_projection(document), now)
}
pub fn normalize_session_document(
    document: &crate::js_json::JsDocument,
    include_prompts: bool,
    now: &str,
) -> Option<Value> {
    normalize_session_record(&normalization_projection(document), include_prompts, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: &str = "2026-10-09T12:00:00.000Z";

    #[test]
    fn raw_nonfinite_identity_is_invalid_while_null_is_absent() {
        use crate::js_json::JsDocument;
        for raw in ["1e400", "-1e400"] {
            let document = JsDocument::parse(
                format!(r#"{{"event":"route","request_id":"r","session_id":{raw}}}"#).as_bytes(),
            )
            .unwrap();
            assert!(normalize_telemetry_document(&document, NOW).is_none());
        }
        let document =
            JsDocument::parse(br#"{"event":"route","request_id":"r","session_id":null}"#).unwrap();
        assert!(normalize_telemetry_document(&document, NOW).is_some());
    }
    #[test]
    fn raw_nonfinite_type_cannot_become_a_valid_prompt_or_transition_list() {
        use crate::js_json::JsDocument;
        let document = JsDocument::parse(br#"{"event":"decision","request_id":"r","requested_model":"claude-opus-5-5","selected_model":"claude-haiku-4-5","prompt_excerpt":1e400,"model_transitions":1e400}"#).unwrap();
        let row = normalize_session_document(&document, true, NOW).unwrap();
        assert_eq!(row["prompt_excerpt"], "");
        assert_eq!(row["model_transitions_truncated"], true);
        assert!(row.get("model_transitions").is_none());
    }

    #[test]
    fn normalization_drops_payload_and_rejects_invalid_explicit_identities() {
        let event = json!({"event":"error","request_id":"r1","session_id":"s1","error_type":"authentication_error",
            "raw_error":"private-canary","body":"private-canary","headers":{"authorization":"private-canary"},
            "model":"claude-opus-5-5","status":401,"reason":"tool_turn_pinned","future":"private-canary"});
        let row = normalize_telemetry_event(&event, NOW).unwrap();
        assert_eq!(row["event"], "request_error");
        assert_eq!(row["status"], 401);
        assert_eq!(row["timestamp"], NOW);
        assert!(!row.to_string().contains("private-canary"));
        for key in [
            "request_id",
            "session_id",
            "agent_id",
            "prompt_id",
            "request_class",
        ] {
            let mut invalid = event.clone();
            invalid[key] = "invalid/private identity".into();
            assert!(normalize_telemetry_event(&invalid, NOW).is_none());
        }
        for id in ["__proto__", "constructor", ""] {
            let mut valid = event.clone();
            valid["session_id"] = id.into();
            assert!(normalize_telemetry_event(&valid, NOW).is_some());
        }
    }

    #[test]
    fn usage_modifiers_and_counters_remain_allowlisted_and_unpriced_when_invalid() {
        assert_eq!(normalize_usage_telemetry(None), None);
        for value in [Value::Null, json!([]), json!("secret")] {
            assert_eq!(
                normalize_usage_telemetry(Some(&value)),
                Some(json!({"pricing_unsupported":true}))
            );
        }
        let usage = json!({"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":0,"cache_creation":null,"secret":"private"});
        assert_eq!(
            normalize_usage_telemetry(Some(&usage)),
            Some(json!({"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":0}))
        );
        let usage = json!({"input_tokens":null,"output_tokens":-1,"cache_creation":{"ephemeral_5m_input_tokens":10,"ephemeral_1h_input_tokens":"bad"},"inference_geo":"unknown"});
        assert_eq!(
            normalize_usage_telemetry(Some(&usage)),
            Some(
                json!({"pricing_unsupported":true,"cache_creation":{"ephemeral_5m_input_tokens":10}})
            )
        );
        assert_eq!(
            normalize_pricing_context(Some(
                &json!({"speed":"standard","service_tier":"auto","unsupported":false,"opaque":"secret"})
            )),
            Some(json!({"speed":"standard","service_tier":"auto"}))
        );
    }

    #[test]
    fn metadata_omits_prompts_and_prompt_mode_redacts_before_unicode_truncation() {
        let body = json!({"event":"decision","schema_version":1,"request_id":"r1","requested_model":"claude-sonnet-5","selected_model":"claude-opus-5-5",
            "prompt_excerpt":format!("{} DB_PASSWORD=synthetic-value tail","😀".repeat(490)),"prompt_truncated":false});
        let metadata = normalize_session_record(&body, false, NOW).unwrap();
        assert!(metadata.get("prompt_excerpt").is_none());
        assert!(metadata.get("prompt_truncated").is_none());
        let prompt = normalize_session_record(&body, true, NOW).unwrap();
        assert_eq!(prompt["schema_version"], 2);
        assert_eq!(
            prompt["prompt_excerpt"].as_str().unwrap().chars().count(),
            500
        );
        assert_eq!(prompt["prompt_truncated"], true);
        assert!(!prompt.to_string().contains("synthetic-value"));
        let mut agent = body;
        agent["request_class"] = "subagent".into();
        agent["prompt_truncated"] = true.into();
        let row = normalize_session_record(&agent, true, NOW).unwrap();
        assert_eq!(row["prompt_excerpt"], "");
        assert_eq!(row["prompt_truncated"], false);
    }

    #[test]
    fn transition_bounds_and_timestamp_validation_preserve_failure_evidence() {
        let base = json!({"event":"outcome","request_id":"r","status":"completed","model_transitions":vec!["claude-sonnet-5";17],"timestamp":"2026-02-30T00:00:00.000Z"});
        let row = normalize_session_record(&base, false, NOW).unwrap();
        assert_eq!(row["model_transitions"].as_array().unwrap().len(), 16);
        assert_eq!(row["model_transitions_truncated"], true);
        assert_eq!(row["timestamp"], "2026-02-30T00:00:00.000Z");
        for value in [
            "2026-13-01T00:00:00.000Z",
            "2026-01-32T00:00:00.000Z",
            "2026-01-01T24:00:00.001Z",
            "secret",
        ] {
            let mut event = base.clone();
            event["timestamp"] = value.into();
            assert_eq!(
                normalize_session_record(&event, false, NOW).unwrap()["timestamp"],
                NOW
            );
        }
        let mut event = base;
        event["model_transitions"] = json!(["private model"]);
        assert_eq!(
            normalize_session_record(&event, false, NOW).unwrap()["model_transitions_truncated"],
            true
        );
    }
}
