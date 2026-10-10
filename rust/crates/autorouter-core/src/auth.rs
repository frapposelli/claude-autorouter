//! Claude-owned authentication and launch environment adaptation.
use crate::config::{AuthMode, ClientProfile, RouterConfig, js_trim};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};

pub const LOCAL_AUTH_HEADER: &str = "x-autorouter-token";
pub type Environment = BTreeMap<OsString, OsString>;

pub fn client_profile_for_launch(profile: ClientProfile, args: &[OsString]) -> ClientProfile {
    let mut permission_mode = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if arg == "--permission-mode" {
            permission_mode = args.next().map(|v| v.to_string_lossy().into_owned());
        } else if let Some(mode) = arg.to_string_lossy().strip_prefix("--permission-mode=") {
            permission_mode = Some(mode.to_owned());
        }
    }
    if permission_mode.as_deref() == Some("auto") {
        ClientProfile::Auto
    } else {
        profile
    }
}

pub fn conflicting_providers(env: &Environment) -> Vec<&'static str> {
    [
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDE_CODE_USE_MANTLE",
        "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    ]
    .into_iter()
    .filter(|key| {
        env.get(OsStr::new(key)).is_some_and(|value| {
            matches!(
                value.to_string_lossy().to_ascii_lowercase().as_str(),
                "1" | "true"
            )
        })
    })
    .collect()
}

/// Recognize the subscription wire format; only Anthropic validates the token.
pub fn is_subscription_request(
    api_key: Option<&str>,
    authorization: Option<&str>,
    beta: Option<&str>,
) -> bool {
    if api_key.is_some_and(|key| !key.is_empty()) {
        return false;
    }
    let Some(authorization) = authorization else {
        return false;
    };
    let Some(prefix) = authorization.get(..7) else {
        return false;
    };
    if !prefix.eq_ignore_ascii_case("Bearer ") {
        return false;
    }
    let credential = &authorization[7..];
    if credential.is_empty()
        || credential.chars().any(|c| {
            let mut buffer = [0; 4];
            js_trim(c.encode_utf8(&mut buffer)).is_empty()
        })
    {
        return false;
    }
    beta.unwrap_or("")
        .split(',')
        .any(|value| js_trim(value).starts_with("oauth-"))
}

fn put(env: &mut Environment, key: &str, value: impl Into<OsString>) {
    env.insert(key.into(), value.into());
}
fn remove(env: &mut Environment, key: &str) {
    env.remove(OsStr::new(key));
}

/// Preserve unrelated and non-UTF-8 environment values without exposing secrets
/// through a process argument or changing the parent's environment.
pub fn build_claude_env(
    config: &RouterConfig,
    base_url: &str,
    parent: &Environment,
) -> Environment {
    let mut env = parent.clone();
    put(&mut env, "ANTHROPIC_BASE_URL", base_url);
    put(&mut env, "CLAUDE_CODE_GATEWAY_HINT_HEADERS", "1");
    if !env.contains_key(OsStr::new("CLAUDE_CODE_STOP_HOOK_BLOCK_CAP"))
        && let Some(cap) = config.stop_hook_block_cap
    {
        put(&mut env, "CLAUDE_CODE_STOP_HOOK_BLOCK_CAP", cap.to_string());
    }
    if !env.contains_key(OsStr::new("ENABLE_TOOL_SEARCH")) {
        put(&mut env, "ENABLE_TOOL_SEARCH", "true");
    }
    match config.client_profile {
        ClientProfile::Compatible => {
            put(&mut env, "ANTHROPIC_MODEL", config.models.haiku.clone());
            put(&mut env, "MAX_THINKING_TOKENS", "0");
        }
        ClientProfile::Auto if !env.contains_key(OsStr::new("ANTHROPIC_MODEL")) => {
            put(&mut env, "ANTHROPIC_MODEL", config.models.sonnet.clone());
        }
        _ => {}
    }
    let subscription = config.auth_mode == AuthMode::Subscription;
    let custom = env
        .get(OsStr::new("ANTHROPIC_CUSTOM_HEADERS"))
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut headers: Vec<String> = custom
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| {
            let name = js_trim(line.split(':').next().unwrap_or("")).to_ascii_lowercase();
            !js_trim(line).is_empty()
                && name != LOCAL_AUTH_HEADER
                && !(subscription && matches!(name.as_str(), "authorization" | "x-api-key"))
        })
        .map(str::to_owned)
        .collect();
    if subscription {
        for key in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ] {
            remove(&mut env, key);
        }
        headers.push(format!(
            "X-Autorouter-Token: {}",
            config.local_token.as_deref().unwrap_or("undefined")
        ));
    } else if let Some(token) = &config.local_token {
        put(&mut env, "ANTHROPIC_API_KEY", token.clone());
        put(&mut env, "ANTHROPIC_AUTH_TOKEN", token.clone());
    } else {
        remove(&mut env, "ANTHROPIC_API_KEY");
        remove(&mut env, "ANTHROPIC_AUTH_TOKEN");
    }
    if headers.is_empty() {
        remove(&mut env, "ANTHROPIC_CUSTOM_HEADERS");
    } else {
        put(&mut env, "ANTHROPIC_CUSTOM_HEADERS", headers.join("\n"));
    }
    for key in ["TYPESAFE_API_KEY", "AUTOROUTER_TOKEN"] {
        remove(&mut env, key);
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::read_config;
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn argument_boundaries_and_last_permission_selection_are_preserved() {
        for (args, expected) in [
            (vec!["--permission-mode", "auto"], ClientProfile::Auto),
            (
                vec!["--permission-mode=auto", "--permission-mode=default"],
                ClientProfile::Native,
            ),
            (vec!["--", "--permission-mode=auto"], ClientProfile::Native),
            (
                vec!["--permission-mode=auto", "--permission-mode"],
                ClientProfile::Native,
            ),
        ] {
            assert!(
                client_profile_for_launch(
                    ClientProfile::Native,
                    &args.into_iter().map(OsString::from).collect::<Vec<_>>()
                ) == expected
            );
        }
    }

    #[test]
    fn subscription_scrubs_only_conflicting_auth_and_preserves_parent() {
        let config = read_config(&json!({"AUTOROUTER_AUTH_MODE":"subscription", "AUTOROUTER_TOKEN":"synthetic-gateway-token", "CLAUDE_CODE_STOP_HOOK_BLOCK_CAP":"2"}),false,Path::new("/")).unwrap_or_else(|e| panic!("{e}"));
        let parent: Environment = [
            ("TYPESAFE_API_KEY","synthetic-evaluator"),("ANTHROPIC_API_KEY","synthetic-api"),("CLAUDE_CODE_OAUTH_TOKEN","synthetic-oauth"),
            ("ANTHROPIC_CUSTOM_HEADERS","X-Other: retained\r\nAuthorization: Bearer stale\nX-Autorouter-Token: stale\nx-api-key: stale"),
            ("ENABLE_TOOL_SEARCH","auto:5"),
        ].into_iter().map(|(k,v)|(k.into(),v.into())).collect();
        let env = build_claude_env(&config, "http://127.0.0.1:8787", &parent);
        for key in [
            "TYPESAFE_API_KEY",
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "AUTOROUTER_TOKEN",
        ] {
            assert!(!env.contains_key(OsStr::new(key)));
        }
        assert_eq!(
            env[OsStr::new("ANTHROPIC_CUSTOM_HEADERS")],
            "X-Other: retained\nX-Autorouter-Token: synthetic-gateway-token"
        );
        assert_eq!(env[OsStr::new("ENABLE_TOOL_SEARCH")], "auto:5");
        assert_eq!(env[OsStr::new("CLAUDE_CODE_STOP_HOOK_BLOCK_CAP")], "2");
        assert!(parent.contains_key(OsStr::new("TYPESAFE_API_KEY")));
    }

    #[test]
    fn subscription_recognition_does_not_validate_provider_eligibility() {
        assert!(is_subscription_request(
            None,
            Some("bEaReR synthetic"),
            Some("other, oauth-2025-04-20")
        ));
        assert!(!is_subscription_request(
            Some("key"),
            Some("Bearer synthetic"),
            Some("oauth-test")
        ));
        assert!(!is_subscription_request(
            None,
            Some("Bearer synthetic\n"),
            Some("oauth-test")
        ));
        assert!(!is_subscription_request(
            None,
            Some("Bearer synthetic"),
            Some("not-oauth-test")
        ));
    }

    #[test]
    fn api_key_and_auto_profiles_keep_explicit_client_choices() {
        let config = read_config(
            &json!({"AUTOROUTER_CLIENT_PROFILE":"auto","AUTOROUTER_TOKEN":"synthetic-local"}),
            false,
            Path::new("/"),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let parent = [(
            OsString::from("ANTHROPIC_MODEL"),
            OsString::from("user-choice"),
        )]
        .into_iter()
        .collect();
        let env = build_claude_env(&config, "http://127.0.0.1:1", &parent);
        assert_eq!(env[OsStr::new("ANTHROPIC_MODEL")], "user-choice");
        assert_eq!(env[OsStr::new("ANTHROPIC_API_KEY")], "synthetic-local");
        assert!(!env.contains_key(OsStr::new("MAX_THINKING_TOKENS")));
    }
}
