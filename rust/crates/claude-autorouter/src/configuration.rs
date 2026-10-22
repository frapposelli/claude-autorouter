use autorouter_core::auth::Environment;
use autorouter_core::config::{
    ClientProfile, RouterConfig, js_trim, parse_session_log_dir, parse_stop_hook_block_cap,
    read_config, read_config_document,
};
use autorouter_core::model_catalog::model_capabilities;
use autorouter_core::policy::apply_policy;
use autorouter_runtime::keychain::Keychain;
use autorouter_runtime::policy::{LoadedPolicy, load_policy};
use autorouter_runtime::user_config::{
    CONFIG_KEYS, ConfigContext, LoadOptions, LoadedConfig, SECRET_CONFIG_KEYS, SaveOptions,
    environment_json, keychain_removals, load_with_policy, pretty_document,
    save_user_config_document, scalar_document,
};
use serde_json::{Map, Value, json};
use std::ffi::{OsStr, OsString};
use std::path::Path;

pub struct CommandOutput {
    pub success: bool,
    pub lines: Vec<String>,
}
fn secret(key: &str) -> bool {
    SECRET_CONFIG_KEYS.contains(&key)
}
fn provider(key: &str) -> Option<&'static str> {
    if key.starts_with("AUTOROUTER_OLLAMA_") {
        Some("ollama")
    } else if key.starts_with("AUTOROUTER_JEV_")
        || matches!(key, "AUTOROUTER_MIN_CONFIDENCE" | "TYPESAFE_API_KEY")
    {
        Some("jev")
    } else {
        None
    }
}
fn safe_text(value: &Value) -> String {
    let text = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    text.chars().filter(|c| !matches!(c,'\u{0}'..='\u{1f}'|'\u{7f}'..='\u{9f}'|'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}')).collect()
}

fn config_report(
    loaded: &LoadedConfig,
    parent: &Environment,
    cwd: &Path,
    check_all: bool,
) -> Result<Value, String> {
    let env = environment_json(&loaded.env);
    let config = read_config_document(&loaded.env_document, false, cwd)?;
    let values = json!({
        "AUTOROUTER_AUTH_MODE":config.auth_mode,"AUTOROUTER_CLIENT_PROFILE":config.client_profile,
        "AUTOROUTER_EVALUATOR":config.evaluator,"AUTOROUTER_UPSTREAM_URL":config.upstream,
        "AUTOROUTER_HAIKU_MODEL":config.models.haiku,"AUTOROUTER_SONNET_MODEL":config.models.sonnet,"AUTOROUTER_OPUS_MODEL":config.models.opus,
        "AUTOROUTER_PORT":config.port,"AUTOROUTER_TOKEN_COUNT_TIMEOUT_MS":config.token_count_timeout_ms,
        "AUTOROUTER_JEV_URL":config.jev_endpoint,"AUTOROUTER_JEV_MODEL":config.jev_model,
        "AUTOROUTER_JEV_TIMEOUT_MS":config.jev_timeout_ms,"AUTOROUTER_MIN_CONFIDENCE":config.min_confidence,
        "AUTOROUTER_OLLAMA_URL":config.ollama_endpoint,"AUTOROUTER_OLLAMA_MODEL":config.ollama_model,
        "AUTOROUTER_OLLAMA_TIMEOUT_MS":config.ollama_timeout_ms,"AUTOROUTER_OLLAMA_KEEP_ALIVE":config.ollama_keep_alive,
        "AUTOROUTER_SESSION_LOG_DIR":config.session_log_dir,"AUTOROUTER_SESSION_LOG_MODE":config.session_log_mode,
        "CLAUDE_CODE_STOP_HOOK_BLOCK_CAP":config.stop_hook_block_cap,"AUTOROUTER_STATUSLINE":env["AUTOROUTER_STATUSLINE"] != "0",
        "AUTOROUTER_DEBUG":env["AUTOROUTER_DEBUG"] == "1","ENABLE_TOOL_SEARCH":env.get("ENABLE_TOOL_SEARCH").unwrap_or(&json!("true")),
        "AUTOROUTER_SECRET_STORE":loaded.secret_store,
    });
    let evaluator = values["AUTOROUTER_EVALUATOR"].as_str().unwrap_or_default();
    let mut settings = Map::new();
    for key in CONFIG_KEYS {
        let saved = if loaded.keychain_secrets.iter().any(|k| k == key) {
            "keychain"
        } else {
            "file"
        };
        let source = if loaded.policy_locked.iter().any(|k| k == key) {
            "policy"
        } else if parent.contains_key(OsStr::new(key)) && key != "AUTOROUTER_SECRET_STORE" {
            "environment"
        } else if loaded.values.contains_key(key) {
            saved
        } else {
            "default"
        };
        let active = provider(key).is_none_or(|p| p == evaluator)
            && (key != "ANTHROPIC_API_KEY" || values["AUTOROUTER_AUTH_MODE"] == "api-key");
        let mut setting = json!({"source":source,"active":active});
        if source == "environment" && loaded.values.contains_key(key) {
            setting["overrides_file"] = json!(true);
        }
        if source == "environment" && loaded.unavailable_secrets.iter().any(|k| k == key) {
            setting["keychain_unavailable"] = json!(true);
        }
        if secret(key) {
            setting["secret"] = json!(true);
            setting["present"] = json!(env[key].as_str().is_some_and(|s| !js_trim(s).is_empty()));
        } else {
            setting["value"] = if active {
                values[key].clone()
            } else {
                Value::Null
            };
        }
        settings.insert(key.into(), setting);
    }
    let warnings = if config.client_profile == ClientProfile::Auto
        && [&config.models.sonnet, &config.models.opus]
            .iter()
            .any(|model| model_capabilities(model).is_none_or(|c| !c.shared_auto))
    {
        vec![
            "Auto permission eligibility does not guarantee automatic switching. Older targets can retain the incoming model for shared execution features.",
        ]
    } else {
        Vec::new()
    };
    let mut report = json!({"schema_version":1,"config_path":loaded.path,"config_exists":loaded.exists,"valid":true,
        "checked":if check_all {"all_evaluators"} else {"active_evaluator"},"settings":settings,"warnings":warnings});
    if let Some(path) = &loaded.policy_path {
        report["policy_path"] = json!(path);
    }
    if check_all && let Err(error) = read_config_document(&loaded.env_document, true, cwd) {
        report["valid"] = json!(false);
        report["error"] = json!(error);
    }
    Ok(report)
}

// Keep exact report restoration separate from full RouterConfig serialization:
// that serialization intentionally includes credentials. Only active, public
// settings named here may receive an exact value.
fn restore_effective_report_values(
    document: &mut autorouter_core::js_json::JsDocument,
    report: &Value,
    config: &RouterConfig,
) {
    let Some(settings) = document.get(document.root(), "settings") else {
        return;
    };
    let mut restore = |key: &str, json: String| {
        if report["settings"][key]["active"] == true
            && let Some(setting) = document.get(settings, key)
        {
            document
                .set_field_json(setting, "value", json.as_bytes())
                .expect("exact public configuration value");
        }
    };
    for (tier, key, value) in [
        ("haiku", "AUTOROUTER_HAIKU_MODEL", &config.models.haiku),
        ("sonnet", "AUTOROUTER_SONNET_MODEL", &config.models.sonnet),
        ("opus", "AUTOROUTER_OPUS_MODEL", &config.models.opus),
    ] {
        restore(
            key,
            config
                .exact_models
                .get(tier)
                .map(|value| value.stringify())
                .unwrap_or_else(|| serde_json::to_string(value).expect("model string")),
        );
    }
    restore(
        "AUTOROUTER_JEV_MODEL",
        config
            .exact_jev_model
            .as_ref()
            .map(|value| value.stringify())
            .unwrap_or_else(|| serde_json::to_string(&config.jev_model).expect("model string")),
    );
    restore(
        "AUTOROUTER_OLLAMA_KEEP_ALIVE",
        config.ollama_keep_alive_json(),
    );
    restore(
        "AUTOROUTER_SESSION_LOG_DIR",
        config
            .session_log_dir_exact()
            .map(|value| value.stringify())
            .unwrap_or_else(|| {
                serde_json::to_string(&config.session_log_dir).expect("optional path string")
            }),
    );
}

pub async fn command(
    args: &[OsString],
    context: &ConfigContext<'_>,
    keychain: &mut impl Keychain,
) -> Result<CommandOutput, String> {
    command_with_policy_loader(args, context, keychain, || {
        load_policy().map_err(|error| error.to_string())
    })
    .await
}

// Keep the production policy location and trusted owner in load_policy. The
// dependency is private and per call; there is no CLI or environment override.
async fn command_with_policy_loader(
    args: &[OsString],
    context: &ConfigContext<'_>,
    keychain: &mut impl Keychain,
    mut policy_loader: impl FnMut() -> Result<Option<LoadedPolicy>, String>,
) -> Result<CommandOutput, String> {
    let operation = args.first().and_then(|v| v.to_str()).unwrap_or_default();
    let rest = args.get(1..).unwrap_or_default();
    if operation == "show" {
        if rest.iter().any(|v| v != "--json" && v != "--check-all") {
            return Err("Usage: claude-autorouter config show [--json] [--check-all]".into());
        }
        let check_all = rest.iter().any(|v| v == "--check-all");
        let loaded = async {
            let policy = policy_loader()?;
            load_with_policy(
                context,
                &LoadOptions {
                    allow_missing: true,
                    ..Default::default()
                },
                keychain,
                policy.as_ref(),
            )
            .await
            .map_err(String::from)
        }
        .await;
        let report = match &loaded {
            Ok(loaded) => config_report(loaded,context.env,context.cwd,check_all).unwrap_or_else(|error| json!({"schema_version":1,"config_path":loaded.path,"config_exists":loaded.exists,"valid":false,"error":error})),
            Err(error) => json!({"schema_version":1,"valid":false,"error":error}),
        };
        let mut lines = Vec::new();
        if rest.iter().any(|v| v == "--json") {
            let mut exact_report = scalar_document(&report);
            if let Ok(loaded) = &loaded
                && let Some(settings) = exact_report.get(exact_report.root(), "settings")
            {
                for key in CONFIG_KEYS {
                    let Some(value) = loaded
                        .env_document
                        .get(loaded.env_document.root(), key)
                        .and_then(|node| loaded.env_document.string(node))
                    else {
                        continue;
                    };
                    let value = if key == "AUTOROUTER_SESSION_LOG_DIR" {
                        autorouter_core::config::resolve_js_path(context.cwd, value)
                    } else {
                        value.clone()
                    };
                    if value.to_scalar().is_none()
                        && report["settings"][key]["active"] == true
                        && report["settings"][key]["value"].as_str()
                            == Some(&value.to_well_formed())
                        && let Some(setting) = exact_report.get(settings, key)
                    {
                        exact_report
                            .set_field_json(setting, "value", value.stringify().as_bytes())
                            .expect("exact configuration string");
                    }
                }
                // Pure validation of the same loaded document; no file, policy
                // or Keychain reads occur while restoring report values.
                if let Ok(config) = read_config_document(&loaded.env_document, false, context.cwd) {
                    restore_effective_report_values(&mut exact_report, &report, &config);
                }
            }
            lines.push(pretty_document(&exact_report));
        } else {
            if let Ok(loaded) = &loaded {
                lines.push(format!(
                    "Config: {}{}",
                    loaded.path.to_string_lossy(),
                    if loaded.exists { "" } else { " (not saved)" }
                ));
            }
            for (key, setting) in report["settings"].as_object().into_iter().flatten() {
                let active = setting["active"] == true;
                let value = if setting["secret"] == true {
                    if setting["present"] == true {
                        "[set; hidden]".into()
                    } else {
                        "[unset]".into()
                    }
                } else if !active {
                    "[inactive]".into()
                } else if setting["value"].is_null() {
                    "[unset]".into()
                } else {
                    safe_text(&setting["value"])
                };
                lines.push(format!(
                    "{key}={value} [{}{}{}]",
                    setting["source"].as_str().unwrap_or_default(),
                    if setting["overrides_file"] == true {
                        "; overrides file"
                    } else {
                        ""
                    },
                    if active { "" } else { "; inactive" }
                ));
            }
            for warning in report["warnings"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                lines.push(warning.into());
            }
            if let Some(error) = report["error"].as_str() {
                lines.push(format!("FAIL  {error}"));
            }
        }
        return Ok(CommandOutput {
            success: report["valid"] == true,
            lines,
        });
    }
    if operation != "set" && operation != "unset" {
        return Err("Usage: claude-autorouter config show|set|unset".into());
    }
    let key = rest.first().and_then(|v| v.to_str()).unwrap_or_default();
    if !CONFIG_KEYS.contains(&key) {
        return Err("Unsupported configuration key. Run claude-autorouter config show for supported settings.".into());
    }
    let value = rest.get(1);
    if rest.len() > 2 || (operation == "unset" && value.is_some()) {
        return Err(format!(
            "Usage: claude-autorouter config {operation} KEY{}",
            if operation == "set" { " VALUE" } else { "" }
        ));
    }
    if operation == "set" && secret(key) && value.is_some_and(|v| v != "--stdin") {
        return Err("Secret values are not accepted as command arguments. Use --stdin or the hidden prompt.".into());
    }
    if operation == "set"
        && !secret(key)
        && (value.is_none() || value.is_some_and(|v| v == "--stdin"))
    {
        return Err("Nonsecret settings require a value argument.".into());
    }
    let policy = policy_loader()?;
    let loaded = load_with_policy(
        context,
        &LoadOptions {
            allow_missing: true,
            enforce_policy: false,
            ..Default::default()
        },
        keychain,
        policy.as_ref(),
    )
    .await?;
    let mut next = loaded.values.clone();
    if operation == "unset" {
        next.remove(key);
    } else {
        let value = if secret(key) {
            crate::secret_input::read_secret(key, value.is_some()).await?
        } else {
            value
                .and_then(|v| v.to_str())
                .ok_or("Configuration values must be strings.")?
                .to_owned()
        };
        next.insert(
            key.into(),
            json!(normalized_value(key, &value, context.cwd)?),
        );
    }
    let mut validate = Value::Object(next.clone());
    if operation == "set"
        && let Some(provider) = provider(key)
    {
        validate["AUTOROUTER_EVALUATOR"] = json!(provider);
    }
    read_config(&validate, false, context.cwd)?;
    if let Some(policy) = &loaded.policy {
        apply_policy(&Value::Object(next.clone()), policy, true)?;
    }
    let next_store = next
        .get("AUTOROUTER_SECRET_STORE")
        .and_then(Value::as_str)
        .unwrap_or("file")
        .to_owned();
    let remove_secrets = if key == "AUTOROUTER_SECRET_STORE" {
        keychain_removals(&loaded, &next_store, false, &Map::new())?
    } else if operation == "unset" && secret(key) && loaded.secret_store == "keychain" {
        vec![key.into()]
    } else {
        Vec::new()
    };
    save_user_config_document(
        &loaded.updated_document(&next, &[key]),
        context,
        &SaveOptions {
            overwrite: loaded.exists,
            expected_revision: Some(loaded.revision.clone()),
            remove_secrets,
        },
        keychain,
    )
    .await?;
    let mut lines = Vec::new();
    if key == "AUTOROUTER_SECRET_STORE" {
        let moved = SECRET_CONFIG_KEYS
            .iter()
            .filter(|key| next.contains_key(**key))
            .count();
        let destination = if next_store == "keychain" {
            "macOS Keychain"
        } else {
            "configuration file"
        };
        lines.push(if next_store == loaded.secret_store || moved == 0 {
            format!("Saved {key}. Saved secrets are stored in the {destination}.")
        } else {
            format!(
                "Saved {key}. Moved {moved} saved secret{} to the {destination}.",
                if moved == 1 { "" } else { "s" }
            )
        });
    } else {
        lines.push(format!(
            "{} {key}{}. Other saved settings are unchanged.",
            if operation == "unset" {
                "Removed saved"
            } else {
                "Saved"
            },
            if secret(key) && next_store == "keychain" {
                " in the macOS Keychain"
            } else {
                ""
            }
        ));
        if context.env.contains_key(OsStr::new(key)) {
            lines.push(format!("The current environment still overrides {key}; unset that environment variable to use the saved/default value."));
        }
    }
    Ok(CommandOutput {
        success: true,
        lines,
    })
}
fn normalized_value(key: &str, value: &str, cwd: &Path) -> Result<String, String> {
    if secret(key) {
        let value = js_trim(value);
        if value.is_empty() || value.chars().any(|c| matches!(c, '\r' | '\n' | '\0')) {
            return Err(format!("{key} must be a nonempty, single-line secret."));
        }
        if key == "AUTOROUTER_TOKEN" && value.encode_utf16().count() < 16 {
            return Err("AUTOROUTER_TOKEN must contain at least 16 characters.".into());
        }
        return Ok(value.into());
    }
    if key == "AUTOROUTER_SESSION_LOG_DIR" {
        return Ok(parse_session_log_dir(Some(&json!(value)), cwd)?.unwrap_or_default());
    }
    if key == "CLAUDE_CODE_STOP_HOOK_BLOCK_CAP" {
        return Ok(
            parse_stop_hook_block_cap(&json!(value), "CLAUDE_CODE_STOP_HOOK_BLOCK_CAP")?
                .to_string(),
        );
    }
    if key == "AUTOROUTER_SECRET_STORE" && value != "file" && value != "keychain" {
        return Err("AUTOROUTER_SECRET_STORE must be file or keychain.".into());
    }
    if value
        .chars()
        .any(|c| matches!(c, '\r' | '\n' | '\0' | '\u{1b}'))
    {
        return Err(format!("{key} must be a single-line setting."));
    }
    Ok(value.into())
}

#[cfg(test)]
#[path = "configuration_keychain_contracts.rs"]
mod configuration_keychain_contracts;

#[cfg(test)]
pub(crate) async fn command_with_policy_for_test(
    args: &[OsString],
    context: &ConfigContext<'_>,
    keychain: &mut impl Keychain,
    policy_loader: impl FnMut() -> Result<Option<LoadedPolicy>, String>,
) -> Result<CommandOutput, String> {
    command_with_policy_loader(args, context, keychain, policy_loader).await
}

#[cfg(test)]
mod exact_report_tests {
    use super::*;
    use autorouter_core::js_json::JsDocument;

    #[test]
    fn public_report_restoration_is_exact_private_and_replacement_aware() {
        let keep_alive = format!("{}\"5m\"{}", "[".repeat(256), "]".repeat(256));
        let source = JsDocument::parse(format!(r#"{{"AUTOROUTER_OLLAMA_KEEP_ALIVE":{keep_alive},"AUTOROUTER_HAIKU_MODEL":"synthetic-\ud800","AUTOROUTER_SESSION_LOG_DIR":"./logs-\udfff","TYPESAFE_API_KEY":"private-source-key"}}"#).as_bytes()).unwrap();
        let mut config = read_config_document(&source, false, Path::new("/tmp")).unwrap();
        let report = json!({"settings":{
            "AUTOROUTER_HAIKU_MODEL":{"active":true,"value":"display"},
            "AUTOROUTER_JEV_MODEL":{"active":false,"value":null},
            "AUTOROUTER_OLLAMA_KEEP_ALIVE":{"active":true,"value":null},
            "AUTOROUTER_SESSION_LOG_DIR":{"active":true,"value":"display"},
            "TYPESAFE_API_KEY":{"active":false,"secret":true,"present":true}
        }});
        let restore = |config: &RouterConfig| {
            let mut result = scalar_document(&report);
            restore_effective_report_values(&mut result, &report, config);
            result
        };
        let exact = restore(&config);
        let settings = exact.get(exact.root(), "settings").unwrap();
        let value = |key: &str| {
            exact
                .get(exact.get(settings, key).unwrap(), "value")
                .unwrap()
        };
        assert_eq!(
            exact.stringify_node(value("AUTOROUTER_OLLAMA_KEEP_ALIVE")),
            keep_alive
        );
        assert_eq!(
            exact.stringify_node(value("AUTOROUTER_HAIKU_MODEL")),
            r#""synthetic-\ud800""#
        );
        assert_eq!(
            exact.stringify_node(value("AUTOROUTER_SESSION_LOG_DIR")),
            r#""/tmp/logs-\udfff""#
        );
        assert_eq!(exact.stringify_node(value("AUTOROUTER_JEV_MODEL")), "null");
        let secret = exact.get(settings, "TYPESAFE_API_KEY").unwrap();
        assert_eq!(
            exact.stringify_node(secret),
            report["settings"]["TYPESAFE_API_KEY"].to_string()
        );
        assert!(!exact.stringify().contains("private-source-key"));
        config.replace_ollama_keep_alive(json!(0));
        config.replace_model("haiku", "replacement".into()).unwrap();
        config.replace_session_log_dir(None);
        let replaced = restore(&config).stringify();
        assert!(replaced.contains(r#""value":0"#));
        assert!(replaced.contains(r#""value":"replacement""#));
        assert!(!replaced.contains("\\ud800"));
        assert!(!replaced.contains("logs-"));
    }

    #[test]
    fn tool_search_report_preserves_typed_empty_values_and_precedence_without_io() {
        const KEY: &str = "ENABLE_TOOL_SEARCH";
        for (parent_value, effective, saved, locked, expected) in [
            (
                None,
                None,
                None,
                false,
                json!({"source":"default","active":true,"value":"true"}),
            ),
            (
                Some(""),
                Some(""),
                None,
                false,
                json!({"source":"environment","active":true,"value":""}),
            ),
            (
                Some("null"),
                Some("null"),
                None,
                false,
                json!({"source":"environment","active":true,"value":"null"}),
            ),
            (
                Some("false"),
                Some("false"),
                None,
                false,
                json!({"source":"environment","active":true,"value":"false"}),
            ),
            (
                Some("0"),
                Some("0"),
                None,
                false,
                json!({"source":"environment","active":true,"value":"0"}),
            ),
            (
                Some("[object Object]"),
                Some("[object Object]"),
                None,
                false,
                json!({"source":"environment","active":true,"value":"[object Object]"}),
            ),
            (
                None,
                Some(""),
                Some(""),
                false,
                json!({"source":"file","active":true,"value":""}),
            ),
            (
                Some("false"),
                Some("false"),
                Some("auto:5"),
                false,
                json!({"source":"environment","active":true,"overrides_file":true,"value":"false"}),
            ),
            (
                Some("false"),
                Some("auto:5"),
                Some("auto"),
                true,
                json!({"source":"policy","active":true,"value":"auto:5"}),
            ),
        ] {
            let parent: Environment = parent_value
                .into_iter()
                .map(|value| (KEY.into(), value.into()))
                .collect();
            let mut env = Environment::from([
                ("AUTOROUTER_AUTH_MODE".into(), "subscription".into()),
                (
                    "AUTOROUTER_TOKEN".into(),
                    "synthetic-tool-search-token".into(),
                ),
            ]);
            if let Some(value) = effective {
                env.insert(KEY.into(), value.into());
            }
            let values: Map<String, Value> = saved
                .into_iter()
                .map(|value| (KEY.into(), json!(value)))
                .collect();
            let loaded = LoadedConfig {
                env_document: scalar_document(&environment_json(&env)),
                values_document: scalar_document(&Value::Object(values.clone())),
                env,
                values,
                path: "/synthetic/tool-search-config.json".into(),
                exists: saved.is_some(),
                revision: None,
                secret_store: "file".into(),
                keychain_secrets: Vec::new(),
                unavailable_secrets: Vec::new(),
                policy: None,
                policy_path: None,
                policy_locked: if locked { vec![KEY.into()] } else { Vec::new() },
            };
            let parent_before = parent.clone();
            let env_before = loaded.env.clone();
            let values_before = loaded.values.clone();
            let document_before = loaded.env_document.stringify();
            let values_document_before = loaded.values_document.stringify();
            let locked_before = loaded.policy_locked.clone();
            for check_all in [false, true] {
                let report = config_report(&loaded, &parent, Path::new("."), check_all).unwrap();
                assert_eq!(report["valid"], true);
                assert_eq!(report["settings"][KEY], expected);
                assert_eq!(
                    report["checked"],
                    if check_all {
                        "all_evaluators"
                    } else {
                        "active_evaluator"
                    }
                );
                assert_eq!(parent, parent_before);
                assert_eq!(loaded.env, env_before);
                assert_eq!(loaded.values, values_before);
                assert_eq!(loaded.env_document.stringify(), document_before);
                assert_eq!(loaded.values_document.stringify(), values_document_before);
                assert_eq!(loaded.policy_locked, locked_before);
            }
        }
    }
}
