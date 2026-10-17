//! Claude-owned authentication and launch environment adaptation.
use crate::config::{AuthMode, ClientProfile, RouterConfig, js_trim};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};

pub const LOCAL_AUTH_HEADER: &str = "x-autorouter-token";
pub type Environment = BTreeMap<OsString, OsString>;

/// Safe OS materialization failure; rejected values and causes are never stored.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ChildEnvironmentError {
    nul: bool,
}
impl std::fmt::Display for ChildEnvironmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.nul {
            "Local credential contains a NUL byte"
        } else {
            "Cannot convert local credential to process environment"
        })
    }
}
impl std::fmt::Debug for ChildEnvironmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}
impl std::error::Error for ChildEnvironmentError {}
impl From<ChildEnvironmentError> for String {
    fn from(error: ChildEnvironmentError) -> Self {
        error.to_string()
    }
}

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
) -> Result<Environment, ChildEnvironmentError> {
    let token = config
        .local_token_text()
        .map_err(|_| ChildEnvironmentError { nul: false })?;
    if token.as_deref().is_some_and(|value| value.contains('\0')) {
        return Err(ChildEnvironmentError { nul: true });
    }
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
            token.as_deref().unwrap_or("undefined")
        ));
    } else if let Some(token) = token {
        put(&mut env, "ANTHROPIC_API_KEY", token.as_ref());
        put(&mut env, "ANTHROPIC_AUTH_TOKEN", token.as_ref());
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
    Ok(env)
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
        let env = build_claude_env(&config, "http://127.0.0.1:8787", &parent).unwrap();
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
        let env = build_claude_env(&config, "http://127.0.0.1:1", &parent).unwrap();
        assert_eq!(env[OsStr::new("ANTHROPIC_MODEL")], "user-choice");
        assert_eq!(env[OsStr::new("ANTHROPIC_API_KEY")], "synthetic-local");
        assert!(!env.contains_key(OsStr::new("MAX_THINKING_TOKENS")));
    }

    fn check_child(config: &RouterConfig, expected: &str, present: bool) {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let parent: Environment = [
            ("ANTHROPIC_API_KEY".into(), "old-api".into()),
            ("ANTHROPIC_AUTH_TOKEN".into(), "old-auth".into()),
            ("CLAUDE_CODE_OAUTH_TOKEN".into(), "old-oauth".into()),
            ("TYPESAFE_API_KEY".into(), "old-jev".into()),
            ("AUTOROUTER_TOKEN".into(), "old-local".into()),
            ("KEEP".into(), "unchanged".into()),
            (
                OsString::from_vec(b"opaque-\xff".to_vec()),
                OsString::from_vec(b"value-\xfe".to_vec()),
            ),
        ]
        .into_iter()
        .collect();
        let original = parent.clone();
        let before = serde_json::to_string(config).unwrap();
        let mut env = build_claude_env(config, "http://127.0.0.1:1", &parent).unwrap();
        assert_eq!(parent, original);
        assert_eq!(serde_json::to_string(config).unwrap(), before);
        assert_eq!(
            env[OsStr::from_bytes(b"opaque-\xff")].as_bytes(),
            b"value-\xfe"
        );
        let subscription = config.auth_mode == AuthMode::Subscription;
        env.insert(
            "AR_TEST_SUBSCRIPTION".into(),
            if subscription { "1" } else { "0" }.into(),
        );
        env.insert("AR_TEST_EXPECTED".into(), expected.into());
        env.insert(
            "AR_TEST_PRESENT".into(),
            if present { "1" } else { "0" }.into(),
        );
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "auth::tests::child_environment_fixture",
                "--ignored",
            ])
            .env_clear()
            .envs(env)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stdout)
        );
    }

    #[test]
    #[ignore = "Entered only by the owned child-environment regression"]
    fn child_environment_fixture() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let expected = std::env::var("AR_TEST_EXPECTED").unwrap();
        let subscription = std::env::var("AR_TEST_SUBSCRIPTION").unwrap() == "1";
        let present = std::env::var("AR_TEST_PRESENT").unwrap() == "1";
        for key in ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"] {
            assert_eq!(
                std::env::var(key).ok().as_deref(),
                (!subscription && present).then_some(expected.as_str())
            );
        }
        assert_eq!(
            std::env::var("ANTHROPIC_CUSTOM_HEADERS").ok(),
            subscription.then(|| format!("X-Autorouter-Token: {expected}"))
        );
        assert_eq!(
            std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok().as_deref(),
            (!subscription).then_some("old-oauth")
        );
        for key in ["TYPESAFE_API_KEY", "AUTOROUTER_TOKEN"] {
            assert!(std::env::var_os(key).is_none());
        }
        assert_eq!(std::env::var("KEEP").unwrap(), "unchanged");
        assert_eq!(
            std::env::var_os(OsStr::from_bytes(b"opaque-\xff")),
            Some(OsString::from_vec(b"value-\xfe".to_vec()))
        );
    }

    #[test]
    fn effective_local_tokens_keep_source_values_at_os_materialization() {
        use crate::config::{Credential, read_config_document};
        use crate::js_json::JsDocument;
        for mode in ["api-key", "subscription"] {
            // Twenty values per mode match the retained source OS controls.
            for (raw, expected) in [
                ("null", "null"),
                ("false", "false"),
                ("0", "0"),
                (
                    r#"["synthetic-credential-value-123456789"]"#,
                    "synthetic-credential-value-123456789",
                ),
                (r#"{"synthetic":"credential-input"}"#, "[object Object]"),
                (
                    r#""synthetic-credential-value-123456789""#,
                    "synthetic-credential-value-123456789",
                ),
                (r#""""#, ""),
                ("", "undefined"),
                ("true", "true"),
                ("-0", "0"),
                ("1e400", "Infinity"),
                ("-1e400", "-Infinity"),
                (r#""prefix-\ud800-suffix""#, "prefix-\u{fffd}-suffix"),
                (r#""prefix-\udc00-suffix""#, "prefix-\u{fffd}-suffix"),
                (r#""prefix-\ud83d\ude00-suffix""#, "prefix-😀-suffix"),
                ("[]", ""),
                ("[null]", ""),
                (
                    r#"[null,[false,0,null,["synthetic"]],{}]"#,
                    ",false,0,,synthetic,[object Object]",
                ),
                (
                    "[1e-7,1e20,1e21,-0,1e400]",
                    "1e-7,100000000000000000000,1e+21,0,Infinity",
                ),
                (
                    r#"{"valueOf":false,"synthetic":"value"}"#,
                    "[object Object]",
                ),
            ] {
                let field = if raw.is_empty() {
                    String::new()
                } else {
                    format!(r#","AUTOROUTER_TOKEN":{raw}"#)
                };
                let input = format!(r#"{{"AUTOROUTER_AUTH_MODE":"{mode}"{field}}}"#);
                let document = JsDocument::parse(input.as_bytes()).unwrap();
                let config = read_config_document(&document, false, Path::new("/")).unwrap();
                check_child(&config, expected, !raw.is_empty());
            }
            for (raw, expected) in [
                (Some("null"), "null"),
                (Some("false"), "false"),
                (Some("0"), "0"),
                (None, "undefined"),
                (None, "undefined"),
                (Some(r#""""#), ""),
                (Some("{}"), "[object Object]"),
            ] {
                let mut config = read_config(&json!({"AUTOROUTER_AUTH_MODE":mode,"AUTOROUTER_TOKEN":{"synthetic":"previous-source"}}), false, Path::new("/")).unwrap();
                if raw == Some("{}") {
                    config.auth_mode = if config.auth_mode == AuthMode::Subscription {
                        AuthMode::ApiKey
                    } else {
                        AuthMode::Subscription
                    };
                } else if let Some(raw) = raw {
                    let document = JsDocument::parse(raw.as_bytes()).unwrap();
                    config
                        .replace_credential_document(
                            Credential::LocalToken,
                            &document,
                            document.root(),
                        )
                        .unwrap();
                } else {
                    config.replace_credential(Credential::LocalToken, None);
                }
                check_child(&config, expected, raw.is_some());
            }
        }
    }

    #[test]
    fn local_environment_failures_are_private_and_leave_inputs_unchanged() {
        use std::error::Error;
        for mode in ["api-key", "subscription"] {
            for raw in [
                r#"{"toString":null,"private":"synthetic-secret"}"#,
                r#"[{"toString":"synthetic-secret"}]"#,
                r#""synthetic-secret\u0000""#,
                r#"["synthetic-secret\u0000"]"#,
            ] {
                let input =
                    format!(r#"{{"AUTOROUTER_AUTH_MODE":"{mode}","AUTOROUTER_TOKEN":{raw}}}"#);
                let document = crate::js_json::JsDocument::parse(input.as_bytes()).unwrap();
                let config =
                    crate::config::read_config_document(&document, false, Path::new("/")).unwrap();
                let before = serde_json::to_string(&config).unwrap();
                let parent: Environment = [("KEEP".into(), "synthetic-parent-secret".into())]
                    .into_iter()
                    .collect();
                let original = parent.clone();
                let error = build_claude_env(&config, "http://127.0.0.1:1", &parent).unwrap_err();
                assert!(error.source().is_none());
                assert!(!format!("{error} {error:?}").contains("synthetic"));
                assert_eq!(String::from(error), error.to_string());
                assert_eq!(serde_json::to_string(&config).unwrap(), before);
                assert_eq!(parent, original);
            }
        }
    }
}
