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
    let before = fs::read(home.config()).unwrap();
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
        assert_eq!(fs::read(home.config()).unwrap(), before);
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
    let before = fs::read(home.config()).unwrap();
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
    assert_eq!(fs::read(home.config()).unwrap(), before);
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
    let before = fs::read(home.config()).unwrap();
    for input in [b"a\nb".to_vec(), Vec::new(), vec![b'x'; 16385]] {
        home.write("stdin.txt", input);
        let result = output(
            home.command()
                .args(["config", "set", "TYPESAFE_API_KEY", "--stdin"])
                .stdin(File::open(&stdin).unwrap()),
        );
        assert!(!result.status.success());
        assert_eq!(fs::read(home.config()).unwrap(), before);
        assert!(!String::from_utf8_lossy(&result.stderr).contains("private-"));
    }
}

#[test]
fn every_invalid_edit_preserves_saved_bytes_and_hides_url_or_unknown_key_values() {
    let home = Home::new();
    home.save(&json!({"AUTOROUTER_PORT":"8123"}));
    let before = fs::read(home.config()).unwrap();
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
        assert_eq!(fs::read(home.config()).unwrap(), before);
    }
}
