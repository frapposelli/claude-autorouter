use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path};
use std::process::Command;
use std::time::Duration;

const MAX_FILE: u64 = 64 * 1024 * 1024;

fn manifest(root: &Path) -> Result<Value, String> {
    serde_json::from_slice(&crate::process::read_bounded(
        &root.join("rust/parity/baseline.json"),
        MAX_FILE,
    )?)
    .map_err(|_| "Invalid reference manifest".into())
}

fn path(entry: &Value) -> Result<&Path, String> {
    let path = Path::new(entry["path"].as_str().ok_or("Invalid reference path")?);
    if path.as_os_str().is_empty()
        || !path
            .components()
            .all(|item| matches!(item, Component::Normal(_)))
    {
        return Err("Unsafe reference manifest path".into());
    }
    Ok(path)
}

fn check_bytes(entry: &Value, bytes: &[u8]) -> Result<(), String> {
    if entry["bytes"].as_u64() != Some(bytes.len() as u64)
        || entry["sha256"].as_str() != Some(format!("{:x}", Sha256::digest(bytes)).as_str())
    {
        return Err(format!(
            "Frozen baseline mismatch: {}",
            path(entry)?.display()
        ));
    }
    Ok(())
}

pub fn verify(root: &Path, directory: &Path) -> Result<usize, String> {
    let info = fs::symlink_metadata(directory).map_err(|_| "Reference checkout is incomplete")?;
    if !info.is_dir() || info.file_type().is_symlink() {
        return Err("Reference checkout must be a real directory".into());
    }
    let manifest = manifest(root)?;
    let entries = manifest["files"]
        .as_array()
        .ok_or("Reference manifest has no files")?;
    for entry in entries {
        let relative = path(entry)?;
        let mut current = directory.to_path_buf();
        for part in relative.components() {
            current.push(part);
            let info =
                fs::symlink_metadata(&current).map_err(|_| "Reference checkout is incomplete")?;
            if info.file_type().is_symlink() {
                return Err("Reference checkout must not contain symlinks".into());
            }
        }
        check_bytes(entry, &crate::process::read_bounded(&current, MAX_FILE)?)?;
    }
    Ok(entries.len())
}

/// Build an immutable reference from Git objects without resetting the worktree.
/// Existing destinations are only verified and never overwritten or repaired.
pub fn freeze(root: &Path, destination: &Path) -> Result<usize, String> {
    freeze_with(root, destination, |_| Ok(()))
}

fn freeze_with(
    root: &Path,
    destination: &Path,
    before_publish: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<usize, String> {
    match fs::symlink_metadata(destination) {
        Ok(_) => return verify(root, destination),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("Cannot inspect reference destination".into()),
    }
    let manifest = manifest(root)?;
    let commit = manifest["baseline_commit"]
        .as_str()
        .ok_or("Reference commit is missing")?;
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Reference commit must be an immutable full Git hash".into());
    }
    let entries = manifest["files"]
        .as_array()
        .ok_or("Reference manifest has no files")?;
    let parent = destination
        .parent()
        .ok_or("Reference destination has no parent")?;
    fs::create_dir_all(parent).map_err(|_| "Cannot create reference parent")?;
    let nonce = crate::audit_support::random_hex::<16>()
        .map_err(|_| "Cannot create reference staging name")?;
    let staging = parent.join(format!(
        ".autorouter-reference-{}-{nonce}",
        std::process::id()
    ));
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&staging)
        .map_err(|_| "Cannot create reference staging directory")?;
    let result = (|| {
        for entry in entries {
            let relative = path(entry)?;
            let object = format!("{commit}:{}", relative.display());
            let mut command = Command::new("git");
            command
                .args([
                    "--no-pager",
                    "show",
                    "--no-ext-diff",
                    "--no-textconv",
                    &object,
                ])
                .current_dir(root);
            let bytes = crate::process::capture(&mut command, &[], Duration::from_secs(10))
                .map_err(
                    |_| "Cannot read frozen Git object; ensure the baseline commit was fetched",
                )?;
            check_bytes(entry, &bytes)?;
            let target = staging.join(relative);
            fs::create_dir_all(target.parent().ok_or("Invalid reference path")?)
                .map_err(|_| "Cannot create reference directory")?;
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&target)
                .and_then(|mut file| file.write_all(&bytes))
                .map_err(|_| "Cannot write reference file")?;
        }
        verify(root, &staging)?;
        before_publish(&staging)?;
        publish(root, &staging, destination)
    })();
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

// NOREPLACE is required: plain rename can replace a raced empty directory.
// A losing builder may reuse only a complete verified winner, never repair it.
fn publish(root: &Path, staging: &Path, destination: &Path) -> Result<usize, String> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    match renameat_with(CWD, staging, CWD, destination, RenameFlags::NOREPLACE) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => verify(root, destination),
        Err(_) => Err("Cannot install reference checkout".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn source_drift_fails_even_when_size_is_unchanged() {
        let entry = json!({"path":"src/test.mjs","bytes":4,"sha256":format!("{:x}",Sha256::digest(b"true"))});
        assert!(check_bytes(&entry, b"true").is_ok());
        assert!(check_bytes(&entry, b"null").is_err());
    }

    #[test]
    fn manifest_paths_cannot_escape_the_owned_snapshot() {
        for value in ["../outside", "/absolute", "src/../../outside", ""] {
            assert!(path(&json!({"path":value})).is_err());
        }
    }

    struct Fixture(std::path::PathBuf);
    const FILES: [(&str, &[u8]); 3] = [
        ("src/value.mjs", b"true"),
        ("test/value.test.mjs", b"synthetic reference test\n"),
        ("LICENSE", b"Synthetic reference license\n"),
    ];
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    impl Fixture {
        fn new(git: bool) -> Self {
            use std::os::unix::fs::DirBuilderExt;
            let suffix = crate::audit_support::random_hex::<12>().unwrap();
            let root =
                Self(std::env::temp_dir().join(format!("autorouter-reference-race-{suffix}")));
            fs::DirBuilder::new().mode(0o700).create(&root.0).unwrap();
            root.files(&root.0);
            let commit = if git {
                root.git(&["init", "--quiet", "--template="]);
                root.git(&["add", "--", "src", "test", "LICENSE"]);
                root.git(&[
                    "-c",
                    "user.name=Synthetic Reference",
                    "-c",
                    "user.email=reference@example.invalid",
                    "-c",
                    "commit.gpgSign=false",
                    "-c",
                    "core.hooksPath=/dev/null",
                    "commit",
                    "--quiet",
                    "--no-verify",
                    "-m",
                    "Synthetic frozen reference",
                ]);
                String::from_utf8(root.git(&["rev-parse", "HEAD"]))
                    .unwrap()
                    .trim()
                    .to_owned()
            } else {
                // A verified existing checkout must work with no Git repository.
                "0".repeat(40)
            };
            fs::create_dir_all(root.0.join("rust/parity")).unwrap();
            fs::write(root.0.join("rust/parity/baseline.json"), serde_json::to_vec(&json!({
                "baseline_commit":commit,
                "files":FILES.iter().map(|(path, bytes)| json!({"path":path,
                    "bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))})).collect::<Vec<_>>()
            })).unwrap()).unwrap();
            root
        }
        fn git(&self, args: &[&str]) -> Vec<u8> {
            let mut command = Command::new("git");
            command
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("LC_ALL", "C")
                .current_dir(&self.0)
                .args(args);
            crate::process::capture(&mut command, &[], Duration::from_secs(10)).unwrap()
        }
        fn files(&self, root: &Path) {
            for (relative, bytes) in FILES {
                let path = root.join(relative);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, bytes).unwrap();
            }
        }
        fn seed(&self, destination: &Path, kind: &str) -> Option<std::path::PathBuf> {
            use std::os::unix::fs::{DirBuilderExt, symlink};
            match kind {
                "valid" | "empty" | "corrupt" => {
                    fs::DirBuilder::new()
                        .mode(0o700)
                        .create(destination)
                        .unwrap();
                    if kind != "empty" {
                        self.files(destination);
                    }
                    if kind == "corrupt" {
                        fs::write(destination.join(FILES[0].0), b"null").unwrap();
                    }
                }
                "file" => {
                    fs::write(destination, b"foreign destination must remain untouched").unwrap()
                }
                "symlink" => {
                    let target = destination.with_extension("target");
                    fs::create_dir(&target).unwrap();
                    self.files(&target);
                    symlink(&target, destination).unwrap();
                    return Some(target);
                }
                "dangling" => symlink(destination.with_extension("absent"), destination).unwrap(),
                _ => unreachable!(),
            }
            None
        }
        fn no_staging(&self) {
            assert!(fs::read_dir(&self.0).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".autorouter-reference-")
            }));
        }
    }
    // Record the entry itself and every descendant without following links.
    type EntrySnapshot = (std::path::PathBuf, u64, u64, u32, Vec<u8>);
    fn snapshot(root: &Path) -> Vec<EntrySnapshot> {
        fn visit(path: &Path, root: &Path, entries: &mut Vec<EntrySnapshot>) {
            use std::os::unix::fs::MetadataExt;
            let info = fs::symlink_metadata(path).unwrap();
            let bytes = if info.file_type().is_symlink() {
                fs::read_link(path)
                    .unwrap()
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec()
            } else if info.is_file() {
                fs::read(path).unwrap()
            } else {
                Vec::new()
            };
            entries.push((
                path.strip_prefix(root).unwrap().to_owned(),
                info.dev(),
                info.ino(),
                info.mode(),
                bytes,
            ));
            if info.is_dir() {
                for entry in fs::read_dir(path).unwrap() {
                    visit(&entry.unwrap().path(), root, entries);
                }
            }
        }
        let mut entries = Vec::new();
        visit(root, root, &mut entries);
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }

    #[test]
    fn existing_reference_roots_are_verified_without_git_and_never_modified() {
        let fixture = Fixture::new(false);
        assert!(!fixture.0.join(".git").exists());
        for kind in ["valid", "empty", "corrupt", "file", "symlink", "dangling"] {
            let destination = fixture.0.join(format!("existing-{kind}"));
            let target = fixture.seed(&destination, kind);
            let before = snapshot(&destination);
            let target_before = target.as_ref().map(|path| snapshot(path));
            let result = freeze(&fixture.0, &destination);
            assert_eq!(result.is_ok(), kind == "valid", "{kind}");
            if kind == "valid" {
                assert_eq!(result.unwrap(), FILES.len());
            }
            assert_eq!(snapshot(&destination), before, "{kind}");
            assert_eq!(target.as_ref().map(|path| snapshot(path)), target_before);
            fixture.no_staging();
        }
    }

    #[test]
    fn publication_time_winners_are_verified_without_replacement_or_repair() {
        let fixture = Fixture::new(true);
        for kind in ["valid", "empty", "corrupt", "file", "symlink", "dangling"] {
            let destination = fixture.0.join(format!("raced-{kind}"));
            let mut witness = None;
            let result = freeze_with(&fixture.0, &destination, |staging| {
                assert_eq!(verify(&fixture.0, staging).unwrap(), FILES.len());
                assert!(fs::symlink_metadata(&destination).is_err());
                let target = fixture.seed(&destination, kind);
                witness = Some((
                    snapshot(&destination),
                    target.as_ref().map(|path| snapshot(path)),
                    target,
                ));
                Ok(())
            });
            assert_eq!(result.is_ok(), kind == "valid", "{kind}");
            if kind == "valid" {
                assert_eq!(result.unwrap(), FILES.len());
            }
            let (before, target_before, target) = witness.unwrap();
            assert_eq!(snapshot(&destination), before, "{kind}");
            assert_eq!(target.as_ref().map(|path| snapshot(path)), target_before);
            fixture.no_staging();
        }
    }

    #[test]
    fn concurrent_cold_freezes_publish_one_complete_private_reference() {
        use std::os::unix::fs::MetadataExt;
        use std::sync::mpsc;
        use std::time::Instant;
        let fixture = Fixture::new(true);
        let destination = fixture.0.join("concurrent");
        let deadline = Instant::now() + Duration::from_secs(60);
        let (ready_send, ready_receive) = mpsc::channel();
        std::thread::scope(|scope| {
            let mut workers = Vec::new();
            let mut releases = Vec::new();
            for _ in 0..4 {
                let root = &fixture.0;
                let destination = &destination;
                let ready = ready_send.clone();
                let (release, receive) = mpsc::channel();
                releases.push(release);
                workers.push(scope.spawn(move || {
                    freeze_with(root, destination, |staging| {
                        let info = fs::symlink_metadata(staging).unwrap();
                        ready
                            .send((info.dev(), info.ino(), info.mode()))
                            .map_err(|_| "lost test witness")?;
                        receive
                            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                            .map_err(|_| "bounded publication gate expired".into())
                    })
                }));
            }
            let mut staged = Vec::new();
            for _ in 0..4 {
                staged.push(
                    ready_receive
                        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        .unwrap(),
                );
            }
            assert!(fs::symlink_metadata(&destination).is_err());
            for (_, _, mode) in &staged {
                assert_eq!(mode & 0o777, 0o700);
            }
            for release in releases {
                release.send(()).unwrap();
            }
            for worker in workers {
                assert_eq!(worker.join().unwrap().unwrap(), FILES.len());
            }
            let winner = fs::symlink_metadata(&destination).unwrap();
            assert!(staged.contains(&(winner.dev(), winner.ino(), winner.mode())));
        });
        assert_eq!(verify(&fixture.0, &destination).unwrap(), FILES.len());
        for (relative, expected) in FILES {
            assert_eq!(fs::read(destination.join(relative)).unwrap(), expected);
        }
        fixture.no_staging();
    }
}
