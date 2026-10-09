use autorouter_core::auth::{
    Environment, LOCAL_AUTH_HEADER, build_claude_env, conflicting_providers,
};
use autorouter_core::config::{AuthMode, Evaluator, js_trim, read_config_document, require_keys};
use autorouter_runtime::http_client::NativeHttpClient;
use autorouter_runtime::keychain::Keychain;
use autorouter_runtime::ollama_setup::inspect_ollama;
use autorouter_runtime::user_config::{
    ConfigContext, LoadOptions, SECRET_CONFIG_KEYS, load_user_config,
};
use regex::Regex;
use serde_json::Value;
use std::ffi::OsStr;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::sync::CancellationToken;

pub async fn evaluate_local(
    context: &ConfigContext<'_>,
    keychain: &mut impl Keychain,
    cancellation: &CancellationToken,
    json_output: bool,
    write: &mut impl FnMut(String),
) -> Result<bool, String> {
    use autorouter_runtime::local_diagnostic::{
        format_local_diagnostic, run_local_diagnostic_with_progress,
    };
    let result=async {
        let loaded=load_user_config(context,&LoadOptions::default(),keychain).await?;
        let config=read_config_document(&loaded.env_document,false,context.cwd)?;
        if config.evaluator!=Evaluator::Ollama{return Err("Local evaluation requires AUTOROUTER_EVALUATOR=ollama. It does not call Jev or Anthropic.".into());}
        let transport=std::sync::Arc::new(NativeHttpClient::new().map_err(|_|"Cannot initialize the local Ollama connection.")?);
        run_local_diagnostic_with_progress(transport,&config,cancellation,|progress|{
            if !json_output{
                let message=match progress["event"].as_str(){
                    Some("preflight")=>Some("Checking the local evaluator; no Claude authentication or cloud requests are used."),
                    Some("startup")=>Some("Preparing the local model with a separate 60-second startup deadline…"),
                    Some("case_start")=>Some("Checking a synthetic routing case…"),
                    Some("case_complete")=>Some("Synthetic routing case finished."),_=>None,
                };
                if let Some(message)=message{write(message.into());}
            }
            Ok(())
        }).await.map_err(|error|error.to_string())
    }.await;
    match result {
        Ok(report) => {
            if json_output {
                write(report.to_string());
            } else {
                for line in format_local_diagnostic(&report) {
                    write(line);
                }
            }
            Ok(report["passed"] == true)
        }
        Err(error) => {
            if cancellation.is_cancelled() {
                return Err(error);
            }
            if json_output {
                write(serde_json::json!({"schema_version":1,"type":"local_evaluator_diagnostic","passed":false,"error":{"code":"configuration_error","message":error}}).to_string());
            } else {
                write(format!("FAIL  {error}"));
            }
            Ok(false)
        }
    }
}

async fn bounded(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>, ()> {
    let mut bytes = Vec::new();
    reader
        .take(65537)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| ())?;
    if bytes.len() > 65536 {
        Err(())
    } else {
        Ok(bytes)
    }
}
async fn claude_output(
    args: &[&str],
    env: &Environment,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, ()> {
    let mut child = tokio::process::Command::new("claude")
        .args(args)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| ())?;
    let stdout = child.stdout.take().ok_or(())?;
    let stderr = child.stderr.take().ok_or(())?;
    let work = async {
        let (stdout, _, status) = tokio::try_join!(bounded(stdout), bounded(stderr), async {
            child.wait().await.map_err(|_| ())
        })?;
        if status.success() {
            Ok(stdout)
        } else {
            Err(())
        }
    };
    let result = tokio::select! {biased;_=cancellation.cancelled()=>Err(()),_=tokio::time::sleep(Duration::from_secs(10))=>Err(()),result=work=>result};
    if result.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    result
}
fn report(
    healthy: &mut bool,
    write: &mut impl FnMut(String),
    okay: bool,
    message: impl AsRef<str>,
) {
    if !okay {
        *healthy = false;
    }
    write(format!(
        "{}  {}",
        if okay { "OK" } else { "FAIL" },
        message.as_ref()
    ));
}

pub async fn doctor(
    context: &ConfigContext<'_>,
    keychain: &mut impl Keychain,
    cancellation: &CancellationToken,
    write: &mut impl FnMut(String),
) -> Result<bool, String> {
    doctor_for_platform(
        context,
        keychain,
        cancellation,
        write,
        cfg!(target_os = "macos"),
    )
    .await
}

pub(super) async fn doctor_for_platform(
    context: &ConfigContext<'_>,
    keychain: &mut impl Keychain,
    cancellation: &CancellationToken,
    write: &mut impl FnMut(String),
    macos: bool,
) -> Result<bool, String> {
    let mut healthy = true;
    report(
        &mut healthy,
        write,
        true,
        format!(
            "AutoRouter {} native ({}-{})",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::ARCH,
            std::env::consts::OS
        ),
    );
    let mut config = None;
    let mut effective = context.env.clone();
    let configured=async{
        let loaded=load_user_config(context,&LoadOptions::default(),keychain).await?;
        effective=loaded.env;
        write(format!("Config: {}{}",loaded.path.to_string_lossy(),if loaded.exists{""}else{" (absent; using environment)"}));
        if let Some(path)=loaded.policy_path{write(format!("Organization policy: {} (locks: {}).",path.to_string_lossy(),if loaded.policy_locked.is_empty(){"none".into()}else{loaded.policy_locked.iter().map(|key|key.replacen("AUTOROUTER_","",1)).collect::<Vec<_>>().join(", ")}));}
        if loaded.secret_store=="keychain"{write(format!("Saved secrets: macOS Keychain ({} found).",loaded.keychain_secrets.len()));}
        else if macos && SECRET_CONFIG_KEYS.iter().any(|key|loaded.values.contains_key(*key)){write("WARN  Saved keys are plaintext in the configuration file. Move them into the macOS Keychain: claude-autorouter config set AUTOROUTER_SECRET_STORE keychain".into());}
        config=Some(read_config_document(&loaded.env_document,false,context.cwd)?);
        require_keys(config.as_ref().unwrap())?;
        Ok::<_,String>(())
    }.await;
    match configured {
        Ok(()) => report(
            &mut healthy,
            write,
            true,
            format!(
                "Configuration and required keys present ({})",
                if config
                    .as_ref()
                    .is_some_and(|c| c.auth_mode == AuthMode::Subscription)
                {
                    "subscription"
                } else {
                    "api-key"
                }
            ),
        ),
        Err(error) => report(
            &mut healthy,
            write,
            false,
            format!(
                "{error}. Use claude-autorouter config show --check-all to inspect settings, or setup for first-time configuration."
            ),
        ),
    }
    for key in conflicting_providers(&effective) {
        report(
            &mut healthy,
            write,
            false,
            format!("Unset {key}; AutoRouter uses the Anthropic Messages API"),
        );
    }
    if let Some(config) = &config {
        if let Some(cap) = config.stop_hook_block_cap {
            write(if cap == 0 {
                "Claude Stop/SubagentStop continuation cap disabled (0).".into()
            } else {
                format!("Claude Stop/SubagentStop cap: {cap} continuations without tool use.")
            });
        }
        let profile = serde_json::to_value(config.client_profile).unwrap_or(Value::Null);
        write(format!(
            "Routing profile: {}; Haiku {}; Sonnet {}; Opus {}.",
            profile.as_str().unwrap_or_default(),
            config.models.haiku,
            config.models.sonnet,
            config.models.opus
        ));
        if config.evaluator == Evaluator::Jev {
            write(format!(
                "Evaluator: Jev {}; routing deadline {} ms; confidence floor {}.",
                config.jev_model, config.jev_timeout_ms, config.min_confidence
            ));
        }
        // The cap was printed above in the same position as the established
        // doctor report; the remaining descriptions are shared with setup.
        let mut description = config.clone();
        description.stop_hook_block_cap = None;
        crate::onboarding::describe_settings(&description, write);
        if config.evaluator == Evaluator::Ollama {
            write(format!(
                "Local evaluator: {}; {}.",
                config.ollama_model,
                crate::onboarding::ollama_deadline_text(config.ollama_timeout_ms)
            ));
            write("Model availability is checked below; classification speed and accuracy are not tested.".into());
            let inspection=match NativeHttpClient::new(){Ok(transport)=>inspect_ollama(&transport,config,cancellation,5000).await.map_err(|e|e.message),Err(_)=>Err("Cannot reach local Ollama. Install Ollama from https://ollama.com/download and start it, then retry.".into())};
            match inspection {
                Ok(result) => report(
                    &mut healthy,
                    write,
                    result.installed,
                    if result.installed {
                        format!("Local Ollama model available ({})", result.model)
                    } else {
                        format!(
                            "Ollama model missing ({}). Run claude-autorouter setup --evaluator ollama --ollama-model {} --pull --force.",
                            result.model, result.model
                        )
                    },
                ),
                Err(error) => report(&mut healthy, write, false, error),
            }
        }
    }
    let mut child_env = if let Some(config) = &config {
        let mut config = config.clone();
        config.local_token = Some(String::new());
        build_claude_env(&config, "http://127.0.0.1:1", &effective)
    } else {
        effective
    };
    for key in [
        "ANTHROPIC_BASE_URL",
        "AUTOROUTER_CONFIG",
        "TYPESAFE_API_KEY",
        "AUTOROUTER_TOKEN",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "AUTOROUTER_STATUS_FILE",
    ] {
        child_env.remove(OsStr::new(key));
    }
    let headers = child_env
        .get(OsStr::new("ANTHROPIC_CUSTOM_HEADERS"))
        .map(|value| value.to_string_lossy())
        .unwrap_or_default()
        .lines()
        .filter(|line| {
            !js_trim(line).is_empty()
                && ![LOCAL_AUTH_HEADER, "authorization", "x-api-key"].contains(
                    &js_trim(line.split(':').next().unwrap_or_default())
                        .to_ascii_lowercase()
                        .as_str(),
                )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if headers.is_empty() {
        child_env.remove(OsStr::new("ANTHROPIC_CUSTOM_HEADERS"));
    } else {
        child_env.insert("ANTHROPIC_CUSTOM_HEADERS".into(), headers.into());
    }
    let mut available = false;
    match claude_output(&["--version"], &child_env, cancellation).await {
        Ok(stdout) => {
            let stdout = String::from_utf8_lossy(&stdout);
            let regex = Regex::new(r"(?-u:\b)[0-9]+\.[0-9]+\.[0-9]+(?-u:\b)").unwrap();
            let version = regex.find(&stdout).map(|m| m.as_str());
            report(
                &mut healthy,
                write,
                version.is_some(),
                version
                    .map(|v| format!("Claude Code {v}"))
                    .unwrap_or_else(|| "Could not recognize Claude Code version".into()),
            );
            if let Some(version) = version {
                write(format!(
                    "Historical integration observations cover Claude Code 2.1.284 and 2.1.285; finding an executable does not certify its full compatibility{}",
                    if ["2.1.284", "2.1.285"].contains(&version) {
                        "."
                    } else {
                        " (installed version differs)."
                    }
                ));
            }
            available = version.is_some();
        }
        Err(()) => report(
            &mut healthy,
            write,
            false,
            "Claude Code unavailable. Install claude and ensure it is on PATH.",
        ),
    }
    if available && config.is_some_and(|c| c.auth_mode == AuthMode::Subscription) {
        let status = claude_output(&["auth", "status", "--json"], &child_env, cancellation)
            .await
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).map_err(|_| ()));
        match status {
            Ok(status) => {
                let subscription =
                    status["loggedIn"] == true && status["authMethod"] == "claude.ai";
                report(
                    &mut healthy,
                    write,
                    subscription,
                    if subscription {
                        "Claude subscription login found"
                    } else {
                        "Claude subscription login not found. Run claude auth login."
                    },
                );
            }
            Err(()) => report(
                &mut healthy,
                write,
                false,
                "Could not verify Claude subscription login. Run claude auth login (or update Claude Code).",
            ),
        }
    }
    write("Local checks only; external provider connectivity, key validity, Anthropic model access, and quota are not tested.".into());
    Ok(healthy)
}
