//! Require the actual original producer's bounded public report shapes.
use super::keys;
use serde_json::Value;

pub(super) const CASE_FIELDS: &[&str] = &[
    "case",
    "checks",
    "routes",
    "request_observations",
    "upstream_models",
    "upstream_statuses",
    "proxy_errors",
    "classifier_succeeded",
    "model_changed",
    "provider_usage",
    "api_equivalent_savings",
    "status_lines",
    "passed",
    "gates",
    "result_count",
    "assistant_models",
    "tools_used",
    "permission_denials",
    "parse_errors",
    "failures",
    "tool_outcomes",
    "provider_usage_shapes",
    "successful_tools",
    "denied_tool_details",
    "client_model_usage_estimate",
    "exit_code",
    "exit_signal",
    "supervision",
    "cleanup_confirmed",
    "observed_exit_code",
    "observed_exit_signal",
    "timed_out",
    "output_limit_exceeded",
    "cancelled",
    "stdin_error",
    "duration_ms",
    "stderr_bytes",
    "gateway_cleanup",
    "fixture_removed",
];

pub(super) const PROCESS_DIAGNOSTIC_FIELDS: &[&str] = &[
    "supervision",
    "cleanup_confirmed",
    "observed_exit_code",
    "observed_exit_signal",
];
pub(super) const REFERENCE_PROCESS_DIAGNOSTIC_FIELDS: &[&str] = &[
    "reference_process_supervision",
    "reference_process_cleanup_confirmed",
    "reference_process_observed_exit_code",
    "reference_process_observed_exit_signal",
];
// Union of the pinned nix Signal debug variants, so saved reports remain
// readable across supported macOS/Linux hosts without allowing free text.
const EXIT_SIGNALS: &[&str] = &[
    "SIGHUP",
    "SIGINT",
    "SIGQUIT",
    "SIGILL",
    "SIGTRAP",
    "SIGABRT",
    "SIGBUS",
    "SIGFPE",
    "SIGKILL",
    "SIGUSR1",
    "SIGSEGV",
    "SIGUSR2",
    "SIGPIPE",
    "SIGALRM",
    "SIGTERM",
    "SIGSTKFLT",
    "SIGCHLD",
    "SIGCONT",
    "SIGSTOP",
    "SIGTSTP",
    "SIGTTIN",
    "SIGTTOU",
    "SIGURG",
    "SIGXCPU",
    "SIGXFSZ",
    "SIGVTALRM",
    "SIGPROF",
    "SIGWINCH",
    "SIGIO",
    "SIGPWR",
    "SIGSYS",
    "SIGEMT",
    "SIGINFO",
];
fn signal_observation(v: &Value) -> bool {
    keys(v, &["target", "signal", "outcome", "errno", "kind"])
        && v.as_object().is_some_and(|o| o.len() == 5)
        && enums(&v["target"], &["group", "child"])
        && enums(&v["signal"], &["SIGTERM", "SIGKILL"])
        && (v["target"] != "child" || v["signal"] == "SIGKILL")
        && enums(&v["outcome"], &["absent", "error"])
        && (v["errno"].is_null()
            || v["errno"]
                .as_i64()
                .is_some_and(|n| i32::try_from(n).is_ok()))
        && nullable_enum(&v["kind"], &["direct_kill_error"])
        && (if v["target"] == "group" {
            v["errno"].is_i64() && v["kind"].is_null()
        } else {
            v["kind"] == "direct_kill_error"
        })
        && ((v["outcome"] == "absent")
            == (v["errno"].as_i64() == Some(i64::from(nix::libc::ESRCH))))
}
fn supervision_error(v: &Value) -> bool {
    [
        "observation_error",
        "wait_error",
        "ownership_lost",
        "read_error",
    ]
    .iter()
    .any(|key| v[*key] == true)
        || v["signals"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["outcome"] == "error"))
}
fn supervision(v: &Value) -> bool {
    keys(
        v,
        &[
            "observation_error",
            "wait_error",
            "ownership_lost",
            "read_error",
            "signals",
        ],
    ) && [
        "observation_error",
        "wait_error",
        "ownership_lost",
        "read_error",
    ]
    .iter()
    .all(|key| v[*key].is_boolean())
        && v["signals"].as_array().is_some_and(|rows| {
            rows.len() <= 3
                && rows.iter().all(signal_observation)
                && (supervision_error(v) || !rows.is_empty())
        })
}
fn process_diagnostics(v: &Value, prefix: &str) -> bool {
    let field = |name| v.get(format!("{prefix}{name}"));
    let observed_code = field("observed_exit_code");
    let observed_signal = field("observed_exit_signal");
    if !field("supervision").is_none_or(supervision)
        || !field("cleanup_confirmed").is_none_or(|value| *value == false)
        || !observed_code.is_none_or(|value| value.is_null() || integer(value))
        || !observed_signal.is_none_or(|value| nullable_enum(value, EXIT_SIGNALS))
    {
        return false;
    }
    let error = field("supervision").is_some_and(supervision_error);
    if error && !field("cleanup_confirmed").is_some_and(|value| *value == false) {
        return false;
    }
    if error && field("exit_code").is_some_and(|value| !value.is_null()) {
        let supervision = field("supervision").unwrap();
        if [
            "observation_error",
            "wait_error",
            "ownership_lost",
            "read_error",
        ]
        .iter()
        .any(|key| supervision[*key] == true)
            || !supervision["signals"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| {
                    row["outcome"] != "error"
                        || (row["target"] == "group" && row["signal"] == "SIGKILL")
                })
            || ["timed_out", "output_limit_exceeded", "cancelled"]
                .iter()
                .any(|key| field(key).is_some_and(|value| *value == true))
        {
            return false;
        }
    }
    if observed_code.is_some() || observed_signal.is_some() {
        return observed_code.is_some()
            && observed_signal.is_some()
            && error
            && field("exit_code").is_some_and(Value::is_null);
    }
    true
}

fn integer(v: &Value) -> bool {
    v.as_f64()
        .is_some_and(|n| n.is_finite() && n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0)
}
fn count(v: &Value) -> bool {
    integer(v) && v.as_f64().unwrap() >= 0.0
}
fn text(v: &Value) -> bool {
    v.as_str().is_some_and(|s| s.len() <= 512)
}
fn array(v: &Value, check: impl Fn(&Value) -> bool) -> bool {
    v.as_array().is_some_and(|a| a.iter().all(check))
}
fn enums(v: &Value, allowed: &[&str]) -> bool {
    v.as_str().is_some_and(|s| allowed.contains(&s))
}
fn nullable_enum(v: &Value, allowed: &[&str]) -> bool {
    v.is_null() || enums(v, allowed)
}
fn numeric_map(v: &Value) -> bool {
    v.as_object()
        .is_some_and(|o| o.keys().all(|s| s.len() <= 200) && o.values().all(Value::is_number))
}
fn category(v: &Value) -> bool {
    keys(
        v,
        &[
            "tool",
            "command_category",
            "target_category",
            "result_received",
            "is_error",
            "permission_denied",
        ],
    ) && enums(&v["tool"], &["Read", "Edit", "Bash", "other"])
        && if v["tool"] == "Bash" {
            v.get("command_category").is_some() && v.get("target_category").is_none()
        } else {
            v.get("target_category").is_some() && v.get("command_category").is_none()
        }
        && v.get("command_category").is_none_or(|v| {
            enums(
                v,
                &[
                    "working_directory_lookup",
                    "exact_fixture_test",
                    "fixture_test_with_other_shell_text",
                    "other",
                ],
            )
        })
        && v.get("target_category")
            .is_none_or(|v| enums(v, &["fixture_source", "fixture_test", "other"]))
        && v.get("result_received").is_none_or(Value::is_boolean)
        && v.get("is_error")
            .is_none_or(|v| v.is_null() || v.is_boolean())
        && v.get("permission_denied").is_none_or(Value::is_boolean)
}
fn usage_shape(v: &Value) -> bool {
    keys(v, &["inference_geo", "iterations"])
        && v.get("inference_geo").is_some()
        && v.get("iterations").is_some()
        && nullable_enum(
            &v["inference_geo"],
            &[
                "absent",
                "other",
                "global",
                "us",
                "not_available",
                "standard",
                "message",
                "compaction",
                "advisor_message",
                "fallback_message",
            ],
        )
        && (v["iterations"].is_null()
            || v["iterations"] == "absent"
            || array(&v["iterations"], |entry| {
                keys(
                    entry,
                    &[
                        "type",
                        "input_tokens",
                        "output_tokens",
                        "cache_creation_input_tokens",
                        "cache_read_input_tokens",
                    ],
                ) && entry.get("type").is_some()
                    && nullable_enum(
                        &entry["type"],
                        &[
                            "absent",
                            "other",
                            "global",
                            "us",
                            "not_available",
                            "standard",
                            "message",
                            "compaction",
                            "advisor_message",
                            "fallback_message",
                        ],
                    )
                    && [
                        "input_tokens",
                        "output_tokens",
                        "cache_creation_input_tokens",
                        "cache_read_input_tokens",
                    ]
                    .iter()
                    .all(|k| entry.get(*k).is_none_or(integer))
            }))
}
pub(super) fn route(v: &Value) -> bool {
    let allowed = [
        "requested_model",
        "request_class",
        "message_count",
        "message_roles",
        "request_bytes",
        "system_bytes",
        "tools_bytes",
        "messages_bytes",
        "has_system_messages",
        "thinking_type",
        "thinking_history_count",
        "tool_result_count",
        "tool_count",
        "typed_tools",
        "max_tokens",
        "has_output_effort",
        "has_context_management",
        "expected_classified_tier",
        "scenario_prompt_index",
        "tier",
        "confidence",
        "source",
        "reason",
        "model",
        "latency_ms",
        "evaluation_latency_ms",
        "classified_tier",
        "evaluator",
        "context_check",
        "counted_input_tokens",
        "tool_compatibility",
        "classifier_error",
        "classifier_status",
        "compatibility_reason",
        "continuity_state",
    ];
    if !keys(v, &allowed) {
        return false;
    }
    for key in ["requested_model", "model", "reason", "source"] {
        if !text(&v[key]) {
            return false;
        }
    }
    if !enums(
        &v["request_class"],
        &["main", "unspecified", "compaction", "auxiliary", "other"],
    ) || !enums(
        &v["thinking_type"],
        &["enabled", "adaptive", "disabled", "unspecified"],
    ) {
        return false;
    }
    for key in [
        "message_count",
        "request_bytes",
        "system_bytes",
        "tools_bytes",
        "messages_bytes",
        "thinking_history_count",
        "tool_result_count",
        "tool_count",
    ] {
        if !count(&v[key]) {
            return false;
        }
    }
    for key in [
        "has_system_messages",
        "has_output_effort",
        "has_context_management",
    ] {
        if !v[key].is_boolean() {
            return false;
        }
    }
    if !array(&v["message_roles"], text)
        || !array(&v["typed_tools"], |v| {
            v.is_string() || v.is_number() || v.is_boolean()
        })
    {
        return false;
    }
    if v.get("scenario_prompt_index").is_none()
        || !v
            .get("scenario_prompt_index")
            .is_none_or(|v| v.is_null() || count(v))
    {
        return false;
    }
    for key in ["expected_classified_tier", "classified_tier", "tier"] {
        if !v
            .get(key)
            .is_none_or(|v| nullable_enum(v, &["haiku", "sonnet", "opus"]))
        {
            return false;
        }
    }
    for key in ["latency_ms", "evaluation_latency_ms"] {
        if !v[key].as_f64().is_some_and(|n| n.is_finite() && n >= 0.0) {
            return false;
        }
    }
    for key in [
        "confidence",
        "max_tokens",
        "counted_input_tokens",
        "classifier_status",
    ] {
        if !v.get(key).is_none_or(|v| v.is_null() || v.is_number()) {
            return false;
        }
    }
    for key in [
        "evaluator",
        "context_check",
        "tool_compatibility",
        "classifier_error",
        "compatibility_reason",
        "continuity_state",
    ] {
        if !v.get(key).is_none_or(|v| v.is_null() || text(v)) {
            return false;
        }
    }
    true
}
fn observation(v: &Value) -> bool {
    keys(
        v,
        &[
            "request_id",
            "requested_model",
            "selected_model",
            "source",
            "classified_tier",
            "confirmed_model",
            "http_status",
            "outcome",
        ],
    ) && text(&v["request_id"])
        && ["requested_model", "selected_model", "source"]
            .iter()
            .all(|k| v.get(*k).is_none_or(text))
        && v.get("classified_tier")
            .is_none_or(|v| nullable_enum(v, &["haiku", "sonnet", "opus"]))
        && v.get("confirmed_model")
            .is_none_or(|v| v.is_null() || text(v))
        && v.get("http_status").is_none_or(count)
        && v.get("outcome").is_none_or(|v| {
            enums(
                v,
                &["request_complete", "request_error", "request_cancelled"],
            )
        })
}
fn savings(v: &Value) -> bool {
    keys(
        v,
        &[
            "baseline_model",
            "pricing_version",
            "pricing_date",
            "pricing_source",
            "actual_usd",
            "baseline_usd",
            "saved_usd",
            "percent",
            "requests",
            "unpriced_requests",
            "unpriced_reasons",
            "partial",
        ],
    ) && [
        "baseline_model",
        "pricing_version",
        "pricing_date",
        "pricing_source",
    ]
    .iter()
    .all(|k| text(&v[*k]))
        && ["actual_usd", "baseline_usd", "saved_usd", "percent"]
            .iter()
            .all(|k| v[*k].is_number())
        && count(&v["requests"])
        && count(&v["unpriced_requests"])
        && v["unpriced_reasons"].as_object().is_some_and(|o| {
            o.iter().all(|(k, v)| {
                autorouter_core::telemetry_event::UNPRICED_REASONS.contains(&k.as_str()) && count(v)
            })
        })
        && v.get("partial").is_none_or(|v| *v == true)
}
pub(super) fn case(v: &Value) -> bool {
    for key in CASE_FIELDS.iter().copied().filter(|key| {
        !["case", "cancelled", "stdin_error"].contains(key)
            && !PROCESS_DIAGNOSTIC_FIELDS.contains(key)
    }) {
        if v.get(key).is_none() {
            return false;
        }
    }
    for key in [
        "classifier_succeeded",
        "model_changed",
        "passed",
        "timed_out",
        "output_limit_exceeded",
        "fixture_removed",
    ] {
        if !v[key].is_boolean() {
            return false;
        }
    }
    for key in [
        "result_count",
        "permission_denials",
        "parse_errors",
        "duration_ms",
        "stderr_bytes",
    ] {
        if !count(&v[key]) {
            return false;
        }
    }
    if !v["exit_code"].is_null() && !integer(&v["exit_code"]) {
        return false;
    }
    if !v["exit_signal"].is_null() && !text(&v["exit_signal"]) {
        return false;
    }
    if !process_diagnostics(v, "") {
        return false;
    }
    if !v.get("stdin_error").is_none_or(Value::is_boolean)
        || !v.get("cancelled").is_none_or(Value::is_boolean)
    {
        return false;
    }
    if v.get("cancelled").is_none()
        && !v["failures"]
            .as_array()
            .is_some_and(|a| a.contains(&Value::String("claude_spawn_error".into())))
    {
        return false;
    }
    if !array(&v["routes"], route)
        || !array(&v["request_observations"], observation)
        || !array(&v["upstream_models"], text)
        || !array(&v["assistant_models"], text)
        || !array(&v["upstream_statuses"], count)
        || !array(&v["status_lines"], text)
    {
        return false;
    }
    if !array(&v["proxy_errors"], |row| {
        keys(row, &["event", "status"])
            && enums(&row["event"], &["proxy_error", "invalid_request_shape"])
            && (row["status"].is_null() || count(&row["status"]))
    }) {
        return false;
    }
    for key in ["tools_used", "successful_tools"] {
        if !array(&v[key], |v| enums(v, &["Read", "Edit", "Bash", "other"])) {
            return false;
        }
    }
    if !array(&v["failures"], |v| {
        enums(
            v,
            &[
                "claude_spawn_error",
                "claude_result_error",
                "error_max_turns",
                "error_during_execution",
                "error_max_budget_usd",
            ],
        )
    }) {
        return false;
    }
    if !array(&v["denied_tool_details"], category)
        || !array(&v["tool_outcomes"], |row| {
            category(row)
                && row["result_received"].is_boolean()
                && (row["is_error"].is_null() || row["is_error"].is_boolean())
                && row["permission_denied"].is_boolean()
        })
        || !array(&v["provider_usage_shapes"], usage_shape)
        || !array(&v["api_equivalent_savings"], savings)
    {
        return false;
    }
    if !array(&v["provider_usage"], |row| {
        autorouter_core::telemetry_event::normalize_usage_telemetry(Some(row))
            .is_some_and(|normalized| super::same_number(&normalized, row))
    }) {
        return false;
    }
    if !v["client_model_usage_estimate"]
        .as_object()
        .is_some_and(|o| {
            o.iter().all(|(model, metrics)| {
                model.len() <= 200
                    && numeric_map(metrics)
                    && metrics.as_object().unwrap().keys().all(|k| {
                        ["tokens", "cost", "requests"]
                            .iter()
                            .any(|word| k.to_ascii_lowercase().contains(word))
                    })
            })
        })
    {
        return false;
    }
    let cleanup = &v["gateway_cleanup"];
    if !cleanup["awaited"].is_boolean()
        || !enums(
            &cleanup["listener_observation"],
            &[
                "connection_refused",
                "reachable",
                "unverified_error",
                "unverified_timeout",
            ],
        )
    {
        return false;
    }
    for key in [
        "router_close_returned",
        "native_join_result_available",
        "reference_closed",
        "reference_protocol_valid",
        "reference_process_timed_out",
        "reference_process_output_limit_exceeded",
    ] {
        if !cleanup.get(key).is_none_or(Value::is_boolean) {
            return false;
        }
    }
    cleanup
        .get("reference_process_exit_code")
        .is_none_or(|v| v.is_null() || integer(v))
        && process_diagnostics(cleanup, "reference_process_")
}
