//! Subprocess tests of the isolated collector; no benchmark acceptance claims.
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Collector {
    process: Child,
    channel: Option<BufReader<UnixStream>>,
    pid: i32,
    scratch: std::path::PathBuf,
    descendant_connection: Option<UnixStream>,
    reaped: bool,
}
impl Collector {
    fn new(mode: &str) -> Self {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let name: String = nonce.iter().map(|value| format!("{value:02x}")).collect();
        // macOS's per-user temporary directory can exceed sockaddr_un's path
        // bound. This unpredictable private directory uses the POSIX short root.
        let scratch = std::path::Path::new("/tmp").join(format!("autorouter-resource-test-{name}"));
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(&scratch).unwrap();
        let listener = (mode == "descendant").then(|| {
            let listener = UnixListener::bind(scratch.join("descendant.sock")).unwrap();
            listener.set_nonblocking(true).unwrap();
            listener
        });
        let (parent, child) = UnixStream::pair().unwrap();
        parent
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let input = child.try_clone().unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
        command.arg("__benchmark-resource-child");
        if mode == "blocked" {
            // Ignoring is process-wide and inherited by sleep. Blocking SIGTERM
            // only in a libtest worker leaves its main thread able to receive it.
            command.args([
                "/bin/sh",
                "-c",
                "trap '' TERM; : > \"$AUTOROUTER_RESOURCE_READY\"; exec /bin/sleep 1800",
            ]);
        } else {
            command.arg(std::env::current_exe().unwrap()).args([
                "--exact",
                "resource_child_fixture",
                "--ignored",
                "--nocapture",
            ]);
        }
        let process = command
            .env_clear()
            .env("AUTOROUTER_RESOURCE_FIXTURE", mode)
            .env("AUTOROUTER_RESOURCE_READY", scratch.join("ready"))
            .env(
                "AUTOROUTER_RESOURCE_CONNECTION",
                scratch.join("descendant.sock"),
            )
            .stdin(Stdio::from(OwnedFd::from(input)))
            .stdout(Stdio::from(OwnedFd::from(child)))
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut result = Self {
            process,
            channel: Some(BufReader::new(parent)),
            pid: 0,
            scratch,
            descendant_connection: None,
            reaped: false,
        };
        let started = result.record();
        assert_eq!(started["kind"], "started");
        result.pid = i32::try_from(started["pid"].as_u64().unwrap()).unwrap();
        if let Some(listener) = listener {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut connection = loop {
                match listener.accept() {
                    Ok((connection, _)) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "descendant readiness deadline");
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("descendant readiness: {error}"),
                }
            };
            // Darwin inherits the listener's nonblocking status on accept.
            connection.set_nonblocking(false).unwrap();
            connection
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut ready = [0];
            connection.read_exact(&mut ready).unwrap();
            assert_eq!(&ready, b"D");
            result.descendant_connection = Some(connection);
        }
        result
    }
    fn record(&mut self) -> Value {
        let mut bytes = Vec::new();
        self.channel
            .as_mut()
            .unwrap()
            .take(4097)
            .read_until(b'\n', &mut bytes)
            .unwrap();
        assert!(!bytes.is_empty() && bytes.len() <= 4096);
        serde_json::from_slice(&bytes).unwrap()
    }
    fn wait_ready(&self) {
        let started = Instant::now();
        while !self.scratch.join("ready").exists() {
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn wait_exit(&mut self) -> std::process::ExitStatus {
        let started = Instant::now();
        loop {
            if let Some(status) = self.poll() {
                return status;
            }
            assert!(started.elapsed() < Duration::from_secs(15));
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn poll(&mut self) -> Option<std::process::ExitStatus> {
        let status = self.process.try_wait().unwrap();
        self.reaped |= status.is_some();
        status
    }
    fn assert_reaped(&self) {
        assert_eq!(
            nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(self.pid))),
            Err(nix::errno::Errno::ESRCH)
        );
    }
}
impl Drop for Collector {
    fn drop(&mut self) {
        self.channel.take();
        if self.reaped {
            let _ = fs::remove_dir_all(&self.scratch);
            return;
        }
        // Retain group ownership until cleanup even when a fixture assertion
        // fails while its collector is stopped or cannot service EOF.
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.process.id() as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            if self.process.try_wait().ok().flatten().is_some() {
                let _ = fs::remove_dir_all(&self.scratch);
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = self.process.kill();
        let _ = self.process.wait();
        let _ = fs::remove_dir_all(&self.scratch);
    }
}

#[test]
#[ignore = "Only entered by the collector integration tests in an isolated child"]
fn resource_child_fixture() {
    let mode = std::env::var("AUTOROUTER_RESOURCE_FIXTURE").unwrap();
    match mode.as_str() {
        "small" => {}
        "large" => {
            let mut bytes = vec![0u8; 64 * 1024 * 1024];
            for index in (0..bytes.len()).step_by(4096) {
                bytes[index] = (index / 4096 % 251 + 1) as u8;
            }
            std::hint::black_box(&bytes);
            drop(bytes); // The peak must survive freeing the resident allocation.
        }
        "failure" => std::process::exit(37),
        "descendant" => {
            let mut connection =
                UnixStream::connect(std::env::var_os("AUTOROUTER_RESOURCE_CONNECTION").unwrap())
                    .unwrap();
            // This deliberate orphan tests cleanup after the measured parent
            // exits; the collector's owned group must terminate it.
            #[allow(clippy::zombie_processes)]
            let mut descendant = Command::new("/bin/sh")
                .args(["-c", "printf D; exec /bin/cat >/dev/null"])
                .stdin(Stdio::from(OwnedFd::from(connection.try_clone().unwrap())))
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let mut ready = [0];
            descendant
                .stdout
                .take()
                .unwrap()
                .read_exact(&mut ready)
                .unwrap();
            assert_eq!(&ready, b"D");
            connection.write_all(b"D").unwrap();
            // Deliberately exit without waiting this descendant. Its stdin
            // keeps the outer socket live until owned-group cleanup kills it.
        }
        "owner" => {
            let child = Collector::new("blocked");
            child.wait_ready();
            let report = std::path::PathBuf::from(
                std::env::var_os("AUTOROUTER_RESOURCE_OWNER_REPORT").unwrap(),
            );
            let temporary = report.with_extension("tmp");
            fs::write(
                &temporary,
                serde_json::to_vec(&serde_json::json!({"pid":child.pid,"scratch":child.scratch}))
                    .unwrap(),
            )
            .unwrap();
            fs::rename(temporary, report).unwrap();
            // The outer test kills this owner; its control descriptor closes
            // without running Rust destructors or explicitly stopping helpers.
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        _ => panic!("unknown synthetic mode"),
    }
}

#[test]
fn lifetime_peak_survives_free_and_isolated_collectors_do_not_reuse_previous_peak() {
    let mut peaks = Vec::new();
    for mode in ["small", "large", "small"] {
        let mut child = Collector::new(mode);
        let report = child.record();
        assert_eq!(report["kind"], "finished");
        assert_eq!(report["success"], true);
        assert_eq!(report["stop_requested"], false);
        let usage = &report["resources"];
        let peak = usage["peak_rss_bytes"].as_u64().unwrap();
        let raw = usage["raw_max_rss"].as_u64().unwrap();
        assert_eq!(peak, raw * if cfg!(target_os = "macos") { 1 } else { 1024 });
        assert!(usage["user_cpu_microseconds"].as_i64().unwrap() >= 0);
        assert!(usage["system_cpu_microseconds"].as_i64().unwrap() >= 0);
        assert_eq!(
            report["collector_termination"],
            "owned_group_sigkill_after_report"
        );
        assert_eq!(child.wait_exit().signal(), Some(nix::libc::SIGKILL));
        child.assert_reaped();
        peaks.push(peak);
    }
    // This validates resident-byte accounting, not a timing/performance target.
    assert!(peaks[1] > peaks[0] + 32 * 1024 * 1024, "{peaks:?}");
    assert!(peaks[1] > peaks[2] + 32 * 1024 * 1024, "{peaks:?}");
}

#[test]
fn nonzero_child_status_is_preserved_without_false_clean_success() {
    let mut child = Collector::new("failure");
    let report = child.record();
    assert_eq!(report["success"], false);
    assert_eq!(report["exit_code"], 37);
    assert_eq!(child.wait_exit().signal(), Some(nix::libc::SIGKILL));
    child.assert_reaped();
}

#[test]
fn completed_report_ends_leftover_descendants_without_owner_teardown() {
    let mut child = Collector::new("descendant");
    let report = child.record();
    assert_eq!(report["kind"], "finished");
    assert_eq!(report["success"], true);
    assert_eq!(report["exit_code"], 0);
    assert_eq!(
        report["collector_termination"],
        "owned_group_sigkill_after_report"
    );
    // Keep the driver control endpoint open: cleanup cannot depend on owner
    // EOF or an acknowledgement after receiving the completed report.
    assert_eq!(
        child
            .descendant_connection
            .as_mut()
            .unwrap()
            .read(&mut [0])
            .unwrap(),
        0
    );
    assert_eq!(child.wait_exit().signal(), Some(nix::libc::SIGKILL));
    child.assert_reaped();
}

#[test]
fn control_eof_and_invalid_command_kill_and_reap_only_the_owned_child() {
    let mut unrelated = Collector::new("blocked");
    unrelated.wait_ready();
    for invalid in [false, true] {
        let mut child = Collector::new("blocked");
        child.wait_ready();
        if invalid {
            child
                .channel
                .as_mut()
                .unwrap()
                .get_mut()
                .write_all(b"?")
                .unwrap();
        } else {
            child.channel.take();
        }
        assert!(!child.wait_exit().success());
        child.assert_reaped();
        assert!(unrelated.poll().is_none());
        assert!(nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(unrelated.pid))).is_ok());
    }
    unrelated.channel.take();
    assert!(!unrelated.wait_exit().success());
    unrelated.assert_reaped();
}

#[test]
fn ignored_termination_hits_bounded_cleanup_and_never_reports_success() {
    let mut child = Collector::new("blocked");
    child.wait_ready();
    child
        .channel
        .as_mut()
        .unwrap()
        .get_mut()
        .write_all(b"T")
        .unwrap();
    assert!(!child.wait_exit().success());
    child.assert_reaped();
    let mut remaining = Vec::new();
    child
        .channel
        .as_mut()
        .unwrap()
        .take(4097)
        .read_until(b'\n', &mut remaining)
        .unwrap();
    assert!(remaining.is_empty());
}

#[test]
fn abrupt_owner_exit_closes_control_and_reaps_the_measured_child() {
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).unwrap();
    let name: String = nonce.iter().map(|value| format!("{value:02x}")).collect();
    let report = std::env::temp_dir().join(format!("autorouter-resource-owner-{name}.json"));
    let mut owner = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "resource_child_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env_clear()
        .env("AUTOROUTER_RESOURCE_FIXTURE", "owner")
        .env("AUTOROUTER_RESOURCE_OWNER_REPORT", &report)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let started = Instant::now();
    while !report.exists() {
        if started.elapsed() > Duration::from_secs(5) {
            let _ = owner.kill();
            let _ = owner.wait();
            panic!("Synthetic owner did not become ready");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let value: Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
    let pid = nix::unistd::Pid::from_raw(value["pid"].as_i64().unwrap() as i32);
    owner.kill().unwrap();
    owner.wait().unwrap();
    let started = Instant::now();
    while nix::unistd::getpgid(Some(pid)) != Err(nix::errno::Errno::ESRCH) {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "Owned child survived collector control EOF"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    fs::remove_dir_all(value["scratch"].as_str().unwrap()).unwrap();
    fs::remove_file(report).unwrap();
}
