//! Native and installed-archive CLI regressions. Every path, credential,
//! terminal and Claude executable belongs to a disposable synthetic home.
#![cfg(unix)]
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::pty::openpty;
use nix::sys::signal::{Signal, kill};
use nix::sys::termios::{LocalFlags, Termios, tcgetattr};
use nix::unistd::{Pid, geteuid};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
#[path = "support/job_control.rs"]
mod job_control;

#[test]
fn hidden_secret_real_stop_resume_preserves_private_unicode_editing_and_cancellation() {
    if job_control::helper_entry() {
        return;
    }
    for scenario in [
        "pre-ready-failure",
        "timeout",
        "stopped-anchor",
        "stopped-manager",
        "control-eof",
        "capture-overflow",
        "success",
        "cancel",
    ] {
        job_control::check(scenario);
    }
}

struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "autorouter-native-edge-{}-{stamp}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    fn config(&self) -> PathBuf {
        self.0.join("config.json")
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
            .env("AUTOROUTER_CONFIG", self.config())
            .env("AUTOROUTER_EVALUATOR", "jev")
            .env("TYPESAFE_API_KEY", "synthetic-evaluator-secret");
        command
    }
    fn claude(&self, script: &str) {
        let path = self.0.join("claude");
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn launch(&self) -> Command {
        let mut command = self.command();
        command
            .env_remove("AUTOROUTER_CONFIG")
            .env("TMPDIR", &self.0)
            .env("ANTHROPIC_API_KEY", "synthetic-provider-secret");
        command
    }
    fn assert_clean_launch(&self) {
        assert!(fs::read_dir(&self.0).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("autorouter-status-")
        }));
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct ChildGuard(Child);
impl ChildGuard {
    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "Synthetic CLI exceeded its deadline"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn input_output(command: &mut Command, input: &[u8]) -> Output {
    let mut child = ChildGuard(
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut stdout = child.0.stdout.take().unwrap();
    let mut stderr = child.0.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut value = Vec::new();
        stdout.read_to_end(&mut value).unwrap();
        value
    });
    let err = std::thread::spawn(move || {
        let mut value = Vec::new();
        stderr.read_to_end(&mut value).unwrap();
        value
    });
    child.0.stdin.take().unwrap().write_all(input).unwrap();
    let status = child.wait();
    Output {
        status,
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    }
}
struct Terminal {
    master: File,
    slave: File,
    original: Termios,
    original_flags: i32,
    visible: Vec<u8>,
}
impl Terminal {
    fn new() -> Self {
        let pty = openpty(None, None).unwrap();
        let master = File::from(pty.master);
        let slave = File::from(pty.slave);
        let original = tcgetattr(&slave).unwrap();
        let original_flags = fcntl(&slave, FcntlArg::F_GETFL).unwrap();
        let flags = OFlag::from_bits_truncate(fcntl(&master, FcntlArg::F_GETFL).unwrap());
        fcntl(&master, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).unwrap();
        Self {
            master,
            slave,
            original,
            original_flags,
            visible: Vec::new(),
        }
    }
    fn spawn(&self, command: &mut Command) -> ChildGuard {
        ChildGuard(
            command
                .stdin(self.slave.try_clone().unwrap())
                .stdout(self.slave.try_clone().unwrap())
                .stderr(self.slave.try_clone().unwrap())
                .spawn()
                .unwrap(),
        )
    }
    fn read_available(&mut self) {
        let mut buffer = [0; 4096];
        loop {
            match self.master.read(&mut buffer) {
                Ok(0) => return,
                Ok(count) => {
                    self.visible.extend_from_slice(&buffer[..count]);
                    assert!(
                        self.visible.len() < 65_536,
                        "Unexpected terminal output volume"
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(error) if error.raw_os_error() == Some(5) => return,
                Err(error) => panic!("Synthetic terminal read failed: {error}"),
            }
        }
    }
    fn prompt(&mut self, child: &mut ChildGuard) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.read_available();
            if self
                .visible
                .windows(b"(hidden): ".len())
                .any(|w| w == b"(hidden): ")
            {
                break;
            }
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "Secret command exited before prompt: {}",
                String::from_utf8_lossy(&self.visible)
            );
            assert!(Instant::now() < deadline, "Secret prompt deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
        let state = tcgetattr(&self.slave).unwrap();
        assert!(
            !state
                .local_flags
                .intersects(LocalFlags::ECHO | LocalFlags::ECHONL)
        );
        assert!(!state.local_flags.contains(LocalFlags::ICANON));
    }
    fn type_bytes(&mut self, input: &[u8]) {
        self.master.write_all(input).unwrap();
    }
    fn restored(&mut self) {
        self.read_available();
        let state = tcgetattr(&self.slave).unwrap();
        // macOS marks queued input for reprocessing when canonical mode is
        // restored. PENDIN is kernel bookkeeping, not a changed user mode.
        assert_eq!(
            state.local_flags.difference(LocalFlags::PENDIN),
            self.original.local_flags.difference(LocalFlags::PENDIN)
        );
        assert_eq!(state.input_flags, self.original.input_flags);
        assert_eq!(state.output_flags, self.original.output_flags);
        assert_eq!(state.control_flags, self.original.control_flags);
        assert_eq!(state.control_chars, self.original.control_chars);
        assert_eq!(
            fcntl(&self.slave, FcntlArg::F_GETFL).unwrap() & OFlag::O_NONBLOCK.bits(),
            self.original_flags & OFlag::O_NONBLOCK.bits()
        );
        assert!(!String::from_utf8_lossy(&self.visible).contains("synthetic-PRIVATE"));
    }
}
fn assert_private(path: &Path, mode: u32) {
    let metadata = fs::metadata(path).unwrap();
    assert_eq!(metadata.mode() & 0o777, mode);
    assert_eq!(metadata.uid(), geteuid().as_raw());
}

#[test]
fn hidden_secret_uses_raw_editing_and_restores_terminal_before_exit() {
    for (typed, expected) in [
        ("synthetic-PRIVATE-abc\u{1b}[DZ\r", "synthetic-PRIVATE-abZc"),
        (
            "synthetic-PRIVATE-a😀b\u{1b}[D\u{7f}Z\r",
            "synthetic-PRIVATE-aZb",
        ),
        (
            "discard\u{15}synthetic-PRIVATE-key\r",
            "synthetic-PRIVATE-key",
        ),
    ] {
        let home = Home::new();
        let mut terminal = Terminal::new();
        let mut child = terminal.spawn(home.command().args(["config", "set", "TYPESAFE_API_KEY"]));
        terminal.prompt(&mut child);
        // Split the input at byte boundaries: UTF-8 and escape sequences can
        // arrive in separate terminal reads without changing cursor behavior.
        for byte in typed.as_bytes() {
            terminal.type_bytes(&[*byte]);
        }
        assert!(child.wait().success());
        terminal.restored();
        let saved: Value = serde_json::from_slice(&fs::read(home.config()).unwrap()).unwrap();
        assert_eq!(saved["TYPESAFE_API_KEY"], expected);
        assert_private(&home.config(), 0o600);
    }
}

#[test]
fn hidden_secret_cancellation_restores_echo_and_preserves_saved_configuration() {
    for cancellation in [None, Some(Signal::SIGINT), Some(Signal::SIGTERM)] {
        let home = Home::new();
        let original = br#"{"AUTOROUTER_PORT":"8123","TYPESAFE_API_KEY":"synthetic-old"}"#;
        fs::write(home.config(), original).unwrap();
        let mut terminal = Terminal::new();
        let mut child = terminal.spawn(home.command().args(["config", "set", "TYPESAFE_API_KEY"]));
        terminal.prompt(&mut child);
        terminal.type_bytes(b"synthetic-PRIVATE-not-saved");
        if let Some(signal) = cancellation {
            kill(Pid::from_raw(child.0.id() as i32), signal).unwrap();
        } else {
            terminal.type_bytes(b"\x03");
        }
        assert!(!child.wait().success());
        terminal.restored();
        assert_eq!(fs::read(home.config()).unwrap(), original);
        assert!(String::from_utf8_lossy(&terminal.visible).contains("Setup cancelled"));
    }
}

#[test]
fn stale_hidden_edit_cannot_replace_a_concurrent_configuration_change() {
    let home = Home::new();
    fs::write(home.config(), br#"{"AUTOROUTER_PORT":"8123"}"#).unwrap();
    let mut terminal = Terminal::new();
    let mut child = terminal.spawn(home.command().args(["config", "set", "TYPESAFE_API_KEY"]));
    terminal.prompt(&mut child);
    let concurrent = br#"{"AUTOROUTER_PORT":"9001","AUTOROUTER_DEBUG":"1"}"#;
    fs::write(home.config(), concurrent).unwrap();
    terminal.type_bytes(b"synthetic-PRIVATE-new\r");
    assert!(!child.wait().success());
    terminal.restored();
    assert_eq!(fs::read(home.config()).unwrap(), concurrent);
    assert!(
        String::from_utf8_lossy(&terminal.visible)
            .contains("changed while this operation was running")
    );
}

#[test]
fn stdin_secret_accepts_regular_files_and_exact_limit_but_rejects_terminal_input() {
    let home = Home::new();
    let path = home.0.join("input");
    fs::write(&path, b"  synthetic-PRIVATE-file \r\n").unwrap();
    let output = home
        .command()
        .args(["config", "set", "TYPESAFE_API_KEY", "--stdin"])
        .stdin(File::open(&path).unwrap())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("PRIVATE"));
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(home.config()).unwrap()).unwrap()["TYPESAFE_API_KEY"],
        "synthetic-PRIVATE-file"
    );
    let mut limit = vec![b'x'; 16_384];
    limit[0] = b'S';
    let output = input_output(
        home.command()
            .args(["config", "set", "TYPESAFE_API_KEY", "--stdin"]),
        &limit,
    );
    assert!(output.status.success());
    let before = fs::read(home.config()).unwrap();
    let mut terminal = Terminal::new();
    let mut child =
        terminal.spawn(
            home.command()
                .args(["config", "set", "TYPESAFE_API_KEY", "--stdin"]),
        );
    assert!(!child.wait().success());
    terminal.restored();
    assert_eq!(fs::read(home.config()).unwrap(), before);
    assert!(String::from_utf8_lossy(&terminal.visible).contains("Pipe the secret"));
}

#[test]
fn persisted_settings_are_owned_private_atomic_and_never_follow_write_symlinks() {
    let home = Home::new();
    fs::set_permissions(&home.0, fs::Permissions::from_mode(0o755)).unwrap();
    let path = home.0.join("new-parent").join("nested").join("config.json");
    let output = home
        .command()
        .env("AUTOROUTER_CONFIG", &path)
        .args(["config", "set", "AUTOROUTER_PORT", "8123"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_private(&path, 0o600);
    assert_private(path.parent().unwrap(), 0o700);
    assert_private(path.parent().unwrap().parent().unwrap(), 0o700);
    assert_private(&home.0, 0o755);
    let old_inode = fs::metadata(&path).unwrap().ino();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let output = home
        .command()
        .env("AUTOROUTER_CONFIG", &path)
        .args(["config", "set", "AUTOROUTER_PORT", "8124"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_private(&path, 0o600);
    assert_ne!(fs::metadata(&path).unwrap().ino(), old_inode);
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    let before = fs::read(&path).unwrap();
    symlink(&path, home.config()).unwrap();
    let output = home
        .command()
        .args(["config", "set", "AUTOROUTER_PORT", "9999"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(
        fs::symlink_metadata(home.config())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_file(home.config()).unwrap();
    let dangling = home.0.join("synthetic-PRIVATE-dangling-target");
    symlink(&dangling, home.config()).unwrap();
    let output = home
        .command()
        .args(["config", "set", "AUTOROUTER_PORT", "9999"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!dangling.exists());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("PRIVATE"));
}

fn snapshot(home: &Home, sessions: &str) -> PathBuf {
    let path = home.0.join("snapshot.json");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    fs::write(
        &path,
        format!(
            "{{\"version\":1,\"pid\":{},\"heartbeat_at\":{now},\"sessions\":{sessions}}}",
            std::process::id()
        ),
    )
    .unwrap();
    path
}
fn status(home: &Home, snapshot: &Path, input: &[u8]) -> Output {
    input_output(
        home.command()
            .arg("statusline")
            .env("AUTOROUTER_STATUS_FILE", snapshot)
            .env("NO_COLOR", "")
            .env("COLUMNS", "300"),
        input,
    )
}
#[test]
fn hidden_status_renderer_selects_exact_utf16_identity_before_display_projection() {
    let home = Home::new();
    let path = snapshot(
        &home,
        r#"{"\ud800":{"phase":"ready","actual_model":"claude-haiku-4-5-20251001","completion_confirmed":true},"\ud801":{"phase":"ready","actual_model":"claude-opus-5-5","completion_confirmed":true}}"#,
    );
    let mut exact = autorouter_core::js_json::JsDocument::parse(&fs::read(&path).unwrap()).unwrap();
    exact.set_root_field_json("savings", br#"{"\ud800":{"requests":1,"unpriced_requests":0,"actual_usd":0.01,"baseline_usd":1,"saved_usd":0.11},"\ud801":{"requests":1,"unpriced_requests":0,"actual_usd":0.02,"baseline_usd":1,"saved_usd":0.22}}"#).unwrap();
    fs::write(&path, exact.stringify()).unwrap();
    for (id, expected, excluded, savings, other_savings) in [
        (r#""\ud800""#, "Haiku 4.5", "Opus 5.5", "$0.11", "$0.22"),
        (r#""\ud801""#, "Opus 5.5", "Haiku 4.5", "$0.22", "$0.11"),
    ] {
        let output = status(&home, &path, format!("{{\"session_id\":{id}}}").as_bytes());
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains(expected), "{text}");
        assert!(!text.contains(excluded), "{text}");
        assert!(
            text.contains(savings) && !text.contains(other_savings),
            "{text}"
        );
    }
    let output = status(&home, &path, br#"{"session_id":"\ufffd"}"#);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("awaiting request"));
    assert!(!text.contains("Haiku") && !text.contains("Opus"));
    assert!(!text.contains('$'));
}

#[test]
fn hidden_status_renderer_handles_invalid_and_bounded_input_without_leaks() {
    let home = Home::new();
    fs::write(home.config(), "synthetic-PRIVATE-malformed-config").unwrap();
    let path = snapshot(
        &home,
        r#"{"":{"phase":"ready","selected_model":"claude-sonnet-4-6","completion_confirmed":false},"__proto__":{"phase":"error","selected_model":"claude-opus-5-5","error_type":"\u001b[31moverloaded_error","error_message":"synthetic-PRIVATE-error"}}"#,
    );
    for input in [
        b"[".to_vec(),
        b"null".to_vec(),
        b"[]".to_vec(),
        b"{\"session_id\":null}".to_vec(),
        vec![b' '; 1024 * 1024 + 1],
    ] {
        let output = status(&home, &path, &input);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert!(String::from_utf8_lossy(&output.stdout).contains("awaiting request"));
        assert!(!output.stdout.contains(&27));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("PRIVATE"));
    }
    let output = status(&home, &path, b"{}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Sonnet") && text.contains("unconfirmed"));
    let output = status(&home, &path, br#"{"session_id":"__proto__"}"#);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Opus") && text.contains("error"));
    assert!(!text.contains("PRIVATE") && !text.contains('\u{1b}'));
    for contents in [b"{".to_vec(), vec![b'x'; 1024 * 1024 + 1]] {
        fs::write(&path, contents).unwrap();
        let output = status(&home, &path, b"{}");
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("offline"));
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn opaque_unix_arguments_and_environment_reach_claude_byte_for_byte() {
    let home = Home::new();
    home.claude("#!/bin/sh\nprintf '%s\\000' \"$@\" \"$OPAQUE_VALUE\"\n");
    let raw = OsString::from_vec(b"synthetic-\xff-argument".to_vec());
    let env = OsString::from_vec(b"synthetic-\xfe-environment".to_vec());
    for statusline in ["0", "1"] {
        let output = home
            .launch()
            .arg("claude")
            .arg(&raw)
            .args(["--", "--settings=literal"])
            .env("OPAQUE_VALUE", &env)
            .env("AUTOROUTER_STATUSLINE", statusline)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            output.stdout,
            b"synthetic-\xff-argument\0--\0--settings=literal\0synthetic-\xfe-environment\0"
        );
        if statusline == "0" {
            assert!(output.stderr.is_empty());
        } else {
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("Passing your original settings")
            );
        }
        home.assert_clean_launch();
    }
}

#[test]
fn repeated_launches_preserve_saved_settings_permission_anchors_and_cleanup() {
    let home = Home::new();
    let source = home.0.join("source '[literal]*? $HOME");
    fs::create_dir(&source).unwrap();
    let path = source.join("settings.json");
    let original = br#"{"opaque":"\ud800","permissions":{"allow":["Read(/public/**)"],"ask":["Edit(/review/**)"],"deny":["Read(/private/**)"],"defaultMode":"dontAsk"},"statusLine":{"command":"original"}}"#;
    fs::write(&path, original).unwrap();
    home.claude("#!/bin/sh\n[ \"$1\" = '--settings' ] || exit 81\nprintf '%s\\000' \"$AUTOROUTER_STATUS_FILE\" \"$2\" \"$ANTHROPIC_BASE_URL\"\n/bin/cat \"$2\"\nexit 17\n");
    let mut previous = Vec::new();
    for _ in 0..4 {
        let output = home
            .launch()
            .args(["claude", "--settings"])
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(17));
        assert!(output.stderr.is_empty());
        let fields: Vec<_> = output.stdout.splitn(4, |byte| *byte == 0).collect();
        assert_eq!(fields.len(), 4);
        for field in &fields[..2] {
            let path = PathBuf::from(OsString::from_vec(field.to_vec()));
            assert!(!path.exists());
            assert!(!previous.contains(&path));
            previous.push(path);
        }
        let base = std::str::from_utf8(fields[2])
            .unwrap()
            .strip_prefix("http://")
            .unwrap();
        assert!(std::net::TcpStream::connect(base).is_err());
        let exact = std::str::from_utf8(fields[3]).unwrap();
        assert!(exact.contains("\\ud800"));
        let observed = autorouter_core::js_json::JsDocument::parse(fields[3])
            .unwrap()
            .to_serde_observation_lossy();
        assert_eq!(observed["permissions"]["defaultMode"], "dontAsk");
        let anchor = source
            .to_str()
            .unwrap()
            .chars()
            .fold(String::new(), |mut text, character| {
                if matches!(character, '\\' | '*' | '?' | '[' | ']') {
                    text.push('\\');
                }
                text.push(character);
                text
            });
        assert_eq!(
            observed["permissions"]["allow"],
            json!([format!("Read(/{anchor}/public/**)")])
        );
        assert_eq!(
            observed["permissions"]["ask"],
            json!([format!("Edit(/{anchor}/review/**)")])
        );
        assert_eq!(
            observed["permissions"]["deny"],
            json!([format!("Read(/{anchor}/private/**)")])
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        home.assert_clean_launch();
    }
}

#[test]
fn installed_launcher_owns_private_status_and_settings_until_child_exit() {
    use std::io::{BufRead, BufReader};
    let home = Home::new();
    home.claude("#!/bin/sh\n[ \"$1\" = '--settings' ] || exit 81\nprintf '%s\\n%s\\n' \"$AUTOROUTER_STATUS_FILE\" \"$2\"\nIFS= read -r action\n[ \"$action\" = done ] || exit 82\n");
    let mut child = ChildGuard(
        home.launch()
            .arg("claude")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let output = child.0.stdout.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut output = BufReader::new(output);
        for _ in 0..2 {
            let mut line = String::new();
            output.read_line(&mut line).unwrap();
            if send.send(line).is_err() {
                break;
            }
        }
    });
    let snapshot = PathBuf::from(receive.recv_timeout(Duration::from_secs(5)).unwrap().trim());
    let settings = PathBuf::from(receive.recv_timeout(Duration::from_secs(5)).unwrap().trim());
    reader.join().unwrap();
    assert_eq!(snapshot.parent(), settings.parent());
    assert_private(&snapshot, 0o600);
    assert_private(&settings, 0o600);
    assert_private(snapshot.parent().unwrap(), 0o700);
    assert!(
        !fs::symlink_metadata(&snapshot)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        !fs::symlink_metadata(&settings)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    child.0.stdin.take().unwrap().write_all(b"done\n").unwrap();
    assert!(child.wait().success());
    assert!(!snapshot.exists() && !settings.exists());
    home.assert_clean_launch();
}
