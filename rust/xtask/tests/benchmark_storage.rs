//! Process admission/ownership tests; never numerical benchmarks.
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let mut bytes = [0; 12];
        getrandom::fill(&mut bytes).unwrap();
        let path = std::env::temp_dir().join(format!(
            "autorouter-storage-cli-{}",
            bytes.iter().map(|v| format!("{v:02x}")).collect::<String>()
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct OwnedChild(Child);
impl std::ops::Deref for OwnedChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl std::ops::DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn capture(command: &mut Command) -> Output {
    let scratch = Scratch::new();
    let stdout = scratch.0.join("stdout");
    let stderr = scratch.0.join("stderr");
    let mut child = OwnedChild(
        command
            .stdin(Stdio::null())
            .stdout(fs::File::create(&stdout).unwrap())
            .stderr(fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap(),
    );
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            start.elapsed() < Duration::from_secs(45),
            "Storage CLI test deadline"
        );
        for path in [&stdout, &stderr] {
            assert!(
                fs::metadata(path).unwrap().len() <= 65536,
                "Storage test output bound"
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    for path in [&stdout, &stderr] {
        assert!(
            fs::metadata(path).unwrap().len() <= 65536,
            "Storage final test output bound"
        );
    }
    Output {
        status,
        stdout: fs::read(stdout).unwrap(),
        stderr: fs::read(stderr).unwrap(),
    }
}
#[test]
fn help_and_rejected_modes_need_no_node_or_output_directory() {
    let scratch = Scratch::new();
    for args in [
        vec!["--help"],
        vec![],
        vec!["--validate", "--measure"],
        vec!["--module", "private.mjs"],
    ] {
        let output = capture(
            Command::new(env!("CARGO_BIN_EXE_xtask"))
                .arg("benchmark-storage")
                .args(&args)
                .env_clear()
                .env("PATH", "")
                .current_dir(&scratch.0),
        );
        assert_eq!(output.status.success(), args == ["--help"]);
        assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);
    }
}
#[test]
fn complete_native_validation_needs_no_node_and_retains_every_scenario() {
    let scratch = Scratch::new();
    let destination = scratch.0.join("evidence");
    // Reproduce Cargo's Linux executable layout without linking or modifying
    // the actual build artifact. Both names point only to this private copy.
    let original = fs::metadata(env!("CARGO_BIN_EXE_xtask")).unwrap();
    let native = scratch.0.join("native-xtask");
    fs::copy(env!("CARGO_BIN_EXE_xtask"), &native).unwrap();
    fs::hard_link(&native, scratch.0.join("native-xtask-link")).unwrap();
    let copied = fs::metadata(&native).unwrap();
    assert_eq!(copied.nlink(), 2);
    assert_ne!(
        (copied.dev(), copied.ino()),
        (original.dev(), original.ino())
    );
    let output = capture(
        Command::new(&native)
            .args(["benchmark-storage", "--validate", "--native"])
            .arg(&native)
            .arg("--output")
            .arg(&destination)
            .env_clear()
            .env("PATH", ""),
    );
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value =
        serde_json::from_slice(&fs::read(destination.join("report.json")).unwrap()).unwrap();
    assert_eq!(report["passed"], true);
    assert_eq!(report["expected_executions"], 16);
    assert_eq!(report["numerical_values_retained"], false);
    assert_eq!(report["acceptance_qualified"], false);
    let postflight: Value =
        serde_json::from_slice(&fs::read(destination.join("postflight.json")).unwrap()).unwrap();
    assert_eq!(postflight["passed"], true);
    assert!(
        postflight["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|input| input["unchanged"] == true)
    );
    assert_eq!(fs::metadata(&native).unwrap().nlink(), 2);
    assert_eq!(
        fs::metadata(env!("CARGO_BIN_EXE_xtask")).unwrap().nlink(),
        original.nlink()
    );
    let preflight: Value =
        serde_json::from_slice(&fs::read(destination.join("preflight.json")).unwrap()).unwrap();
    assert_eq!(preflight["reference_verified"], false);
    for reference_only in [
        "rust/parity/storage-reference.mjs",
        "scripts/rust-reference.mjs",
        "rust/parity/baseline.json",
    ] {
        assert!(
            !destination
                .join("input-snapshot")
                .join(reference_only)
                .exists()
        );
        assert!(preflight["inputs"].as_array().unwrap().iter().all(|input| {
            !std::path::Path::new(input["path"].as_str().unwrap()).ends_with(reference_only)
        }));
    }
    assert!(
        report["executions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["scratch_removed"] == true)
    );
    let before = fs::read(destination.join("report.json")).unwrap();
    let rerun = capture(
        Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["benchmark-storage", "--validate", "--output"])
            .arg(&destination)
            .env_clear()
            .env("PATH", ""),
    );
    assert!(!rerun.status.success());
    assert_eq!(fs::read(destination.join("report.json")).unwrap(), before);
}
#[test]
fn native_child_owner_loss_releases_held_initial_io() {
    let scratch = Scratch::new();
    let child = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("__storage-child")
        .arg("status_readiness_timeout")
        .arg(&scratch.0)
        .args(["validate", "paired"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut child = OwnedChild(child);
    let entered = Instant::now();
    loop {
        if fs::read(scratch.0.join("held-io.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|v| v["stage"] == "initial_write_held" && v["active"] == 1)
        {
            break;
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "Child exited before entering held I/O"
        );
        assert!(
            entered.elapsed() < Duration::from_secs(5),
            "Child did not enter held I/O"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(child.stdin.take());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(8) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Storage child outlived owner");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(!status.success());
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .take(65537)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 65536);
    let report: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(report["passed"], false);
    assert_eq!(report["cleanup"]["active_io"], 0);
    assert_eq!(report["cleanup"]["scratch_empty"], true);
    assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);
}

fn preflight_diagnostic(
    destination: &std::path::Path,
    stderr: &std::path::Path,
    child: &mut Child,
) -> String {
    let executable = std::path::Path::new(env!("CARGO_BIN_EXE_xtask"));
    let metadata = fs::metadata(executable).ok();
    #[cfg(target_os = "linux")]
    let observed_executable = fs::read_link(format!("/proc/{}/exe", child.id())).ok();
    #[cfg(not(target_os = "linux"))]
    let observed_executable: Option<PathBuf> = None;
    let observed_metadata = observed_executable
        .as_ref()
        .and_then(|path| fs::metadata(path).ok());
    let stderr_length = fs::metadata(stderr).ok().map(|value| value.len());
    let mut stderr_bytes = Vec::new();
    if let Ok(file) = fs::File::open(stderr) {
        let _ = file.take(4096).read_to_end(&mut stderr_bytes);
    }
    let identity = |value: Option<&std::fs::Metadata>| {
        value.map(|value| {
            serde_json::json!({"bytes":value.len(),"links":value.nlink(),
            "device":value.dev(),"inode":value.ino(),"regular":value.is_file()})
        })
    };
    serde_json::json!({
        "stage":"waiting_for_storage_preflight",
        "child_exit":child.try_wait().ok().flatten().map(|status| status.to_string()),
        "requested_executable":identity(metadata.as_ref()),
        "observed_linux_executable":identity(observed_metadata.as_ref()),
        "output_directory_exists":destination.is_dir(),
        "input_snapshot_exists":destination.join("input-snapshot").is_dir(),
        "preflight_exists":destination.join("preflight.json").is_file(),
        "stderr_bytes":stderr_length,
        "stderr_prefix":String::from_utf8_lossy(&stderr_bytes),
        "stderr_prefix_limit":4096
    })
    .to_string()
}
#[test]
fn driver_sigint_retains_partial_failure_and_removes_owned_scratch() {
    let scratch = Scratch::new();
    let destination = scratch.0.join("interrupted");
    let stderr = scratch.0.join("driver-stderr.log");
    let error_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&stderr)
        .unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["benchmark-storage", "--validate", "--output"])
        .arg(&destination)
        .env_clear()
        .env("PATH", "")
        .stdout(Stdio::null())
        .stderr(error_file)
        .spawn()
        .unwrap();
    let mut child = OwnedChild(child);
    let started = Instant::now();
    while !destination.join("preflight.json").exists() {
        assert!(
            fs::metadata(&stderr).unwrap().len() <= 65536,
            "Storage driver stderr bound"
        );
        assert!(
            child.try_wait().unwrap().is_none(),
            "Storage driver exited before preflight: {}",
            preflight_diagnostic(&destination, &stderr, &mut child)
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "Storage driver preflight deadline: {}",
            preflight_diagnostic(&destination, &stderr, &mut child)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .unwrap();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(12) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Interrupted storage driver did not stop");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(
        fs::metadata(&stderr).unwrap().len() <= 65536,
        "Storage final driver stderr bound"
    );
    assert!(!status.success());
    let report: Value =
        serde_json::from_slice(&fs::read(destination.join("report.json")).unwrap()).unwrap();
    assert_eq!(report["passed"], false);
    assert_eq!(report["completed"], false);
    assert!(
        report["executions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["scratch_removed"] == true)
    );
}
