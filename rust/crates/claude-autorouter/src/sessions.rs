use crate::configuration::CommandOutput;
use autorouter_core::config::parse_session_log_dir;
use autorouter_core::session_history::report_lines;
use autorouter_runtime::keychain::Keychain;
use autorouter_runtime::session_history::{HistoryOptions, read_session_history};
use autorouter_runtime::user_config::{
    ConfigContext, LoadOptions, environment_json, load_user_config,
};
use serde_json::json;
use std::ffi::OsString;
use std::path::PathBuf;

#[cfg(test)]
#[path = "sessions_contracts.rs"]
mod contracts;

pub async fn command(
    args: &[OsString],
    context: &ConfigContext<'_>,
    keychain: &mut impl Keychain,
) -> Result<CommandOutput, String> {
    let operation = args.first().and_then(|a| a.to_str()).unwrap_or_default();
    let json = args.iter().any(|a| a == "--json");
    let positional: Vec<_> = args.iter().skip(1).filter(|a| *a != "--json").collect();
    if !["list", "show"].contains(&operation)
        || positional
            .iter()
            .any(|a| a.to_string_lossy().starts_with("--"))
        || if operation == "list" {
            !positional.is_empty()
        } else {
            positional.len() != 1
        }
    {
        return Err(
            "Usage: claude-autorouter sessions list [--json] | sessions show ID [--json]".into(),
        );
    }
    let loaded = load_user_config(
        context,
        &LoadOptions {
            allow_missing: true,
            read_secrets: false,
            enforce_policy: false,
        },
        keychain,
    )
    .await?;
    let env = environment_json(&loaded.env);
    let directory = parse_session_log_dir(env.get("AUTOROUTER_SESSION_LOG_DIR"), context.cwd)?;
    let Some(directory) = directory else {
        let message = "Session logging is disabled. Set AUTOROUTER_SESSION_LOG_DIR to record future sessions.";
        let report = json!({"schema_version":1,"type":"session_history","logging_enabled":false,"sessions":[],"message":message});
        return Ok(CommandOutput {
            success: operation == "list",
            lines: vec![if json {
                serde_json::to_string_pretty(&report)
                    .map_err(|_| "Could not format session history.")?
            } else {
                message.into()
            }],
        });
    };
    let mut report = read_session_history(
        PathBuf::from(directory),
        HistoryOptions {
            id: if operation == "show" {
                Some(positional[0].to_string_lossy().into_owned())
            } else {
                None
            },
            ..Default::default()
        },
    )
    .await?;
    let lines = if json {
        report["logging_enabled"] = json!(true);
        vec![
            serde_json::to_string_pretty(&report)
                .map_err(|_| "Could not format session history.")?,
        ]
    } else {
        report_lines(&report, operation == "show")
    };
    Ok(CommandOutput {
        success: true,
        lines,
    })
}
