//! Organization allowlists and locks, applied after saved/environment settings.
use crate::config::js_trim;
use serde_json::{Map, Value, json};

pub const POLICY_KEYS: [&str; 5] = [
    "allowed_evaluators",
    "allowed_auth_modes",
    "session_log_mode",
    "upstream_url",
    "jev_url",
];
const ALLOWLISTS: [(&str, &str, &str, &[&str]); 2] = [
    (
        "allowed_evaluators",
        "AUTOROUTER_EVALUATOR",
        "ollama",
        &["jev", "ollama"],
    ),
    (
        "allowed_auth_modes",
        "AUTOROUTER_AUTH_MODE",
        "api-key",
        &["api-key", "subscription"],
    ),
];
const LOCKS: [(&str, &str); 3] = [
    ("session_log_mode", "AUTOROUTER_SESSION_LOG_MODE"),
    ("upstream_url", "AUTOROUTER_UPSTREAM_URL"),
    ("jev_url", "AUTOROUTER_JEV_URL"),
];

pub fn validate_policy(parsed: &Value) -> Result<Value, String> {
    let object = parsed
        .as_object()
        .ok_or("The AutoRouter organization policy must be a JSON object.")?;
    if object
        .keys()
        .any(|key| !POLICY_KEYS.contains(&key.as_str()))
    {
        return Err("The AutoRouter organization policy contains an unsupported key.".into());
    }
    let mut policy = Map::new();
    for (name, _, _, allowed) in ALLOWLISTS {
        let Some(value) = object.get(name) else {
            continue;
        };
        let list = value
            .as_array()
            .filter(|list| {
                !list.is_empty()
                    && list
                        .iter()
                        .all(|v| v.as_str().is_some_and(|s| allowed.contains(&s)))
            })
            .ok_or_else(|| {
                format!(
                    "Policy {name} must be a nonempty list of: {}.",
                    allowed.join(", ")
                )
            })?;
        let mut unique = Vec::new();
        for value in list {
            if !unique.contains(value) {
                unique.push(value.clone());
            }
        }
        policy.insert(name.into(), Value::Array(unique));
    }
    for (name, _) in LOCKS {
        let Some(value) = object.get(name) else {
            continue;
        };
        let valid = if name == "session_log_mode" {
            value == "metadata" || value == "prompts"
        } else {
            value.as_str().is_some_and(|s| {
                let suffix = s
                    .strip_prefix("http://")
                    .or_else(|| s.strip_prefix("https://"));
                suffix.is_some_and(|suffix| !suffix.is_empty())
                    && s.chars().all(|c| {
                        let mut buf = [0; 4];
                        c > '\u{1f}'
                            && c != '\u{7f}'
                            && !js_trim(c.encode_utf8(&mut buf)).is_empty()
                    })
            })
        };
        if !valid {
            return Err(format!("Policy {name} has an invalid value."));
        }
        policy.insert(name.into(), value.clone());
    }
    Ok(Value::Object(policy))
}

/// Input policy must have passed validate_policy. Repair commands may disable
/// allowlist rejection; locks always apply and are reported in stable order.
pub fn apply_policy(env: &Value, policy: &Value, allowlists: bool) -> Result<Value, String> {
    let mut result = env.as_object().cloned().unwrap_or_default();
    for (name, key, fallback, _) in ALLOWLISTS {
        let Some(allowed) = policy.get(name).and_then(Value::as_array) else {
            continue;
        };
        if !allowlists {
            continue;
        }
        let fallback = json!(fallback);
        let value = result
            .get(key)
            .filter(|v| !v.is_null())
            .unwrap_or(&fallback);
        if !allowed.contains(value) {
            return Err(format!(
                "{key} is not permitted by the AutoRouter organization policy. Allowed: {}.",
                allowed
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    let mut locked = Vec::new();
    for (name, key) in LOCKS {
        if let Some(value) = policy.get(name) {
            result.insert(key.into(), value.clone());
            locked.push(key);
        }
    }
    Ok(json!({"env":result,"locked":locked}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_policy_is_closed_and_does_not_echo_payload() {
        for value in [
            json!([]),
            json!({"PRIVATE_KEY_NAME":1}),
            json!({"allowed_evaluators":[]}),
            json!({"allowed_evaluators":["PRIVATE_VALUE"]}),
            json!({"session_log_mode":"PRIVATE_VALUE"}),
            json!({"upstream_url":"PRIVATE_VALUE"}),
        ] {
            assert!(!validate_policy(&value).unwrap_err().contains("PRIVATE_"));
        }
    }
    #[test]
    fn defaults_are_subject_to_allowlists_and_repairs_still_keep_locks() {
        let policy = validate_policy(&json!({"allowed_evaluators":["ollama","ollama"],"allowed_auth_modes":["subscription"],"session_log_mode":"metadata"})).unwrap();
        assert_eq!(policy["allowed_evaluators"], json!(["ollama"]));
        assert!(
            apply_policy(&json!({}), &policy, true)
                .unwrap_err()
                .contains("AUTOROUTER_AUTH_MODE")
        );
        let env = json!({"AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_SESSION_LOG_MODE":"prompts","KEEP":"1"});
        let output = apply_policy(&env, &policy, true).unwrap();
        assert_eq!(output["env"]["AUTOROUTER_SESSION_LOG_MODE"], "metadata");
        assert_eq!(output["env"]["KEEP"], "1");
        assert_eq!(output["locked"], json!(["AUTOROUTER_SESSION_LOG_MODE"]));
        assert_eq!(env["AUTOROUTER_SESSION_LOG_MODE"], "prompts");
        assert!(apply_policy(&json!({"AUTOROUTER_EVALUATOR":"jev"}), &policy, false).is_ok());
    }
}
