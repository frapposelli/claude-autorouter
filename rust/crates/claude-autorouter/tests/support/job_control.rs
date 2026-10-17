//! A session-owning supervisor keeps the foreground CLI group nonorphaned.
//! No signal or terminal mutation reaches the cargo/libtest parent. The outer
//! test retains the PTY master until its direct supervisor has been reaped.
use super::{Home, Terminal, assert_private};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::sys::signal::{Signal, kill, killpg};
use nix::sys::termios::{LocalFlags, Termios, cfgetispeed, cfgetospeed, tcgetattr};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{Pid, getpgid, getpgrp, getpid, getsid, setsid, tcgetpgrp, ttyname};
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const TEST: &str =
    "hidden_secret_real_stop_resume_preserves_private_unicode_editing_and_cancellation";
const MODE: &str = "AUTOROUTER_TEST_JOB_CONTROL_MODE";
const TARGET: &str = "AUTOROUTER_TEST_JOB_CONTROL_TARGET";
const SLAVE: &str = "AUTOROUTER_TEST_JOB_CONTROL_SLAVE";
const SCENARIO: &str = "AUTOROUTER_TEST_JOB_CONTROL_SCENARIO";
const ANCHOR: &str = "AUTOROUTER_TEST_JOB_CONTROL_ANCHOR";
const PREFIX: &str = "synthetic-PRIVATE-a🧪😀b";
const ORIGINAL: &[u8] = br#"{"AUTOROUTER_PORT":"8123","TYPESAFE_API_KEY":"synthetic-old","AUTOROUTER_SECRET_STORE":"file"}"#;
const LIMIT: usize = 65_536;

fn report(value: Value) {
    // Parent/control-output failure must never skip owned-process cleanup.
    let mut output = std::io::stdout().lock();
    let _ = writeln!(output, "JOB_CONTROL {value}");
    let _ = output.flush();
}
fn nonblocking(fd: &impl std::os::fd::AsFd) {
    let flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL).unwrap());
    fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).unwrap();
}
fn read_available(reader: &mut impl Read, bytes: &mut Vec<u8>) {
    let mut buffer = [0; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return,
            Ok(count) => {
                assert!(bytes.len() + count <= LIMIT, "job-control output bound");
                bytes.extend_from_slice(&buffer[..count]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.raw_os_error() == Some(5) => {
                return;
            }
            Err(e) => panic!("job-control read: {e}"),
        }
    }
}
fn write_bounded(writer: &mut File, bytes: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut offset = 0;
    while offset < bytes.len() {
        match writer.write(&bytes[offset..]) {
            Ok(0) => panic!("job-control input closed"),
            Ok(count) => offset += count,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("job-control write: {e}"),
        }
        assert!(Instant::now() < deadline, "job-control input deadline");
        if offset < bytes.len() {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
fn terminal_state(t: &Termios) -> Value {
    json!({"input":t.input_flags.bits(),"output":t.output_flags.bits(),
        "control":t.control_flags.bits(),"local":t.local_flags.difference(LocalFlags::PENDIN).bits(),
        "characters":t.control_chars.to_vec(),"input_speed":cfgetispeed(t) as u32,
        "output_speed":cfgetospeed(t) as u32})
}
fn mutable_flags(fd: &impl std::os::fd::AsFd) -> i32 {
    // F_SETFL status flags relevant to this shared terminal. Raw F_GETFL is
    // reported separately: XNU FWASWRITTEN (0x10000) records prior writes.
    // https://github.com/apple/darwin-xnu/blob/main/bsd/sys/fcntl.h
    fcntl(fd, FcntlArg::F_GETFL).unwrap()
        & (OFlag::O_NONBLOCK | OFlag::O_APPEND | OFlag::O_ASYNC | OFlag::O_ACCMODE).bits()
}
struct ForegroundChild {
    child: Child,
    reaped: bool,
}
impl ForegroundChild {
    fn pid(&self) -> Pid {
        Pid::from_raw(self.child.id() as i32)
    }
    fn poll(&mut self) -> WaitStatus {
        let status = waitpid(
            self.pid(),
            Some(WaitPidFlag::WNOHANG | WaitPidFlag::WUNTRACED | WaitPidFlag::WCONTINUED),
        )
        .unwrap();
        // waitpid may reap an unexpected exit even when checking for a stop.
        // Reserve the PID only until that point, never signal it afterwards.
        if matches!(status, WaitStatus::Exited(..) | WaitStatus::Signaled(..)) {
            self.reaped = true;
        }
        status
    }
}
impl Drop for ForegroundChild {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let _ = kill(self.pid(), Signal::SIGKILL);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match waitpid(self.pid(), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) => std::thread::sleep(Duration::from_millis(2)),
                Ok(WaitStatus::Exited(..) | WaitStatus::Signaled(..))
                | Err(nix::errno::Errno::ECHILD) => {
                    self.reaped = true;
                    break;
                }
                _ => break,
            }
        }
        report(json!({"phase":"cleanup","reaped":self.reaped}));
    }
}
fn child_command(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
        .env_clear()
        .env(MODE, mode);
    for key in ["HOME", "PATH", "AUTOROUTER_CONFIG", TARGET, SLAVE, SCENARIO] {
        command.env(key, std::env::var_os(key).unwrap());
    }
    command
}
pub fn helper_entry() -> bool {
    match std::env::var(MODE).as_deref() {
        Ok("exec") => {
            // Spawn first, then make its group foreground, then release this
            // gate. No terminal read can accidentally stop the child via TTIN.
            let gate = PathBuf::from(std::env::var_os("HOME").unwrap()).join("foreground-ready");
            let deadline = Instant::now() + Duration::from_secs(5);
            while !gate.exists() {
                assert!(Instant::now() < deadline, "foreground gate deadline");
                std::thread::sleep(Duration::from_millis(2));
            }
            let mut command = Command::new(std::env::var_os(TARGET).unwrap());
            command
                .args(["config", "set", "TYPESAFE_API_KEY"])
                .env_clear();
            for key in ["HOME", "PATH", "AUTOROUTER_CONFIG"] {
                command.env(key, std::env::var_os(key).unwrap());
            }
            command
                .env("AUTOROUTER_EVALUATOR", "jev")
                .env("AUTOROUTER_SECRET_STORE", "file");
            panic!("exec synthetic CLI: {}", command.exec());
        }
        Ok("supervisor") | Ok("manager") => {
            let result = std::panic::catch_unwind(|| {
                if std::env::var(MODE).unwrap() == "supervisor" {
                    anchor();
                } else {
                    supervise();
                }
            });
            report(json!({"phase":"complete","passed":result.is_ok()}));
            // The direct foreground child was reaped or bounded-cleaned by its
            // guard. Avoid a libtest panic dump into the synthetic terminal.
            std::process::exit(if result.is_ok() { 0 } else { 1 });
        }
        _ => false,
    }
}
struct AnchorGuard {
    manager: Pid,
    reaped: bool,
}
impl AnchorGuard {
    fn clean_manager(&mut self) {
        if self.reaped {
            return;
        }
        let _ = killpg(self.manager, Signal::SIGKILL);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match waitpid(self.manager, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(..) | WaitStatus::Signaled(..)) => {
                    self.reaped = true;
                    break;
                }
                Ok(WaitStatus::StillAlive) => std::thread::sleep(Duration::from_millis(2)),
                _ => break,
            }
        }
    }
}
impl Drop for AnchorGuard {
    fn drop(&mut self) {
        self.clean_manager();
        // Always last: this process is the group leader and cannot have had
        // its PID reused. Includes CLI cleanup even on a closed output pipe.
        let _ = killpg(getpgrp(), Signal::SIGKILL);
    }
}
// The outer test owns this group leader directly and never reaps it before
// killing its foreground group. The CLI joins this anchored group; its parent
// manager is in a different same-session group, preserving nonorphaned job control.
fn anchor() {
    let session = setsid().unwrap();
    assert_eq!(session, getpid());
    let scenario = std::env::var(SCENARIO).unwrap();
    assert_ne!(scenario, "pre-ready-failure", "injected pre-ready failure");
    let slave = OpenOptions::new()
        .read(true)
        .write(true)
        .open(std::env::var_os(SLAVE).unwrap())
        .unwrap();
    assert_eq!(tcgetpgrp(&slave).unwrap(), getpgrp());
    let mut command = child_command("manager");
    command
        .env(ANCHOR, getpid().as_raw().to_string())
        .process_group(0)
        .stdin(Stdio::null());
    // AnchorGuard reaps through bounded waitpid(WNOHANG), not Child::wait.
    #[allow(clippy::zombie_processes)]
    let manager = command.spawn().unwrap();
    let manager_pid = Pid::from_raw(manager.id() as i32);
    let mut guard = AnchorGuard {
        manager: manager_pid,
        reaped: false,
    };
    report(json!({"phase":"anchor","anchor":getpid().as_raw(),"manager":manager_pid.as_raw()}));
    let mut control = File::from(nix::unistd::dup(std::io::stdin()).unwrap());
    nonblocking(&control);
    let deadline = Instant::now() + Duration::from_secs(25);
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let mut stopped = false;
    let mut overflowed = false;
    loop {
        if scenario == "capture-overflow" && home.join("overflow-output").exists() && !overflowed {
            let _ = std::io::stdout().write_all(&vec![b'x'; LIMIT + 1]);
            overflowed = true;
        }
        if scenario == "stopped-anchor" && home.join("stop-anchor").exists() && !stopped {
            report(json!({"phase":"anchor_stopping"}));
            kill(getpid(), Signal::SIGSTOP).unwrap();
            stopped = true;
        }
        if scenario == "stopped-manager" && !stopped {
            match waitpid(
                manager_pid,
                Some(WaitPidFlag::WNOHANG | WaitPidFlag::WUNTRACED),
            )
            .unwrap()
            {
                WaitStatus::Stopped(_, Signal::SIGSTOP) => {
                    report(json!({"phase":"manager_stopped"}));
                    stopped = true;
                }
                WaitStatus::StillAlive => {}
                WaitStatus::Exited(..) | WaitStatus::Signaled(..) => {
                    guard.reaped = true;
                    break;
                }
                status => panic!("Unexpected manager stop: {status:?}"),
            }
        }
        let mut bytes = [0; 64];
        match control.read(&mut bytes) {
            Ok(_) => break, // EOF or the parent's finish command requests cleanup.
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => break,
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    // Only its direct owner signals this separately grouped manager. The outer
    // test never signals a reported manager PID, even if this anchor exits.
    guard.clean_manager();
    if guard.reaped {
        let _ = fs::write(home.join("anchor-cleaned"), b"clean");
    }
    report(json!({"phase":"anchor_cleanup","manager_reaped":guard.reaped}));
    assert!(guard.reaped, "manager cleanup deadline");
    // Keep this anchor alive and its PID/PGID reserved until the outer owner
    // kills its foreground group. If that owner disappears, the same bounded
    // watchdog kills this self-owned group; no foreign numeric PID is used.
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(2));
    }
    let _ = killpg(getpgrp(), Signal::SIGKILL);
}
fn supervise() {
    let scenario = std::env::var(SCENARIO).unwrap();
    let session = getsid(None).unwrap();
    let anchor = Pid::from_raw(std::env::var(ANCHOR).unwrap().parse().unwrap());
    assert_eq!(session, anchor);
    assert_ne!(getpgrp(), anchor);
    let slave = OpenOptions::new()
        .read(true)
        .write(true)
        .open(std::env::var_os(SLAVE).unwrap())
        .unwrap();
    assert_eq!(tcgetpgrp(&slave).unwrap(), anchor);
    let original = terminal_state(&tcgetattr(&slave).unwrap());
    let original_flags = mutable_flags(&slave);
    let raw_flags = fcntl(&slave, FcntlArg::F_GETFL).unwrap();
    let mut command = child_command("exec");
    command
        .process_group(anchor.as_raw())
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave.try_clone().unwrap());
    let mut child = ForegroundChild {
        child: command.spawn().unwrap(),
        reaped: false,
    };
    assert_eq!(getsid(Some(child.pid())).unwrap(), session);
    assert_eq!(getpgid(Some(child.pid())).unwrap(), anchor);
    report(
        json!({"phase":"owned","session":session.as_raw(),"manager":getpid().as_raw(),"manager_group":getpgrp().as_raw(),"child":child.pid().as_raw(),"foreground_group":anchor.as_raw(),"original_flags":raw_flags}),
    );
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    fs::write(home.join("foreground-ready"), b"ready").unwrap();
    if scenario == "stopped-manager" {
        kill(getpid(), Signal::SIGSTOP).unwrap();
    }
    let deadline = Instant::now()
        + if scenario == "timeout" {
            Duration::from_millis(150)
        } else {
            Duration::from_secs(15)
        };
    let config = PathBuf::from(std::env::var_os("AUTOROUTER_CONFIG").unwrap());

    let mut cycle = 0;
    let mut resuming = false;
    loop {
        match child.poll() {
            WaitStatus::Stopped(pid, signal) => {
                assert_eq!(pid, child.pid());
                assert_eq!(signal, Signal::SIGTSTP);
                let restored = terminal_state(&tcgetattr(&slave).unwrap());
                report(
                    json!({"phase":"stopped","cycle":cycle,"signal":signal as i32,"terminal_restored":restored==original,"raw_flags":fcntl(&slave,FcntlArg::F_GETFL).unwrap(),"mutable_flags_restored":mutable_flags(&slave)==original_flags}),
                );
                assert_eq!(restored, original, "terminal settings while stopped");
                assert_eq!(
                    mutable_flags(&slave),
                    original_flags,
                    "mutable descriptor flags while stopped"
                );
                assert_eq!(fs::read(&config).unwrap(), ORIGINAL);
            }
            WaitStatus::Exited(_, code) => {
                assert_eq!(terminal_state(&tcgetattr(&slave).unwrap()), original);
                assert_eq!(mutable_flags(&slave), original_flags);
                let success = scenario == "success";
                assert_eq!(code, if success { 0 } else { 1 });
                if success {
                    let saved: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
                    assert_eq!(saved["TYPESAFE_API_KEY"], "synthetic-PRIVATE-a🧪Zb");
                    assert_private(&config, 0o600);
                } else {
                    assert_eq!(fs::read(&config).unwrap(), ORIGINAL);
                }
                report(
                    json!({"phase":"exit","code":code,"reaped":child.reaped,"terminal_restored":true,"mutable_flags_restored":true,"configuration_checked":true}),
                );
                return;
            }
            WaitStatus::StillAlive | WaitStatus::Continued(_) => {}
            status => panic!("Unexpected foreground status: {status:?}"),
        }
        if !resuming && home.join(format!("resume-{cycle}")).exists() {
            kill(child.pid(), Signal::SIGCONT).unwrap();
            resuming = true;
        }
        if resuming {
            let state = tcgetattr(&slave).unwrap();
            if !state.local_flags.intersects(
                LocalFlags::ECHO | LocalFlags::ECHONL | LocalFlags::ICANON | LocalFlags::ISIG,
            ) {
                assert_ne!(mutable_flags(&slave) & OFlag::O_NONBLOCK.bits(), 0);
                report(json!({"phase":"resumed","cycle":cycle,"hidden":true}));
                cycle += 1;
                resuming = false;
            }
        }
        assert!(Instant::now() < deadline, "supervisor fixture deadline");
        std::thread::sleep(Duration::from_millis(2));
    }
}

struct Supervisor {
    child: Child,
    stdout: Option<std::process::ChildStdout>,
    input: Option<std::process::ChildStdin>,
    terminal: Terminal,
    bytes: Vec<u8>,
    reaped: bool,
    cleaned: bool,
    cleanup_path: PathBuf,
    capture_failed: bool,
}
impl Supervisor {
    fn pump(&mut self) {
        self.terminal.read_available();
        if let Some(stdout) = &mut self.stdout {
            read_available(stdout, &mut self.bytes);
        }
    }
    fn rows(&self) -> Vec<Value> {
        String::from_utf8_lossy(&self.bytes)
            .lines()
            .filter_map(|line| {
                let (_, json) = line.split_once("JOB_CONTROL ")?;
                serde_json::from_str(json).ok()
            })
            .collect()
    }
    fn phase(&mut self, phase: &str, cycle: Option<usize>) {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            self.pump();
            if self
                .rows()
                .iter()
                .any(|r| r["phase"] == phase && cycle.is_none_or(|c| r["cycle"] == c))
            {
                return;
            }
            if self
                .rows()
                .iter()
                .any(|r| r["phase"] == "complete" && r["passed"] == false)
            {
                panic!(
                    "helper failed before {phase}: {}",
                    String::from_utf8_lossy(&self.bytes)
                );
            }
            assert!(Instant::now() < deadline, "supervisor {phase} deadline");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn cleanup_pump(&mut self) {
        fn drain(reader: &mut impl Read, target: &mut Vec<u8>) -> bool {
            let mut buffer = [0; 4096];
            let mut valid = true;
            for _ in 0..16 {
                match reader.read(&mut buffer) {
                    Ok(0) => return valid,
                    Ok(count) => {
                        let available = LIMIT.saturating_sub(target.len());
                        target.extend_from_slice(&buffer[..count.min(available)]);
                        valid &= count <= available;
                    }
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.raw_os_error() == Some(5) =>
                    {
                        return valid;
                    }
                    Err(_) => return false,
                }
            }
            valid
        }
        self.capture_failed |= !drain(&mut self.terminal.master, &mut self.terminal.visible);
        if let Some(stdout) = &mut self.stdout {
            self.capture_failed |= !drain(stdout, &mut self.bytes);
        }
    }
    fn finish(&mut self) {
        if self.cleaned {
            return;
        }
        let anchor = Pid::from_raw(self.child.id() as i32);
        // This is the outer's own unreaped leader; SIGCONT/killpg cannot target
        // a reused group. The outer never sends a signal to the manager PID.
        let _ = killpg(anchor, Signal::SIGCONT);
        if let Some(mut input) = self.input.take() {
            let _ = input.write_all(b"finish\n");
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            self.cleanup_pump();
            if fs::read(&self.cleanup_path).is_ok_and(|b| b == b"clean") {
                break;
            }
            if self
                .rows()
                .iter()
                .any(|r| r["phase"] == "complete" && r["passed"] == false)
                && !self.rows().iter().any(|r| r["phase"] == "anchor")
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = killpg(anchor, Signal::SIGKILL);
        self.cleaned = true;
        // No descendant group signals follow this point. A responsive manager
        // was already reaped by the anchor; a stalled anchor's responsive
        // manager sees the CLI exit and its bounded loop exits. Simultaneously
        // stopped/unresponsive helpers are explicitly unsupported by this
        // portable fixture; deadlines still return failure without stale-PID signals.
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            self.cleanup_pump();
            if self.child.try_wait().ok().flatten().is_some() {
                self.reaped = true;
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
impl Drop for Supervisor {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        // Avoid a second panic during cleanup if a capture assertion failed.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.finish()));
        if !self.reaped {
            if !self.cleaned {
                let _ = killpg(Pid::from_raw(self.child.id() as i32), Signal::SIGKILL);
                self.cleaned = true;
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                let mut bytes = [0; 4096];
                let _ = self.terminal.master.read(&mut bytes);
                if self.child.try_wait().ok().flatten().is_some() {
                    self.reaped = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

pub fn check(scenario: &str) {
    let home = Home::new();
    fs::write(home.config(), ORIGINAL).unwrap();
    fs::set_permissions(home.config(), fs::Permissions::from_mode(0o600)).unwrap();
    let terminal = Terminal::new();
    let target = std::env::var_os("AUTOROUTER_TEST_EXECUTABLE")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_claude-autorouter").into());
    let diagnostic_path = home.0.join("supervisor.stderr");
    let diagnostic = File::create(&diagnostic_path).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
        .current_dir(&home.0)
        .env_clear()
        .env(MODE, "supervisor")
        .env(TARGET, target)
        .env(SLAVE, ttyname(&terminal.slave).unwrap())
        .env(SCENARIO, scenario)
        .env("HOME", &home.0)
        .env("PATH", &home.0)
        .env("AUTOROUTER_CONFIG", home.config())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(diagnostic);
    let mut child = command.spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    nonblocking(&stdout);
    let input = child.stdin.take();
    nonblocking(input.as_ref().unwrap());
    let mut supervisor = Supervisor {
        child,
        stdout: Some(stdout),
        input,
        terminal,
        bytes: Vec::new(),
        reaped: false,
        cleaned: false,
        cleanup_path: home.0.join("anchor-cleaned"),
        capture_failed: false,
    };
    if ["success", "cancel"].contains(&scenario) {
        supervisor.phase("owned", None);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            supervisor.pump();
            if supervisor
                .terminal
                .visible
                .windows(b"(hidden): ".len())
                .any(|p| p == b"(hidden): ")
            {
                break;
            }
            assert!(Instant::now() < deadline, "hidden prompt deadline");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            !tcgetattr(&supervisor.terminal.slave)
                .unwrap()
                .local_flags
                .intersects(
                    LocalFlags::ECHO | LocalFlags::ECHONL | LocalFlags::ICANON | LocalFlags::ISIG
                )
        );
        write_bounded(&mut supervisor.terminal.master, PREFIX.as_bytes());
        for cycle in 0..2 {
            write_bounded(&mut supervisor.terminal.master, b"\x1a");
            supervisor.phase("stopped", Some(cycle));
            assert!(
                !String::from_utf8_lossy(&supervisor.terminal.visible)
                    .contains("synthetic-PRIVATE")
            );
            fs::write(home.0.join(format!("resume-{cycle}")), b"resume").unwrap();
            supervisor.phase("resumed", Some(cycle));
            if cycle == 0 {
                write_bounded(&mut supervisor.terminal.master, b"\x1b[D\x7fZ");
            }
        }
        write_bounded(
            &mut supervisor.terminal.master,
            if scenario == "success" {
                b"\r"
            } else {
                b"-after-resume\x03"
            },
        );
    }
    if scenario == "stopped-anchor" {
        supervisor.phase("owned", None);
        fs::write(home.0.join("stop-anchor"), b"stop").unwrap();
        supervisor.phase("anchor_stopping", None);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !anchor_stopped(supervisor.child.id()) {
            assert!(Instant::now() < deadline, "anchor stopped-state deadline");
            std::thread::sleep(Duration::from_millis(2));
        }
        println!("owned anchor observed stopped without reaping");
    } else if scenario == "stopped-manager" {
        supervisor.phase("manager_stopped", None);
    } else if scenario == "control-eof" {
        supervisor.phase("owned", None);
        drop(supervisor.input.take());
        drop(supervisor.stdout.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        // AnchorGuard kills its group even when both diagnostic/control pipes
        // disappear. Observe the cleanup marker before any reap attempt.
        while !supervisor.cleanup_path.exists() {
            supervisor.cleanup_pump();
            assert!(Instant::now() < deadline, "EOF cleanup marker deadline");
            std::thread::sleep(Duration::from_millis(2));
        }
        loop {
            supervisor.cleanup_pump();
            if let Some(status) = supervisor.child.try_wait().unwrap() {
                use std::os::unix::process::ExitStatusExt;
                supervisor.reaped = true;
                supervisor.cleaned = true;
                assert_eq!(status.signal(), Some(Signal::SIGKILL as i32));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "EOF self-owned group cleanup deadline"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    } else if scenario == "capture-overflow" {
        supervisor.phase("owned", None);
        fs::write(home.0.join("overflow-output"), b"overflow").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            loop {
                supervisor.pump();
                assert!(Instant::now() < deadline, "overflow fixture deadline");
                std::thread::sleep(Duration::from_millis(2));
            }
        }));
        let panic = failed.expect_err("overflow must reject the bounded capture");
        let message = panic
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("");
        assert_eq!(
            message, "job-control output bound",
            "unexpected overflow fixture failure"
        );
        supervisor.capture_failed = true;
    } else {
        let deadline = Instant::now() + Duration::from_secs(18);
        loop {
            supervisor.pump();
            if supervisor.rows().iter().any(|r| r["phase"] == "complete") {
                break;
            }
            assert!(Instant::now() < deadline, "manager completion deadline");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    supervisor.finish();
    let rows = supervisor.rows();
    let mut diagnostics = Vec::new();
    File::open(diagnostic_path)
        .unwrap()
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut diagnostics)
        .unwrap();
    assert!(diagnostics.len() <= LIMIT, "supervisor diagnostic bound");
    if scenario != "capture-overflow" {
        println!(
            "scenario={scenario}\n{}",
            String::from_utf8_lossy(&supervisor.bytes)
        );
    } else {
        println!("scenario=capture-overflow; bounded capture rejected and cleanup completed");
    }
    if ["pre-ready-failure", "timeout"].contains(&scenario) {
        assert!(
            rows.iter()
                .any(|r| r["phase"] == "complete" && r["passed"] == false)
        );
        if scenario == "timeout" {
            assert!(
                rows.iter()
                    .any(|r| r["phase"] == "cleanup" && r["reaped"] == true)
            );
        }
    } else if ["success", "cancel"].contains(&scenario) {
        assert!(
            diagnostics.is_empty(),
            "{}",
            String::from_utf8_lossy(&diagnostics)
        );
        assert_eq!(rows.iter().filter(|r| r["phase"] == "stopped").count(), 2);
        assert_eq!(rows.iter().filter(|r| r["phase"] == "resumed").count(), 2);
        assert!(
            rows.iter()
                .any(|r| r["phase"] == "exit" && r["reaped"] == true)
        );
        assert!(
            rows.iter()
                .any(|r| r["phase"] == "complete" && r["passed"] == true)
        );
        assert!(
            !String::from_utf8_lossy(&supervisor.terminal.visible).contains("synthetic-PRIVATE")
        );
        if scenario == "cancel" {
            assert!(
                String::from_utf8_lossy(&supervisor.terminal.visible).contains("Setup cancelled")
            );
        }
    }
    assert!(supervisor.reaped);
    if scenario != "pre-ready-failure" {
        assert!(fs::read(&supervisor.cleanup_path).is_ok_and(|bytes| bytes == b"clean"));
        println!("owned manager cleanup verified independently of output delivery");
    }
    assert_private(&home.0, 0o700);
    assert_private(&home.config(), 0o600);
}

// Test-only /bin/ps dependency: nix has no safe macOS waitid(WNOWAIT)
// wrapper. waitpid(WUNTRACED) could reap an unexpected anchor exit and lose
// the foreground group's PID reservation. ps observes without reaping.
fn anchor_stopped(pid: u32) -> bool {
    struct Probe(Child, bool);
    impl Drop for Probe {
        fn drop(&mut self) {
            if self.1 {
                return;
            }
            let _ = self.0.kill();
            let deadline = Instant::now() + Duration::from_secs(1);
            while Instant::now() < deadline {
                if self.0.try_wait().ok().flatten().is_some() {
                    self.1 = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
    let mut command = Command::new("/bin/ps");
    command
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = Probe(
        command.spawn().expect("test-only /bin/ps unavailable"),
        false,
    );
    let mut stdout = child.0.stdout.take().unwrap();
    nonblocking(&stdout);
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut bytes = Vec::new();
    loop {
        read_available(&mut stdout, &mut bytes);
        assert!(bytes.len() <= 128, "ps output bound");
        if let Some(status) = child.0.try_wait().unwrap() {
            child.1 = true;
            read_available(&mut stdout, &mut bytes);
            assert!(bytes.len() <= 128);
            assert!(status.success(), "owned-anchor ps observation failed");
            return String::from_utf8(bytes).unwrap().trim().starts_with('T');
        }
        assert!(Instant::now() < deadline, "ps observation deadline");
        std::thread::sleep(Duration::from_millis(2));
    }
}
