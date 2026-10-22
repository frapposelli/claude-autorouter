#![cfg(unix)]
mod support;
use serde_json::{Value, json};
use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use support::{Home, hidden_secret, output, success};

#[test]
fn show_preserves_saved_bytes_and_reports_complete_redacted_provenance() {
    let home = Home::new();
    home.save(&json!({"AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_PORT":"8123","AUTOROUTER_EVALUATOR":"jev","TYPESAFE_API_KEY":"private-saved-key","AUTOROUTER_TOKEN":"private-token-value","ANTHROPIC_API_KEY":"private-unused-key"}));
    let before = home.saved_bytes();
    for json_output in [true, false] {
        let mut command = home.command();
        command
            .args(["config", "show"])
            .env("AUTOROUTER_PORT", "9123")
            .env("TYPESAFE_API_KEY", "private-environment-key");
        if json_output {
            command.arg("--json");
        }
        let text = success(&output(&mut command));
        assert!(!text.contains("private-"));
        if json_output {
            let report: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                report["settings"]["AUTOROUTER_PORT"],
                json!({"source":"environment","active":true,"overrides_file":true,"value":9123})
            );
            assert_eq!(report["settings"]["AUTOROUTER_AUTH_MODE"]["source"], "file");
            assert_eq!(
                report["settings"]["AUTOROUTER_JEV_MODEL"]["source"],
                "default"
            );
            assert_eq!(
                report["settings"]["TYPESAFE_API_KEY"],
                json!({"source":"environment","active":true,"overrides_file":true,"secret":true,"present":true})
            );
            assert_eq!(report["settings"]["ANTHROPIC_API_KEY"]["active"], false);
        } else {
            assert!(text.contains("TYPESAFE_API_KEY=[set; hidden]"));
        }
        assert_eq!(home.saved_bytes(), before);
    }
}

#[test]
fn show_without_saved_settings_and_inactive_full_checks_are_read_only() {
    let home = Home::new();
    let text = success(&output(home.command().args(["config", "show", "--json"])));
    let report: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(report["config_exists"], false);
    assert!(!home.config().exists());
    home.save(&json!({"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_JEV_URL":"https://private-user:private-secret@example.test"}));
    let before = home.saved_bytes();
    let text = success(&output(home.command().args(["config", "show", "--json"])));
    let report: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        report["settings"]["AUTOROUTER_JEV_URL"],
        json!({"source":"file","active":false,"value":null})
    );
    let result = output(
        home.command()
            .args(["config", "show", "--check-all", "--json"]),
    );
    assert!(!result.status.success());
    assert!(result.stderr.is_empty());
    let text = String::from_utf8(result.stdout).unwrap();
    let report: Value = serde_json::from_str(&text).unwrap();
    assert!(
        report["error"]
            .as_str()
            .unwrap()
            .contains("AUTOROUTER_JEV_URL")
    );
    assert!(!text.contains("private-"));
    assert_eq!(home.saved_bytes(), before);
}

#[test]
fn set_and_unset_change_only_the_named_saved_value_despite_runtime_overrides() {
    let home = Home::new();
    let saved = json!({"AUTOROUTER_AUTH_MODE":"subscription","TYPESAFE_API_KEY":"test-secret","AUTOROUTER_PORT":"8123","ENABLE_TOOL_SEARCH":"auto:5"});
    home.save(&saved);
    let text = success(&output(
        home.command()
            .args(["config", "set", "AUTOROUTER_PORT", "0"])
            .env("AUTOROUTER_PORT", "9123")
            .env("AUTOROUTER_DEBUG", "1"),
    ));
    let mut expected = saved.clone();
    expected["AUTOROUTER_PORT"] = json!("0");
    assert_eq!(home.saved(), expected);
    assert!(text.contains("environment still overrides AUTOROUTER_PORT"));
    assert_eq!(
        fs::metadata(home.config()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    success(&output(home.command().args([
        "config",
        "unset",
        "AUTOROUTER_PORT",
    ])));
    expected.as_object_mut().unwrap().remove("AUTOROUTER_PORT");
    assert_eq!(home.saved(), expected);
}

#[test]
fn secret_argument_stdin_and_hidden_edits_preserve_files_and_never_echo_values() {
    let home = Home::new();
    let result =
        output(
            home.command()
                .args(["config", "set", "TYPESAFE_API_KEY", "private-argv-secret"]),
        );
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    let error = String::from_utf8(result.stderr).unwrap();
    assert!(error.contains("not accepted"));
    assert!(!error.contains("private-argv-secret"));
    assert!(!home.config().exists());
    let stdin = home.write("stdin.txt", b"private-stdin-secret\n");
    let text = success(&output(
        home.command()
            .args(["config", "set", "TYPESAFE_API_KEY", "--stdin"])
            .stdin(File::open(&stdin).unwrap()),
    ));
    assert!(!text.contains("private-"));
    let result = hidden_secret(
        home.command().args(["config", "set", "ANTHROPIC_API_KEY"]),
        "ANTHROPIC_API_KEY",
        "private-hidden-secret",
    );
    assert!(result.status.success());
    assert!(!String::from_utf8_lossy(&result.stdout).contains("private-"));
    assert!(!String::from_utf8_lossy(&result.stderr).contains("private-"));
    assert_eq!(
        home.saved(),
        json!({"TYPESAFE_API_KEY":"private-stdin-secret","ANTHROPIC_API_KEY":"private-hidden-secret"})
    );
    let before = home.saved_bytes();
    for input in [b"a\nb".to_vec(), Vec::new(), vec![b'x'; 16385]] {
        home.write("stdin.txt", input);
        let result = output(
            home.command()
                .args(["config", "set", "TYPESAFE_API_KEY", "--stdin"])
                .stdin(File::open(&stdin).unwrap()),
        );
        assert!(!result.status.success());
        assert_eq!(home.saved_bytes(), before);
        assert!(!String::from_utf8_lossy(&result.stderr).contains("private-"));
    }
}

#[test]
fn every_invalid_edit_preserves_saved_bytes_and_hides_url_or_unknown_key_values() {
    let home = Home::new();
    home.save(&json!({"AUTOROUTER_PORT":"8123"}));
    let before = home.saved_bytes();
    for (key, value) in [
        ("AUTOROUTER_PORT", ""),
        ("AUTOROUTER_MIN_CONFIDENCE", " "),
        ("AUTOROUTER_DEBUG", "true"),
        (
            "AUTOROUTER_JEV_URL",
            "https://private-user:private-secret@example.test",
        ),
        ("AUTOROUTER_UPSTREAM_URL", "private-invalid-url"),
        ("AUTOROUTER_SONNET_MODEL", ""),
        ("AUTOROUTER_OLLAMA_TIMEOUT_MS", ""),
        ("private-unknown-key", "value"),
    ] {
        let result = output(home.command().args(["config", "set", key, value]));
        assert!(!result.status.success());
        assert!(!String::from_utf8_lossy(&result.stderr).contains("private-"));
        assert!(result.stdout.is_empty());
        assert_eq!(home.saved_bytes(), before);
    }
}

#[test]
fn local_token_secret_edits_enforce_utf16_minimum_and_redacted_presence() {
    const SHORT: &str = "synthetic-12345";
    const MINIMUM: &str = "synthetic-123456";
    const ASTRAL: &str = "🦀🦀🦀🦀🦀🦀🦀🦀";
    assert_eq!(SHORT.encode_utf16().count(), 15);
    assert_eq!(MINIMUM.encode_utf16().count(), 16);
    assert_eq!(
        (ASTRAL.chars().count(), ASTRAL.encode_utf16().count()),
        (8, 16)
    );
    let home = Home::new();
    let original = json!({"AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_EVALUATOR":"ollama",
        "AUTOROUTER_SECRET_STORE":"file","AUTOROUTER_PORT":"8123","ENABLE_TOOL_SEARCH":"auto:5"});
    home.save(&original);
    let before = home.saved_bytes();
    let private = |result: &std::process::Output| {
        for bytes in [&result.stdout, &result.stderr] {
            let text = String::from_utf8_lossy(bytes);
            assert!(!text.contains("synthetic-") && !text.contains('🦀'));
        }
    };
    let stdin = home.write("local-token-stdin.txt", format!("{SHORT}\n"));
    let result = output(
        home.command()
            .args(["config", "set", "AUTOROUTER_TOKEN", "--stdin"])
            .stdin(File::open(&stdin).unwrap()),
    );
    private(&result);
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    assert_eq!(
        result.stderr,
        b"AUTOROUTER_TOKEN must contain at least 16 characters.\n"
    );
    assert_eq!(home.saved_bytes(), before);

    let mut expected = original.clone();
    for value in [MINIMUM, ASTRAL] {
        home.write("local-token-stdin.txt", format!("{value}\n"));
        let result = output(
            home.command()
                .args(["config", "set", "AUTOROUTER_TOKEN", "--stdin"])
                .stdin(File::open(&stdin).unwrap()),
        );
        private(&result);
        assert_eq!(
            success(&result),
            "Saved AUTOROUTER_TOKEN. Other saved settings are unchanged.\n"
        );
        expected["AUTOROUTER_TOKEN"] = json!(value);
        assert_eq!(home.saved(), expected);
        assert_eq!(
            fs::metadata(home.config()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let saved = home.saved_bytes();
        for environment_override in [false, true] {
            let mut command = home.command();
            command.args(["config", "show", "--json"]);
            if environment_override {
                command.env("AUTOROUTER_TOKEN", "");
            }
            let result = output(&mut command);
            private(&result);
            let report: Value = serde_json::from_str(&success(&result)).unwrap();
            assert_eq!(report["valid"], true);
            assert_eq!(
                report["settings"]["AUTOROUTER_TOKEN"],
                if environment_override {
                    json!({"source":"environment","active":true,"overrides_file":true,"secret":true,"present":false})
                } else {
                    json!({"source":"file","active":true,"secret":true,"present":true})
                }
            );
            assert_eq!(home.saved_bytes(), saved);
        }
    }
    let result = output(home.command().args(["config", "unset", "AUTOROUTER_TOKEN"]));
    private(&result);
    assert_eq!(
        success(&result),
        "Removed saved AUTOROUTER_TOKEN. Other saved settings are unchanged.\n"
    );
    assert_eq!(home.saved(), original);
    assert_eq!(
        fs::metadata(home.config()).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn ollama_model_and_keep_alive_show_set_and_inactive_provenance_are_read_only() {
    use std::os::unix::fs::MetadataExt;
    let home = Home::new();
    let defaults: Value = serde_json::from_str(&success(&output(
        home.command().args(["config", "show", "--json"]),
    )))
    .unwrap();
    for (key, value) in [
        ("AUTOROUTER_OLLAMA_MODEL", "nimble:9b-q4_K_M"),
        ("AUTOROUTER_OLLAMA_KEEP_ALIVE", "5m"),
    ] {
        assert_eq!(
            defaults["settings"][key],
            json!({"source":"default","active":true,"value":value})
        );
    }
    assert!(!home.config().exists());
    for (key, saved_value, environment_value, error) in [
        (
            "AUTOROUTER_OLLAMA_MODEL",
            "team/decision-model:v1",
            "team/override:v2",
            "Ollama requires a valid local model tag; cloud models are not supported.",
        ),
        (
            "AUTOROUTER_OLLAMA_KEEP_ALIVE",
            "9999h",
            "0",
            "AUTOROUTER_OLLAMA_KEEP_ALIVE must be 0 or a positive duration such as 5m",
        ),
    ] {
        let base = json!({"AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_PORT":"8123"});
        home.save(&base);
        let text = success(&output(
            home.command()
                .args(["config", "set", key, saved_value])
                .env(key, environment_value),
        ));
        assert!(text.contains(&format!("environment still overrides {key}")));
        let mut expected = base.clone();
        expected[key] = json!(saved_value);
        assert_eq!(home.saved(), expected);
        let before = home.saved_bytes();
        let metadata = fs::metadata(home.config()).unwrap();
        let identity = (
            metadata.dev(),
            metadata.ino(),
            metadata.permissions().mode() & 0o777,
        );
        assert_eq!(identity.2, 0o600);
        for override_value in [None, Some(environment_value), Some("")] {
            let mut command = home.command();
            command.args(["config", "show", "--json"]);
            if let Some(value) = override_value {
                command.env(key, value);
            }
            let result = output(&mut command);
            assert!(result.stderr.is_empty());
            let report: Value = serde_json::from_slice(&result.stdout).unwrap();
            if override_value == Some("") {
                assert!(!result.status.success());
                assert_eq!(report["valid"], false);
                assert_eq!(report["error"], error);
                assert!(report.get("settings").is_none());
            } else {
                assert!(result.status.success());
                assert_eq!(report["valid"], true);
                let setting = if let Some(value) = override_value {
                    json!({"source":"environment","active":true,"overrides_file":true,"value":value})
                } else {
                    json!({"source":"file","active":true,"value":saved_value})
                };
                assert_eq!(report["settings"][key], setting);
            }
            let metadata = fs::metadata(home.config()).unwrap();
            assert_eq!(
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.permissions().mode() & 0o777
                ),
                identity
            );
            assert_eq!(home.saved_bytes(), before);
        }
        let result = output(
            home.command()
                .args(["config", "set", key, ""])
                .env(key, environment_value),
        );
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert!(String::from_utf8(result.stderr).unwrap().contains(error));
        assert_eq!(home.saved_bytes(), before);
        success(&output(home.command().args(["config", "unset", key])));
        assert_eq!(home.saved(), base);
        let saved =
            json!({"AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_AUTH_MODE":"subscription",(key):""});
        home.save(&saved);
        let before = home.saved_bytes();
        for check_all in [false, true] {
            let mut command = home.command();
            command.args(["config", "show", "--json"]).env(key, "");
            if check_all {
                command.arg("--check-all");
            }
            let result = output(&mut command);
            assert!(result.stderr.is_empty());
            assert_eq!(result.status.success(), !check_all);
            let report: Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(
                report["checked"],
                if check_all {
                    "all_evaluators"
                } else {
                    "active_evaluator"
                }
            );
            assert_eq!(report["valid"], !check_all);
            assert_eq!(
                report["settings"][key],
                json!({"source":"environment","active":false,"overrides_file":true,"value":null})
            );
            if check_all {
                assert_eq!(report["error"], error);
            } else {
                assert!(report.get("error").is_none());
            }
            assert_eq!(home.saved_bytes(), before);
        }
    }
}

#[test]
fn session_log_directory_set_show_empty_override_and_validation_preserve_provenance() {
    const KEY: &str = "AUTOROUTER_SESSION_LOG_DIR";
    const ERROR: &str = "AUTOROUTER_SESSION_LOG_DIR must be a directory path, or an empty string to disable session logging";
    let home = Home::new();
    let show = |environment: Option<&str>| {
        let before = home.config().exists().then(|| home.saved_bytes());
        let mut command = home.command();
        command.args(["config", "show", "--json"]);
        if let Some(value) = environment {
            command.env(KEY, value);
        }
        let result = output(&mut command);
        assert!(result.status.success());
        assert!(result.stderr.is_empty());
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["valid"], true);
        assert_eq!(home.config().exists(), before.is_some());
        if let Some(bytes) = before {
            assert_eq!(home.saved_bytes(), bytes);
        }
        report
    };
    assert_eq!(
        show(None)["settings"][KEY],
        json!({"source":"default","active":true,"value":null})
    );
    let base = json!({"AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_PORT":"8123"});
    home.save(&base);
    let expected_path = fs::canonicalize(&home.0).unwrap().join("saved\u{85}logs");
    let text = success(&output(
        home.command()
            .args(["config", "set", KEY, "./discard/../saved\u{85}logs"])
            .env(KEY, "./runtime logs"),
    ));
    assert!(text.contains("environment still overrides AUTOROUTER_SESSION_LOG_DIR"));
    let mut expected = base.clone();
    expected[KEY] = json!(expected_path);
    assert_eq!(home.saved(), expected);
    assert_eq!(
        show(None)["settings"][KEY],
        json!({"source":"file","active":true,"value":expected_path})
    );
    assert_eq!(
        show(Some(""))["settings"][KEY],
        json!({"source":"environment","active":true,"overrides_file":true,"value":null})
    );
    let before = home.saved_bytes();
    let result = output(
        home.command()
            .args(["config", "set", KEY, "./private\u{7f}logs"])
            .env(KEY, "./runtime logs"),
    );
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert_eq!(String::from_utf8(result.stderr).unwrap().trim(), ERROR);
    assert_eq!(home.saved_bytes(), before);
    success(&output(home.command().args(["config", "set", KEY, ""])));
    expected[KEY] = json!("");
    assert_eq!(home.saved(), expected);
    assert_eq!(
        show(None)["settings"][KEY],
        json!({"source":"file","active":true,"value":null})
    );
    success(&output(home.command().args(["config", "unset", KEY])));
    assert_eq!(home.saved(), base);
    assert_eq!(
        fs::metadata(home.config()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!expected_path.exists());
    assert!(!home.0.join("discard").exists());
    assert!(!home.0.join("runtime logs").exists());
}

// The exact saved path and its UTF-8 filesystem projection are separate views.
// Both CLI implementations must read the same private file without rewriting it.
#[test]
fn saved_utf16_session_path_keeps_exact_report_and_reads_projected_history() {
    use autorouter_core::js_json::{JsDocument, JsNode};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    const KEY: &str = "AUTOROUTER_SESSION_LOG_DIR";
    const ID: &str = "autorouter-session-path-read";
    const ROW: &str = r#"{"schema_version":2,"event":"decision","timestamp":"2026-10-18T00:00:00.000Z","request_id":"path-request","session_id":"synthetic-path-session","requested_model":"claude-opus-5-5","selected_model":"claude-sonnet-5-5"}"#;
    let home = Home::new();
    let cwd = fs::canonicalize(&home.0).unwrap();
    let directory = home.0.join("actual-logs-\u{fffd}");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let bytes = format!("{ROW}\n").into_bytes();
    let file = home.write(&format!("actual-logs-\u{fffd}/{ID}.jsonl"), &bytes);
    let identity = |path: &std::path::Path| {
        let m = fs::symlink_metadata(path).unwrap();
        assert!(!m.file_type().is_symlink());
        if m.is_file() {
            assert_eq!(m.nlink(), 1);
        }
        (
            m.dev(),
            m.ino(),
            m.len(),
            m.permissions().mode() & 0o777,
            m.modified().unwrap(),
        )
    };
    let file_before = identity(&file);
    assert_eq!(file_before.3, 0o600);
    let directory_before = identity(&directory);
    assert_eq!(directory_before.3, 0o700);
    let limits = json!({"maxFiles":100,"maxDirectoryEntries":10000,"maxFileBytes":4194304,
        "maxTotalBytes":16777216,"maxLineBytes":16384,"maxRecords":5000,"maxLines":10000});
    let mut summary: Value = serde_json::from_str(r#"{
        "id":"autorouter-session-path-read","session_id":"synthetic-path-session",
        "started_at":"2026-10-18T00:00:00.000Z","updated_at":"2026-10-18T00:00:00.000Z",
        "requests":1,"decisions":1,"outcomes":0,"completed":0,"failed":0,"cancelled":0,
        "pending":1,"unconfirmed":0,"outcome_only":0,"selected_models":{"claude-sonnet-5-5":1},
        "confirmed_models":{},"sources":{},"routing_reasons":{},"classifier_errors":{},
        "fallbacks":0,"fallback_rate":0,"decision_latency_ms":{"samples":0},
        "total_latency_ms":{"samples":0},"baseline_models":[],"mixed_baselines":false,
        "pricing_versions":[],"mixed_pricing_versions":false,"pricing_facts":[],"unversioned_outcomes":0,
        "savings":{"basis":"API-equivalent estimate, not subscription charges","actual_usd":0,
            "baseline_usd":0,"saved_usd":0,"percent":0,"priced_requests":0,"unpriced_requests":1,
            "unpriced_reasons":{"missing_outcome":1}},
        "coverage":{"bytes_read":0,"file_bytes":0,"lines_read":1,"invalid_records":0,
            "oversized_lines":0,"mixed_session_records":0,"truncated":false,"incomplete_tail":false,
            "partial":false,"legacy_decisions":0,"duplicate_records":0,"conflicting_requests":0}
    }"#).unwrap();
    summary["coverage"]["bytes_read"] = json!(bytes.len());
    summary["coverage"]["file_bytes"] = json!(bytes.len());
    let expected_list = json!({"schema_version":1,"type":"session_history","sessions":[summary.clone()],
        "limits":limits.clone(),"logging_enabled":true,"coverage":{"partial":false,"directory_entries":1,
        "matching_files":1,"skipped_files":0,"unreadable_files":0,"bytes_read":bytes.len(),
        "directory_scan_truncated":false,"byte_limit_reached":false}});
    let expected_show = json!({"schema_version":1,"type":"session_history","summary":summary,
        "records":[serde_json::from_str::<Value>(ROW).unwrap()],"limits":limits,"logging_enabled":true});
    for (profile, suffix, unit) in [
        ("lone-surrogate", r"\ud800", 0xd800u16),
        ("replacement", "\u{fffd}", 0xfffdu16),
    ] {
        home.write("config.json", format!(r#"{{"AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_SESSION_LOG_DIR":"./actual-logs-{suffix}"}}"#));
        let config_before = home.saved_bytes();
        let config_identity = identity(&home.config());
        let names = home.names();
        let text = success(&output(home.command().args(["config", "show", "--json"])));
        assert!(text.len() < 16_384);
        let report = JsDocument::parse(text.as_bytes()).unwrap();
        assert_eq!(
            report.stringify_node(report.get(report.root(), "valid").unwrap()),
            "true"
        );
        let settings = report.get(report.root(), "settings").unwrap();
        let setting = report.get(settings, KEY).unwrap();
        let Some(JsNode::Object(fields)) = report.node(setting) else {
            panic!("path setting is an object")
        };
        assert_eq!(fields.entries().len(), 3);
        assert!(report.string_is(report.get(setting, "source"), "file"));
        assert_eq!(
            report.stringify_node(report.get(setting, "active").unwrap()),
            "true"
        );
        let expected: Vec<u16> = format!("{}/actual-logs-", cwd.display())
            .encode_utf16()
            .chain([unit])
            .collect();
        assert_eq!(
            report
                .string(report.get(setting, "value").unwrap())
                .unwrap()
                .units(),
            expected
        );
        eprintln!(
            "session_log_directory_utf16={}",
            json!({"profile":profile,"operation":"config-show","report_json":text})
        );
        for (args, expected) in [
            (vec!["sessions", "list", "--json"], &expected_list),
            (vec!["sessions", "show", ID, "--json"], &expected_show),
        ] {
            let text = success(&output(home.command().args(&args)));
            assert!(text.len() < 8192);
            assert!(!text.contains(cwd.to_str().unwrap()));
            assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), *expected);
            eprintln!(
                "session_log_directory_utf16={}",
                json!({"profile":profile,"operation":args[1],"report_json":text})
            );
        }
        assert_eq!(home.saved_bytes(), config_before);
        assert_eq!(identity(&home.config()), config_identity);
        assert_eq!(home.names(), names);
        assert_eq!(fs::read(&file).unwrap(), bytes);
        assert_eq!(identity(&file), file_before);
        assert_eq!(identity(&directory), directory_before);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    }
}

#[test]
fn endpoint_settings_preserve_saved_literals_effective_provenance_and_backend_guards() {
    struct Case {
        key: &'static str,
        active: &'static str,
        other: &'static str,
        scoped: bool,
        default: &'static str,
        literal: &'static str,
        effective: &'static str,
        runtime: &'static str,
        runtime_effective: &'static str,
        invalid: &'static str,
        error: &'static str,
    }
    for case in [
        Case {
            key: "AUTOROUTER_UPSTREAM_URL",
            active: "ollama",
            other: "jev",
            scoped: false,
            default: "https://api.anthropic.com",
            literal: "https://example.test/gateway/",
            effective: "https://example.test/gateway",
            runtime: "https://environment.example.test/gateway/",
            runtime_effective: "https://environment.example.test/gateway",
            invalid: "https://synthetic-user:synthetic-pass@example.test/",
            error: "AUTOROUTER_UPSTREAM_URL must use HTTPS (HTTP is allowed on loopback) with no credentials, query, or fragment",
        },
        Case {
            key: "AUTOROUTER_JEV_URL",
            active: "jev",
            other: "ollama",
            scoped: true,
            default: "https://api.typesafe.ai/v1/systemone",
            literal: "https://example.test/v1/systemone/",
            effective: "https://example.test/v1/systemone",
            runtime: "https://environment.example.test/v1/systemone/",
            runtime_effective: "https://environment.example.test/v1/systemone",
            invalid: "https://synthetic-user:synthetic-pass@example.test/",
            error: "AUTOROUTER_JEV_URL must use HTTPS (HTTP is allowed on loopback) with no credentials, query, or fragment",
        },
        Case {
            key: "AUTOROUTER_OLLAMA_URL",
            active: "ollama",
            other: "jev",
            scoped: true,
            default: "http://127.0.0.1:11434",
            literal: "http://localhost:11434/",
            effective: "http://127.0.0.1:11434",
            runtime: "http://[::1]:11435/",
            runtime_effective: "http://[::1]:11435",
            invalid: "https://example.test/",
            error: "Ollama must use a loopback base URL without a path, credentials, a query, or a fragment.",
        },
    ] {
        let home = Home::new();
        let show = |evaluator: &str, value: Option<&str>, all: bool| {
            let before = home.config().exists().then(|| home.saved_bytes());
            let mut command = home.command();
            command
                .args(["config", "show", "--json"])
                .env("AUTOROUTER_EVALUATOR", evaluator)
                .env("AUTOROUTER_AUTH_MODE", "api-key");
            if let Some(value) = value {
                command.env(case.key, value);
            }
            if all {
                command.arg("--check-all");
            }
            let result = output(&mut command);
            assert!(result.stderr.is_empty());
            let report: Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(home.config().exists(), before.is_some());
            if let Some(bytes) = before {
                assert_eq!(home.saved_bytes(), bytes);
            }
            for secret in ["synthetic-user", "synthetic-pass"] {
                assert!(!String::from_utf8_lossy(&result.stdout).contains(secret));
            }
            (result.status.success(), report)
        };
        let (ok, report) = show(case.active, None, false);
        assert!(ok);
        assert_eq!(report["valid"], true);
        assert_eq!(
            report["settings"][case.key],
            json!({"source":"default","active":true,"value":case.default})
        );
        let base = json!({"AUTOROUTER_AUTH_MODE":"api-key","AUTOROUTER_EVALUATOR":case.other,"AUTOROUTER_PORT":"8123"});
        home.save(&base);
        let text = success(&output(
            home.command()
                .args(["config", "set", case.key, case.literal])
                .env("AUTOROUTER_EVALUATOR", case.other)
                .env(case.key, case.runtime),
        ));
        assert!(text.contains(&format!("environment still overrides {}", case.key)));
        let mut expected = base.clone();
        expected[case.key] = json!(case.literal);
        assert_eq!(home.saved(), expected);
        let (ok, report) = show(case.active, None, false);
        assert!(ok);
        assert_eq!(report["valid"], true);
        assert_eq!(
            report["settings"][case.key],
            json!({"source":"file","active":true,"value":case.effective})
        );
        let (ok, report) = show(case.active, Some(case.runtime), false);
        assert!(ok);
        assert_eq!(report["valid"], true);
        assert_eq!(
            report["settings"][case.key],
            json!({"source":"environment","active":true,"overrides_file":true,"value":case.runtime_effective})
        );
        let before = home.saved_bytes();
        let invalid = output(
            home.command()
                .args(["config", "set", case.key, case.invalid])
                .env("AUTOROUTER_EVALUATOR", case.other)
                .env(case.key, case.runtime),
        );
        assert!(!invalid.status.success());
        assert!(invalid.stdout.is_empty());
        assert_eq!(
            String::from_utf8(invalid.stderr).unwrap().trim(),
            case.error
        );
        assert_eq!(home.saved_bytes(), before);
        for all in [false, true] {
            let (ok, report) = show(case.other, Some(case.invalid), all);
            assert_eq!(ok, case.scoped && !all);
            assert_eq!(report["valid"], case.scoped && !all);
            if case.scoped {
                assert_eq!(
                    report["checked"],
                    if all {
                        "all_evaluators"
                    } else {
                        "active_evaluator"
                    }
                );
                assert_eq!(
                    report["settings"][case.key],
                    json!({"source":"environment","active":false,"overrides_file":true,"value":null})
                );
            } else {
                assert!(report.get("settings").is_none());
            }
            if all || !case.scoped {
                assert_eq!(report["error"], case.error);
            } else {
                assert!(report.get("error").is_none());
            }
        }
        success(&output(home.command().args(["config", "unset", case.key])));
        assert_eq!(home.saved(), base);
        assert_eq!(
            fs::metadata(home.config()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(home.names(), ["config.json"]);
    }
}
