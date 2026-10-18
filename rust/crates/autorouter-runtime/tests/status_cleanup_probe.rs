//! Failed public liveness probes are contained per directory. The hook proof
//! runs in an isolated process initialized before the private Tokio runtime.
use autorouter_runtime::status_cleanup::{CleanupOptions, remove_stale_status_directories};
use serde_json::json;
use std::fs::{self, Permissions};
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

#[path = "support/privacy_child.rs"]
mod privacy_child;

const PRIVATE: &str = "synthetic-private-cleanup-probe-payload-161803";
const PUBLIC: &str = "unrelated-cleanup-host-hook-public-marker";
const NOW: u64 = 1_800_000_000_000;
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let suffix = autorouter_runtime::random_hex::<12>().unwrap();
        let path = std::env::temp_dir().join(format!("autorouter-probe-{PRIVATE}-{suffix}"));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn directory(root: &Path, name: &str, pid: u64, heartbeat: u64) -> PathBuf {
    use std::io::Write;
    let path = root.join(name);
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
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
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path.join(name))
            .unwrap();
        file.write_all(bytes.as_bytes()).unwrap();
    }
    fs::File::open(&path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_millis(NOW - 30_000))
        .unwrap();
    path
}

async fn cleanup(
    root: &Root,
    calls: &Arc<Mutex<Vec<u64>>>,
    alive: impl Fn(u64) -> bool + Send + Sync + 'static,
) -> usize {
    let observed = calls.clone();
    remove_stale_status_directories(CleanupOptions {
        directory: root.0.clone(),
        now_ms: NOW as f64,
        uid: Some(fs::metadata(&root.0).unwrap().uid()),
        alive: Box::new(move |pid| {
            observed.lock().unwrap().push(pid);
            alive(pid)
        }),
    })
    .await
}

#[test]
fn failed_probe_skips_only_its_directory_and_private_panic_output_is_suppressed() {
    privacy_child::assert_private_child(
        "cleanup_probe_hook_child",
        PRIVATE,
        PUBLIC,
        1,
        "Synthetic cleanup probe child exceeded its bound",
    );
}

#[test]
#[ignore = "isolated child invoked by failed_probe_skips_only_its_directory_and_private_panic_output_is_suppressed"]
fn cleanup_probe_hook_child() {
    privacy_child::initialize_privacy();
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
        let settings = fs::read(failed.join("claude-settings.json")).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let removed = cleanup(&root, &calls, |pid| match pid {
            1 => panic!("{PRIVATE}"),
            2 => false,
            3 => true,
            _ => panic!("Unexpected synthetic PID"),
        })
        .await;
        assert_eq!(removed, 1);
        assert_eq!(*calls.lock().unwrap(), [1, 2, 3]);
        assert!(failed.exists());
        assert!(!dead.exists());
        assert!(live.exists());
        assert_eq!(
            fs::read(failed.join("claude-settings.json")).unwrap(),
            settings
        );
        // Readable enumeration with denied child search is a real lstat fault.
        calls.lock().unwrap().clear();
        if nix::unistd::geteuid().as_raw() != 0 {
            fs::set_permissions(&root.0, Permissions::from_mode(0o400)).unwrap();
            let mut entries = tokio::fs::read_dir(&root.0).await.unwrap();
            assert!(entries.next_entry().await.unwrap().is_some());
            drop(entries);
            let error = tokio::fs::symlink_metadata(&failed).await.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::PermissionDenied);
            assert_eq!(error.raw_os_error(), Some(13));
            assert_eq!(cleanup(&root, &calls, |pid| pid == 3).await, 0);
            assert!(calls.lock().unwrap().is_empty());
            fs::set_permissions(&root.0, Permissions::from_mode(0o700)).unwrap();
            assert_eq!(
                fs::read(failed.join("claude-settings.json")).unwrap(),
                settings
            );
        }
        assert_eq!(cleanup(&root, &calls, |pid| pid == 3).await, 1);
        assert_eq!(*calls.lock().unwrap(), [1, 3]);
        let name = format!("autorouter-status-x-{PRIVATE}");
        let gone = directory(&root.0, &name, 5, NOW);
        let dead = directory(&root.0, "autorouter-status-z-dead", 4, NOW);
        calls.lock().unwrap().clear();
        let vanished = gone.clone();
        let fault = Arc::new(Mutex::new(None));
        let observed = fault.clone();
        assert_eq!(
            cleanup(&root, &calls, move |pid| {
                if pid == 3 {
                    fs::remove_dir_all(&vanished).unwrap();
                    *observed.lock().unwrap() = fs::symlink_metadata(&vanished).err();
                }
                pid == 3
            })
            .await,
            1
        );
        assert_eq!(
            fault.lock().unwrap().as_ref().unwrap().kind(),
            ErrorKind::NotFound
        );
        assert_eq!(*calls.lock().unwrap(), [3, 4]);
        assert!(!dead.exists());
        directory(&root.0, &name, 5, NOW);
        calls.lock().unwrap().clear();
        assert_eq!(cleanup(&root, &calls, |pid| pid == 3).await, 1);
        assert_eq!(*calls.lock().unwrap(), [3, 5]);
        assert!(!gone.exists());
        assert_eq!(
            fs::read(live.join("claude-settings.json")).unwrap(),
            settings
        );
        assert!(std::panic::catch_unwind(|| panic!("{PUBLIC}")).is_err());
    });
}
