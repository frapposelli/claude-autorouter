//! Best-effort cleanup of abandoned private status/settings directories.
//! Ownership, permissions, symlinks, grace periods and the 200-entry bound are
//! checked before any removal; callers inject liveness for synthetic tests.
use crate::status_store::{process_alive, read_snapshot};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
const PREFIX: &str = "autorouter-status-";
const MAX_ENTRIES: usize = 200;
const GRACE_MS: f64 = 600000.0;
const STALE_MS: f64 = 86400000.0;
pub struct CleanupOptions {
    pub directory: PathBuf,
    pub now_ms: f64,
    pub uid: Option<u32>,
    pub alive: Box<dyn Fn(u32) -> bool + Send + Sync>,
}
impl Default for CleanupOptions {
    fn default() -> Self {
        Self {
            directory: std::env::temp_dir(),
            now_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as f64,
            uid: Some(nix::unistd::getuid().as_raw()),
            alive: Box::new(process_alive),
        }
    }
}
fn mtime_ms(metadata: &std::fs::Metadata) -> f64 {
    metadata.mtime() as f64 * 1000.0 + metadata.mtime_nsec() as f64 / 1_000_000.0
}
fn valid_pid(value: &serde_json::Value) -> Option<u32> {
    value
        .as_f64()
        .filter(|n| n.is_finite() && n.fract() == 0.0 && *n > 0.0 && *n <= u32::MAX as f64)
        .map(|n| n as u32)
}
pub async fn remove_stale_status_directories(options: CleanupOptions) -> usize {
    let Some(uid) = options.uid else { return 0 };
    let Ok(mut entries) = tokio::fs::read_dir(&options.directory).await else {
        return 0;
    };
    let mut names = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(PREFIX) {
            names.push(name);
        }
    }
    // Node's readdir returns names sorted by its platform directory adapter.
    names.sort();
    names.truncate(MAX_ENTRIES);
    let mut removed = 0;
    for name in names {
        let path = options.directory.join(name);
        let Ok(metadata) = tokio::fs::symlink_metadata(&path).await else {
            continue;
        };
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != uid
            || metadata.mode() & 0o077 != 0
        {
            continue;
        }
        let snapshot = read_snapshot(&path.join("state.json")).await;
        let stale = if let Some(pid) = snapshot.as_ref().and_then(|v| valid_pid(&v["pid"])) {
            let heartbeat = snapshot
                .as_ref()
                .and_then(|v| v["heartbeat_at"].as_f64())
                .filter(|n| n.is_finite())
                .unwrap_or(0.0);
            !(options.alive)(pid) || options.now_ms - heartbeat > STALE_MS
        } else {
            options.now_ms - mtime_ms(&metadata) > GRACE_MS
        };
        if !stale {
            continue;
        }
        // Recheck the directory inode and trust facts after reading a snapshot
        // so a replacement cannot broaden cleanup to another path.
        if !same_private_directory(&path, &metadata, uid).await {
            continue;
        }
        if tokio::fs::remove_dir_all(&path).await.is_ok() {
            removed += 1;
        }
    }
    removed
}
async fn same_private_directory(path: &Path, original: &std::fs::Metadata, uid: u32) -> bool {
    tokio::fs::symlink_metadata(path)
        .await
        .is_ok_and(|current| {
            current.is_dir()
                && !current.file_type().is_symlink()
                && current.uid() == uid
                && current.mode() & 0o077 == 0
                && current.dev() == original.dev()
                && current.ino() == original.ino()
        })
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::os::unix::fs::{DirBuilderExt, symlink};
    fn root() -> PathBuf {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).unwrap();
        let root = std::env::temp_dir().join(format!(
            "autorouter-cleanup-test-{:x}",
            u64::from_le_bytes(bytes)
        ));
        fs::create_dir(&root).unwrap();
        root
    }
    fn directory(
        root: &Path,
        name: &str,
        snapshot: Option<serde_json::Value>,
        mode: u32,
    ) -> PathBuf {
        let path = root.join(name);
        fs::DirBuilder::new().mode(mode).create(&path).unwrap();
        fs::write(
            path.join("claude-settings.json"),
            r#"{"env":{"PRIVATE":"synthetic"}}"#,
        )
        .unwrap();
        if let Some(snapshot) = snapshot {
            fs::write(path.join("state.json"), snapshot.to_string()).unwrap();
        }
        path
    }
    fn options(root: &Path, now: f64) -> CleanupOptions {
        CleanupOptions {
            directory: root.to_owned(),
            now_ms: now,
            uid: Some(fs::metadata(root).unwrap().uid()),
            alive: Box::new(|pid| pid == 222222),
        }
    }
    #[tokio::test]
    async fn malformed_oversized_and_missing_pid_snapshots_observe_the_same_grace_period() {
        let root = root();
        let now = 1_800_000_000_000u64;
        let starting = directory(&root, "autorouter-status-starting", None, 0o700);
        fs::File::open(&starting)
            .unwrap()
            .set_modified(UNIX_EPOCH + std::time::Duration::from_millis(now - 30_000))
            .unwrap();
        let mut abandoned = Vec::new();
        for (name, content) in [
            ("bad-json", "not json".to_owned()),
            ("oversized", " ".repeat(1024 * 1024 + 1)),
            ("no-pid", r#"{"pid":"abc"}"#.to_owned()),
        ] {
            let path = directory(&root, &format!("autorouter-status-{name}"), None, 0o700);
            fs::write(path.join("state.json"), content).unwrap();
            fs::File::open(&path)
                .unwrap()
                .set_modified(UNIX_EPOCH + std::time::Duration::from_millis(now - 660_000))
                .unwrap();
            abandoned.push(path);
        }
        assert_eq!(
            remove_stale_status_directories(options(&root, now as f64)).await,
            3
        );
        assert!(starting.exists());
        for path in abandoned {
            assert!(!path.exists());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn removes_dead_and_reused_pids_but_keeps_live_and_untrusted_paths() {
        let root = root();
        let now = 1_800_000_000_000.0;
        let dead = directory(
            &root,
            "autorouter-status-dead",
            Some(json!({"pid":111111,"heartbeat_at":now})),
            0o700,
        );
        let live = directory(
            &root,
            "autorouter-status-live",
            Some(json!({"pid":222222,"heartbeat_at":now-3600000.0})),
            0o700,
        );
        let reused = directory(
            &root,
            "autorouter-status-reused",
            Some(json!({"pid":222222,"heartbeat_at":now-90000000.0})),
            0o700,
        );
        let loose = directory(
            &root,
            "autorouter-status-loose",
            Some(json!({"pid":111111})),
            0o755,
        );
        let target = directory(&root, "unrelated", Some(json!({"pid":111111})), 0o700);
        symlink(&target, root.join("autorouter-status-link")).unwrap();
        let regular = root.join("autorouter-status-file");
        fs::write(&regular, "synthetic-keep").unwrap();
        assert_eq!(
            remove_stale_status_directories(options(&root, now)).await,
            2
        );
        assert!(!dead.exists());
        assert!(!reused.exists());
        for kept in [&live, &loose, &target] {
            assert!(kept.exists());
        }
        assert!(target.join("claude-settings.json").exists());
        assert_eq!(fs::read_to_string(regular).unwrap(), "synthetic-keep");
        assert!(
            root.join("autorouter-status-link")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn unreadable_snapshot_grace_and_foreign_owner_are_conservative() {
        let root = root();
        let path = directory(&root, "autorouter-status-starting", None, 0o700);
        let now = mtime_ms(&fs::metadata(&path).unwrap());
        assert_eq!(
            remove_stale_status_directories(options(&root, now + 30000.0)).await,
            0
        );
        let mut foreign = options(&root, now + 700000.0);
        foreign.uid = foreign.uid.map(|uid| uid + 1);
        assert_eq!(remove_stale_status_directories(foreign).await, 0);
        assert_eq!(
            remove_stale_status_directories(options(&root, now + 700000.0)).await,
            1
        );
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn cleanup_is_bounded_and_missing_directory_is_ignored() {
        let root = root();
        for i in 0..205 {
            directory(
                &root,
                &format!("autorouter-status-{i:03}"),
                Some(json!({"pid":111111})),
                0o700,
            );
        }
        assert_eq!(
            remove_stale_status_directories(options(&root, 1e15)).await,
            200
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 5);
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            remove_stale_status_directories(CleanupOptions {
                directory: root,
                ..CleanupOptions::default()
            })
            .await,
            0
        );
    }
}
