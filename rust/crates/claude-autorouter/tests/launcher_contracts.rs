#![cfg(unix)]
mod support;
use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid;
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::net::TcpStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use support::{Home, Response, Server, output, quote, success};

const HAIKU: &str = "claude-haiku-4-5-20251001";
const SONNET: &str = "claude-sonnet-5-5";
fn launch(home: &Home) -> Command {
    let mut command = home.command();
    command
        .arg("claude")
        .env("AUTOROUTER_AUTH_MODE", "subscription")
        .env("TYPESAFE_API_KEY", "synthetic-jev-key");
    command
}
fn save_jev(home: &Home) {
    home.save(&json!({"AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_JEV_URL":"http://127.0.0.1:1/v1/systemone"}));
}
fn no_status(home: &Home) {
    assert!(
        !home
            .names()
            .iter()
            .any(|name| name.starts_with("autorouter-status-"))
    );
}
fn closed(base: &str) {
    assert!(
        TcpStream::connect(base.strip_prefix("http://").unwrap()).is_err(),
        "Gateway survived its launcher"
    );
}

#[test]
fn command_help_matrix_needs_no_credentials_or_readable_configuration() {
    let home = Home::new();
    for command in ["setup", "doctor", "config", "serve"] {
        let result = output(
            home.command()
                .args([command, "--help"])
                .env("AUTOROUTER_CONFIG", "/missing/autorouter-config.json"),
        );
        let text = success(&result);
        assert!(text.contains(&format!("Usage: claude-autorouter {command}")));
        assert!(text.len() < 2000);
    }
    assert!(home.names().is_empty());
}

#[test]
fn direct_help_matrix_bypasses_invalid_local_endpoint_and_all_temporary_resources() {
    let home = Home::new();
    let scratch = home.0.join("scratch");
    fs::create_dir(&scratch).unwrap();
    home.claude("#!/bin/sh\nset -eu\n[ -z \"${ANTHROPIC_BASE_URL+x}${AUTOROUTER_STATUS_FILE+x}${TYPESAFE_API_KEY+x}${AUTOROUTER_TOKEN+x}\" ] || exit 71\n[ \"$#\" = 1 ] || exit 72\nprintf 'CLAUDE_DIRECT:%s\\n' \"$1\"\n");
    for arg in ["--help", "-h", "--version", "-v"] {
        let result = output(
            home.command()
                .args(["claude", arg])
                .env("TMPDIR", &scratch)
                .env("AUTOROUTER_EVALUATOR", "ollama")
                .env("AUTOROUTER_OLLAMA_URL", "not a valid endpoint")
                .env("TYPESAFE_API_KEY", "private-placeholder")
                .env("AUTOROUTER_TOKEN", "private-placeholder"),
        );
        assert_eq!(success(&result), format!("CLAUDE_DIRECT:{arg}\n"));
    }
    assert_eq!(fs::read_dir(scratch).unwrap().count(), 0);
    assert!(!home.config().exists());
}

#[test]
fn subscription_launch_uses_only_local_header_auth_and_cleans_its_listener_and_overlay() {
    let home = Home::new();
    save_jev(&home);
    let settings_copy = home.0.join("settings-copy.json");
    let status_copy = home.0.join("status-copy.json");
    home.claude(&format!("#!/bin/sh\nset -eu\n[ \"$AUTOROUTER_EVALUATOR\" = jev ] || exit 71\n[ \"$#\" = 4 ] && [ \"$1\" = --settings ] && [ \"$3\" = --model ] && [ \"$4\" = sonnet ] || exit 72\n[ -z \"${{ANTHROPIC_API_KEY+x}}${{ANTHROPIC_AUTH_TOKEN+x}}${{CLAUDE_CODE_OAUTH_TOKEN+x}}${{TYPESAFE_API_KEY+x}}\" ] || exit 73\ncase \"$ANTHROPIC_CUSTOM_HEADERS\" in 'X-Autorouter-Token: '*) ;; *) exit 74;; esac\ntoken=${{ANTHROPIC_CUSTOM_HEADERS#'X-Autorouter-Token: '}}\n[ \"${{#token}}\" = 64 ] || exit 75\n/bin/cp \"$2\" {}\n/bin/cp \"$AUTOROUTER_STATUS_FILE\" {}\n/usr/bin/curl --silent --show-error --fail --max-time 5 -H \"$ANTHROPIC_CUSTOM_HEADERS\" \"$ANTHROPIC_BASE_URL/health\" --output /dev/null\nprintf '%s\\n%s\\n%s\\n' \"$ANTHROPIC_BASE_URL\" \"$AUTOROUTER_STATUS_FILE\" \"$2\"\n",quote(&settings_copy),quote(&status_copy)));
    let result = output(
        launch(&home)
            .args(["--model", "sonnet"])
            .env("ANTHROPIC_API_KEY", "must-be-removed"),
    );
    let text = success(&result);
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 3);
    let settings: Value = serde_json::from_slice(&fs::read(settings_copy).unwrap()).unwrap();
    assert_eq!(settings["statusLine"]["type"], "command");
    assert_eq!(settings["statusLine"]["refreshInterval"], 1);
    let status_command = settings["statusLine"]["command"].as_str().unwrap();
    let executable = std::env::var_os("AUTOROUTER_TEST_NATIVE_EXECUTABLE")
        .or_else(|| std::env::var_os("AUTOROUTER_TEST_EXECUTABLE"))
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_claude-autorouter").into());
    let executable = fs::canonicalize(executable).unwrap();
    assert_eq!(status_command, format!("{} statusline", quote(&executable)));
    // Exercise the generated shell command with no Node on PATH. npm's
    // legitimate node_modules directory may itself contain the word "node";
    // exact executable identity and actual invocation establish independence.
    success(&output(
        Command::new("/bin/sh")
            .args(["-c", status_command])
            .env_clear()
            .env("PATH", &home.0)
            .env("AUTOROUTER_STATUS_FILE", &status_copy)
            .current_dir(&home.0)
            .stdin(Stdio::null()),
    ));
    let snapshot: Value = serde_json::from_slice(&fs::read(status_copy).unwrap()).unwrap();
    assert_eq!(snapshot["version"], 1);
    assert_eq!(snapshot["sessions"], json!({}));
    assert!(!Path::new(lines[1]).exists());
    assert!(!Path::new(lines[2]).exists());
    closed(lines[0]);
    no_status(&home);
}

#[test]
fn four_permission_argument_cases_route_without_changing_claude_permissions_or_settings() {
    let home = Home::new();
    home.save(&json!({"AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_CLIENT_PROFILE":"compatible"}));
    let settings = r#"{"permissions":{"disableAutoMode":"disable","deny":["Bash(rm *)"]}}"#;
    let settings_path = home.write("settings.json", settings);
    let args_path = home.0.join("observed-args.txt");
    let jev = Server::new(|_| {
        Response::json(json!({"answers":{"tier":{"choice":"haiku","confidence":0.99}}}))
    });
    let upstream = Server::new(|request| {
        Response::json(
            json!({"model":request.json()["model"],"content":[{"type":"text","text":"Synthetic reply"}],"stop_reason":"end_turn"}),
        )
    });
    home.claude(&format!("#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > {}\n[ \"$ANTHROPIC_MODEL\" = \"$EXPECTED_CLIENT_MODEL\" ] || exit 71\n[ \"${{MAX_THINKING_TOKENS-unset}}\" = \"$EXPECTED_THINKING\" ] || exit 72\n[ \"$CLAUDE_CODE_GATEWAY_HINT_HEADERS\" = 1 ] || exit 73\n[ -z \"${{CLAUDE_CODE_ENABLE_AUTO_MODE+x}}${{CLAUDE_CODE_AUTO_MODE_SERVER+x}}${{CLAUDE_CODE_AUTO_MODE_MODEL+x}}\" ] || exit 74\n[ \"$(/bin/cat \"$CLAUDE_CONFIG_DIR/settings.json\")\" = \"$EXPECTED_SETTINGS\" ] || exit 75\nbody=$(printf '{{\"model\":\"%s\",\"max_tokens\":64,\"messages\":[{{\"role\":\"user\",\"content\":\"Return the length of an empty array.\"}}]}}' \"$ANTHROPIC_MODEL\")\n/usr/bin/curl --silent --show-error --fail --max-time 5 -H 'content-type: application/json' -H \"x-api-key: $ANTHROPIC_API_KEY\" -H 'x-claude-code-session-id: synthetic-auto-launch' -H 'x-claude-code-request-class: main' --data-binary \"$body\" \"$ANTHROPIC_BASE_URL/v1/messages\"\n",quote(&args_path)));
    for (profile, flags, model, thinking) in [
        (
            "compatible",
            vec!["--permission-mode", "auto"],
            SONNET,
            "unset",
        ),
        ("native", vec!["--permission-mode=auto"], SONNET, "unset"),
        (
            "compatible",
            vec!["--permission-mode", "auto", "--permission-mode=manual"],
            HAIKU,
            "0",
        ),
        (
            "compatible",
            vec!["--", "--permission-mode=auto"],
            HAIKU,
            "0",
        ),
    ] {
        let mut args = vec!["--settings", settings];
        args.extend(flags);
        let result = output(
            home.command()
                .arg("claude")
                .args(&args)
                .env("AUTOROUTER_AUTH_MODE", "api-key")
                .env("AUTOROUTER_CLIENT_PROFILE", profile)
                .env("AUTOROUTER_STATUSLINE", "0")
                .env("ANTHROPIC_API_KEY", "synthetic-upstream-key")
                .env("TYPESAFE_API_KEY", "synthetic-jev-key")
                .env("AUTOROUTER_UPSTREAM_URL", upstream.url())
                .env("AUTOROUTER_JEV_URL", format!("{}/v1/systemone", jev.url()))
                .env("CLAUDE_CONFIG_DIR", &home.0)
                .env("EXPECTED_SETTINGS", settings)
                .env("EXPECTED_CLIENT_MODEL", model)
                .env("EXPECTED_THINKING", thinking),
        );
        assert!(
            result.status.success(),
            "profile={profile}, args={args:?}, child code={:?}, stderr={}",
            result.status.code(),
            String::from_utf8_lossy(&result.stderr)
        );
        let response: Value = serde_json::from_str(&success(&result)).unwrap();
        assert_eq!(response["model"], model);
        assert_eq!(
            fs::read_to_string(&args_path).unwrap(),
            args.join("\n") + "\n"
        );
        assert_eq!(
            upstream.calls().last().unwrap().json(),
            json!({"model":model,"max_tokens":64,"messages":[{"role":"user","content":"Return the length of an empty array."}]})
        );
        assert_eq!(fs::read_to_string(&settings_path).unwrap(), settings);
        no_status(&home);
    }
    assert_eq!(jev.calls().len(), 4);
    assert_eq!(upstream.calls().len(), 4);
}

#[test]
fn disabled_or_malformed_status_settings_keep_original_arguments_and_clear_stale_state() {
    let home = Home::new();
    save_jev(&home);
    home.claude("#!/bin/sh\nset -eu\n[ \"$AUTOROUTER_EVALUATOR\" = jev ] || exit 71\n[ -z \"${AUTOROUTER_STATUS_FILE+x}\" ] || exit 72\nprintf '%s\\n' \"$@\"\n");
    for (enabled, supplied) in [
        (
            "0",
            r#"{"statusLine":{"type":"command","command":"my-status"}}"#,
        ),
        ("1", "{invalid-settings"),
    ] {
        let result = output(
            launch(&home)
                .args(["--settings", supplied])
                .env("AUTOROUTER_STATUSLINE", enabled)
                .env("AUTOROUTER_STATUS_FILE", "/stale/other-session.json"),
        );
        assert!(result.status.success());
        assert_eq!(
            result.stdout,
            format!("--settings\n{supplied}\n").as_bytes()
        );
        if enabled == "1" {
            assert!(String::from_utf8_lossy(&result.stderr).contains("status line unavailable"));
        } else {
            assert!(result.stderr.is_empty());
        }
        no_status(&home);
    }
}

struct Running(Child, bool);
impl Drop for Running {
    fn drop(&mut self) {
        if !self.1 {
            let _ = killpg(Pid::from_raw(self.0.id() as i32), Signal::SIGKILL);
            let _ = self.0.wait();
        }
    }
}
#[test]
fn both_launcher_modes_forward_each_requested_signal_once_and_reap_the_child() {
    for direct in [false, true] {
        for (name, signal) in [("INT", Signal::SIGINT), ("TERM", Signal::SIGTERM)] {
            let home = Home::new();
            save_jev(&home);
            let signal_file = home.0.join("signals.txt");
            home.claude(&format!("#!/bin/sh\nset -eu\ntrap 'printf received >> \"$SIGNAL_FILE\"; exit 23' {}\nprintf '{{\"marker\":\"lifecycle_ready\",\"gateway\":\"synthetic\"}}\\n'\nprintf '%s\\n%s\\n%s\\n' \"$$\" \"${{AUTOROUTER_STATUS_FILE-}}\" \"${{ANTHROPIC_BASE_URL-}}\"\ni=0\nwhile [ \"$i\" -lt 1000 ]; do i=$((i+1)); /bin/sleep 0.01; done\nexit 99\n",name));
            let mut command = launch(&home);
            command.env("SIGNAL_FILE", &signal_file);
            if direct {
                command.arg("--help");
            }
            let mut child = Running(
                command
                    .process_group(0)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap(),
                false,
            );
            let stdout = child.0.stdout.take().unwrap();
            let stderr = child.0.stderr.take().unwrap();
            let (tx, rx) = mpsc::channel();
            let (tail_tx, tail_rx) = mpsc::channel();
            let reader = std::thread::spawn(move || {
                let mut reader = BufReader::new(stdout).take(65536);
                let mut records = Vec::new();
                // The fake child can handle a signal as soon as it prints its
                // first marker. Wait for its complete record before signaling.
                for _ in 0..4 {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    assert!(line.ends_with('\n'));
                    records.push(line.trim_end_matches('\n').to_owned());
                }
                tx.send(records).unwrap();
                let mut rest = String::new();
                reader.read_to_string(&mut rest).unwrap();
                tail_tx.send(rest).unwrap();
            });
            let (error_tx, error_rx) = mpsc::channel();
            let error_reader = std::thread::spawn(move || {
                let mut errors = Vec::new();
                stderr.take(65537).read_to_end(&mut errors).unwrap();
                assert!(errors.len() <= 65536);
                error_tx.send(errors).unwrap();
            });
            let records = rx.recv_timeout(Duration::from_secs(10)).unwrap();
            let ready: Value = serde_json::from_str(&records[0]).unwrap();
            assert_eq!(ready["gateway"], "synthetic");
            kill(Pid::from_raw(child.0.id() as i32), signal).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let status = loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    // Reaping ends ownership of the numeric process-group ID.
                    child.1 = true;
                    break status;
                }
                assert!(Instant::now() < deadline, "Launcher did not stop");
                std::thread::sleep(Duration::from_millis(3));
            };
            assert_eq!(status.code(), Some(23));
            let errors = error_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(errors.is_empty());
            assert!(
                tail_rx
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .is_empty()
            );
            reader.join().unwrap();
            error_reader.join().unwrap();
            let pid = Pid::from_raw(records[1].parse().unwrap());
            assert_eq!(kill(pid, None).unwrap_err(), nix::errno::Errno::ESRCH);
            assert_eq!(fs::read_to_string(signal_file).unwrap(), "received");
            if !direct {
                assert!(!Path::new(&records[2]).exists());
                closed(&records[3]);
            }
            no_status(&home);
        }
    }
}

#[test]
fn spawn_failures_remove_allocated_status_resources_and_leave_saved_configuration_untouched() {
    for failure in ["missing", "permission", "interpreter"] {
        let home = Home::new();
        save_jev(&home);
        let original = fs::read(home.config()).unwrap();
        if failure == "permission" {
            home.write("claude", b"#!/bin/sh\nexit 0\n");
        }
        if failure == "interpreter" {
            home.claude("#!/nonexistent/synthetic-interpreter\n");
        }
        let result = output(&mut launch(&home));
        assert_eq!(result.status.code(), Some(1));
        assert!(result.stdout.is_empty());
        assert_eq!(
            result.stderr,
            b"Could not launch Claude Code. Ensure `claude` is installed and on PATH.\n"
        );
        assert_eq!(fs::read(home.config()).unwrap(), original);
        no_status(&home);
        assert!(!home.names().iter().any(|name| name.contains("settings")));
    }
}
