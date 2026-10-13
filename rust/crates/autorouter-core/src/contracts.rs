//! Owned first-party contracts. Open provider JSON and legacy embedding values
//! enter through explicit adapters; they are never flattened into these types.
use crate::config::Evaluator;
use crate::js_json::JsString;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
        pub enum $name { $(#[serde(rename = $text)] $variant),+ }
        impl $name {
            pub const VARIANTS: &'static [Self] = &[$(Self::$variant),+];
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
            pub fn parse(text: &str) -> Option<Self> {
                match text { $($text => Some(Self::$variant)),+, _ => None }
            }
        }
        // Observation comparisons remain convenient without accepting arbitrary
        // strings as producer inputs or enum values.
        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool { self.as_str() == *other }
        }
    };
}
string_enum!(Tier { Haiku => "haiku", Sonnet => "sonnet", Opus => "opus" });
impl Tier {
    pub const ALL: [Self; 3] = [Self::Haiku, Self::Sonnet, Self::Opus];
    pub fn from_value(value: Option<&Value>) -> Option<Self> {
        Self::parse(value?.as_str()?)
    }
}
impl Ord for Tier {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (*self as u8).cmp(&(*other as u8))
    }
}
impl PartialOrd for Tier {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
string_enum!(DecisionSource {
    Jev => "jev", Ollama => "ollama", Cache => "cache", Fallback => "fallback",
    Passthrough => "passthrough"
});
string_enum!(ClassifierError {
    Timeout => "timeout", HttpError => "http_error", InvalidResponse => "invalid_response",
    NetworkError => "network_error", CapacityExhausted => "capacity_exhausted"
});
string_enum!(LifecycleName {
    RequestStart => "request_start", Route => "route", UpstreamResponse => "upstream_response",
    UpstreamModel => "upstream_model", UpstreamUsage => "upstream_usage", UpstreamError => "upstream_error",
    RequestComplete => "request_complete", RequestError => "request_error", RequestCancelled => "request_cancelled"
});
string_enum!(ContinuityState {
    Selected => "selected", Confirmed => "confirmed", Unknown => "unknown",
    CapacityExhausted => "capacity_exhausted"
});
string_enum!(ContextCheck {
    WithinBudget => "within_budget", OverBudget => "over_budget", CountUnavailable => "count_unavailable"
});
string_enum!(DiagnosticName {
    ProxyError => "proxy_error", UpstreamResponse => "upstream_response",
    UpstreamModel => "upstream_model", UpstreamError => "upstream_error",
    InvalidRequestShape => "invalid_request_shape"
});
#[derive(Serialize)]
pub struct DiagnosticEvent {
    pub event: DiagnosticName,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
}
impl DiagnosticEvent {
    pub fn status(event: DiagnosticName, status: u16) -> Self {
        Self {
            event,
            status: Some(status),
            model: None,
            error_type: None,
        }
    }
    pub fn observation(&self) -> Value {
        serde_json::to_value(self).expect("bounded diagnostic contract")
    }
}
#[derive(Serialize)]
pub struct InvalidRequestShape {
    pub event: DiagnosticName,
    pub model_type: &'static str,
    pub messages_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<MessageShape>>,
}
#[derive(Serialize)]
pub struct MessageShape {
    pub role: String,
    pub content_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<BlockShape>>,
}
#[derive(Serialize)]
pub struct BlockShape {
    pub value_type: &'static str,
    pub type_type: &'static str,
}

#[derive(Clone, Serialize)]
pub struct ClassifierDecision {
    pub tier: Tier,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classified_tier: Option<Tier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    pub evaluator: Evaluator,
    pub source: DecisionSource,
    pub reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classifier_error: Option<ClassifierError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classifier_status: Option<u16>,
}

/// Display metadata has scalar strings. Exact wire identity lives alongside it
/// in TypedRouteDecision and is never reconstructed from this projection.
#[derive(Clone, Serialize)]
pub struct RoutingDecision {
    pub model: String,
    pub source: DecisionSource,
    pub reason: String,
    pub latency_ms: f64,
    pub evaluation_latency_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classified_tier: Option<Tier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluator: Option<Evaluator>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classifier_error: Option<ClassifierError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classifier_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compatibility_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuity_state: Option<ContinuityState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_check: Option<ContextCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counted_input_tokens: Option<u64>,
}
impl RoutingDecision {
    pub fn passthrough(model: String, reason: &str) -> Self {
        Self {
            model,
            source: DecisionSource::Passthrough,
            reason: reason.into(),
            latency_ms: 0.0,
            evaluation_latency_ms: 0.0,
            tier: None,
            classified_tier: None,
            confidence: None,
            evaluator: None,
            classifier_error: None,
            classifier_status: None,
            compatibility_reason: None,
            continuity_state: None,
            context_check: None,
            counted_input_tokens: None,
        }
    }
    pub fn classified(value: &ClassifierDecision) -> Self {
        Self {
            source: value.source,
            reason: value.reason.into(),
            tier: Some(value.tier),
            classified_tier: value.classified_tier,
            confidence: value.confidence,
            evaluator: Some(value.evaluator),
            classifier_error: value.classifier_error,
            classifier_status: value.classifier_status,
            ..Self::passthrough(String::new(), "")
        }
    }
    pub fn observation(&self) -> Value {
        serde_json::to_value(self).expect("bounded routing contract serializes")
    }
    /// Core policy's historical observation precedes runtime timing fields.
    pub fn policy_observation(&self) -> Value {
        let mut value = self.observation();
        value.as_object_mut().unwrap().remove("latency_ms");
        value
            .as_object_mut()
            .unwrap()
            .remove("evaluation_latency_ms");
        value
    }
}
pub struct TypedRouteDecision {
    pub metadata: RoutingDecision,
    pub model: JsString,
}
#[derive(Default)]
pub struct ExactModelFields<'a> {
    pub model: Option<&'a JsString>,
    pub requested_model: Option<&'a JsString>,
    pub selected_model: Option<&'a JsString>,
}
impl<'a> ExactModelFields<'a> {
    pub fn overrides(self) -> Vec<(&'static str, &'a JsString)> {
        [
            ("model", self.model),
            ("requested_model", self.requested_model),
            ("selected_model", self.selected_model),
        ]
        .into_iter()
        .filter_map(|(name, value)| value.map(|value| (name, value)))
        .collect()
    }
}
/// Flattening a closed typed struct preserves the historical flat log shape;
/// no arbitrary map or open provider envelope can be passed as its fields.
#[derive(Serialize)]
pub struct RouteLog<'a> {
    #[serde(flatten)]
    pub decision: &'a RoutingDecision,
    pub event: LifecycleName,
    pub requested_model: &'a str,
    pub request_adjustments: &'a [&'static str],
}

#[derive(Clone, Default, Serialize)]
pub struct RequestContext {
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_class: Option<String>,
}
string_enum!(RecordName { Decision => "decision", Outcome => "outcome" });
#[derive(Serialize)]
pub struct DecisionRecord {
    pub schema_version: u64,
    pub event: RecordName,
    pub timestamp: String,
    #[serde(flatten)]
    pub context: RequestContext,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_excerpt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_truncated: Option<bool>,
    #[serde(flatten)]
    pub fields: TelemetryFields,
}
string_enum!(OutcomeStatus { Completed => "completed", Error => "error", Cancelled => "cancelled" });
#[derive(Serialize)]
pub struct LifecycleEvent<'a> {
    #[serde(flatten)]
    pub context: &'a RequestContext,
    pub event: LifecycleName,
    #[serde(flatten)]
    pub fields: &'a TelemetryFields,
}
#[derive(Serialize)]
pub struct OutcomeRecord<'a> {
    #[serde(flatten)]
    pub context: &'a RequestContext,
    #[serde(flatten)]
    pub fields: &'a TelemetryFields,
    pub event: RecordName,
    pub status: OutcomeStatus,
    pub usage_complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing_eligible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unpriced_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_transitions: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_transitions_truncated: Option<bool>,
}

/// Opaque *nested* observer metadata, admitted only through the reviewed usage
/// normalizer. Number representation and nested nulls are retained as before.
#[derive(Clone, Serialize)]
#[serde(transparent)]
pub struct UsageTelemetry(Value);
impl UsageTelemetry {
    pub fn from_observation(value: Value) -> Self {
        Self(
            crate::telemetry_event::normalize_usage_telemetry(Some(&value))
                .unwrap_or_else(|| Value::Object(Map::new())),
        )
    }
}
/// Request pricing values are extracted at the provider-envelope boundary.
#[derive(Clone, Serialize)]
pub struct PricingContext {
    pub speed: String,
    pub inference_geo: String,
    pub service_tier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing_unsupported: Option<bool>,
}

/// First-party lifecycle producers can name only documented fields. Open
/// custom events remain explicit legacy adapters, never flattened producers.
#[derive(Clone, Default, Serialize)]
pub struct TelemetryFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmed_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<DecisionSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluator: Option<Evaluator>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluation_latency_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing_latency_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_latency_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_response_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_latency_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classifier_error: Option<ClassifierError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classifier_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classified_tier: Option<Tier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compatibility_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuity_state: Option<ContinuityState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_check: Option<ContextCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counted_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_confirmed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageTelemetry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing_context: Option<PricingContext>,
}
impl TelemetryFields {
    pub fn observation(&self) -> Value {
        serde_json::to_value(self).expect("bounded telemetry contract serializes")
    }
    pub fn route(
        decision: &RoutingDecision,
        requested: &JsString,
        pricing: PricingContext,
    ) -> Self {
        Self {
            model: Some(decision.model.clone()),
            requested_model: Some(requested.to_well_formed()),
            source: Some(decision.source),
            evaluator: decision.evaluator,
            reason: Some(decision.reason.clone()),
            latency_ms: Some(decision.latency_ms),
            evaluation_latency_ms: Some(decision.evaluation_latency_ms),
            routing_latency_ms: Some(decision.latency_ms),
            classifier_error: decision.classifier_error,
            classifier_status: decision.classifier_status,
            classified_tier: decision.classified_tier,
            compatibility_reason: decision.compatibility_reason.clone(),
            continuity_state: decision.continuity_state,
            context_check: decision.context_check,
            counted_input_tokens: decision.counted_input_tokens,
            pricing_context: Some(pricing),
            ..Self::default()
        }
    }
}
