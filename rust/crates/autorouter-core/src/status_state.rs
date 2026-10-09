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
