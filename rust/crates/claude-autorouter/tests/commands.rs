#![cfg(unix)]
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "autorouter-native-command-{}-{stamp}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    fn claude(&self, text: &str) {
        let path = self.0.join("claude");
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn command(&self) -> Command {
        let executable = std::env::var_os("AUTOROUTER_TEST_EXECUTABLE")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_claude-autorouter").into());
        let mut command = Command::new(executable);
        command
            .current_dir(&self.0)
            .env_clear()
            .env("HOME", &self.0)
            .env("PATH", &self.0)
            .env("AUTOROUTER_CONFIG", self.0.join("missing-config.json"))
            .env("TYPESAFE_API_KEY", "synthetic-evaluator-secret")
            .env("AUTOROUTER_TOKEN", "synthetic-gateway-secret");
        command
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn help_and_version_are_native_and_do_not_read_configuration() {
    let home = Home::new();
    let version = home.command().arg("--version").output().unwrap();
    assert!(version.status.success());
    assert_eq!(
        version.stdout,
        format!("{}\n", env!("CARGO_PKG_VERSION")).as_bytes()
    );
    assert!(version.stderr.is_empty());
    for args in [
        vec![],
        vec!["--help"],
        vec!["help", "config"],
        vec!["config", "show", "--help"],
    ] {
        let result = home.command().args(args).output().unwrap();
        assert!(result.status.success());
        assert!(result.stderr.is_empty());
        assert!(
            String::from_utf8(result.stdout)
                .unwrap()
                .contains("claude-autorouter")
        );
    }
    assert_eq!(fs::read_dir(&home.0).unwrap().count(), 0);
}

#[test]
fn direct_claude_help_forwards_literal_arguments_and_removes_router_secrets() {
    let home = Home::new();
    home.claude("#!/bin/sh\n[ -z \"$TYPESAFE_API_KEY\" ] || exit 71\n[ -z \"$AUTOROUTER_TOKEN\" ] || exit 72\nprintf 'claude:%s\\n' \"$1\"\nexit 7\n");
    for arg in ["--help", "-h", "--version", "-v"] {
        let result = home.command().args(["claude", arg]).output().unwrap();
        assert_eq!(result.status.code(), Some(7));
        assert_eq!(result.stdout, format!("claude:{arg}\n").as_bytes());
        assert!(result.stderr.is_empty());
    }
    assert_eq!(fs::read_dir(&home.0).unwrap().count(), 1);
}

#[test]
fn missing_claude_has_stable_bounded_diagnostic() {
    let home = Home::new();
    let result = home.command().args(["claude", "--help"]).output().unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    assert_eq!(
        result.stderr,
        b"Could not launch Claude Code. Ensure `claude` is installed and on PATH.\n"
    );
}

#[test]
fn direct_help_forwards_termination_and_reaps_the_child() {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    let home = Home::new();
    // The fake child has its own finite lifetime, so a launcher regression
    // cannot leave an unbounded busy loop. No Node executable is available.
    home.claude("#!/bin/sh\ntrap 'exit 143' TERM\nprintf 'ready\\n'\ni=0\nwhile [ \"$i\" -lt 100 ]; do i=$((i+1)); /bin/sleep 0.01; done\nexit 99\n");
    let mut child = home
        .command()
        .args(["claude", "--help"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "ready\n");
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert_eq!(status.code(), Some(143));
            break;
        }
        if start.elapsed() > Duration::from_secs(3) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Claude passthrough failed to forward termination");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn config_show_is_redacted_and_reports_source_and_inactive_settings_without_node() {
    let home = Home::new();
    fs::write(home.0.join("missing-config.json"), r#"{"AUTOROUTER_PORT":"8123","TYPESAFE_API_KEY":"synthetic-saved-private","ANTHROPIC_API_KEY":"synthetic-anthropic","AUTOROUTER_AUTH_MODE":"subscription"}"#).unwrap();
    let result = home
        .command()
        .args(["config", "show", "--json"])
        .env("AUTOROUTER_PORT", "9000")
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(result.stderr.is_empty());
    let text = String::from_utf8(result.stdout).unwrap();
    assert!(!text.contains("synthetic-"));
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        report["settings"]["AUTOROUTER_PORT"],
        serde_json::json!({"source":"environment","active":true,"overrides_file":true,"value":9000})
    );
    assert_eq!(
        report["settings"]["ANTHROPIC_API_KEY"],
        serde_json::json!({"source":"file","active":false,"secret":true,"present":true})
    );
    assert_eq!(report["settings"]["AUTOROUTER_JEV_URL"]["active"], false);
    assert_eq!(
        report["settings"]["AUTOROUTER_JEV_URL"]["value"],
        serde_json::Value::Null
    );
}

#[test]
fn config_edits_validate_saved_values_and_preserve_unrelated_settings() {
    let home = Home::new();
    let path = home.0.join("missing-config.json");
    fs::write(
        &path,
        r#"{"AUTOROUTER_PORT":"8123","ANTHROPIC_API_KEY":"synthetic-kept"}"#,
    )
    .unwrap();
    let before = fs::read(&path).unwrap();
    let invalid = home
        .command()
        .args(["config", "set", "AUTOROUTER_PORT", "invalid"])
        .env("AUTOROUTER_PORT", "9000")
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert_eq!(fs::read(&path).unwrap(), before);
    let inactive = home
        .command()
        .args(["config", "set", "AUTOROUTER_JEV_TIMEOUT_MS", "invalid"])
        .output()
        .unwrap();
    assert!(!inactive.status.success());
    assert_eq!(fs::read(&path).unwrap(), before);
    let valid = home
        .command()
        .args(["config", "set", "AUTOROUTER_PORT", "8124"])
        .output()
        .unwrap();
    assert!(valid.status.success());
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["ANTHROPIC_API_KEY"], "synthetic-kept");
    assert_eq!(saved["AUTOROUTER_PORT"], "8124");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let unset = home
        .command()
        .args(["config", "unset", "AUTOROUTER_PORT"])
        .output()
        .unwrap();
    assert!(unset.status.success());
    assert!(
        !serde_json::from_slice::<serde_json::Value>(&fs::read(&path).unwrap())
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("AUTOROUTER_PORT")
    );
}

#[test]
fn secret_input_is_bounded_and_never_accepted_as_a_command_argument() {
    let home = Home::new();
    let path = home.0.join("missing-config.json");
    let refused = home
        .command()
        .args([
            "config",
            "set",
            "TYPESAFE_API_KEY",
            "synthetic-PRIVATE_ARGUMENT",
        ])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(
        !String::from_utf8(refused.stderr)
            .unwrap()
            .contains("PRIVATE_ARGUMENT")
    );
    assert!(!path.exists());
    let mut child = home
        .command()
        .args(["config", "set", "TYPESAFE_API_KEY", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b" synthetic-private-stdin\r\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("synthetic-private")
    );
    assert!(output.stderr.is_empty());
    let before = fs::read(&path).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&before).unwrap()["TYPESAFE_API_KEY"],
        "synthetic-private-stdin"
    );
    let mut child = home
        .command()
        .args(["config", "set", "TYPESAFE_API_KEY", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&vec![b'x'; 16_385])
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert_eq!(
        output.stderr,
        b"Could not read a bounded secret from stdin.\n"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn invalid_config_show_uses_json_stdout_and_never_echoes_corrupt_secrets() {
    let home = Home::new();
    fs::write(
        home.0.join("missing-config.json"),
        "{\"TYPESAFE_API_KEY\":\"synthetic-PRIVATE_BROKEN\",",
    )
    .unwrap();
    let output = home
        .command()
        .args(["config", "show", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stderr.is_empty());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("PRIVATE"));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!({"schema_version":1,"valid":false,"error":"AutoRouter configuration must contain valid JSON."})
    );
}

#[test]
fn native_setup_jev_preserves_unrelated_saved_preferences_and_requires_explicit_replace() {
    let home = Home::new();
    let path = home.0.join("missing-config.json");
    let initial = home
        .command()
        .args(["setup", "--evaluator", "jev", "--secret-store", "file"])
        .output()
        .unwrap();
    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );
    assert!(!String::from_utf8_lossy(&initial.stdout).contains("synthetic-evaluator-secret"));
    let mut saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["AUTOROUTER_AUTH_MODE"], "subscription");
    assert_eq!(saved["TYPESAFE_API_KEY"], "synthetic-evaluator-secret");
    saved["AUTOROUTER_PORT"] = serde_json::json!("8123");
    saved["TYPESAFE_API_KEY"] = serde_json::json!("synthetic-saved-key");
    fs::write(&path, saved.to_string()).unwrap();
    let before = fs::read(&path).unwrap();
    assert!(
        !home
            .command()
            .args(["setup"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    let updated = home
        .command()
        .args(["setup", "--force", "--client-profile", "auto"])
        .env("AUTOROUTER_PORT", "9000")
        .output()
        .unwrap();
    assert!(
        updated.status.success(),
        "{}",
        String::from_utf8_lossy(&updated.stderr)
    );
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["AUTOROUTER_PORT"], "8123");
    assert_eq!(saved["TYPESAFE_API_KEY"], "synthetic-saved-key");
    assert_eq!(saved["AUTOROUTER_CLIENT_PROFILE"], "auto");
    let replaced = home
        .command()
        .args([
            "setup",
            "--replace",
            "--evaluator",
            "jev",
            "--secret-store",
            "file",
        ])
        .output()
        .unwrap();
    assert!(replaced.status.success());
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(!saved.as_object().unwrap().contains_key("AUTOROUTER_PORT"));
    assert_eq!(saved["TYPESAFE_API_KEY"], "synthetic-evaluator-secret");
}

#[test]
fn invalid_setup_arguments_fail_before_prompting_network_or_writes() {
    let home = Home::new();
    for args in [
        vec!["--auth-mode", "invalid"],
        vec!["--evaluator", "invalid"],
        vec!["--secret-store", "invalid"],
        vec!["--ollama-timeout-ms", "30001"],
        vec!["--evaluator", "jev", "--pull"],
        vec!["--session-log-dir"],
        vec!["--stop-hook-block-cap", "-1"],
    ] {
        let output = home.command().arg("setup").args(args).output().unwrap();
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-"));
        assert!(!home.0.join("missing-config.json").exists());
    }
}

#[test]
fn plain_doctor_uses_native_diagnostics_and_scrubbed_claude_auth_checks() {
    let home = Home::new();
    fs::write(
        home.0.join("missing-config.json"),
        r#"{"AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_AUTH_MODE":"subscription"}"#,
    )
    .unwrap();
    home.claude("#!/bin/sh\n[ -z \"$TYPESAFE_API_KEY$ANTHROPIC_API_KEY$ANTHROPIC_AUTH_TOKEN$CLAUDE_CODE_OAUTH_TOKEN$AUTOROUTER_TOKEN$AUTOROUTER_CONFIG$AUTOROUTER_STATUS_FILE$ANTHROPIC_BASE_URL\" ] || exit 71\ncase \"$ANTHROPIC_CUSTOM_HEADERS\" in *synthetic-PRIVATE*) exit 72;; esac\nif [ \"$1\" = '--version' ]; then printf '2.1.285 (Claude Code)\\n'; else printf '{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"private\":\"synthetic-PRIVATE-auth-output\"}\\n'; fi\n");
    let output=home.command().arg("doctor").env("ANTHROPIC_API_KEY","synthetic-PRIVATE-api").env("CLAUDE_CODE_OAUTH_TOKEN","synthetic-PRIVATE-oauth").env("ANTHROPIC_CUSTOM_HEADERS","X-Kept: yes\nAuthorization: synthetic-PRIVATE-header\nx-api-key: synthetic-PRIVATE-api").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("native ("));
    assert!(stdout.contains("OK  Claude subscription login found"));
    assert!(stdout.contains("OK  Claude Code 2.1.285"));
    assert!(!stdout.contains("PRIVATE"));
    assert!(!stdout.contains("Node.js"));
}

#[test]
fn doctor_rejects_unbounded_child_output_without_echoing_it() {
    let home = Home::new();
    fs::write(
        home.0.join("missing-config.json"),
        r#"{"AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_AUTH_MODE":"subscription"}"#,
    )
    .unwrap();
    home.claude("#!/bin/sh\ni=0\nwhile [ \"$i\" -lt 10000 ]; do printf 'synthetic-PRIVATE-output'; i=$((i+1)); done\n");
    let output = home.command().arg("doctor").output().unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("PRIVATE"));
    assert!(output.stderr.is_empty());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Claude Code unavailable"));
}

#[test]
fn internal_status_renderer_is_native_bounded_and_ignores_configuration() {
    let home = Home::new();
    fs::write(
        home.0.join("missing-config.json"),
        "PRIVATE_MALFORMED_CONFIG",
    )
    .unwrap();
    let snapshot = home.0.join("snapshot.json");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    fs::write(
        &snapshot,
        serde_json::json!({"version":1,"pid":std::process::id(),"heartbeat_at":now,"sessions":{}})
            .to_string(),
    )
    .unwrap();
    let output = home
        .command()
        .arg("statusline")
        .env("AUTOROUTER_STATUS_FILE", &snapshot)
        .env("NO_COLOR", "")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("offline"));
    assert!(!output.stdout.contains(&27));
    fs::write(&snapshot, vec![b'x'; 1024 * 1024 + 1]).unwrap();
    let output = home
        .command()
        .arg("statusline")
        .env("AUTOROUTER_STATUS_FILE", &snapshot)
        .env("TERM", "dumb")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(String::from_utf8_lossy(&output.stdout).contains("offline"));
    assert!(!output.stdout.contains(&27));
}

fn launch_command(home: &Home) -> Command {
    let mut command = home.command();
    command
        .env_remove("AUTOROUTER_CONFIG")
        .env("AUTOROUTER_EVALUATOR", "jev")
        .env("ANTHROPIC_API_KEY", "synthetic-provider-secret")
        .env("TMPDIR", &home.0);
    command
}
fn status_directories(home: &Home) -> Vec<PathBuf> {
    fs::read_dir(&home.0)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("autorouter-status-")
        })
        .collect()
}
#[test]
fn full_launcher_preserves_arguments_scrubs_credentials_and_cleans_private_overlay() {
    let home = Home::new();
    home.claude(r#"#!/bin/sh
[ -z "$TYPESAFE_API_KEY" ] || exit 71
[ -z "$AUTOROUTER_TOKEN" ] || exit 72
[ -z "$AUTOROUTER_CONFIG" ] || exit 73
[ "$ANTHROPIC_API_KEY" != synthetic-provider-secret ] || exit 74
[ "$ANTHROPIC_API_KEY" = "$ANTHROPIC_AUTH_TOKEN" ] || exit 75
[ "$MAX_THINKING_TOKENS" = 0 ] || exit 76
[ "$CLAUDE_CODE_GATEWAY_HINT_HEADERS" = 1 ] || exit 77
[ "$1" = --settings ] || exit 78
[ -f "$2" ] || exit 79
[ -f "$AUTOROUTER_STATUS_FILE" ] || exit 80
[ "$3" = 'literal $HOME `command` * [value]' ] || exit 81
printf '%s\n%s\n%s\n' "$AUTOROUTER_STATUS_FILE" "$2" "$ANTHROPIC_BASE_URL"
/usr/bin/curl --silent --show-error --fail -H "x-api-key: $ANTHROPIC_API_KEY" "$ANTHROPIC_BASE_URL/health" || exit 82
exit 7
"#);
    let result = launch_command(&home)
        .args(["claude", "literal $HOME `command` * [value]"])
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stderr.is_empty());
    let output = String::from_utf8(result.stdout).unwrap();
    let lines: Vec<_> = output.lines().collect();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[3], "{\"status\":\"ok\"}");
    assert!(!PathBuf::from(lines[0]).exists());
    assert!(!PathBuf::from(lines[1]).exists());
    assert!(status_directories(&home).is_empty());
    let address = lines[2].strip_prefix("http://").unwrap();
    assert!(
        std::net::TcpStream::connect(address).is_err(),
        "Gateway listener survived child exit"
    );
}
#[test]
fn failed_child_spawn_and_unsafe_settings_leave_no_status_directory() {
    let home = Home::new();
    let result = launch_command(&home).arg("claude").output().unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(
        result.stderr,
        b"Could not launch Claude Code. Ensure `claude` is installed and on PATH.\n"
    );
    assert!(status_directories(&home).is_empty());
    home.claude(
        "#!/bin/sh\n[ -z \"$AUTOROUTER_STATUS_FILE\" ] || exit 71\nprintf '%s\\n' \"$@\"\n",
    );
    let unsafe_settings = r#"{"sandbox":{"filesystem":{"allowRead":["relative/path"]}},"permissions":{"defaultMode":"dontAsk"}}"#;
    let result = launch_command(&home)
        .args([
            "claude",
            "--settings",
            unsafe_settings,
            "--permission-mode",
            "auto",
        ])
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(
        String::from_utf8(result.stdout).unwrap(),
        format!("--settings\n{unsafe_settings}\n--permission-mode\nauto\n")
    );
    assert!(
        String::from_utf8(result.stderr)
            .unwrap()
            .contains("could not safely prepare session settings")
    );
    assert!(status_directories(&home).is_empty());
}
#[test]
fn launcher_checks_subscription_bare_and_provider_conflicts_before_allocating_resources() {
    let home = Home::new();
    for (command, key, value, error) in [
        (
            vec!["claude", "--bare"],
            "AUTOROUTER_AUTH_MODE",
            "subscription",
            "--bare disables Claude Code OAuth",
        ),
        (
            vec!["claude"],
            "CLAUDE_CODE_USE_VERTEX",
            "true",
            "Unset CLAUDE_CODE_USE_VERTEX",
        ),
        (
            vec!["serve", "unexpected"],
            "AUTOROUTER_AUTH_MODE",
            "api-key",
            "Usage: claude-autorouter serve",
        ),
    ] {
        let result = launch_command(&home)
            .env(key, value)
            .args(command)
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(1));
        assert!(String::from_utf8(result.stderr).unwrap().starts_with(error));
        assert!(status_directories(&home).is_empty());
    }
}
#[test]
fn full_launcher_forwards_signal_waits_for_child_and_removes_overlay() {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    let home = Home::new();
    home.claude("#!/bin/sh\ntrap 'exit 143' TERM\nprintf '%s\\n' \"$AUTOROUTER_STATUS_FILE\"\ni=0\nwhile [ \"$i\" -lt 300 ]; do i=$((i+1)); /bin/sleep 0.01; done\nexit 99\n");
    let mut child = launch_command(&home)
        .arg("claude")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut path = String::new();
    reader.read_line(&mut path).unwrap();
    assert!(PathBuf::from(path.trim()).exists());
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Launcher did not forward termination and finish")
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.code(), Some(143));
    assert!(!PathBuf::from(path.trim()).exists());
    assert!(status_directories(&home).is_empty());
}
