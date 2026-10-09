//! Development fixture boundary shared by the differential runner and tests.
//! This is not a public product command and never loads a user's configuration.
use serde_json::{Value, json};
use std::path::Path;

fn field<'a>(input: &'a Value, name: &str) -> Result<&'a Value, String> {
    input
        .get(name)
        .ok_or_else(|| "Invalid fixture input".into())
}

fn text<'a>(input: &'a Value, name: &str) -> Result<&'a str, String> {
    field(input, name)?
        .as_str()
        .ok_or_else(|| "Invalid fixture input".into())
}

fn document_input(input: &Value, raw: bool) -> Result<crate::js_json::JsDocument, String> {
    let bytes = if raw {
        field(input, "bytes")?
            .as_array()
            .ok_or("Invalid fixture input")?
            .iter()
            .map(|v| {
                v.as_u64()
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or("Invalid fixture byte")
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        input.to_string().into_bytes()
    };
    crate::js_json::JsDocument::parse(&bytes).map_err(|_| "Invalid JSON fixture".into())
}

pub fn execute(fixture: &Value, cwd: &Path) -> Result<Value, String> {
    let op = text(fixture, "op")?;
    let input = field(fixture, "input")?;
    match op {
        "validate_request_json" => Ok(crate::request_validation::validate_request_document(
            &document_input(input, true)?,
        )),
        "prepare_request_json" => {
            let document = document_input(input, true)?;
            let (request, adjustments) =
                crate::model_request::prepare_request_document(&document, text(input, "target")?);
            Ok(json!({"serialized":request.stringify().into_bytes(),"adjustments":adjustments}))
        }
        "target_compatibility_json" => Ok(crate::auto_routing::target_compatibility_document(
            &document_input(input, true)?,
            text(input, "target")?,
            input["auto_mode"].as_bool().unwrap_or(false),
        )),
        "can_route_auto_json" => Ok(json!(crate::auto_routing::can_route_auto_request_document(
            &document_input(input, true)?,
            text(input, "target")?
        ))),
        "safeguards_json" => Ok(json!(
            crate::auto_routing::has_routable_safeguards_document(&document_input(input, true)?)
        )),
        "history_session" => {
            let bytes = field(input, "bytes")?
                .as_array()
                .ok_or("Invalid fixture input")?
                .iter()
                .map(|v| {
                    v.as_u64()
                        .and_then(|n| u8::try_from(n).ok())
                        .ok_or("Invalid fixture input")
                })
                .collect::<Result<Vec<_>, _>>()?;
            let limits = crate::session_history::HistoryLimits::from_overrides(&input["limits"])?;
            let capacity = bytes
                .len()
                .min(limits.max_file_bytes)
                .min(limits.max_total_bytes);
            let report = crate::session_history::parse_session_with_locale(
                &bytes[..capacity],
                bytes.len() as u64,
                "autorouter-session-fixture",
                &limits,
                input["locale"].as_str().unwrap_or("en-US"),
            );
            let mut result = json!({"schema_version":1,"type":"session_history","summary":report["summary"],"records":report["records"],"limits":limits});
            if input["order"] == true {
                result["selected_order"] = json!(
                    result["summary"]["selected_models"]
                        .as_object()
                        .unwrap()
                        .keys()
                        .collect::<Vec<_>>()
                );
            }
            if input["text"] == true && input["limits"].as_object().is_none_or(|v| v.is_empty()) {
                result["text"] = json!(crate::session_history::report_lines(&result, true));
            }
            Ok(result)
        }
        "evaluation_policy" => serde_json::to_value(
            crate::evaluation_report::create_evaluation_policy(Some(input))?,
        )
        .map_err(|_| "Invalid policy fixture".into()),
        "quality_threshold" => Ok(json!(crate::evaluation_report::parse_quality_threshold(
            input
        )?)),
        "routing_report" => {
            crate::evaluation_report::evaluate_routing_report(&input["rows"], &input["options"])
        }
        "live_case" => crate::evaluation_report::evaluate_live_case(input),
        "live_report" => {
            crate::evaluation_report::evaluate_live_report(&input["cases"], &input["options"])
        }
        "router" => crate::router::run_fixture(input),
        "context_size" => {
            let bytes = field(input, "bytes")?
                .as_array()
                .ok_or("Invalid fixture input")?
                .iter()
                .map(|v| {
                    v.as_u64()
                        .and_then(|n| u8::try_from(n).ok())
                        .ok_or("Invalid fixture input")
                })
                .collect::<Result<Vec<_>, _>>()?;
            let document =
                crate::js_json::JsDocument::parse(&bytes).map_err(|_| "Invalid JSON fixture")?;
            let model = input
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    document
                        .get(document.root(), "model")
                        .and_then(|n| document.string(n))
                        .and_then(|s| s.to_scalar())
                })
                .unwrap_or_default();
            Ok(json!(crate::router::context_size_bytes(&document, &model)))
        }
        "build_state" => Ok(crate::prompt_state::build_state(
            field(input, "body")?,
            input.get("limit").and_then(Value::as_u64).unwrap_or(12000) as usize,
        )),
        "build_state_json" | "build_ollama_state_json" => {
            let bytes = field(input, "bytes")?
                .as_array()
                .ok_or("Invalid fixture input")?
                .iter()
                .map(|v| {
                    v.as_u64()
                        .and_then(|n| u8::try_from(n).ok())
                        .ok_or("Invalid fixture input")
                })
                .collect::<Result<Vec<_>, _>>()?;
            let document =
                crate::js_json::JsDocument::parse(&bytes).map_err(|_| "Invalid JSON fixture")?;
            let state = if op == "build_ollama_state_json" {
                crate::prompt_state::build_ollama_state_document(
                    &document,
                    input.get("limit").and_then(Value::as_u64).unwrap_or(3000) as usize,
                )
            } else {
                crate::prompt_state::build_state_document(
                    &document,
                    input.get("limit").and_then(Value::as_u64).unwrap_or(12000) as usize,
                )
            };
            Ok(json!({"serialized": state.stringify().into_bytes()}))
        }
        "prompt_excerpt" | "prompt_excerpt_json" => {
            let maximum = match input.get("max_chars") {
                None => 500,
                Some(value) => value
                    .as_f64()
                    .filter(|n| {
                        n.is_finite()
                            && n.fract() == 0.0
                            && *n >= 0.0
                            && *n <= 9_007_199_254_740_991.0
                    })
                    .ok_or("maxChars must be a nonnegative safe integer")?
                    as usize,
            };
            if op == "prompt_excerpt_json" {
                let bytes = field(input, "bytes")?
                    .as_array()
                    .ok_or("Invalid fixture input")?
                    .iter()
                    .map(|v| {
                        v.as_u64()
                            .and_then(|n| u8::try_from(n).ok())
                            .ok_or("Invalid fixture input")
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let document = crate::js_json::JsDocument::parse(&bytes)
                    .map_err(|_| "Invalid JSON fixture")?;
                Ok(json!(crate::prompt_state::prompt_excerpt_document(
                    &document, maximum
                )))
            } else {
                Ok(json!(crate::prompt_state::prompt_excerpt(
                    field(input, "body")?,
                    maximum
                )))
            }
        }
        "status_state" => crate::status_state::run_fixture(input),
        "render_statusline" => Ok(json!(crate::statusline::render_status_line(
            &input["input"],
            &input["snapshot"],
            &input["options"]
        ))),
        "goal_feedback_indexes" => Ok(json!(crate::prompt_state::goal_feedback_indexes(input))),
        "js_json" => {
            let bytes = field(input, "bytes")?
                .as_array()
                .ok_or("JSON fixture needs bytes")?
                .iter()
                .map(|value| {
                    value
                        .as_u64()
                        .and_then(|value| u8::try_from(value).ok())
                        .ok_or("Invalid JSON fixture byte")
                })
                .collect::<Result<Vec<_>, _>>()?;
            match crate::js_json::JsDocument::parse(&bytes) {
                Err(_) => Ok(json!({"valid":false})),
                Ok(document) => {
                    let mut result =
                        json!({"valid":true,"serialized":document.stringify().into_bytes()});
                    match document.node(document.root()) {
                        Some(crate::js_json::JsNode::String(value)) => {
                            result["string_units"] = json!(value.units())
                        }
                        Some(crate::js_json::JsNode::Number(value)) => {
                            result["number_bits"] = json!(value.to_be_bytes())
                        }
                        _ => {}
                    }
                    Ok(result)
                }
            }
        }
        "estimate_savings" => Ok(crate::savings::estimate_outcome_savings(input)),
        "savings_tracker" => crate::savings::run_fixture(input),
        "normalize_usage" => Ok(
            crate::telemetry_event::normalize_usage_telemetry(Some(input)).unwrap_or(Value::Null),
        ),
        "normalize_pricing" => Ok(
            crate::telemetry_event::normalize_pricing_context(Some(input)).unwrap_or(Value::Null),
        ),
        "normalize_telemetry_json" => Ok(crate::telemetry_event::normalize_telemetry_document(
            &document_input(input, true)?,
            "2026-10-09T12:00:00.000Z",
        )
        .unwrap_or(Value::Null)),
        "normalize_session_json" => Ok(crate::telemetry_event::normalize_session_document(
            &document_input(input, true)?,
            input["include_prompts"].as_bool().unwrap_or(true),
            "2026-10-09T12:00:00.000Z",
        )
        .unwrap_or(Value::Null)),
        "normalize_telemetry" => Ok(crate::telemetry_event::normalize_telemetry_event(
            input,
            "2026-10-09T12:00:00.000Z",
        )
        .unwrap_or(Value::Null)),
        "normalize_session" => Ok(crate::telemetry_event::normalize_session_record(
            field(input, "entry")?,
            input
                .get("include_prompts")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            "2026-10-09T12:00:00.000Z",
        )
        .unwrap_or(Value::Null)),
        "redact" => Ok(json!(crate::redaction::redact_sensitive(
            input.as_str().ok_or("Invalid fixture input")?
        ))),
        "model_catalog" => Ok(input
            .get("model")
            .and_then(Value::as_str)
            .and_then(crate::model_catalog::catalog)
            .unwrap_or(Value::Null)),
        "prepare_request" => Ok(crate::model_request::prepare_request(
            field(input, "body")?,
            text(input, "target")?,
        )),
        "target_compatibility" => Ok(crate::auto_routing::target_compatibility_document(
            &document_input(field(input, "body")?, false)?,
            text(input, "target")?,
            input
                .get("auto_mode")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        )),
        "can_route_auto" => Ok(json!(crate::auto_routing::can_route_auto_request_document(
            &document_input(field(input, "body")?, false)?,
            text(input, "target")?
        ))),
        "safeguards" => Ok(json!(
            crate::auto_routing::has_routable_safeguards_document(&document_input(input, false)?)
        )),
        "validate_request" => Ok(crate::request_validation::validate_request_shape(input)),
        "turn_state" => crate::turn_state::run_fixture(input),
        "apply_policy" => crate::policy::apply_policy(
            &input["env"],
            &input["policy"],
            input["allowlists"] != false,
        ),
        "conflicting_providers" => Ok(json!(crate::auth::conflicting_providers(
            &fixture_environment(input)
        ))),
        "subscription_request" => Ok(json!(crate::auth::is_subscription_request(
            input["x-api-key"].as_str(),
            input["authorization"].as_str(),
            input["anthropic-beta"].as_str()
        ))),
        "client_profile" => {
            let config = crate::config::read_config(
                &json!({"AUTOROUTER_CLIENT_PROFILE":input["profile"]}),
                false,
                cwd,
            )?;
            let args = input["args"]
                .as_array()
                .ok_or("Invalid arguments")?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(std::ffi::OsString::from)
                        .ok_or("Invalid argument")
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!(crate::auth::client_profile_for_launch(
                config.client_profile,
                &args
            )))
        }
        "build_claude_env" => {
            let config = crate::config::read_config(&input["config"], false, cwd)?;
            let parent = fixture_environment(&input["parent"]);
            let child = crate::auth::build_claude_env(&config, "http://127.0.0.1:1234", &parent);
            let projected = |env: crate::auth::Environment| -> Value {
                Value::Object(
                    env.into_iter()
                        .map(|(key, value)| {
                            (
                                key.to_string_lossy().into_owned(),
                                json!(value.to_string_lossy()),
                            )
                        })
                        .collect(),
                )
            };
            Ok(json!({"child":projected(child),"parent":projected(parent)}))
        }
        "read_config" => {
            let directory = cwd.join(input.get("cwd").and_then(Value::as_str).unwrap_or("."));
            let config = crate::config::read_config(
                input.get("env").unwrap_or(&json!({})),
                input
                    .get("validate_all")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                &directory,
            )?;
            if input["require_keys"] == true {
                crate::config::require_keys(&config)?;
            }
            serde_json::to_value(config).map_err(|_| "Configuration serialization failed".into())
        }
        _ => Err("Unknown fixture operation".into()),
    }
}

fn fixture_environment(input: &Value) -> crate::auth::Environment {
    input
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| value.as_str().map(|value| (key.into(), value.into())))
        .collect()
}

pub fn response(fixture: &Value, cwd: &Path) -> Value {
    match execute(fixture, cwd) {
        Ok(result) => json!({"id": fixture["id"], "op": fixture["op"], "result": result}),
        Err(error) => json!({"id": fixture["id"], "op": fixture["op"], "error": error}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_operation_is_an_error_not_a_successful_null() {
        let result = response(
            &json!({"id":"unknown", "op":"not_implemented", "input":{}}),
            Path::new("/"),
        );
        assert_eq!(
            result,
            json!({"id":"unknown", "op":"not_implemented", "error":"Unknown fixture operation"})
        );
    }

    #[test]
    fn invalid_request_returns_the_policy_result_without_echoing_payload() {
        let result = response(
            &json!({"id":"bad-model", "op":"validate_request", "input":{"model":null,"secret":"synthetic-canary"}}),
            Path::new("/"),
        );
        assert_eq!(result["result"]["valid"], false);
        assert_eq!(
            result["result"]["error"],
            "Invalid Messages API request shape: model"
        );
        assert!(!result.to_string().contains("synthetic-canary"));
    }
}
