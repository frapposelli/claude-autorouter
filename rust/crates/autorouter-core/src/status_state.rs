//! Bounded foreground status and independent savings attribution. Persistence
//! belongs to the runtime; this state machine never retains request bodies.
use crate::{savings::SavingsTracker, telemetry_event::normalize_telemetry_event};
use serde_json::{Map, Value, json};

const TIMINGS: &[&str] = &[
    "evaluation_latency_ms",
    "routing_latency_ms",
    "decision_latency_ms",
    "latency_ms",
    "first_response_ms",
    "total_latency_ms",
];
#[derive(Default)]
pub struct StatusState {
    sessions: Vec<(String, Value)>,
    savings: SavingsTracker,
}
fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
fn copy(state: &mut Value, event: &Value, key: &str) {
    if let Some(value) = event.get(key) {
        state[key] = value.clone();
    }
}
fn context_usage(usage: &Value, model: &Value) -> Option<Value> {
    if model.as_str().is_none_or(str::is_empty)
        || !usage.is_object()
        || usage["pricing_unsupported"] == true
    {
        return None;
    }
    let mut total = 0.0;
    for key in [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ] {
        let value = match usage.get(key) {
            Some(v) => v.as_f64()?,
            None if key != "input_tokens" => 0.0,
            None => return None,
        };
        if !value.is_finite()
            || value < 0.0
            || value.fract() != 0.0
            || value > 9_007_199_254_740_991.0
        {
            return None;
        }
        total += value;
    }
    if total > 9_007_199_254_740_991.0 {
        return None;
    }
    Some(json!({"model": model, "input_tokens": total}))
}
impl StatusState {
    pub fn new(baseline: &str) -> Self {
        Self {
            sessions: Vec::new(),
            savings: SavingsTracker::new(baseline),
        }
    }
    pub fn update(&mut self, event: &Value, now_ms: u64) {
        let Some(event) = normalize_telemetry_event(event, "2026-10-09T12:00:00.000Z") else {
            return;
        };
        self.savings.update(&event);
        if !text(&event, "agent_id").is_empty()
            || !matches!(text(&event, "request_class"), "" | "main")
        {
            return;
        }
        let session = text(&event, "session_id").to_owned();
        let index = self.sessions.iter().position(|(key, _)| key == &session);
        if text(&event, "event") == "request_start" {
            let mut state = json!({"phase":"routing", "updated_at":now_ms});
            copy(&mut state, &event, "request_id");
            if let Some(index) = index {
                let previous = self.sessions.remove(index).1;
                if !text(&previous, "last_model").is_empty() {
                    copy(&mut state, &previous, "last_model");
                }
                if let Some(usage) = previous
                    .get("context_usage")
                    .or_else(|| previous.get("last_context_usage"))
                {
                    state["last_context_usage"] = usage.clone();
                }
            }
            for key in ["requested_model", "prompt_id"] {
                if !text(&event, key).is_empty() {
                    copy(&mut state, &event, key);
                }
            }
            self.sessions.push((session, state));
            if self.sessions.len() > 100 {
                self.sessions.remove(0);
            }
            return;
        }
        let Some(index) = index else {
            return;
        };
        let state = &mut self.sessions[index].1;
        if state.get("request_id") != event.get("request_id") {
            return;
        }
        state["updated_at"] = json!(now_ms);
        let terminal = matches!(text(state, "phase"), "error" | "cancelled" | "ready");
        for key in TIMINGS {
            copy(state, &event, key);
        }
        copy(state, &event, "completion_confirmed");
        match text(&event, "event") {
            "route" => {
                copy(state, &event, "requested_model");
                if let Some(model) = event.get("selected_model").or_else(|| event.get("model")) {
                    state["selected_model"] = model.clone();
                }
                for key in [
                    "source",
                    "evaluator",
                    "reason",
                    "compatibility_reason",
                    "continuity_state",
                    "classified_tier",
                    "context_check",
                    "counted_input_tokens",
                    "classifier_error",
                    "classifier_status",
                ] {
                    copy(state, &event, key);
                }
                if !terminal {
                    state["phase"] = json!("connecting");
                }
            }
            "upstream_response" => {
                copy(state, &event, "status");
                if event["status"].as_f64().is_some_and(|s| s >= 400.0) {
                    state.as_object_mut().unwrap().remove("context_usage");
                    state["phase"] = json!("error");
                    state["error_type"] = event
                        .get("error_type")
                        .cloned()
                        .unwrap_or(json!("http_error"));
                } else if !terminal {
                    state["phase"] = json!("streaming");
                }
            }
            "upstream_model" => {
                if let Some(model) = event
                    .get("confirmed_model")
                    .or_else(|| event.get("model"))
                    .filter(|m| m.as_str().is_some_and(|s| !s.is_empty()))
                {
                    state["actual_model"] = model.clone();
                    state["last_model"] = model.clone();
                }
                if !terminal {
                    state["phase"] = json!("streaming");
                }
            }
            "upstream_usage" => {
                if !matches!(text(state, "phase"), "error" | "cancelled")
                    && let Some(usage) = context_usage(&event["usage"], &state["actual_model"])
                {
                    state["context_usage"] = usage;
                }
            }
            "upstream_error" | "request_error" => {
                state.as_object_mut().unwrap().remove("context_usage");
                state["phase"] = json!("error");
                state["error_type"] = event.get("error_type").cloned().unwrap_or_else(|| {
                    json!(if event["event"] == "upstream_error" {
                        "unknown_error"
                    } else {
                        "request_error"
                    })
                });
                copy(state, &event, "status");
            }
            "request_complete" if !terminal => state["phase"] = json!("ready"),
            "request_cancelled" => {
                state.as_object_mut().unwrap().remove("context_usage");
                if state["phase"] != "error" {
                    state["phase"] = json!("cancelled");
                }
            }
            _ => {}
        }
    }
    pub fn snapshot(&self, pid: u32, heartbeat_ms: u64) -> Value {
        let sessions: Map<String, Value> = self.sessions.iter().cloned().collect();
        json!({"version":1,"pid":pid,"heartbeat_at":heartbeat_ms,"sessions":sessions,"savings":self.savings.snapshot()})
    }
    pub fn clear(&mut self) {
        self.sessions.clear();
        self.savings.clear();
    }
}
pub fn run_fixture(input: &Value) -> Result<Value, String> {
    let mut state = input
        .get("baseline_model")
        .map_or_else(StatusState::default, |v| {
            StatusState::new(v.as_str().unwrap_or("unknown"))
        });
    let mut now = input.get("now").and_then(Value::as_u64).unwrap_or(100000);
    let pid = input.get("pid").and_then(Value::as_u64).unwrap_or(123) as u32;
    let mut results = Vec::new();
    for step in input["steps"].as_array().ok_or("Invalid fixture input")? {
        match text(step, "op") {
            "now" => now = step["value"].as_u64().ok_or("Invalid fixture input")?,
            "event" => state.update(&step["event"], now),
            "snapshot" => results.push(state.snapshot(pid, now)),
            _ => return Err("unknown status step".into()),
        }
    }
    Ok(json!(results))
}
#[cfg(test)]
mod tests {
    use super::*;

    fn equal_js_json(left: &Value, right: &Value) -> bool {
        match (left, right) {
            (Value::Number(a), Value::Number(b)) if a != b => {
                if !a.is_f64() && !b.is_f64() {
                    return false;
                }
                let (Some(a_float), Some(b_float)) = (a.as_f64(), b.as_f64()) else {
                    return false;
                };
                if a_float != b_float {
                    return false;
                }
                if a.is_f64() && b.is_f64() {
                    return true;
                }
                let integer = if a.is_f64() { b } else { a };
                if let Some(value) = integer.as_u64() {
                    (0.0..18_446_744_073_709_551_616.0).contains(&a_float)
                        && a_float as u64 == value
                } else if let Some(value) = integer.as_i64() {
                    (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&a_float)
                        && a_float as i64 == value
                } else {
                    false
                }
            }
            (Value::Array(a), Value::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal_js_json(a, b))
            }
            (Value::Object(a), Value::Object(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .all(|(key, a)| b.get(key).is_some_and(|b| equal_js_json(a, b)))
            }
            _ => left == right,
        }
    }

    #[test]
    fn snapshot_comparison_rejects_lost_fields_types_and_adjacent_numbers() {
        assert!(equal_js_json(&json!({"x": 14.0}), &json!({"x": 14})));
        assert!(!equal_js_json(&json!({"x": null}), &json!({})));
        assert!(!equal_js_json(&json!({"x": 14}), &json!({"x": "14"})));
        assert!(!equal_js_json(&json!(0.0105), &json!(0.010500000000000002)));
        assert!(!equal_js_json(
            &json!(9_007_199_254_740_993_u64),
            &json!(9_007_199_254_740_992_f64)
        ));
    }

    #[test]
    fn frozen_status_schedules_match_complete_saved_snapshots_and_rendered_strings() {
        let corpus = include_str!("../../../parity/cases/status-state-contracts.jsonl");
        assert_eq!(
            format!(
                "{:x}",
                <sha2::Sha256 as sha2::Digest>::digest(corpus.as_bytes())
            ),
            "2decdad48ee550646a91fea40b87ec9fad38de2c9150f4c3416ebc89e7ee1bf5"
        );
        let mut snapshots = 0;
        let mut renders = 0;
        for line in corpus.lines() {
            let row: Value = serde_json::from_str(line).unwrap();
            let input = &row["input"];
            let actual = if row["op"] == "render_statusline" {
                renders += 1;
                json!(crate::statusline::render_status_line(
                    &input["input"],
                    &input["snapshot"],
                    &input["options"]
                ))
            } else {
                snapshots += 1;
                let mut state = input
                    .get("baseline_model")
                    .map_or_else(StatusState::default, |baseline| {
                        StatusState::new(baseline.as_str().unwrap())
                    });
                for event in input["events"].as_array().unwrap() {
                    let document = crate::js_json::JsDocument::parse(
                        event["json"].as_str().unwrap().as_bytes(),
                    )
                    .unwrap();
                    // Preserve invalid non-finite metadata as a rejected-value
                    // sentinel; ordinary serde parsing cannot represent it.
                    let projected = crate::telemetry_event::normalization_projection(&document);
                    let before = projected.clone();
                    state.update(&projected, event["now"].as_u64().unwrap());
                    assert_eq!(projected, before, "{} changed an event", row["id"]);
                }
                state.snapshot(
                    input["pid"].as_u64().unwrap().try_into().unwrap(),
                    input["heartbeat_at"].as_u64().unwrap(),
                )
            };
            assert!(
                equal_js_json(&actual, &row["node_expected"]),
                "{}: actual={actual:?} expected={:?}",
                row["id"],
                row["node_expected"]
            );
        }
        assert_eq!((snapshots, renders), (45, 4));
    }

    #[test]
    fn evaluator_identity_and_private_fields_stay_separate_across_local_cache_and_fallback() {
        let mut state = StatusState::default();
        let event =
            |name: &str| json!({"event":name,"request_id":"request-1","session_id":"session-a"});
        for source in ["ollama", "cache", "fallback"] {
            state.update(&event("request_start"), 1);
            let mut route = event("route");
            route["model"] = json!("claude-sonnet-5");
            route["source"] = json!(source);
            route["evaluator"] = json!("ollama");
            route["classified_tier"] = json!("haiku");
            for key in ["evaluator_model", "evaluator_url", "evaluator_prompt"] {
                route[key] = json!("PRIVATE synthetic metadata");
            }
            if source == "fallback" {
                route["classifier_error"] = json!("timeout");
            }
            state.update(&route, 2);
            let snapshot = state.snapshot(123, 3);
            let current = &snapshot["sessions"]["session-a"];
            assert_eq!(current["source"], source);
            assert_eq!(current["evaluator"], "ollama");
            assert_eq!(current["classified_tier"], "haiku");
            assert_eq!(current["phase"], "connecting");
            assert!(!snapshot.to_string().contains("PRIVATE"));
        }
        state.update(
            &json!({"event":"request_start","request_id":"request-2","session_id":"session-a"}),
            4,
        );
        for (request, agent, evaluator, source) in [
            ("request-1", "", "ollama", "ollama"),
            ("request-2", "agent-1", "ollama", "ollama"),
            ("request-2", "", "PRIVATE evaluator", "PRIVATE source"),
        ] {
            state.update(&json!({"event":"route","request_id":request,"session_id":"session-a","agent_id":agent,"evaluator":evaluator,"source":source}), 5);
        }
        let snapshot = state.snapshot(123, 6);
        let current = &snapshot["sessions"]["session-a"];
        assert!(current.get("evaluator").is_none());
        assert!(current.get("source").is_none());
        state.update(&json!({"event":"route","request_id":"request-2","session_id":"session-a","evaluator":"jev","source":"jev"}), 7);
        assert_eq!(
            state.snapshot(123, 8)["sessions"]["session-a"]["evaluator"],
            "jev"
        );
    }

    #[test]
    fn session_eviction_and_malformed_metadata_never_expose_private_extras() {
        let mut state = StatusState::default();
        for index in 0..102 {
            state.update(&json!({"event":"request_start","request_id":"r","session_id":format!("session-{index}")}), 1);
        }
        let document = crate::js_json::JsDocument::parse(format!(r#"{{"event":"route","request_id":"r","session_id":"session-101","model":"{}","requested_model":"\u001b[31mred","source":"SECRET prompt body Bearer key","reason":"SECRET prompt body Bearer key","classifier_error":"SECRET prompt body Bearer key","classifier_status":-1,"latency_ms":1e400,"body":"SECRET prompt body Bearer key","headers":{{"authorization":"SECRET prompt body Bearer key"}},"error":"SECRET prompt body Bearer key","message":"SECRET prompt body Bearer key"}}"#, "x".repeat(1000)).as_bytes()).unwrap();
        state.update(
            &crate::telemetry_event::normalization_projection(&document),
            2,
        );
        for error in [
            "SECRET prompt body Bearer key",
            "token_secret_without_spaces",
        ] {
            state.update(&json!({"event":"upstream_error","request_id":"r","session_id":"session-101","error_type":error,"error":"SECRET prompt body Bearer key"}), 3);
            let snapshot = state.snapshot(123, 4);
            let sessions = snapshot["sessions"].as_object().unwrap();
            assert_eq!(sessions.len(), 100);
            assert!(!sessions.contains_key("session-0"));
            assert!(!sessions.contains_key("session-1"));
            assert!(sessions["session-101"].get("selected_model").is_none());
            assert!(sessions["session-101"].get("requested_model").is_none());
            assert_eq!(sessions["session-101"]["error_type"], "unknown_error");
            assert!(!snapshot.to_string().contains("SECRET"));
            assert!(!snapshot.to_string().contains("token_secret_without_spaces"));
        }
    }
    #[test]
    fn superseded_and_agent_requests_cannot_replace_foreground() {
        let mut state = StatusState::default();
        for event in [
            json!({"event":"request_start","session_id":"s","request_id":"old"}),
            json!({"event":"request_start","session_id":"s","request_id":"new"}),
            json!({"event":"upstream_model","session_id":"s","request_id":"old","model":"claude-opus-5-5"}),
            json!({"event":"request_start","session_id":"s","request_id":"agent","agent_id":"worker"}),
        ] {
            state.update(&event, 1);
        }
        let snapshot = state.snapshot(123, 2);
        assert_eq!(snapshot["sessions"]["s"]["request_id"], "new");
        assert!(snapshot["sessions"]["s"].get("actual_model").is_none());
    }
    #[test]
    fn failed_context_cannot_be_restored_by_late_usage_and_sessions_are_bounded() {
        let mut state = StatusState::default();
        for i in 0..101 {
            state.update(
                &json!({"event":"request_start","request_id":"r","session_id":format!("s{i}")}),
                0,
            );
        }
        assert_eq!(
            state.snapshot(1, 1)["sessions"].as_object().unwrap().len(),
            100
        );
        assert!(state.snapshot(1, 1)["sessions"].get("s0").is_none());
        for event in [
            json!({"event":"upstream_model","model":"claude-sonnet-4-6"}),
            json!({"event":"request_cancelled"}),
            json!({"event":"upstream_usage","usage":{"input_tokens":100}}),
        ] {
            let mut event = event;
            event["session_id"] = json!("s100");
            event["request_id"] = json!("r");
            state.update(&event, 2);
        }
        assert!(
            state.snapshot(1, 3)["sessions"]["s100"]
                .get("context_usage")
                .is_none()
        );
    }
}
