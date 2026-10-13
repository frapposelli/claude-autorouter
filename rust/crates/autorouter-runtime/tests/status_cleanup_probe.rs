//! Failed public liveness probes are contained per directory. The hook proof
//! runs in an isolated process initialized before the private Tokio runtime.
use autorouter_runtime::callback_privacy::initialize_observer_privacy;
use autorouter_runtime::status_cleanup::{CleanupOptions, remove_stale_status_directories};
use serde_json::json;
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

const PRIVATE: &str = "synthetic-private-cleanup-probe-payload-161803";
const PUBLIC: &str = "unrelated-cleanup-host-hook-public-marker";
const NOW: u64 = 1_800_000_000_000;
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let mut random = [0; 12];
        getrandom::fill(&mut random).unwrap();
        let path = std::env::temp_dir().join(format!(
            "autorouter-probe-contract-{}",
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn directory(root: &Path, name: &str, pid: u64, heartbeat: u64) -> PathBuf {
    use std::io::Write;
    let path = root.join(name);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .unwrap();
    for (name, bytes) in [
        (
            "state.json",
            json!({"pid":pid,"heartbeat_at":heartbeat}).to_string(),
        ),
        (
            "claude-settings.json",
            json!({"env":{"SYNTHETIC_PRIVATE_SETTING":PRIVATE}}).to_string(),
        ),
    ] {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path.join(name))
            .unwrap();
        file.write_all(bytes.as_bytes()).unwrap();
    }
    std::fs::File::open(&path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_millis(NOW - 30_000))
        .unwrap();
    path
}

#[test]
fn failed_probe_skips_only_its_directory_and_private_panic_output_is_suppressed() {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "cleanup_probe_hook_child",
            "--ignored",
            "--nocapture",
        ])
        .env("RUST_BACKTRACE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let read = |stream: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stream.take(65_537).read_to_end(&mut bytes).unwrap();
            bytes
        })
    };
    let stdout = read(Box::new(child.stdout.take().unwrap()));
    let stderr = read(Box::new(child.stderr.take().unwrap()));
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Synthetic cleanup probe child exceeded its bound");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = stdout.join().unwrap();
    let stderr = stderr.join().unwrap();
    assert!(status.success());
    assert!(stdout.len() <= 65_536 && stderr.len() <= 65_536);
    let stdout = String::from_utf8(stdout).unwrap();
    let stderr = String::from_utf8(stderr).unwrap();
    assert!(!stdout.contains(PRIVATE));
    assert!(!stderr.contains(PRIVATE));
    assert_eq!(stderr.matches(PUBLIC).count(), 1);
}

#[test]
#[ignore = "isolated child invoked by failed_probe_skips_only_its_directory_and_private_panic_output_is_suppressed"]
fn cleanup_probe_hook_child() {
    std::panic::set_hook(Box::new(|info| {
        if let Some(payload) = info.payload().downcast_ref::<&str>() {
            eprintln!("{payload}");
        } else if let Some(payload) = info.payload().downcast_ref::<String>() {
            eprintln!("{payload}");
        }
    }));
    initialize_observer_privacy();
    // The initializer precedes this worker runtime; no process hook is replaced.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = Root::new();
        let failed = directory(&root.0, "autorouter-status-a-failed", 1, NOW - 86_400_001);
        let dead = directory(&root.0, "autorouter-status-b-dead", 2, NOW);
        let live = directory(&root.0, "autorouter-status-c-live", 3, NOW);
        let settings = std::fs::read(failed.join("claude-settings.json")).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let observed = calls.clone();
        let removed = remove_stale_status_directories(CleanupOptions {
            directory: root.0.clone(),
            now_ms: NOW as f64,
            uid: Some(std::fs::metadata(&root.0).unwrap().uid()),
            alive: Box::new(move |pid| {
                observed.lock().unwrap().push(pid);
                match pid {
                    1 => panic!("{PRIVATE}"),
                    2 => false,
                    3 => true,
                    _ => panic!("Unexpected synthetic PID"),
                }
            }),
        })
        .await;
        assert_eq!(removed, 1);
        assert_eq!(*calls.lock().unwrap(), [1, 2, 3]);
        assert!(failed.exists());
        assert!(!dead.exists());
        assert!(live.exists());
        assert_eq!(
            std::fs::read(failed.join("claude-settings.json")).unwrap(),
            settings
        );
        assert!(std::panic::catch_unwind(|| panic!("{PUBLIC}")).is_err());
    });
}
