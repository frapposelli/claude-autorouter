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
                render_history_json(&report).map_err(|_| "Could not format session history.")?
            } else {
                message.into()
            }],
        });
    };
    let report = read_session_history(
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
        vec![render_enabled_history_json(report).map_err(|_| "Could not format session history.")?]
    } else {
        report_lines(&report, operation == "show")
    };
    Ok(CommandOutput {
        success: true,
        lines,
    })
}

fn render_history_json(report: &serde_json::Value) -> Result<String, String> {
    crate::json_output::pretty(report).map_err(|_| "Could not format session history.".into())
}

// The enabled command already owns the report. Keep this distinct from the
// generic writer and disabled-logging branch.
fn render_enabled_history_json(mut report: serde_json::Value) -> Result<String, String> {
    report["logging_enabled"] = json!(true);
    project_history_file_bytes(&mut report);
    render_history_json(&report)
}

// Native reader APIs keep exact u64 metadata. Only the owned outward report
// uses Source's ordinary Stats.size Number and its shortest decimal spelling.
fn project_history_file_bytes(report: &mut serde_json::Value) {
    fn summary(summary: &mut serde_json::Value) {
        let Some(field) = summary
            .get_mut("coverage")
            .and_then(|coverage| coverage.get_mut("file_bytes"))
        else {
            return;
        };
        if let Some(size) = field
            .as_u64()
            .filter(|size| *size > autorouter_core::config::MAX_SAFE_INTEGER)
        {
            // Every u64 is finite as f64, including u64::MAX rounding to 2^64.
            // Casting back would saturate and restore the wrong outward token.
            *field = json!(size as f64);
        }
    }
    if let Some(value) = report.get_mut("summary") {
        summary(value);
    }
    if let Some(values) = report
        .get_mut("sessions")
        .and_then(serde_json::Value::as_array_mut)
    {
        for value in values {
            summary(value);
        }
    }
}
