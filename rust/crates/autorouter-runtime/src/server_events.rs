//! Bounded per-request outcome bookkeeping. Sinks never own routing state and
//! cannot turn selection or observed metadata into successful delivery.
use autorouter_core::config::SessionLogMode;
use autorouter_core::js_json::{JsDocument, JsNode, JsString};
use autorouter_core::prompt_state::prompt_excerpt_document;
use autorouter_core::savings::{PRICING_VERSION, estimate_outcome_savings};
use autorouter_core::telemetry_event::normalize_session_record;
use serde_json::{Map, Value, json};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

/// Raw callbacks retain the JavaScript document, including UTF-16 identities.
pub type EventSink = Arc<dyn Fn(JsDocument) + Send + Sync>;
/// Persistent records already crossed the allowlisted, scalar-only boundary.
pub type RecordSink = Arc<dyn Fn(Value) + Send + Sync>;
#[derive(Clone, Default)]
pub struct EventSinks {
    pub log: Option<EventSink>,
    pub status: Option<EventSink>,
    pub decision: Option<EventSink>,
    pub record: Option<RecordSink>,
}
pub fn emit(sink: &Option<EventSink>, event: Value) {
    emit_document(sink, event_document(event, &[]));
}
pub fn emit_document(sink: &Option<EventSink>, event: JsDocument) {
    if let Some(sink) = sink {
        let _ = catch_unwind(AssertUnwindSafe(|| sink(event)));
    }
}
/// Build an exact raw event while overriding explicitly named string fields.
pub fn event_document(value: Value, strings: &[(&str, &JsString)]) -> JsDocument {
    let mut document = JsDocument::parse(value.to_string().as_bytes()).expect("event JSON");
    for (key, value) in strings {
        document
            .set_root_field_json(key, value.stringify().as_bytes())
            .expect("event object");
    }
    document
}
fn overlay_document(value: Value, fields: &JsDocument) -> JsDocument {
    let mut document = event_document(value, &[]);
    if let Some(JsNode::Object(object)) = fields.node(fields.root()) {
        for (key, node) in object.entries() {
            // Event field names are internal ASCII constants, never payload keys.
            if let Some(key) = key.to_scalar() {
                document
                    .set_root_field_json(&key, fields.stringify_node(*node).as_bytes())
                    .expect("event object");
            }
        }
    }
    document
}
pub fn timestamp() -> String {
    static FORMAT: LazyLock<Vec<time::format_description::FormatItem<'static>>> =
        LazyLock::new(|| {
            time::format_description::parse_borrowed::<2>(
                "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z",
            )
            .expect("fixed timestamp format")
        });
    time::OffsetDateTime::now_utc()
        .format(&FORMAT)
        .unwrap_or_default()
}
fn rounded_ms(start: Instant) -> f64 {
    (start.elapsed().as_secs_f64() * 100_000.0).round() / 100.0
}
fn merge(base: &mut Map<String, Value>, fields: &Value) {
    if let Some(fields) = fields.as_object() {
        base.extend(fields.clone());
    }
}
struct State {
    outcome: Map<String, Value>,
    forwarded: Option<Instant>,
    finished: bool,
    transition_tail: Option<JsString>,
}
pub struct RequestEvents {
    pub context: Value,
    started: Instant,
    mode: SessionLogMode,
    sinks: EventSinks,
    state: Mutex<State>,
}
impl RequestEvents {
    pub fn new(context: Value, baseline: &str, mode: SessionLogMode, sinks: EventSinks) -> Self {
        Self{context,started:Instant::now(),mode,sinks,state:Mutex::new(State{outcome:json!({"baseline_model":baseline,"pricing_version":PRICING_VERSION,"completion_confirmed":false}).as_object().unwrap().clone(),forwarded:None,finished:false,transition_tail:None})}
    }
    pub fn log(&self, event: Value) {
        emit(&self.sinks.log, event);
    }
    pub fn log_document(&self, event: JsDocument) {
        emit_document(&self.sinks.log, event);
    }
    pub fn forwarding(&self) {
        self.state.lock().unwrap().forwarded = Some(Instant::now());
    }
    pub fn confirmed(&self, confirmed: bool) {
        self.state
            .lock()
            .unwrap()
            .outcome
            .insert("completion_confirmed".into(), json!(confirmed));
    }
    fn record(&self, row: Value) {
        if self.sinks.record.is_none() {
            return;
        }
        let now = timestamp();
        let mut entry = Map::new();
        entry.insert("timestamp".into(), json!(now));
        merge(&mut entry, &self.context);
        merge(&mut entry, &row);
        if let Some(row) = normalize_session_record(
            &Value::Object(entry),
            self.mode != SessionLogMode::Metadata,
            &now,
        ) && let Some(sink) = &self.sinks.record
        {
            let _ = catch_unwind(AssertUnwindSafe(|| sink(row)));
        }
    }
    pub fn status(&self, event: &str, fields: Value) {
        self.status_document(
            event,
            event_document(
                if fields.is_object() {
                    fields
                } else {
                    json!({})
                },
                &[],
            ),
        );
    }
    pub fn status_document(&self, event: &str, mut exact: JsDocument) {
        let mut fields = exact.to_serde_observation_lossy();
        if !fields.is_object() {
            return;
        }
        let (status, outcome) = {
            let mut state = self.state.lock().unwrap();
            if state.finished {
                return;
            }
            if event == "route" {
                merge(&mut state.outcome, &fields);
                for (to, from) in [
                    ("selected_model", "model"),
                    ("routing_latency_ms", "latency_ms"),
                    ("decision_latency_ms", "latency_ms"),
                ] {
                    if let Some(value) = fields.get(from) {
                        state.outcome.insert(to.into(), value.clone());
                    }
                }
            }
            if event == "upstream_response" {
                state
                    .outcome
                    .insert("http_status".into(), fields["status"].clone());
                let latency = json!(rounded_ms(state.forwarded.unwrap_or(self.started)));
                fields["first_response_ms"] = latency.clone();
                state.outcome.insert("first_response_ms".into(), latency);
                if fields["status"].as_f64().is_some_and(|s| s >= 400.0) {
                    state
                        .outcome
                        .insert("error_type".into(), json!("http_error"));
                }
            }
            if event == "upstream_model" {
                let model = fields["model"].clone();
                state
                    .outcome
                    .insert("confirmed_model".into(), model.clone());
                state
                    .outcome
                    .entry("model_transitions")
                    .or_insert_with(|| json!([]));
                let identity = exact
                    .get(exact.root(), "model")
                    .and_then(|node| exact.string(node))
                    .cloned();
                let different = state.transition_tail != identity;
                let transitions = state
                    .outcome
                    .get_mut("model_transitions")
                    .unwrap()
                    .as_array_mut()
                    .unwrap();
                if different {
                    if transitions.len() < 16 {
                        transitions.push(model);
                        state.transition_tail = identity;
                    } else {
                        state
                            .outcome
                            .insert("model_transitions_truncated".into(), json!(true));
                    }
                }
            }
            if event == "upstream_usage" {
                state
                    .outcome
                    .insert("usage".into(), fields["usage"].clone());
            }
            if event == "upstream_error" || event == "request_error" {
                state.outcome.insert(
                    "error_type".into(),
                    fields
                        .get("error_type")
                        .cloned()
                        .unwrap_or_else(|| json!("request_error")),
                );
                if let Some(status) = fields
                    .get("status")
                    .filter(|v| v.as_f64().is_some_and(|n| n != 0.0))
                {
                    state.outcome.insert("http_status".into(), status.clone());
                }
            }
            if matches!(
                event,
                "request_complete" | "request_error" | "request_cancelled"
            ) {
                state.finished = true;
                fields["total_latency_ms"] = json!(rounded_ms(self.started));
                fields["completion_confirmed"] = state.outcome["completion_confirmed"].clone();
            }
            let mut status = self.context.as_object().cloned().unwrap_or_default();
            status.insert("event".into(), json!(event));
            merge(&mut status, &fields);
            let completed = if state.finished && self.sinks.record.is_some() {
                let mut completed = self.context.as_object().cloned().unwrap_or_default();
                completed.extend(state.outcome.clone());
                completed.insert("event".into(), json!("outcome"));
                completed.insert(
                    "status".into(),
                    json!(if state.outcome.contains_key("error_type") {
                        "error"
                    } else if event == "request_cancelled" {
                        "cancelled"
                    } else {
                        "completed"
                    }),
                );
                let usage = state.outcome.get("usage");
                let valid = |key| {
                    usage
                        .and_then(|u| u.get(key))
                        .and_then(Value::as_f64)
                        .is_some_and(|n| {
                            n.is_finite()
                                && (0.0..=9_007_199_254_740_991.0).contains(&n)
                                && n.fract() == 0.0
                        })
                };
                completed.insert(
                    "usage_complete".into(),
                    json!(
                        state.outcome["completion_confirmed"] == true
                            && valid("input_tokens")
                            && valid("output_tokens")
                    ),
                );
                completed.insert("total_latency_ms".into(), json!(rounded_ms(self.started)));
                Some(Value::Object(completed))
            } else {
                None
            };
            (Value::Object(status), completed)
        };
        // Fields added by lifecycle bookkeeping override any caller value.
        for key in [
            "first_response_ms",
            "total_latency_ms",
            "completion_confirmed",
        ] {
            if let Some(value) = status.get(key) {
                exact
                    .set_root_field_json(key, value.to_string().as_bytes())
                    .expect("event object");
            }
        }
        emit_document(&self.sinks.status, overlay_document(status, &exact));
        if let Some(mut completed) = outcome {
            let estimate = estimate_outcome_savings(&completed);
            completed["pricing_eligible"] = estimate["priced"].clone();
            if estimate["priced"] != true {
                completed["unpriced_reason"] = estimate["unpriced_reason"].clone();
            }
            self.record(completed);
        }
    }
    pub fn decision(&self, document: &JsDocument, decision: &Value) {
        let model = decision
            .get("model")
            .and_then(Value::as_str)
            .map(JsString::from_scalar);
        self.decision_inner(document, decision, model.as_ref());
    }
    pub fn decision_exact(&self, document: &JsDocument, decision: &Value, model: &JsString) {
        self.decision_inner(document, decision, Some(model));
    }
    fn decision_inner(&self, document: &JsDocument, decision: &Value, model: Option<&JsString>) {
        if self.sinks.decision.is_none() && self.sinks.record.is_none() {
            return;
        }
        let mut row = self.context.as_object().cloned().unwrap_or_default();
        row.insert("schema_version".into(), json!(2));
        row.insert("event".into(), json!("decision"));
        row.insert("timestamp".into(), json!(timestamp()));
        if self.mode != SessionLogMode::Metadata {
            let include = self
                .context
                .get("request_class")
                .is_none_or(|c| c == "main" || c == "");
            let excerpt = if include {
                prompt_excerpt_document(document, 501)
            } else {
                String::new()
            };
            let chars: Vec<_> = excerpt.chars().collect();
            row.insert(
                "prompt_excerpt".into(),
                json!(chars.iter().take(500).collect::<String>()),
            );
            row.insert("prompt_truncated".into(), json!(chars.len() > 500));
        }
        if let Some(model) = document
            .get(document.root(), "model")
            .and_then(|n| document.string(n))
        {
            row.insert("requested_model".into(), json!(model.to_well_formed()));
        }
        for (to, from) in [
            ("selected_model", "model"),
            ("decision_latency_ms", "latency_ms"),
            ("routing_latency_ms", "latency_ms"),
            ("evaluation_latency_ms", "evaluation_latency_ms"),
            ("source", "source"),
            ("reason", "reason"),
            ("evaluator", "evaluator"),
            ("classified_tier", "classified_tier"),
            ("classifier_error", "classifier_error"),
            ("classifier_status", "classifier_status"),
            ("compatibility_reason", "compatibility_reason"),
            ("continuity_state", "continuity_state"),
            ("context_check", "context_check"),
            ("counted_input_tokens", "counted_input_tokens"),
        ] {
            if let Some(value) = decision.get(from) {
                row.insert(to.into(), value.clone());
            }
        }
        let row = Value::Object(row);
        self.record(row.clone());
        let mut strings = Vec::new();
        if let Some(requested) = document
            .get(document.root(), "model")
            .and_then(|node| document.string(node))
        {
            strings.push(("requested_model", requested));
        }
        if let Some(model) = model {
            strings.push(("selected_model", model));
        }
        emit_document(&self.sinks.decision, event_document(row, &strings));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn events(mode: SessionLogMode) -> (RequestEvents, Arc<Mutex<Vec<Value>>>) {
        let rows = Arc::new(Mutex::new(Vec::new()));
        let sink = rows.clone();
        let events = RequestEvents::new(
            json!({"request_id":"synthetic-request","session_id":"synthetic-session"}),
            "claude-opus-4-8",
            mode,
            EventSinks {
                record: Some(Arc::new(move |row| sink.lock().unwrap().push(row))),
                ..Default::default()
            },
        );
        (events, rows)
    }
    #[test]
    fn observation_without_confirmed_delivery_stays_unpriced_and_finish_is_once() {
        let (events, rows) = events(SessionLogMode::Metadata);
        events.status("route",json!({"model":"claude-sonnet-4-6","source":"ollama","reason":"classified","latency_ms":2}));
        events.status("upstream_model", json!({"model":"claude-sonnet-4-6"}));
        events.status(
            "upstream_usage",
            json!({"usage":{"input_tokens":100,"output_tokens":4}}),
        );
        events.status("request_cancelled", json!({}));
        events.confirmed(true);
        events.status("request_complete", json!({}));
        let rows = rows.lock().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["completion_confirmed"], false);
        assert_eq!(rows[0]["status"], "cancelled");
        assert_eq!(rows[0]["pricing_eligible"], false);
    }
    #[test]
    fn metadata_omits_prompts_and_prompt_records_are_redacted() {
        let document=JsDocument::parse(br#"{"model":"claude-sonnet-4-6","messages":[{"role":"user","content":"Fix this: api_key=synthetic-private-secret"}]}"#).unwrap();
        for mode in [SessionLogMode::Metadata, SessionLogMode::Prompts] {
            let (events, rows) = events(mode);
            events.decision(
                &document,
                &json!({"model":"claude-sonnet-4-6","source":"ollama","reason":"classified"}),
            );
            let rows = rows.lock().unwrap();
            assert_eq!(rows.len(), 1);
            assert!(!rows[0].to_string().contains("synthetic-private-secret"));
            assert_eq!(
                rows[0].get("prompt_excerpt").is_some(),
                mode == SessionLogMode::Prompts
            );
        }
    }
    #[test]
    fn exact_callbacks_do_not_relax_normalized_model_privacy() {
        let raw = Arc::new(Mutex::new(Vec::new()));
        let records = Arc::new(Mutex::new(Vec::new()));
        let decision_sink = raw.clone();
        let record_sink = records.clone();
        let events = RequestEvents::new(
            json!({"request_id":"r"}),
            "claude-opus-5-5",
            SessionLogMode::Metadata,
            EventSinks {
                decision: Some(Arc::new(move |row| decision_sink.lock().unwrap().push(row))),
                record: Some(Arc::new(move |row| record_sink.lock().unwrap().push(row))),
                ..Default::default()
            },
        );
        let document = JsDocument::parse(br#"{"model":"custom-\ud800","messages":[]}"#).unwrap();
        let selected = JsString::from_utf16(vec![0xd801]);
        events.decision_exact(
            &document,
            &json!({"model":"\u{fffd}","source":"passthrough"}),
            &selected,
        );
        let raw = raw.lock().unwrap();
        let row = &raw[0];
        assert_eq!(
            row.string(row.get(row.root(), "requested_model").unwrap())
                .unwrap()
                .units(),
            &"custom-".encode_utf16().chain([0xd800]).collect::<Vec<_>>()
        );
        assert_eq!(
            row.string(row.get(row.root(), "selected_model").unwrap())
                .unwrap()
                .units(),
            &[0xd801]
        );
        assert!(
            records.lock().unwrap().is_empty(),
            "invalid normalized model cannot become a record"
        );
    }
    #[test]
    fn exact_model_transition_identity_applies_capacity_before_normalization() {
        let (events, rows) = events(SessionLogMode::Metadata);
        for unit in 0xd800..0xd810 {
            let model = JsString::from_utf16(vec![unit]);
            events.status_document(
                "upstream_model",
                event_document(json!({}), &[("model", &model)]),
            );
        }
        events.status("upstream_model", json!({"model":"claude-sonnet-4-6"}));
        events.status("request_complete", json!({}));
        let rows = rows.lock().unwrap();
        assert_eq!(rows[0]["model_transitions"], json!([]));
        assert_eq!(rows[0]["model_transitions_truncated"], true);
        assert_eq!(rows[0]["confirmed_model"], "claude-sonnet-4-6");
    }
}
