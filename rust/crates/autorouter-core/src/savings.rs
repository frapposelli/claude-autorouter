//! API-equivalent estimates using recorded token counts and reviewed prices.
//! These figures never claim subscription charges or quota credits. All costs
//! accumulate as exact integers in units of 1/100,000,000 USD.

use num_bigint::BigInt;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::request_validation::nonnegative_safe_integer;
use crate::telemetry_event::{
    UNPRICED_REASONS, normalize_session_record, sanitize_identifier, sanitize_model,
};

pub const PRICING_VERSION: &str = "2026-09-29.1";
pub const PRICING_DATE: &str = "2026-09-29";
pub const PRICING_SOURCE: &str = "https://platform.claude.com/docs/en/about-claude/pricing";
const DEFAULT_BASELINE: &str = "claude-opus-5-5";
const MAX_SESSIONS: usize = 100;
const MAX_INFLIGHT: usize = 1000;
const MAX_HISTORY: usize = 10000;
const MAX_SAFE: u64 = 9_007_199_254_740_991;
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

#[derive(Clone, Copy, PartialEq, Eq)]
struct Prices {
    input: u64,
    output: u64,
    write5m: u64,
    write1h: u64,
    read: u64,
}
const HAIKU_45: Prices = Prices {
    input: 100,
    output: 500,
    write5m: 125,
    write1h: 200,
    read: 10,
};
const SONNET_5: Prices = Prices {
    input: 200,
    output: 1000,
    write5m: 250,
    write1h: 400,
    read: 20,
};
const OPUS_5: Prices = Prices {
    input: 500,
    output: 2500,
    write5m: 625,
    write1h: 1000,
    read: 50,
};
const OPUS_55: Prices = Prices {
    input: 400,
    output: 2000,
    write5m: 500,
    write1h: 800,
    read: 20,
};

fn prices(model: &str) -> Option<&'static Prices> {
    match model {
        "claude-haiku-4-5" | "claude-haiku-4-5-20251001" => Some(&HAIKU_45),
        "claude-sonnet-5" | "claude-sonnet-5-5" => Some(&SONNET_5),
        "claude-opus-5" => Some(&OPUS_5),
        "claude-opus-5-5" => Some(&OPUS_55),
        _ => None,
    }
}

fn opus(model: &str) -> bool {
    matches!(model, "claude-opus-5" | "claude-opus-5-5")
}

pub fn pricing_facts() -> Value {
    let models=["claude-haiku-4-5","claude-haiku-4-5-20251001","claude-sonnet-5","claude-sonnet-5-5","claude-opus-5","claude-opus-5-5"]
        .iter().map(|model| {
            let p=prices(model).expect("reviewed price identity");
            ((*model).to_owned(),json!({"input":p.input,"output":p.output,"write5m":p.write5m,"write1h":p.write1h,"read":p.read}))
        }).collect::<Map<_,_>>();
    json!({"version":PRICING_VERSION,"date":PRICING_DATE,"source":PRICING_SOURCE,
        "currency":"USD","unit":"cents_per_million_tokens","models":models})
}

fn standard_pricing(context: Option<&Value>, allow_unavailable_geo: bool) -> bool {
    let Some(context) = context else {
        return true;
    };
    context.is_object()
        && context
            .get("pricing_unsupported")
            .is_none_or(|value| value.as_bool() == Some(false))
        && context
            .get("unsupported")
            .is_none_or(|value| value.as_bool() == Some(false))
        && context
            .get("speed")
            .is_none_or(|value| value.as_str() == Some("standard"))
        && context.get("inference_geo").is_none_or(|value| {
            value.as_str() == Some("global")
                || (allow_unavailable_geo && value.as_str() == Some("not_available"))
        })
        && context.get("service_tier").is_none_or(|value| {
            matches!(value.as_str(), Some("auto" | "standard_only" | "standard"))
        })
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Usage {
    input: u64,
    output: u64,
    read: u64,
    write5m: u64,
    write1h: u64,
}

fn normalize_usage(value: Option<&Value>, model_prices: Option<&Prices>) -> Option<Usage> {
    let value = value?;
    if !value.is_object() || !standard_pricing(Some(value), model_prices == Some(&HAIKU_45)) {
        return None;
    }
    let input = nonnegative_safe_integer(value.get("input_tokens")?)?;
    let output = nonnegative_safe_integer(value.get("output_tokens")?)?;
    let read = value
        .get("cache_read_input_tokens")
        .map_or(Some(0), nonnegative_safe_integer)?;
    let creation = value
        .get("cache_creation_input_tokens")
        .map(nonnegative_safe_integer);
    if creation == Some(None) {
        return None;
    }
    let creation = creation.flatten();
    let (mut write5m, mut write1h) = (0, 0);
    if let Some(cache) = value.get("cache_creation") {
        let cache = cache.as_object()?;
        write5m = cache
            .get("ephemeral_5m_input_tokens")
            .map_or(Some(0), nonnegative_safe_integer)?;
        write1h = cache
            .get("ephemeral_1h_input_tokens")
            .map_or(Some(0), nonnegative_safe_integer)?;
        let total = write5m + write1h;
        if total > MAX_SAFE || creation.is_some_and(|creation| creation != total) {
            return None;
        }
    } else if creation.is_some_and(|creation| creation > 0) {
        return None;
    }
    Some(Usage {
        input,
        output,
        read,
        write5m,
        write1h,
    })
}

fn cost(usage: Usage, prices: &Prices) -> BigInt {
    [
        (usage.input, prices.input),
        (usage.output, prices.output),
        (usage.read, prices.read),
        (usage.write5m, prices.write5m),
        (usage.write1h, prices.write1h),
    ]
    .into_iter()
    .map(|(count, price)| BigInt::from(count) * price)
    .sum()
}

#[derive(Default)]
struct Session {
    actual: BigInt,
    baseline: BigInt,
    requests: u64,
    unpriced_requests: u64,
    unpriced_reasons: HashMap<&'static str, u64>,
    partial: bool,
}

struct Request {
    session_id: String,
    invalid: bool,
    failed: bool,
    model: Option<Value>,
    prices: Option<&'static Prices>,
    usage: Option<Usage>,
    unpriced_reason: Option<&'static str>,
}

/// Bounded request/session attribution with exact aggregate costs. No request
/// bodies or raw provider errors are retained. Snapshots are detached values.
pub struct SavingsTracker {
    baseline: Option<&'static Prices>,
    baseline_name: String,
    sessions: HashMap<String, Session>,
    session_order: VecDeque<String>,
    inflight: BTreeMap<u128, Request>,
    inflight_keys: HashMap<String, u128>,
    sequence: u128,
    keys_by_sequence: HashMap<u128, String>,
    settled: HashSet<String>,
    settled_order: VecDeque<String>,
    evicted: HashSet<String>,
    evicted_order: VecDeque<String>,
    history_partial: bool,
}

impl Default for SavingsTracker {
    fn default() -> Self {
        Self::new(DEFAULT_BASELINE)
    }
}

impl SavingsTracker {
    pub fn new(baseline_model: &str) -> Self {
        let name = Value::String(baseline_model.into());
        Self {
            baseline: if opus(baseline_model) {
                prices(baseline_model)
            } else {
                None
            },
            baseline_name: sanitize_model(&name).unwrap_or("unknown").into(),
            sessions: HashMap::new(),
            session_order: VecDeque::new(),
            inflight: BTreeMap::new(),
            inflight_keys: HashMap::new(),
            keys_by_sequence: HashMap::new(),
            sequence: 0,
            settled: HashSet::new(),
            settled_order: VecDeque::new(),
            evicted: HashSet::new(),
            evicted_order: VecDeque::new(),
            history_partial: false,
        }
    }

    fn remember(&mut self, key: String) {
        if self.settled.insert(key.clone()) {
            self.settled_order.push_back(key);
        }
        while self.settled_order.len() > MAX_HISTORY {
            if let Some(key) = self.settled_order.pop_front() {
                self.settled.remove(&key);
            }
            self.history_partial = true;
        }
    }

    fn remove_request(&mut self, key: &str) -> Option<Request> {
        let sequence = self.inflight_keys.remove(key)?;
        self.keys_by_sequence.remove(&sequence);
        self.inflight.remove(&sequence)
    }

    fn finish(&mut self, key: &str, failed: bool, partial: bool, failure_reason: &'static str) {
        let Some(request) = self.remove_request(key) else {
            return;
        };
        self.remember(key.into());
        let Some(session) = self.sessions.get_mut(&request.session_id) else {
            return;
        };
        if partial {
            session.partial = true;
        }
        if failed
            || request.failed
            || request.invalid
            || request.prices.is_none()
            || request.usage.is_none()
            || self.baseline.is_none()
        {
            session.unpriced_requests += 1;
            let reason = if failed {
                failure_reason
            } else if request.failed {
                "request_failed"
            } else {
                request
                    .unpriced_reason
                    .unwrap_or(if self.baseline.is_none() {
                        "unknown_baseline"
                    } else if request.prices.is_none() {
                        "missing_model"
                    } else {
                        "missing_usage"
                    })
            };
            let count = session.unpriced_reasons.entry(reason).or_default();
            *count = (*count + 1).min(MAX_SAFE);
            return;
        }
        if let (Some(usage), Some(prices), Some(baseline)) =
            (request.usage, request.prices, self.baseline)
        {
            session.actual += cost(usage, prices);
            session.baseline += cost(usage, baseline);
            session.requests += 1;
        }
    }

    fn session_for(&mut self, session_id: &str) {
        if self.sessions.contains_key(session_id) {
            self.session_order.retain(|id| id != session_id);
            self.session_order.push_back(session_id.into());
            return;
        }
        self.sessions.insert(
            session_id.into(),
            Session {
                partial: self.history_partial || self.evicted.contains(session_id),
                ..Session::default()
            },
        );
        self.session_order.push_back(session_id.into());
        while self.sessions.len() > MAX_SESSIONS {
            let Some(oldest) = self.session_order.pop_front() else {
                break;
            };
            self.sessions.remove(&oldest);
            if self.evicted.insert(oldest.clone()) {
                self.evicted_order.push_back(oldest.clone());
            }
            let keys: Vec<String> = self
                .inflight
                .iter()
                .filter(|(_, request)| request.session_id == oldest)
                .filter_map(|(sequence, _)| self.keys_by_sequence.get(sequence).cloned())
                .collect();
            for key in keys {
                self.remove_request(&key);
                self.remember(key);
            }
            if self.evicted.len() > MAX_HISTORY {
                if let Some(id) = self.evicted_order.pop_front() {
                    self.evicted.remove(&id);
                }
                self.history_partial = true;
            }
        }
    }

    pub fn update(&mut self, event: &Value) {
        let Some(name) = event
            .get("event")
            .and_then(Value::as_str)
            .filter(|name| EVENTS.contains(name))
        else {
            return;
        };
        let session_id = match event.get("session_id") {
            None | Some(Value::Null) => "",
            Some(Value::String(value)) if value.is_empty() => "",
            Some(value) => match sanitize_identifier(value) {
                Some(value) => value,
                None => return,
            },
        };
        let Some(request_id) = event.get("request_id").and_then(sanitize_identifier) else {
            return;
        };
        let key = format!("{session_id}\0{request_id}");
        if name == "request_start" {
            if self.inflight_keys.contains_key(&key) || self.settled.contains(&key) {
                return;
            }
            self.session_for(session_id);
            while self.inflight.len() >= MAX_INFLIGHT {
                let Some(key) = self
                    .inflight
                    .first_key_value()
                    .and_then(|(sequence, _)| self.keys_by_sequence.get(sequence))
                    .cloned()
                else {
                    break;
                };
                self.finish(&key, true, true, "request_evicted");
            }
            let invalid = !standard_pricing(event.get("pricing_context"), false);
            let request = Request {
                session_id: session_id.into(),
                invalid,
                failed: false,
                model: None,
                prices: None,
                usage: None,
                unpriced_reason: invalid.then_some("unsupported_pricing"),
            };
            self.sequence += 1;
            self.inflight.insert(self.sequence, request);
            self.keys_by_sequence.insert(self.sequence, key.clone());
            self.inflight_keys.insert(key, self.sequence);
            return;
        }
        let Some(sequence) = self.inflight_keys.get(&key).copied() else {
            return;
        };
        let Some(request) = self.inflight.get_mut(&sequence) else {
            return;
        };
        match name {
            "route" => {
                if !standard_pricing(event.get("pricing_context"), false) {
                    request.invalid = true;
                    request.unpriced_reason.get_or_insert("unsupported_pricing");
                }
            }
            "upstream_response" => {
                if !event
                    .get("status")
                    .and_then(Value::as_f64)
                    .is_some_and(|value| value.fract() == 0.0 && (200.0..300.0).contains(&value))
                {
                    request.failed = true;
                }
            }
            "upstream_model" => {
                let current = event.get("model");
                let rates = current.and_then(Value::as_str).and_then(prices);
                if rates.is_none() {
                    request.invalid = true;
                    request.unpriced_reason.get_or_insert("unknown_model");
                }
                if request
                    .model
                    .as_ref()
                    .is_some_and(|previous| !js_same_primitive(Some(previous), current))
                {
                    request.invalid = true;
                    request.unpriced_reason = Some("mixed_models");
                }
                request.model = current.cloned();
                request.prices = rates;
            }
            "upstream_usage" => {
                let usage = normalize_usage(event.get("usage"), request.prices);
                let standard = standard_pricing(event.get("pricing_context"), false);
                if usage.is_none() || !standard {
                    request.invalid = true;
                    request.unpriced_reason.get_or_insert(
                        if !standard
                            || !standard_pricing(
                                event.get("usage"),
                                request.prices == Some(&HAIKU_45),
                            )
                        {
                            "unsupported_pricing"
                        } else {
                            "invalid_usage"
                        },
                    );
                } else if request.usage.is_some() && request.usage != usage {
                    request.invalid = true;
                    request.unpriced_reason.get_or_insert("conflicting_usage");
                } else {
                    request.usage = usage;
                }
            }
            "upstream_error" => request.failed = true,
            "request_complete" => self.finish(
                &key,
                event.get("completion_confirmed").and_then(Value::as_bool) == Some(false),
                false,
                "unconfirmed_completion",
            ),
            "request_error" => self.finish(&key, true, false, "request_failed"),
            "request_cancelled" => self.finish(&key, true, false, "request_cancelled"),
            _ => {}
        }
    }

    pub fn snapshot(&self) -> Value {
        self.session_order.iter().filter_map(|id|self.sessions.get(id).map(|session| {
            let saved=&session.baseline-&session.actual;
            let number=|value:&BigInt|value.to_string().parse::<f64>().unwrap_or(f64::INFINITY);
            let actual=number(&session.actual);let baseline=number(&session.baseline);let saved=number(&saved);
            let reasons=UNPRICED_REASONS.iter().filter_map(|reason|session.unpriced_reasons.get(reason)
                .filter(|count|**count>0).map(|count|((*reason).to_owned(),json!(count)))).collect::<Map<_,_>>();
            let mut result=json!({"baseline_model":self.baseline_name,"pricing_version":PRICING_VERSION,"pricing_date":PRICING_DATE,
                "pricing_source":PRICING_SOURCE,"actual_usd":actual/100_000_000.0,"baseline_usd":baseline/100_000_000.0,
                "saved_usd":saved/100_000_000.0,"percent":if baseline==0.0 {0.0} else {saved/baseline*100.0},
                "requests":session.requests,"unpriced_requests":session.unpriced_requests,"unpriced_reasons":reasons});
            if session.partial || self.history_partial {result["partial"]=true.into();}
            (id.clone(),result)
        })).collect::<Map<_,_>>().into()
    }

    pub fn clear(&mut self) {
        self.sessions.clear();
        self.session_order.clear();
        self.inflight.clear();
        self.inflight_keys.clear();
        self.keys_by_sequence.clear();
        self.settled.clear();
        self.settled_order.clear();
        self.evicted.clear();
        self.evicted_order.clear();
        self.history_partial = false;
    }
}

fn js_same_primitive(left: Option<&Value>, right: Option<&Value>) -> bool {
    match (left, right) {
        (Some(Value::Object(_) | Value::Array(_)), _)
        | (_, Some(Value::Object(_) | Value::Array(_))) => false,
        (Some(Value::Number(a)), Some(Value::Number(b))) => a.as_f64() == b.as_f64(),
        _ => left == right,
    }
}

pub fn estimate_outcome_savings(value: &Value) -> Value {
    let unpriced = |reason: &str| json!({"priced":false,"unpriced_reason":reason});
    let Some(outcome) = normalize_session_record(value, false, "1970-01-01T00:00:00.000Z") else {
        return unpriced("invalid_telemetry");
    };
    if outcome["event"] != "outcome" {
        return unpriced("invalid_telemetry");
    }
    match outcome["status"].as_str() {
        Some("cancelled") => return unpriced("request_cancelled"),
        Some("error") => return unpriced("request_failed"),
        _ => {}
    }
    if outcome["completion_confirmed"] != true {
        return unpriced("unconfirmed_completion");
    }
    if outcome["pricing_version"] != PRICING_VERSION {
        return unpriced("unknown_pricing_version");
    }
    let Some(baseline) = outcome
        .get("baseline_model")
        .and_then(Value::as_str)
        .filter(|model| opus(model))
    else {
        return unpriced("unknown_baseline");
    };
    if outcome["usage_complete"] == false {
        return unpriced("incomplete_usage");
    }
    let mut models = HashSet::new();
    if let Some(model) = outcome.get("confirmed_model").and_then(Value::as_str) {
        models.insert(model);
    }
    if let Some(transitions) = outcome.get("model_transitions").and_then(Value::as_array) {
        for model in transitions.iter().filter_map(Value::as_str) {
            models.insert(model);
        }
    }
    if outcome["model_transitions_truncated"] == true || models.len() > 1 {
        return unpriced("mixed_models");
    }
    if outcome["pricing_eligible"] == false {
        return unpriced(
            outcome
                .get("unpriced_reason")
                .and_then(Value::as_str)
                .unwrap_or("unsupported_pricing"),
        );
    }
    let mut tracker = SavingsTracker::new(baseline);
    let event = |name: &str| json!({"event":name,"request_id":"outcome","session_id":"outcome"});
    let mut start = event("request_start");
    if let Some(context) = outcome.get("pricing_context") {
        start["pricing_context"] = context.clone();
    }
    tracker.update(&start);
    for (field, name, destination) in [
        ("http_status", "upstream_response", "status"),
        ("confirmed_model", "upstream_model", "model"),
        ("usage", "upstream_usage", "usage"),
    ] {
        if let Some(value) = outcome.get(field) {
            let mut input = event(name);
            input[destination] = value.clone();
            tracker.update(&input);
        }
    }
    tracker.update(&event("request_complete"));
    let snapshot = tracker.snapshot();
    let result = &snapshot["outcome"];
    if result["unpriced_requests"].as_u64().unwrap_or(0) > 0 {
        let reason = result["unpriced_reasons"]
            .as_object()
            .and_then(|reasons| reasons.keys().next())
            .map(String::as_str)
            .unwrap_or("invalid_telemetry");
        return unpriced(reason);
    }
    let mut output = json!({"priced":true});
    for key in [
        "actual_usd",
        "baseline_usd",
        "saved_usd",
        "percent",
        "baseline_model",
        "pricing_version",
    ] {
        output[key] = result[key].clone();
    }
    output
}

/// Development fixture interface. Event arrays return a final snapshot;
/// explicit steps return snapshots taken before/after update or clear actions.
pub fn run_fixture(input: &Value) -> Result<Value, String> {
    let baseline = match input.get("baseline_model") {
        None => DEFAULT_BASELINE,
        Some(value) => value.as_str().unwrap_or(""),
    };
    let mut tracker = SavingsTracker::new(baseline);
    if let Some(events) = input.get("events").and_then(Value::as_array) {
        for event in events {
            tracker.update(event);
        }
        return Ok(tracker.snapshot());
    }
    let steps = input
        .get("steps")
        .and_then(Value::as_array)
        .ok_or("Invalid savings fixture")?;
    let mut snapshots = Vec::new();
    for step in steps {
        match step.get("op").and_then(Value::as_str) {
            Some("update") => tracker.update(step.get("event").ok_or("Invalid savings update")?),
            Some("snapshot") => snapshots.push(tracker.snapshot()),
            Some("clear") => tracker.clear(),
            _ => return Err("Unknown savings fixture operation".into()),
        }
    }
    Ok(snapshots.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    const HAIKU: &str = "claude-haiku-4-5-20251001";
    fn event(name: &str, id: &str) -> Value {
        json!({"event":name,"session_id":"session-a","request_id":id})
    }
    fn complete(tracker: &mut SavingsTracker, id: &str, model: &str, usage: Value) {
        tracker.update(&event("request_start", id));
        let mut observed = event("upstream_model", id);
        observed["model"] = model.into();
        tracker.update(&observed);
        let mut observed = event("upstream_usage", id);
        observed["usage"] = usage;
        tracker.update(&observed);
        tracker.update(&event("request_complete", id));
    }
    fn usage() -> Value {
        json!({"input_tokens":1000000,"output_tokens":1000000})
    }
    fn outcome(extra: Value) -> Value {
        let mut value = json!({"event":"outcome","request_id":"saved","status":"completed","confirmed_model":HAIKU,
            "completion_confirmed":true,"baseline_model":DEFAULT_BASELINE,"pricing_version":PRICING_VERSION,"usage":usage()});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        value
    }

    #[test]
    fn exact_rates_cache_ttls_and_negative_savings_are_retained() {
        let mut tracker = SavingsTracker::default();
        complete(&mut tracker, "one", HAIKU, usage());
        let snapshot = tracker.snapshot();
        let row = &snapshot["session-a"];
        assert_eq!(row["actual_usd"], 6.0);
        assert_eq!(row["baseline_usd"], 24.0);
        assert_eq!(row["saved_usd"], 18.0);
        assert_eq!(row["percent"], 75.0);
        assert!(!snapshot.to_string().contains("input_tokens"));
        let mut tracker = SavingsTracker::default();
        complete(
            &mut tracker,
            "cache",
            HAIKU,
            json!({"input_tokens":100,"output_tokens":200,"cache_read_input_tokens":300,
            "cache_creation_input_tokens":1500,"cache_creation":{"ephemeral_5m_input_tokens":500,"ephemeral_1h_input_tokens":1000}}),
        );
        assert_eq!(tracker.snapshot()["session-a"]["actual_usd"], 0.003755);
        assert_eq!(tracker.snapshot()["session-a"]["baseline_usd"], 0.01496);
        assert_eq!(tracker.snapshot()["session-a"]["saved_usd"], 0.011205);
        let mut tracker = SavingsTracker::default();
        complete(&mut tracker, "opus5", "claude-opus-5", usage());
        assert_eq!(tracker.snapshot()["session-a"]["saved_usd"], -6.0);
        assert_eq!(tracker.snapshot()["session-a"]["percent"], -25.0);
        let mut tracker = SavingsTracker::new("claude-opus-5");
        complete(&mut tracker, "override", HAIKU, usage());
        assert_eq!(tracker.snapshot()["session-a"]["saved_usd"], 24.0);
    }

    #[test]
    fn malformed_usage_and_unsupported_modifiers_never_gain_a_guessed_price() {
        for tokens in [
            Value::Null,
            json!({}),
            json!({"input_tokens":1}),
            json!({"output_tokens":1}),
            json!({"input_tokens":-1,"output_tokens":1}),
            json!({"input_tokens":1,"output_tokens":1.5}),
            json!({"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":100}),
            json!({"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":100,"cache_creation":{"ephemeral_5m_input_tokens":99}}),
            json!({"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":null}),
            json!({"input_tokens":1,"output_tokens":1,"cache_creation":{"ephemeral_5m_input_tokens":null}}),
        ] {
            let mut tracker = SavingsTracker::default();
            complete(&mut tracker, "invalid", HAIKU, tokens);
            assert_eq!(tracker.snapshot()["session-a"]["unpriced_requests"], 1);
        }
        for modifiers in [
            json!({"speed":"fast"}),
            json!({"speed":null}),
            json!({"inference_geo":"us"}),
            json!({"service_tier":"priority"}),
            json!({"service_tier":"batch"}),
            json!({"pricing_unsupported":true}),
        ] {
            let mut tokens = usage();
            tokens
                .as_object_mut()
                .unwrap()
                .extend(modifiers.as_object().unwrap().clone());
            let mut tracker = SavingsTracker::default();
            complete(&mut tracker, "modifier", HAIKU, tokens);
            assert_eq!(
                tracker.snapshot()["session-a"]["unpriced_reasons"]["unsupported_pricing"],
                1
            );
        }
    }

    #[test]
    fn geography_exception_belongs_only_to_confirmed_haiku_usage() {
        for model in [
            HAIKU,
            "claude-haiku-4-5",
            "claude-sonnet-5",
            DEFAULT_BASELINE,
            "claude-opus-5",
            "unknown",
        ] {
            let mut tokens = usage();
            tokens["inference_geo"] = "not_available".into();
            let mut tracker = SavingsTracker::default();
            complete(&mut tracker, "geo", model, tokens);
            assert_eq!(
                tracker.snapshot()["session-a"]["requests"],
                if model == HAIKU || model == "claude-haiku-4-5" {
                    1
                } else {
                    0
                }
            );
        }
    }

    #[test]
    fn overlapping_calls_and_duplicate_notifications_have_independent_settlement() {
        let mut tracker = SavingsTracker::default();
        for (id, model) in [
            ("main", HAIKU),
            ("agent", "claude-sonnet-5"),
            ("aux", DEFAULT_BASELINE),
        ] {
            tracker.update(&event("request_start", id));
            let mut model_event = event("upstream_model", id);
            model_event["model"] = model.into();
            tracker.update(&model_event);
            let mut usage_event = event("upstream_usage", id);
            usage_event["usage"] = usage();
            tracker.update(&usage_event);
        }
        for id in ["agent", "aux", "main", "main"] {
            tracker.update(&event("request_complete", id));
        }
        complete(&mut tracker, "main", HAIKU, usage());
        assert_eq!(tracker.snapshot()["session-a"]["requests"], 3);
        assert_eq!(tracker.snapshot()["session-a"]["actual_usd"], 42.0);
        let mut detached = tracker.snapshot();
        detached["session-a"]["actual_usd"] = 999.into();
        assert_eq!(tracker.snapshot()["session-a"]["actual_usd"], 42.0);
    }

    #[test]
    fn errors_cancellation_and_unconfirmed_completion_never_count_as_savings() {
        for (name, reason) in [
            ("request_error", "request_failed"),
            ("request_cancelled", "request_cancelled"),
            ("upstream_error", "request_failed"),
            ("request_complete", "unconfirmed_completion"),
        ] {
            let mut tracker = SavingsTracker::default();
            tracker.update(&event("request_start", "r"));
            let mut model = event("upstream_model", "r");
            model["model"] = HAIKU.into();
            tracker.update(&model);
            let mut tokens = event("upstream_usage", "r");
            tokens["usage"] = usage();
            tracker.update(&tokens);
            let mut failure = event(name, "r");
            failure["completion_confirmed"] = false.into();
            tracker.update(&failure);
            tracker.update(&event("request_complete", "r"));
            assert_eq!(
                tracker.snapshot()["session-a"]["unpriced_reasons"][reason],
                1
            );
            assert_eq!(tracker.snapshot()["session-a"]["requests"], 0);
        }
    }

    #[test]
    fn bounded_inflight_and_duplicate_history_report_partial_coverage() {
        let mut tracker = SavingsTracker::default();
        for index in 0..1001 {
            tracker.update(&event("request_start", &format!("r{index}")));
        }
        assert_eq!(
            tracker.snapshot()["session-a"]["unpriced_reasons"]["request_evicted"],
            1
        );
        complete(&mut tracker, "r0", HAIKU, usage());
        complete(&mut tracker, "r1000", HAIKU, usage());
        assert_eq!(tracker.snapshot()["session-a"]["requests"], 1);
        assert_eq!(tracker.snapshot()["session-a"]["partial"], true);
        tracker.clear();
        for index in 0..10001 {
            complete(&mut tracker, &format!("r{index}"), HAIKU, usage());
        }
        assert_eq!(tracker.snapshot()["session-a"]["actual_usd"], 60006.0);
        assert_eq!(tracker.snapshot()["session-a"]["partial"], true);
    }

    #[test]
    fn evicted_sessions_ignore_old_requests_and_mark_returning_sessions_partial() {
        let mut tracker = SavingsTracker::default();
        for index in 0..102 {
            let mut e = event("request_start", "r");
            e["session_id"] = format!("s{index}").into();
            tracker.update(&e);
        }
        assert_eq!(tracker.snapshot().as_object().unwrap().len(), 100);
        let mut e = event("request_start", "r");
        e["session_id"] = "s0".into();
        tracker.update(&e);
        assert!(tracker.snapshot().get("s0").is_none());
        e["request_id"] = "new".into();
        tracker.update(&e);
        assert_eq!(tracker.snapshot()["s0"]["partial"], true);
    }

    #[test]
    fn historical_estimates_require_recorded_provenance_and_clean_outcomes() {
        let result = estimate_outcome_savings(&outcome(json!({})));
        assert_eq!(result["priced"], true);
        assert_eq!(result["saved_usd"], 18.0);
        for (extra, reason) in [
            (
                json!({"completion_confirmed":false}),
                "unconfirmed_completion",
            ),
            (
                json!({"pricing_version":"future"}),
                "unknown_pricing_version",
            ),
            (
                json!({"baseline_model":"claude-sonnet-5"}),
                "unknown_baseline",
            ),
            (json!({"status":"cancelled"}), "request_cancelled"),
            (json!({"status":"error"}), "request_failed"),
            (json!({"http_status":500}), "request_failed"),
            (json!({"usage_complete":false}), "incomplete_usage"),
            (
                json!({"usage":{"input_tokens":null,"output_tokens":1}}),
                "unsupported_pricing",
            ),
            (json!({"pricing_eligible":false}), "unsupported_pricing"),
            (
                json!({"model_transitions":["claude-sonnet-5","claude-sonnet-5-5"]}),
                "mixed_models",
            ),
            (
                json!({"model_transitions":[HAIKU,"private model"]}),
                "mixed_models",
            ),
            (json!({"model_transitions":vec![HAIKU;17]}), "mixed_models"),
        ] {
            assert_eq!(
                estimate_outcome_savings(&outcome(extra)),
                json!({"priced":false,"unpriced_reason":reason})
            );
        }
        for (field, reason) in [
            ("usage", "missing_usage"),
            ("confirmed_model", "missing_model"),
            ("pricing_version", "unknown_pricing_version"),
            ("baseline_model", "unknown_baseline"),
        ] {
            let mut value = outcome(json!({}));
            value.as_object_mut().unwrap().shift_remove(field);
            assert_eq!(
                estimate_outcome_savings(&value),
                json!({"priced":false,"unpriced_reason":reason})
            );
        }
    }
}
