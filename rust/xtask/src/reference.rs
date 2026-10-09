use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    if destination.exists() {
        return verify(root, destination);
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
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "Invalid system clock")?
        .as_nanos();
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
        // The destination remains untouched if another builder has installed it.
        if destination.exists() {
            return Err("Reference destination appeared during construction".into());
        }
        fs::rename(&staging, destination).map_err(|_| "Cannot install reference checkout")?;
        Ok(entries.len())
    })();
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
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
}
