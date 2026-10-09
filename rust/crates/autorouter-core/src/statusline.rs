//! Terminal status rendering. Field allowlists, confirmation qualifiers and
//! shrinking priority mirror the frozen CLI; raw provider errors never appear.
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;
const PHASES: &[&str] = &[
    "routing",
    "connecting",
    "streaming",
    "ready",
    "error",
    "cancelled",
];
fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
fn number(v: &Value) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite())
}
fn count(v: &Value) -> Option<f64> {
    number(v).filter(|n| *n >= 0.0 && *n <= 9_007_199_254_740_991.0 && n.fract() == 0.0)
}
fn js_number(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => {
            let s = crate::config::js_trim(s);
            if s.is_empty() {
                0.0
            } else if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                u64::from_str_radix(hex, 16)
                    .map(|v| v as f64)
                    .unwrap_or(f64::NAN)
            } else {
                s.parse().unwrap_or(f64::NAN)
            }
        }
        Value::Array(values) if values.is_empty() => 0.0,
        Value::Array(values) if values.len() == 1 => match &values[0] {
            Value::Object(_) => f64::NAN,
            value => js_number(value),
        },
        _ => f64::NAN,
    }
}
fn numeric(n: f64) -> String {
    if n == 0.0 {
        "0".into()
    } else {
        ryu_js::Buffer::new().format(n).into()
    }
}
fn rounded(n: f64) -> String {
    numeric((n + 0.5).floor())
}
fn clean(v: &Value, limit: usize) -> String {
    static OSC: LazyLock<Regex> =
        LazyLock::new(|| Regex::new("\u{1b}\\][\\s\\S]*?(?:\u{7}|\u{1b}\\\\)").unwrap());
    static CSI: LazyLock<Regex> =
        LazyLock::new(|| Regex::new("\u{1b}\\[[0-?]*[ -/]*[@-~]").unwrap());
    let Some(value) = v.as_str() else {
        return String::new();
    };
    let value = OSC.replace_all(value, "");
    let value = CSI.replace_all(&value, "");
    let mut result = String::new();
    let mut space = false;
    for ch in value.chars() {
        if matches!(ch as u32,0..=31|127..=159|0x202a..=0x202e|0x2066..=0x2069) {
            continue;
        }
        if crate::config::js_trim(&ch.to_string()).is_empty() {
            space = !result.is_empty();
            continue;
        }
        if space {
            result.push(' ');
            space = false;
        }
        result.push(ch);
    }
    String::from_utf16_lossy(&result.encode_utf16().take(limit).collect::<Vec<_>>())
}
fn width(s: &str) -> usize {
    s.chars()
        .map(|ch| {
            if matches!(ch as u32,0..=0x10ff|0x2000..=0x2e7f) {
                1
            } else {
                2
            }
        })
        .sum()
}
fn shorten(value: &str, limit: usize) -> String {
    if width(value) <= limit {
        return value.into();
    };
    let mut result = String::new();
    let mut used = 0;
    for ch in value.chars() {
        let size = width(&ch.to_string());
        if used + size + 1 > limit {
            break;
        };
        result.push(ch);
        used += size;
    }
    result.push('…');
    result
}
fn model_name(value: &Value) -> String {
    static MODEL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^claude-(haiku|sonnet|opus)-(\d+)").unwrap());
    let name = clean(value, 48);
    let Some(m) = MODEL.captures(&name) else {
        return name;
    };
    let family = match m[1].to_ascii_lowercase().as_str() {
        "haiku" => "Haiku",
        "sonnet" => "Sonnet",
        _ => "Opus",
    };
    let mut result = format!("{family} {}", &m[2]);
    let end = m.get(0).unwrap().end();
    if let Some(rest) = name[end..].strip_prefix('-') {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if (1..=2).contains(&digits) && (digits == rest.len() || rest.as_bytes()[digits] == b'-') {
            result.push('.');
            result.push_str(&rest[..digits]);
        }
    }
    result
}
fn capacity(value: f64) -> String {
    if value % 1_000_000.0 == 0.0 {
        format!("{}M", numeric(value / 1_000_000.0))
    } else if value % 1000.0 == 0.0 {
        format!("{}K", numeric(value / 1000.0))
    } else {
        numeric(value)
    }
}
fn context_label(input: &Value, state: &Value) -> String {
    let client = &input["context_window"];
    let client_capacity = count(&client["context_window_size"]).filter(|v| *v > 0.0);
    let cli = number(&client["used_percentage"])
        .filter(|v| (0.0..=100.0).contains(v))
        .map(|n| {
            format!(
                "CLI ctx {}%{}",
                rounded(n),
                client_capacity
                    .map(|c| format!("/{}", capacity(c)))
                    .unwrap_or_default()
            )
        })
        .unwrap_or_default();
    if client.is_object() && client.get("current_usage") == Some(&Value::Null) {
        return cli;
    };
    let current = &state["context_usage"];
    let current = current.is_object()
        && current["model"] == state["actual_model"]
        && matches!(text(state, "phase"), "streaming" | "ready");
    let historical = !current
        && matches!(
            text(state, "phase"),
            "routing" | "connecting" | "streaming" | "error" | "cancelled"
        )
        && state["last_context_usage"].is_object();
    let usage = if current {
        &state["context_usage"]
    } else if historical {
        &state["last_context_usage"]
    } else {
        &Value::Null
    };
    let Some(window) = crate::model_catalog::model_context_window(text(usage, "model")) else {
        return cli;
    };
    let Some(tokens) = count(&usage["input_tokens"]) else {
        return cli;
    };
    let api = format!(
        "{}API ctx {}%/{}",
        if historical { "last " } else { "" },
        rounded(tokens * 100.0 / window as f64),
        capacity(window as f64)
    );
    if !cli.is_empty() && client_capacity != Some(window as f64) {
        format!("{api} · {cli}")
    } else {
        api
    }
}
struct Savings {
    full: String,
    compact: String,
    color: &'static str,
}
fn savings_labels(entry: &Value) -> Option<Savings> {
    if !entry.is_object() {
        return None;
    };
    let unpriced = count(&entry["unpriced_requests"]);
    let suffix = unpriced
        .filter(|n| *n > 0.0)
        .map(|n| format!(" · unpriced {}", numeric(n)))
        .unwrap_or_default();
    let unavailable = || {
        Some(Savings {
            full: format!("savings unavailable{suffix}"),
            compact: "savings unavailable".into(),
            color: "33",
        })
    };
    let (Some(requests), Some(unpriced)) = (count(&entry["requests"]), unpriced) else {
        return unavailable();
    };
    if requests == 0.0 {
        return if unpriced > 0.0 { unavailable() } else { None };
    };
    let Some(baseline) = number(&entry["baseline_usd"]).filter(|v| *v >= 0.0) else {
        return unavailable();
    };
    if number(&entry["actual_usd"]).is_none_or(|v| v < 0.0) {
        return unavailable();
    };
    let Some(saved) = number(&entry["saved_usd"]) else {
        return unavailable();
    };
    let extra = saved < 0.0;
    let amount = saved.abs();
    let money = if amount > 0.0 && amount < 0.01 {
        "<$0.01".into()
    } else {
        format!("${}", ryu_js::Buffer::new().format_to_fixed(amount, 2))
    };
    let percent = if baseline > 0.0 {
        number(&entry["percent"])
            .map(|n| format!("{}%", rounded(n.abs())))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let partial = entry["partial"] == true || unpriced > 0.0;
    let label = if extra { "est extra" } else { "est saved" };
    let ending = if partial {
        " vs Opus partial"
    } else {
        " vs Opus"
    };
    Some(Savings {
        full: format!(
            "{label} {money}{}{ending}{suffix}",
            if percent.is_empty() {
                String::new()
            } else {
                format!(" ({percent})")
            }
        ),
        compact: format!(
            "{label} {}{ending}",
            if percent.is_empty() { &money } else { &percent }
        ),
        color: if extra || partial { "33" } else { "32" },
    })
}
fn reason(value: &str) -> &'static str {
    match value {
        "tool_turn_pinned" => "turn pinned",
        "prompt_turn_pinned" => "prompt pinned",
        "goal_turn_pinned" => "goal pinned",
        "thinking_history" => "thinking pinned",
        "unknown_continuation" => "continuity unknown",
        "mid_conversation_system" => "system features",
        "requires_sonnet_capabilities" => "capability guard",
        "model_specific_features" => "model features",
        "model_incompatible" => "model guard",
        "large_or_multimodal_request" => "large request",
        "context_capacity" => "large context",
        "internal_request" => "internal request",
        "unknown_model" => "custom model",
        "low_confidence" => "low confidence",
        "auto_mode_floor" => "Auto floor from Haiku",
        "auto_mode_safeguards" => "Auto safety",
        "auto_mode_incompatible" => "Auto model guard",
        _ => "",
    }
}
fn compatibility(value: &str) -> &'static str {
    match value {
        "unknown_model" => "unknown model",
        "invalid_request_shape" => "request shape",
        "auto_model" => "Auto support",
        "request_extension" => "request extension",
        "safeguards" => "safety review",
        "output_limit" => "output limit",
        "speed" => "speed",
        "execution_facility" => "execution features",
        "tool_type" => "tool type",
        "system_message" => "system messages",
        "message_effort" => "message effort",
        "inline_tool" => "inline tools",
        "content_extension" => "content extension",
        "assistant_prefill" => "prefill",
        "tool_choice" | "forced_tool_choice" => "tool choice",
        "context_management" => "context edits",
        "thinking_mode" => "thinking mode",
        "thinking_extension" => "thinking fields",
        "thinking_effort" => "thinking effort",
        "output_extension" => "output fields",
        "effort" => "effort",
        "task_budget" => "task budget",
        "sampling" => "sampling",
        _ => "",
    }
}
#[derive(Default)]
struct Parts {
    brand: String,
    model: String,
    prefix: String,
    suffix: String,
    status: String,
    detail: String,
    saving: String,
    context: String,
}
impl Parts {
    fn model_label(&self) -> String {
        if self.model.is_empty() {
            String::new()
        } else {
            format!("{}{}{}", self.prefix, self.model, self.suffix)
        }
    }
    fn width(&self) -> usize {
        width(
            &[
                self.brand.as_str(),
                self.model_label().as_str(),
                self.status.as_str(),
                self.detail.as_str(),
                self.saving.as_str(),
                self.context.as_str(),
            ]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · "),
        )
    }
}
fn paint(s: &str, code: &str, color: bool) -> String {
    if color {
        format!("\u{1b}[{code}m{s}\u{1b}[0m")
    } else {
        s.into()
    }
}
pub fn render_status_line(input: &Value, snapshot: &Value, options: &Value) -> String {
    let color = options
        .get("color")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let now = options
        .get("now")
        .and_then(Value::as_f64)
        .unwrap_or(100000.0);
    let columns = options.get("columns").map(js_number).unwrap_or(f64::NAN);
    let available = if columns.is_finite() && columns > 0.0 {
        columns.floor().max(1.0) as usize
    } else {
        120
    };
    let online = snapshot.is_object()
        && snapshot["version"].as_f64() == Some(1.0)
        && count(&snapshot["pid"]).is_some_and(|p| p > 0.0)
        && number(&snapshot["heartbeat_at"])
            .is_some_and(|h| h > 0.0 && h <= now + 5000.0 && now - h <= 20000.0)
        && options.get("alive") != Some(&Value::Bool(false));
    if !online {
        if width("● AutoRouter offline") > available {
            return paint(&shorten("AutoRouter offline", available), "2;31", color);
        };
        return format!(
            "{} {} {}",
            paint("●", "31", color),
            paint("AutoRouter", "1", color),
            paint("offline", "2;31", color)
        );
    }
    let session = if input.is_object() {
        match input.get("session_id") {
            None => Some(""),
            Some(Value::String(s)) => Some(s.as_str()),
            _ => None,
        }
    } else {
        None
    };
    let candidate = session
        .and_then(|s| snapshot["sessions"].as_object()?.get(s))
        .unwrap_or(&Value::Null);
    let savings = session
        .and_then(|s| snapshot["savings"].as_object()?.get(s))
        .and_then(savings_labels);
    let state = if candidate.is_object() && PHASES.contains(&text(candidate, "phase")) {
        candidate
    } else {
        &Value::Null
    };
    let phase = text(state, "phase");
    let completion_unknown = phase == "ready" && state["completion_confirmed"] == false;
    let mut p = Parts {
        status: if completion_unknown {
            "completion unknown".into()
        } else if phase.is_empty() {
            "awaiting request".into()
        } else {
            phase.into()
        },
        brand: "● AutoRouter".into(),
        ..Parts::default()
    };
    let last = model_name(&state["last_model"]);
    let actual = model_name(&state["actual_model"]);
    let selected = model_name(&state["selected_model"]);
    let mut confirmed = false;
    if phase == "streaming" && !actual.is_empty() {
        p.model = actual;
        confirmed = true;
    } else if phase == "connecting" && !selected.is_empty() {
        p.model = selected;
        p.suffix = " selected".into();
    } else if phase == "streaming" && !selected.is_empty() {
        p.model = selected;
        p.suffix = " unconfirmed".into();
    } else if phase == "ready" && !actual.is_empty() {
        p.model = actual;
        p.prefix = "last ".into();
        confirmed = true;
    } else if phase == "ready" && !selected.is_empty() {
        p.model = selected;
        p.suffix = " unconfirmed".into();
    } else if matches!(phase, "error" | "cancelled") && !actual.is_empty() {
        p.model = actual;
        p.prefix = "last ".into();
        confirmed = true;
    } else if matches!(phase, "error" | "cancelled") && !selected.is_empty() {
        p.model = selected;
        p.suffix = " selected".into();
    } else if !last.is_empty() {
        p.model = last;
        p.prefix = "last ".into();
        confirmed = true;
    }
    if phase == "ready" && p.model.is_empty() && !completion_unknown {
        p.status = "awaiting request".into();
    }
    let error_type = clean(&state["error_type"], 32);
    if phase == "error" {
        p.status =
            if let Some(status) = count(&state["status"]).filter(|n| (400.0..=599.0).contains(n)) {
                format!("error {}", numeric(status))
            } else if !error_type.is_empty() {
                format!("error {error_type}")
            } else {
                "error".into()
            };
    }
    let source =
        if ["jev", "ollama", "cache", "fallback", "passthrough"].contains(&text(state, "source")) {
            text(state, "source")
        } else {
            ""
        };
    let evaluator = match text(state, "evaluator") {
        "ollama" => "Ollama",
        "jev" => "Jev",
        _ => "",
    };
    let mut details = Vec::new();
    if !source.is_empty() {
        let label = match source {
            "passthrough" => "pass-through".into(),
            "jev" => "Jev".into(),
            "ollama" => "Ollama".into(),
            _ if !evaluator.is_empty() => format!("{evaluator} {source}"),
            _ => source.to_owned(),
        };
        let evaluation = state
            .get("evaluation_latency_ms")
            .filter(|v| !v.is_null())
            .unwrap_or(&state["latency_ms"]);
        let timing = number(evaluation)
            .filter(|n| *n >= 0.0)
            .map(|n| format!(" {}ms", rounded(n.min(999999.0))))
            .unwrap_or_default();
        let classified = text(state, "classified_tier");
        let chosen = text(state, "selected_model")
            .strip_prefix("claude-")
            .and_then(|s| s.split_once('-'))
            .map(|v| v.0)
            .unwrap_or("");
        let over = if source != "fallback"
            && ["haiku", "sonnet", "opus"].contains(&classified)
            && ["haiku", "sonnet", "opus"].contains(&chosen)
            && classified != chosen
        {
            format!(
                "→{}{}",
                classified[..1].to_ascii_uppercase(),
                &classified[1..]
            )
        } else {
            String::new()
        };
        details.push(format!("{label}{over}{timing}"));
        if let Some(latency) = number(&state["routing_latency_ms"])
            .filter(|n| *n >= 0.0 && state.get("evaluation_latency_ms").is_some())
        {
            details.push(format!("route {}ms", rounded(latency.min(999999.0))));
        }
    }
    let mut cause = String::new();
    if source == "fallback" {
        cause = match text(state, "classifier_error") {
            "timeout" => "timeout",
            "http_error" => "HTTP error",
            "invalid_response" => "invalid response",
            "network_error" => "network error",
            "capacity_exhausted" => "evaluator busy",
            _ => "",
        }
        .into();
        if text(state, "classifier_error") == "http_error"
            && let Some(status) =
                count(&state["classifier_status"]).filter(|n| (100.0..=599.0).contains(n))
        {
            cause = format!("HTTP {}", numeric(status));
        }
        if !cause.is_empty() {
            details.push(cause.clone());
        }
    }
    let mut guard = if text(state, "reason") == "context_capacity"
        && state["context_check"] == "count_unavailable"
    {
        "size unverified"
    } else {
        reason(text(state, "reason"))
    }
    .to_owned();
    if state["continuity_state"] == "unknown" {
        guard = "continuity unknown".into();
    } else if state["continuity_state"] == "capacity_exhausted" {
        guard = "continuity capacity full".into();
    } else if matches!(
        text(state, "reason"),
        "model_incompatible" | "auto_mode_incompatible"
    ) {
        let suffix = compatibility(text(state, "compatibility_reason"));
        if !suffix.is_empty() {
            guard = format!("{guard}: {suffix}");
        }
    }
    let short_guard = if state["continuity_state"] == "capacity_exhausted" {
        "state full"
    } else if guard == "Auto floor from Haiku" {
        "Auto floor"
    } else if guard.contains(": ") {
        reason(text(state, "reason"))
    } else {
        &guard
    }
    .to_owned();
    if !guard.is_empty() {
        details.push(guard.clone());
    }
    if phase == "error" && !error_type.is_empty() && !p.status.contains(&error_type) {
        details.push(error_type);
    }
    p.context = context_label(input, state);
    p.detail = details.join(" · ");
    p.saving = savings.as_ref().map(|s| s.full.clone()).unwrap_or_default();
    let mut compact_fallback = false;
    let mut compact_guard = false;
    let mut fallback_phase = String::new();
    if p.width() > available {
        p.context.clear();
    }
    if p.width() > available && source != "fallback" && phase != "error" {
        p.detail = guard.clone();
    }
    if p.width() > available && !p.saving.is_empty() {
        p.saving = savings.as_ref().unwrap().compact.clone();
    }
    if p.width() > available {
        p.saving.clear();
    }
    if p.width() > available && source == "fallback" {
        compact_fallback = true;
        if completion_unknown || matches!(phase, "error" | "cancelled") {
            fallback_phase = p.status.clone();
        }
        let fallback = format!(
            "{}fallback{}",
            if evaluator.is_empty() {
                String::new()
            } else {
                format!("{evaluator} ")
            },
            if cause.is_empty() {
                String::new()
            } else {
                format!(": {cause}")
            }
        );
        p.status = if fallback_phase.is_empty() {
            fallback
        } else {
            format!("{fallback_phase} · {fallback}")
        };
        p.detail.clear();
    }
    if p.width() > available && source != "fallback" && !guard.is_empty() {
        compact_guard = true;
        p.status = if completion_unknown || matches!(phase, "error" | "cancelled") {
            format!("{} · {short_guard}", p.status)
        } else {
            short_guard
        };
        p.detail.clear();
    }
    if p.width() > available {
        p.detail.clear();
    }
    if p.width() > available {
        p.brand = "● AR".into();
    }
    if p.width() > available {
        p.brand.clear();
    }
    if p.width() > available && completion_unknown {
        p.status = "completion unknown".into();
        p.detail.clear();
        compact_fallback = false;
    }
    if p.width() > available && compact_fallback {
        let fallback = format!(
            "fallback{}",
            if cause.is_empty() {
                String::new()
            } else {
                format!(": {cause}")
            }
        );
        p.status = if fallback_phase.is_empty() {
            fallback
        } else {
            format!("{fallback_phase} · {fallback}")
        };
    }
    if p.width() > available && !p.model.is_empty() {
        if phase == "streaming" && !compact_fallback && !compact_guard {
            p.status = "stream".into();
        }
        let used = width(&format!("{}{}{}", p.prefix, p.suffix, p.status)) + 3;
        if available > used {
            p.model = shorten(&p.model, available - used);
        } else {
            p.model.clear();
            p.prefix.clear();
            p.suffix.clear();
            if width(&format!("● AutoRouter · {}", p.status)) <= available {
                p.brand = "● AutoRouter".into();
            } else if width(&format!("● AR · {}", p.status)) <= available {
                p.brand = "● AR".into();
            }
        }
    }
    if p.width() > available
        && compact_fallback
        && fallback_phase.is_empty()
        && available >= width("fallback")
        && available < width("fallback: ") + 2
    {
        p.status = "fallback".into();
    }
    if p.width() > available {
        p.status = shorten(
            &p.status,
            available
                .saturating_sub(width(&p.model_label()) + if p.model.is_empty() { 0 } else { 3 })
                .max(1),
        );
    }
    let attention = if phase == "error" {
        "31"
    } else if completion_unknown || source == "fallback" {
        "33"
    } else if phase == "cancelled" {
        "2"
    } else if matches!(phase, "streaming" | "ready") {
        "32"
    } else {
        "36"
    };
    let mut chunks = Vec::new();
    if !p.brand.is_empty() {
        chunks.push(format!(
            "{} {}",
            paint("●", attention, color),
            paint(p.brand.strip_prefix("● ").unwrap_or(&p.brand), "1", color)
        ));
    }
    if !p.model.is_empty() {
        chunks.push(paint(
            &p.model_label(),
            if confirmed { "37" } else { "33" },
            color,
        ));
    }
    chunks.push(paint(&p.status, attention, color));
    if !p.detail.is_empty() {
        chunks.push(paint(
            &p.detail,
            if source == "fallback" { "33" } else { "2" },
            color,
        ));
    }
    if !p.saving.is_empty() {
        chunks.push(paint(&p.saving, savings.as_ref().unwrap().color, color));
    }
    if !p.context.is_empty() {
        chunks.push(paint(&p.context, "2", color));
    }
    chunks.join(&paint(" · ", "2", color))
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn selection_and_incomplete_completion_are_not_reported_as_confirmed() {
        let mut snapshot = json!({"version":1,"pid":1,"heartbeat_at":100000,"sessions":{"s":{"phase":"ready","selected_model":"claude-sonnet-4-6","completion_confirmed":false}}});
        let line = render_status_line(
            &json!({"session_id":"s"}),
            &snapshot,
            &json!({"now":100000,"color":false,"columns":120}),
        );
        assert!(line.contains("unconfirmed"));
        assert!(line.contains("completion unknown"));
        snapshot["sessions"]["s"]["phase"] = json!("error");
        snapshot["sessions"]["s"]["error_type"] =
            json!("\u{1b}]0;synthetic secret\u{7}bad\u{1b}[31m");
        let line = render_status_line(
            &json!({"session_id":"s"}),
            &snapshot,
            &json!({"now":100000,"color":false,"columns":120}),
        );
        assert!(!line.contains("secret"));
        assert!(!line.contains('\u{1b}'));
    }
    #[test]
    fn narrow_fallback_keeps_failure_cause() {
        let snapshot = json!({"version":1,"pid":1,"heartbeat_at":100000,"sessions":{"s":{"phase":"ready","actual_model":"claude-opus-5-5","source":"fallback","evaluator":"ollama","classifier_error":"timeout"}}});
        let line = render_status_line(
            &json!({"session_id":"s"}),
            &snapshot,
            &json!({"now":100000,"color":false,"columns":40}),
        );
        assert!(line.contains("fallback"));
        assert!(line.contains("timeout"));
        assert!(width(&line) <= 40);
    }
}
