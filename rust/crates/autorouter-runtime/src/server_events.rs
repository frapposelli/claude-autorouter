//! Bounded per-request outcome bookkeeping. Sinks never own routing state and
//! cannot turn selection or observed metadata into successful delivery.
use autorouter_core::config::SessionLogMode;
use autorouter_core::contracts::{
    DecisionRecord, ExactModelFields, LifecycleEvent, LifecycleName, OutcomeRecord, OutcomeStatus,
    RecordName, RequestContext, TelemetryFields, TypedRouteDecision,
};
use autorouter_core::js_json::{JsDocument, JsNode, JsString};
use autorouter_core::prompt_state::prompt_excerpt_document;
use autorouter_core::savings::{PRICING_VERSION, estimate_outcome_savings};
use autorouter_core::telemetry_event::normalize_session_record;
use serde_json::{Map, Value, json};
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
        let _ = crate::callback_privacy::contain_callback(|| sink(event));
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
struct TypedOutcome {
    fields: TelemetryFields,
    transitions: Option<Vec<String>>,
    transitions_truncated: Option<bool>,
}
struct State {
    outcome: Map<String, Value>,
    typed_outcome: Option<TypedOutcome>,
    forwarded: Option<Instant>,
    finished: bool,
    transition_tail: Option<JsString>,
}
pub struct RequestEvents {
    pub context: Value,
    typed_context: Option<RequestContext>,
    started: Instant,
    mode: SessionLogMode,
    sinks: EventSinks,
    state: Mutex<State>,
}
impl RequestEvents {
    pub fn new_typed(
        context: RequestContext,
        baseline: &str,
        mode: SessionLogMode,
        sinks: EventSinks,
    ) -> Self {
        let mut events = Self::new(
            serde_json::to_value(&context).expect("bounded context contract"),
            baseline,
            mode,
            sinks,
        );
        events.typed_context = Some(context);
        events.state.lock().unwrap().typed_outcome = Some(TypedOutcome {
            fields: TelemetryFields {
                baseline_model: Some(baseline.into()),
                pricing_version: Some(PRICING_VERSION.into()),
                completion_confirmed: Some(false),
                ..TelemetryFields::default()
            },
            transitions: None,
            transitions_truncated: None,
        });
        events
    }
    /// Legacy custom context boundary. First-party callers use new_typed.
    pub fn new(context: Value, baseline: &str, mode: SessionLogMode, sinks: EventSinks) -> Self {
        Self{context,typed_context:None,started:Instant::now(),mode,sinks,state:Mutex::new(State{typed_outcome:None,outcome:json!({"baseline_model":baseline,"pricing_version":PRICING_VERSION,"completion_confirmed":false}).as_object().unwrap().clone(),forwarded:None,finished:false,transition_tail:None})}
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
        let mut state = self.state.lock().unwrap();
        if let Some(outcome) = &mut state.typed_outcome {
            outcome.fields.completion_confirmed = Some(confirmed);
        }
        state
            .outcome
            .insert("completion_confirmed".into(), json!(confirmed));
    }
    /// A custom GatewayRouter may supply arbitrary raw decision fields. Keep
    /// that request on the original observation path for its whole lifecycle.
    pub fn use_legacy_observations(&self) {
        self.state.lock().unwrap().typed_outcome = None;
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
            let _ = crate::callback_privacy::contain_callback(|| sink(row));
        }
    }
    pub fn lifecycle(&self, event: LifecycleName, fields: TelemetryFields) {
        self.lifecycle_exact(event, fields, ExactModelFields::default());
    }
    pub fn lifecycle_exact(
        &self,
        event: LifecycleName,
        fields: TelemetryFields,
        exact: ExactModelFields<'_>,
    ) {
        let overrides = exact.overrides();
        let strings = overrides.as_slice();
        let Some(context) = &self.typed_context else {
            self.status_document(
                event.as_str(),
                event_document(fields.observation(), strings),
            );
            return;
        };
        self.lifecycle_typed(context, event, fields, strings);
    }
    fn lifecycle_typed(
        &self,
        context: &RequestContext,
        event: LifecycleName,
        mut fields: TelemetryFields,
        strings: &[(&str, &JsString)],
    ) {
        let (status, completed) = {
            let mut state = self.state.lock().unwrap();
            if state.finished {
                return;
            }
            if state.typed_outcome.is_none() {
                drop(state);
                self.status_document(
                    event.as_str(),
                    event_document(fields.observation(), strings),
                );
                return;
            }
            let forwarded = state.forwarded.unwrap_or(self.started);
            let mut tail = state.transition_tail.clone();
            let outcome = state.typed_outcome.as_mut().unwrap();
            match event {
                LifecycleName::Route => {
                    let baseline = outcome.fields.baseline_model.take();
                    let pricing = outcome.fields.pricing_version.take();
                    let confirmed = outcome.fields.completion_confirmed;
                    outcome.fields = fields.clone();
                    outcome.fields.baseline_model = baseline;
                    outcome.fields.pricing_version = pricing;
                    outcome.fields.completion_confirmed = confirmed;
                    outcome.fields.selected_model = fields.model.clone();
                    outcome.fields.routing_latency_ms = fields.latency_ms;
                    outcome.fields.decision_latency_ms = fields.latency_ms;
                }
                LifecycleName::UpstreamResponse => {
                    outcome.fields.http_status = fields.status;
                    let latency = rounded_ms(forwarded);
                    fields.first_response_ms = Some(latency);
                    outcome.fields.first_response_ms = Some(latency);
                    if fields.status.is_some_and(|status| status >= 400) {
                        outcome.fields.error_type = Some("http_error".into());
                    }
                }
                LifecycleName::UpstreamModel => {
                    outcome.fields.confirmed_model = fields.model.clone();
                    let transitions = outcome.transitions.get_or_insert_with(Vec::new);
                    let identity = strings
                        .iter()
                        .find_map(|(key, value)| (*key == "model").then_some((*value).clone()))
                        .or_else(|| fields.model.as_deref().map(JsString::from_scalar));
                    if identity != tail {
                        if transitions.len() < 16 {
                            if let Some(model) = &fields.model {
                                transitions.push(model.clone());
                            }
                            tail = identity;
                        } else {
                            outcome.transitions_truncated = Some(true);
                        }
                    }
                }
                LifecycleName::UpstreamUsage => outcome.fields.usage = fields.usage.clone(),
                LifecycleName::UpstreamError | LifecycleName::RequestError => {
                    outcome.fields.error_type = Some(
                        fields
                            .error_type
                            .clone()
                            .unwrap_or_else(|| "request_error".into()),
                    );
                    if fields.status.is_some_and(|status| status != 0) {
                        outcome.fields.http_status = fields.status;
                    }
                }
                _ => {}
            }
            let finished = matches!(
                event,
                LifecycleName::RequestComplete
                    | LifecycleName::RequestError
                    | LifecycleName::RequestCancelled
            );
            if finished {
                fields.total_latency_ms = Some(rounded_ms(self.started));
                fields.completion_confirmed = outcome.fields.completion_confirmed;
            }
            let status = serde_json::to_value(LifecycleEvent {
                context,
                event,
                fields: &fields,
            })
            .expect("bounded lifecycle contract");
            let completed = if finished && self.sinks.record.is_some() {
                let mut complete = outcome.fields.clone();
                complete.total_latency_ms = Some(rounded_ms(self.started));
                let usage = complete
                    .usage
                    .as_ref()
                    .map(|usage| serde_json::to_value(usage).expect("bounded usage contract"));
                let valid = |key| {
                    usage
                        .as_ref()
                        .and_then(|usage| usage.get(key))
                        .and_then(Value::as_f64)
                        .filter(|n| {
                            n.is_finite()
                                && (0.0..=9_007_199_254_740_991.0).contains(n)
                                && n.fract() == 0.0
                        })
                };
                let mut completed = OutcomeRecord {
                    context,
                    fields: &complete,
                    event: RecordName::Outcome,
                    status: if complete.error_type.is_some() {
                        OutcomeStatus::Error
                    } else if event == LifecycleName::RequestCancelled {
                        OutcomeStatus::Cancelled
                    } else {
                        OutcomeStatus::Completed
                    },
                    usage_complete: complete.completion_confirmed == Some(true)
                        && valid("input_tokens").is_some()
                        && valid("output_tokens").is_some(),
                    model_transitions: outcome.transitions.as_deref(),
                    model_transitions_truncated: outcome.transitions_truncated,
                    pricing_eligible: None,
                    unpriced_reason: None,
                };
                let estimate = estimate_outcome_savings(
                    &serde_json::to_value(&completed).expect("bounded outcome contract"),
                );
                completed.pricing_eligible = estimate["priced"].as_bool();
                if completed.pricing_eligible != Some(true) {
                    completed.unpriced_reason =
                        estimate["unpriced_reason"].as_str().map(str::to_owned);
                }
                Some(serde_json::to_value(completed).expect("bounded outcome contract"))
            } else {
                None
            };
            state.transition_tail = tail;
            state.finished = finished;
            (status, completed)
        };
        emit_document(&self.sinks.status, event_document(status, strings));
        if let Some(completed) = completed {
            // Legacy lifecycle fields cross the JavaScript document boundary
            // before outcome bookkeeping. Preserve its IEEE-754 observation
            // numbers here as well, while keeping typed producer inputs.
            self.record(event_document(completed, &[]).to_serde_observation_lossy());
        }
    }
    /// Legacy raw event facade. These open values are never producer contracts.
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
    pub fn decision_typed(&self, document: &JsDocument, decision: &TypedRouteDecision) {
        if self.sinks.decision.is_none() && self.sinks.record.is_none() {
            return;
        }
        let Some(context) = &self.typed_context else {
            // Explicit custom context facade retains its legacy open metadata.
            self.decision_exact(document, &decision.metadata.observation(), &decision.model);
            return;
        };
        let metadata = &decision.metadata;
        let requested = document
            .get(document.root(), "model")
            .and_then(|node| document.string(node));
        let prompt = if self.mode == SessionLogMode::Metadata {
            None
        } else {
            let include = context
                .request_class
                .as_deref()
                .is_none_or(|c| c == "main" || c.is_empty());
            let excerpt = if include {
                prompt_excerpt_document(document, 501)
                    .expect("Fixed diagnostic excerpt limit501 is safe")
            } else {
                String::new()
            };
            let mut chars = excerpt.chars();
            let text: String = chars.by_ref().take(500).collect();
            Some((text, chars.next().is_some()))
        };
        let record = DecisionRecord {
            schema_version: 2,
            event: RecordName::Decision,
            timestamp: timestamp(),
            context: context.clone(),
            prompt_excerpt: prompt.as_ref().map(|p| p.0.clone()),
            prompt_truncated: prompt.map(|p| p.1),
            fields: TelemetryFields {
                requested_model: requested.map(JsString::to_well_formed),
                selected_model: Some(metadata.model.clone()),
                decision_latency_ms: Some(metadata.latency_ms),
                routing_latency_ms: Some(metadata.latency_ms),
                evaluation_latency_ms: Some(metadata.evaluation_latency_ms),
                source: Some(metadata.source),
                reason: Some(metadata.reason.clone()),
                evaluator: metadata.evaluator,
                classified_tier: metadata.classified_tier,
                classifier_error: metadata.classifier_error,
                classifier_status: metadata.classifier_status,
                compatibility_reason: metadata.compatibility_reason.clone(),
                continuity_state: metadata.continuity_state,
                context_check: metadata.context_check,
                counted_input_tokens: metadata.counted_input_tokens,
                ..TelemetryFields::default()
            },
        };
        let row = serde_json::to_value(record).expect("bounded decision record contract");
        self.record(row.clone());
        let strings = ExactModelFields {
            selected_model: Some(&decision.model),
            requested_model: requested,
            model: None,
        }
        .overrides();
        emit_document(&self.sinks.decision, event_document(row, &strings));
    }
    /// Legacy custom decision boundary; arbitrary input is normalized before storage.
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
                    .expect("Fixed diagnostic excerpt limit501 is safe")
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
    use autorouter_core::contracts::{
        DecisionSource, PricingContext, RoutingDecision, Tier, UsageTelemetry,
    };

    fn strip_dynamic_times(mut value: Value) -> Value {
        if let Some(object) = value.as_object_mut() {
            for key in ["timestamp", "first_response_ms", "total_latency_ms"] {
                object.remove(key);
            }
        }
        value
    }
    #[test]
    fn typed_lifecycle_matches_legacy_shapes_records_and_exact_model_transitions() {
        for mode in [SessionLogMode::Metadata, SessionLogMode::Prompts] {
            for ending in [
                LifecycleName::RequestComplete,
                LifecycleName::RequestError,
                LifecycleName::RequestCancelled,
            ] {
                let make = |typed| {
                    let statuses = Arc::new(Mutex::new(Vec::<JsDocument>::new()));
                    let decisions = Arc::new(Mutex::new(Vec::<JsDocument>::new()));
                    let records = Arc::new(Mutex::new(Vec::<Value>::new()));
                    let status_sink = statuses.clone();
                    let decision_sink = decisions.clone();
                    let record_sink = records.clone();
                    let sinks = EventSinks {
                        status: Some(Arc::new(move |row| status_sink.lock().unwrap().push(row))),
                        decision: Some(Arc::new(move |row| {
                            decision_sink.lock().unwrap().push(row)
                        })),
                        record: Some(Arc::new(move |row| record_sink.lock().unwrap().push(row))),
                        ..EventSinks::default()
                    };
                    let context = RequestContext {
                        request_id: "synthetic-request".into(),
                        session_id: Some("synthetic-session".into()),
                        agent_id: Some("synthetic-agent".into()),
                        prompt_id: Some("synthetic-prompt".into()),
                        request_class: Some("main".into()),
                    };
                    let events = if typed {
                        RequestEvents::new_typed(context, "claude-opus-5-5", mode, sinks)
                    } else {
                        RequestEvents::new(
                            serde_json::to_value(context).unwrap(),
                            "claude-opus-5-5",
                            mode,
                            sinks,
                        )
                    };
                    (events, statuses, decisions, records)
                };
                let (typed, typed_status, typed_decisions, typed_records) = make(true);
                let (legacy, legacy_status, legacy_decisions, legacy_records) = make(false);
                let document = JsDocument::parse(br#"{"model":"claude-sonnet-5-5","messages":[{"role":"user","content":"Synthetic contribution task"}]}"#).unwrap();
                let requested = JsString::from_scalar("claude-sonnet-5-5");
                let mut metadata =
                    RoutingDecision::passthrough("claude-sonnet-5-5".into(), "classified");
                metadata.source = DecisionSource::Jev;
                metadata.tier = Some(Tier::Sonnet);
                metadata.classified_tier = Some(Tier::Sonnet);
                metadata.latency_ms = 2.0;
                metadata.evaluation_latency_ms = 1.0;
                let decision = TypedRouteDecision {
                    metadata,
                    model: requested.clone(),
                };
                typed.decision_typed(&document, &decision);
                legacy.decision_exact(&document, &decision.metadata.observation(), &decision.model);
                let fields = TelemetryFields::route(
                    &decision.metadata,
                    &requested,
                    PricingContext {
                        speed: "standard".into(),
                        inference_geo: "global".into(),
                        service_tier: "auto".into(),
                        pricing_unsupported: None,
                    },
                );
                typed.lifecycle_exact(
                    LifecycleName::Route,
                    fields.clone(),
                    ExactModelFields {
                        model: Some(&requested),
                        requested_model: Some(&requested),
                        selected_model: None,
                    },
                );
                legacy.status_document(
                    "route",
                    event_document(
                        fields.observation(),
                        &[("model", &requested), ("requested_model", &requested)],
                    ),
                );
                typed.forwarding();
                legacy.forwarding();
                typed.lifecycle(
                    LifecycleName::UpstreamResponse,
                    TelemetryFields {
                        status: Some(200),
                        ..TelemetryFields::default()
                    },
                );
                legacy.status("upstream_response", json!({"status":200}));
                // Two different lone surrogates share a scalar display string;
                // transition ownership must still use their exact identities.
                for unit in [0xd800, 0xd800, 0xd801] {
                    let mut units: Vec<u16> = "synthetic-".encode_utf16().collect();
                    units.push(unit);
                    let model = JsString::from_utf16(units);
                    typed.lifecycle_exact(
                        LifecycleName::UpstreamModel,
                        TelemetryFields {
                            model: Some(model.to_well_formed()),
                            ..TelemetryFields::default()
                        },
                        ExactModelFields {
                            model: Some(&model),
                            ..ExactModelFields::default()
                        },
                    );
                    legacy.status_document(
                        "upstream_model",
                        event_document(json!({}), &[("model", &model)]),
                    );
                }
                let usage = json!({"input_tokens":10.0,"output_tokens":20.0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0});
                typed.lifecycle(
                    LifecycleName::UpstreamUsage,
                    TelemetryFields {
                        usage: Some(UsageTelemetry::from_observation(usage.clone())),
                        ..TelemetryFields::default()
                    },
                );
                legacy.status("upstream_usage", json!({"usage":usage}));
                typed.confirmed(ending == LifecycleName::RequestComplete);
                legacy.confirmed(ending == LifecycleName::RequestComplete);
                let fields = if ending == LifecycleName::RequestError {
                    TelemetryFields {
                        status: Some(502),
                        error_type: Some("request_error".into()),
                        ..TelemetryFields::default()
                    }
                } else {
                    TelemetryFields::default()
                };
                typed.lifecycle(ending, fields.clone());
                legacy.status(ending.as_str(), fields.observation());
                typed.lifecycle(LifecycleName::RequestComplete, TelemetryFields::default()); // finished admission is closed
                let compare_documents =
                    |left: &Arc<Mutex<Vec<JsDocument>>>, right: &Arc<Mutex<Vec<JsDocument>>>| {
                        let left = left.lock().unwrap();
                        let right = right.lock().unwrap();
                        assert_eq!(left.len(), right.len());
                        for (left, right) in left.iter().zip(right.iter()) {
                            assert_eq!(
                                strip_dynamic_times(left.to_serde_observation_lossy()),
                                strip_dynamic_times(right.to_serde_observation_lossy())
                            );
                            for key in ["model", "selected_model", "requested_model"] {
                                assert_eq!(
                                    left.get(left.root(), key)
                                        .and_then(|node| left.string(node)),
                                    right
                                        .get(right.root(), key)
                                        .and_then(|node| right.string(node))
                                );
                            }
                        }
                    };
                compare_documents(&typed_status, &legacy_status);
                compare_documents(&typed_decisions, &legacy_decisions);
                let typed_records = typed_records.lock().unwrap();
                let legacy_records = legacy_records.lock().unwrap();
                assert_eq!(typed_records.len(), legacy_records.len());
                for (left, right) in typed_records.iter().zip(legacy_records.iter()) {
                    assert_eq!(
                        strip_dynamic_times(left.clone()),
                        strip_dynamic_times(right.clone())
                    );
                }
                if mode == SessionLogMode::Metadata {
                    assert!(
                        typed_records
                            .iter()
                            .all(|row| row.get("prompt_excerpt").is_none()
                                && row.get("prompt_truncated").is_none())
                    );
                }
            }
        }
    }
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
