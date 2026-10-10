use autorouter_core::config::{
    ClientProfile, DEFAULT_OLLAMA_MODEL, RouterConfig, SessionLogMode, js_trim,
    parse_session_log_dir, parse_stop_hook_block_cap, read_config, require_keys,
    validate_ollama_model,
};
use autorouter_core::policy::apply_policy;
use autorouter_runtime::http_client::NativeHttpClient;
use autorouter_runtime::keychain::Keychain;
use autorouter_runtime::ollama_setup::{SetupOptions, setup_ollama};
use autorouter_runtime::policy::{LoadedPolicy, load_policy};
use autorouter_runtime::user_config::{
    CONFIG_KEYS, ConfigContext, LoadOptions, SECRET_CONFIG_KEYS, SaveOptions, environment_json,
    keychain_removals, load_with_policy, save_user_config_document, scalar_document,
};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::future::Future;
use tokio_util::sync::CancellationToken;

const USAGE: &str = "Usage: claude-autorouter setup [--auth-mode subscription|api-key] [--client-profile compatible|native|auto] [--evaluator jev|ollama] [--ollama-model TAG] [--ollama-timeout-ms N] [--stop-hook-block-cap N] [--session-log-dir DIR] [--session-log-mode metadata|prompts] [--secret-store file|keychain] [--pull] [--force|--replace]";
#[cfg(test)]
#[path = "onboarding_contract_tests.rs"]
mod contract_tests;
#[cfg(test)]
#[path = "onboarding_keychain_contracts.rs"]
mod keychain_contracts;
#[cfg(test)]
#[path = "policy_command_contracts.rs"]
mod policy_command_contracts;
fn value(env: &Value, key: &str, fallback: &str) -> String {
    env.get(key)
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_owned()
}
fn check_cancel(cancellation: &CancellationToken) -> Result<(), String> {
    if cancellation.is_cancelled() {
        Err("Setup cancelled".into())
    } else {
        Ok(())
    }
}
pub fn ollama_deadline_text(timeout: u64) -> String {
    if timeout == 0 {
        "routing deadline disabled".into()
    } else {
        format!("routing deadline {timeout} ms per request")
    }
}
pub fn describe_settings(config: &RouterConfig, write: &mut impl FnMut(String)) {
    if config.client_profile == ClientProfile::Auto {
        write("Auto-compatible profile: Sonnet/Opus task routing. Claude controls permission-mode availability and safety checks.".into());
    }
    if let Some(cap) = config.stop_hook_block_cap {
        write(if cap == 0 {
            "Claude Stop/SubagentStop continuation cap disabled (0).".into()
        } else {
            format!("Claude Stop/SubagentStop cap: {cap} continuations without tool use.")
        });
    }
    if config.session_log_dir.is_some() {
        write(if config.session_log_mode==SessionLogMode::Metadata{"Session logs enabled with metadata only; prompt excerpts are omitted."}else{"Session decision logs enabled; files include up to 500 characters of user prompt text per decision."}.into());
    }
}
pub async fn setup(
    args: &[OsString],
    context: &ConfigContext<'_>,
    keychain: &mut impl Keychain,
    cancellation: &CancellationToken,
    write: &mut impl FnMut(String),
) -> Result<(), String> {
    setup_with_prompt(
        args,
        context,
        keychain,
        cancellation,
        write,
        cfg!(target_os = "macos"),
        &mut NativeSecretPrompt,
    )
    .await
}

trait SecretPrompt {
    fn read(&mut self, key: &str) -> impl Future<Output = Result<String, String>>;
}

struct NativeSecretPrompt;
impl SecretPrompt for NativeSecretPrompt {
    async fn read(&mut self, key: &str) -> Result<String, String> {
        crate::secret_input::read_secret(key, false).await
    }
}

async fn setup_with_prompt(
    args: &[OsString],
    context: &ConfigContext<'_>,
    keychain: &mut impl Keychain,
    cancellation: &CancellationToken,
    write: &mut impl FnMut(String),
    macos: bool,
    prompt: &mut impl SecretPrompt,
) -> Result<(), String> {
    setup_with_dependencies(
        args,
        context,
        keychain,
        cancellation,
        write,
        macos,
        SetupDependencies {
            prompt,
            policy_loader: load_policy,
        },
    )
    .await
}

struct SetupDependencies<'a, P, L> {
    prompt: &'a mut P,
    policy_loader: L,
}

// Both production entrypoints above always use the fixed-path policy loader.
// A child test module can supply a fixture loader without a global override.
async fn setup_with_dependencies(
    args: &[OsString],
    context: &ConfigContext<'_>,
    keychain: &mut impl Keychain,
    cancellation: &CancellationToken,
    write: &mut impl FnMut(String),
    macos: bool,
    dependencies: SetupDependencies<
        '_,
        impl SecretPrompt,
        impl FnMut() -> Result<Option<LoadedPolicy>, String>,
    >,
) -> Result<(), String> {
    check_cancel(cancellation)?;
    let SetupDependencies {
        prompt,
        mut policy_loader,
    } = dependencies;
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
    let replace = args.iter().any(|a| a == "--replace");
    let merge_existing = loaded.exists && !replace;
    let exact_log_dir = if merge_existing && !args.iter().any(|a| a == "--session-log-dir") {
        loaded
            .values_document
            .get(loaded.values_document.root(), "AUTOROUTER_SESSION_LOG_DIR")
            .and_then(|node| loaded.values_document.string(node))
            .filter(|value| value.to_scalar().is_none())
            .map(|value| autorouter_core::config::resolve_js_path(context.cwd, value))
    } else {
        None
    };
    let environment = environment_json(context.env);
    let effective = if merge_existing {
        Value::Object(loaded.values.clone())
    } else {
        environment.clone()
    };
    let mut explicit_evaluator = false;
    let mut explicit_auth = false;
    let mut auth_mode = value(&effective, "AUTOROUTER_AUTH_MODE", "subscription");
    let mut profile = value(&effective, "AUTOROUTER_CLIENT_PROFILE", "compatible");
    let mut evaluator = value(&effective, "AUTOROUTER_EVALUATOR", "ollama");
    let mut model = None;
    let mut ollama_timeout = None;
    let mut stop_cap = effective.get("CLAUDE_CODE_STOP_HOOK_BLOCK_CAP").cloned();
    let mut log_dir = effective.get("AUTOROUTER_SESSION_LOG_DIR").cloned();
    let mut log_mode = effective.get("AUTOROUTER_SESSION_LOG_MODE").cloned();
    let mut secret_store = None;
    let mut pull = false;
    let mut overwrite = replace;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.to_str().unwrap_or_default() {
            "--auth-mode" => {
                auth_mode = args
                    .next()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                explicit_auth = true;
            }
            "--client-profile" => {
                profile = args
                    .next()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            }
            "--evaluator" => {
                evaluator = args
                    .next()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                explicit_evaluator = true;
            }
            "--ollama-model" => {
                model = Some(
                    args.next()
                        .ok_or("--ollama-model requires a model tag")?
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            "--ollama-timeout-ms" => {
                let raw = args
                    .next()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let raw = js_trim(&raw);
                let number = if !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()) {
                    raw.parse::<f64>().ok()
                } else {
                    None
                };
                let number=number.filter(|n|*n<=30000.0).ok_or("--ollama-timeout-ms requires an integer between 0 and 30000 (0 disables the routing deadline)")?;
                ollama_timeout = Some((number as u64).to_string());
            }
            "--stop-hook-block-cap" => {
                stop_cap = Some(json!(parse_stop_hook_block_cap(
                    &args
                        .next()
                        .map(|s| json!(s.to_string_lossy()))
                        .unwrap_or(Value::Null),
                    "--stop-hook-block-cap"
                )?))
            }
            "--session-log-dir" => {
                let raw = args
                    .next()
                    .map(|s| s.to_string_lossy().into_owned())
                    .filter(|s| !s.starts_with("--"))
                    .ok_or("--session-log-dir requires a directory path")?;
                parse_session_log_dir(Some(&json!(raw)), context.cwd).map_err(|error| {
                    error.replace("AUTOROUTER_SESSION_LOG_DIR", "--session-log-dir")
                })?;
                log_dir = Some(json!(raw));
            }
            "--session-log-mode" => {
                let raw = args.next().and_then(|s| s.to_str()).unwrap_or_default();
                if raw != "metadata" && raw != "prompts" {
                    return Err("--session-log-mode must be metadata or prompts".into());
                }
                log_mode = Some(json!(raw));
            }
            "--secret-store" => {
                let raw = args.next().and_then(|s| s.to_str()).unwrap_or_default();
                if raw != "file" && raw != "keychain" {
                    return Err("--secret-store must be file or keychain".into());
                }
                secret_store = Some(raw.to_owned());
            }
            "--pull" => pull = true,
            "--force" => overwrite = true,
            "--replace" => {}
            _ => return Err(USAGE.into()),
        }
    }
    if auth_mode != "subscription" && auth_mode != "api-key" {
        return Err("--auth-mode must be subscription or api-key".into());
    }
    if !["compatible", "native", "auto"].contains(&profile.as_str()) {
        return Err("--client-profile must be compatible, native or auto".into());
    }
    if evaluator != "jev" && evaluator != "ollama" {
        return Err("--evaluator must be jev or ollama".into());
    }
    if evaluator != "ollama" && (model.is_some() || ollama_timeout.is_some() || pull) {
        return Err(
            "Ollama model, deadline and download options require --evaluator ollama".into(),
        );
    }
    let stop_cap = stop_cap
        .as_ref()
        .map(|v| parse_stop_hook_block_cap(v, "CLAUDE_CODE_STOP_HOOK_BLOCK_CAP"))
        .transpose()?;
    let log_dir = log_dir
        .as_ref()
        .map(|v| parse_session_log_dir(Some(v), context.cwd).map(|s| s.unwrap_or_default()))
        .transpose()?;
    if !overwrite && loaded.exists {
        return Err("AutoRouter configuration already exists. Use setup --force to update it while preserving unrelated settings.".into());
    }
    write(if evaluator=="ollama"{"AutoRouter evaluates bounded prompt excerpts locally with Ollama. Complete requests still go to Anthropic."}else{"AutoRouter sends bounded prompt excerpts to TypeSafe Jev and complete requests to Anthropic."}.into());
    let mut values = if replace {
        serde_json::Map::new()
    } else {
        loaded.values.clone()
    };
    let mut replaced_keys = Vec::new();
    for key in CONFIG_KEYS {
        let selected = explicit_evaluator
            && if evaluator == "ollama" {
                key.starts_with("AUTOROUTER_OLLAMA_")
            } else {
                key.starts_with("AUTOROUTER_JEV_") || key == "AUTOROUTER_MIN_CONFIDENCE"
            };
        if (!merge_existing || selected)
            && !SECRET_CONFIG_KEYS.contains(&key)
            && let Some(value) = environment.get(key)
        {
            replaced_keys.push(key);
            values.insert(key.into(), value.clone());
        }
    }
    values.insert("AUTOROUTER_AUTH_MODE".into(), json!(auth_mode));
    values.insert("AUTOROUTER_CLIENT_PROFILE".into(), json!(profile));
    values.insert("AUTOROUTER_EVALUATOR".into(), json!(evaluator));
    if let Some(cap) = stop_cap {
        values.insert(
            "CLAUDE_CODE_STOP_HOOK_BLOCK_CAP".into(),
            json!(cap.to_string()),
        );
    }
    if let Some(directory) = log_dir {
        values.insert("AUTOROUTER_SESSION_LOG_DIR".into(), json!(directory));
    }
    if let Some(mode) = log_mode {
        values.insert("AUTOROUTER_SESSION_LOG_MODE".into(), mode);
    }
    if let Some(store) = secret_store {
        values.insert("AUTOROUTER_SECRET_STORE".into(), json!(store));
    }
    let defaulted_store =
        !values.contains_key("AUTOROUTER_SECRET_STORE") && macos && (!loaded.exists || replace);
    if defaulted_store {
        values.insert("AUTOROUTER_SECRET_STORE".into(), json!("keychain"));
    }
    if evaluator == "ollama" {
        let model = model
            .or_else(|| {
                if explicit_evaluator {
                    environment
                        .get("AUTOROUTER_OLLAMA_MODEL")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| value(&effective, "AUTOROUTER_OLLAMA_MODEL", DEFAULT_OLLAMA_MODEL));
        values.insert(
            "AUTOROUTER_OLLAMA_MODEL".into(),
            json!(validate_ollama_model(&model)?),
        );
        for key in [
            "AUTOROUTER_OLLAMA_URL",
            "AUTOROUTER_OLLAMA_TIMEOUT_MS",
            "AUTOROUTER_OLLAMA_KEEP_ALIVE",
        ] {
            if (!merge_existing || explicit_evaluator)
                && let Some(value) = environment.get(key)
            {
                values.insert(key.into(), value.clone());
            }
        }
        if let Some(timeout) = ollama_timeout {
            values.insert("AUTOROUTER_OLLAMA_TIMEOUT_MS".into(), json!(timeout));
        }
    }
    let mut keys = Vec::new();
    if evaluator == "jev" {
        keys.push("TYPESAFE_API_KEY");
    }
    if auth_mode == "api-key" {
        keys.push("ANTHROPIC_API_KEY");
    }
    read_config(&Value::Object(values.clone()), false, context.cwd)?;
    if let Some(policy) = &loaded.policy {
        apply_policy(&Value::Object(values.clone()), policy, true)?;
    }
    if values.get("AUTOROUTER_SECRET_STORE") == Some(&json!("keychain")) && !macos {
        return Err("--secret-store keychain is available only on macOS".into());
    }
    for key in &keys {
        check_cancel(cancellation)?;
        let selected = !merge_existing
            || if *key == "TYPESAFE_API_KEY" {
                explicit_evaluator
            } else {
                explicit_auth
            };
        let trim = |env: &Value| {
            env.get(*key)
                .and_then(Value::as_str)
                .map(js_trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let supplied = (if selected { trim(&environment) } else { None })
            .or_else(|| trim(&effective))
            .or_else(|| trim(&environment));
        let supplied = match supplied {
            Some(value) => value,
            None => prompt.read(key).await?,
        };
        check_cancel(cancellation)?;
        let supplied = js_trim(&supplied);
        if supplied.is_empty() || supplied.chars().any(|c| matches!(c, '\r' | '\n' | '\0')) {
            return Err(format!("{key} must be a nonempty, single-line key"));
        }
        values.insert((*key).into(), json!(supplied));
    }
    let config = read_config(&Value::Object(values.clone()), false, context.cwd)?;
    require_keys(&config)?;
    check_cancel(cancellation)?;
    describe_settings(&config, write);
    if evaluator == "ollama" {
        write(format!(
            "Local evaluator: {}; {}.",
            config.ollama_model,
            ollama_deadline_text(config.ollama_timeout_ms)
        ));
        let transport = NativeHttpClient::new()
            .map_err(|_| "Cannot initialize the local Ollama connection.")?;
        if let Err(error) = setup_ollama(
            &transport,
            &config,
            cancellation,
            &SetupOptions {
                pull,
                ..Default::default()
            },
            write,
        )
        .await
        {
            let suffix = if !explicit_evaluator && !merge_existing && !cancellation.is_cancelled() {
                " Local Ollama is the default evaluator; add --pull to download its model, or use TypeSafe Jev instead: claude-autorouter setup --evaluator jev"
            } else {
                ""
            };
            return Err(format!("{}{suffix}", error.message));
        }
    }
    check_cancel(cancellation)?;
    let mut saved_store = values
        .get("AUTOROUTER_SECRET_STORE")
        .and_then(Value::as_str)
        .unwrap_or("file")
        .to_owned();
    let options = SaveOptions {
        overwrite,
        expected_revision: Some(loaded.revision.clone()),
        remove_secrets: keychain_removals(&loaded, &saved_store, replace, &values)?,
    };
    let document_for_save = |values: &serde_json::Map<String, Value>| {
        let mut document = if replace {
            scalar_document(&Value::Object(values.clone()))
        } else {
            loaded.updated_document(values, &replaced_keys)
        };
        if let Some(path) = &exact_log_dir {
            document
                .set_root_field_json("AUTOROUTER_SESSION_LOG_DIR", path.stringify().as_bytes())
                .expect("saved path");
        }
        document
    };
    let saved_document = document_for_save(&values);
    if let Err(error) =
        save_user_config_document(&saved_document, context, &options, keychain).await
    {
        if !defaulted_store || keys.is_empty() || !error.contains("Keychain") {
            return Err(error);
        }
        write("The macOS Keychain is unavailable; saving keys in the private configuration file instead. Move them later with: claude-autorouter config set AUTOROUTER_SECRET_STORE keychain".into());
        values.remove("AUTOROUTER_SECRET_STORE");
        saved_store = "file".into();
        save_user_config_document(
            &document_for_save(&values),
            context,
            &SaveOptions {
                overwrite,
                expected_revision: Some(loaded.revision.clone()),
                remove_secrets: keychain_removals(&loaded, &saved_store, replace, &values)?,
            },
            keychain,
        )
        .await?;
    }
    write(format!(
        "Saved {auth_mode} configuration to {}",
        loaded.path.to_string_lossy()
    ));
    if !keys.is_empty() && saved_store == "keychain" {
        write("Keys are stored in the macOS Keychain; settings are stored in this file with owner-only permissions. Environment variables take precedence.".into());
    } else {
        write(format!(
            "{} stored locally in this file with owner-only permissions. Environment variables take precedence.",
            if keys.is_empty() {
                "Settings are"
            } else {
                "Keys and settings are"
            }
        ));
        if !keys.is_empty() && macos && saved_store == "file" && !defaulted_store {
            write("Keys are plaintext in this file. Move them into the macOS Keychain: claude-autorouter config set AUTOROUTER_SECRET_STORE keychain".into());
        }
    }
    write(
        "Next: claude-autorouter doctor, then claude-autorouter claude from your project.".into(),
    );
    Ok(())
}
