//! Fixed-path, fail-closed organization policy loading.
use autorouter_core::policy::validate_policy;
use serde_json::Value;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub struct LoadedPolicy {
    pub path: PathBuf,
    pub values: Value,
}

pub fn default_policy_path() -> &'static Path {
    if cfg!(target_os = "macos") {
        Path::new("/Library/Application Support/claude-autorouter/policy.json")
    } else {
        Path::new("/etc/claude-autorouter/policy.json")
    }
}

pub fn load_policy() -> Result<Option<LoadedPolicy>, String> {
    load_policy_at(default_policy_path(), 0)
}

// This injected adapter boundary is for isolated tests and embedders; product
// commands use load_policy(), with no environment override for its location.
pub fn load_policy_at(path: &Path, trusted_uid: u32) -> Result<Option<LoadedPolicy>, String> {
    match fs::symlink_metadata(path) {
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(None);
        }
        Err(error) => {
            let code = if error.kind() == ErrorKind::PermissionDenied {
                "EACCES"
            } else {
                "error"
            };
            return Err(format!(
                "Could not read the AutoRouter organization policy ({code}). Refusing to start."
            ));
        }
        Ok(_) => {}
    }
    for (target, kind) in [
        (path, "file"),
        (path.parent().unwrap_or_else(|| Path::new(".")), "directory"),
    ] {
        let metadata = fs::symlink_metadata(target)
            .map_err(|_| "Could not read the AutoRouter organization policy. Refusing to start.")?;
        let correct_kind = if kind == "file" {
            metadata.is_file()
        } else {
            metadata.is_dir()
        };
        if metadata.file_type().is_symlink()
            || !correct_kind
            || metadata.uid() != trusted_uid
            || metadata.mode() & 0o022 != 0
        {
            return Err(format!(
                "The AutoRouter organization policy {kind} must be a regular {kind} owned by root and not writable by group or others. Refusing to start."
            ));
        }
    }
    let bytes = fs::read(path)
        .map_err(|_| "Could not read the AutoRouter organization policy. Refusing to start.")?;
    // Policy enums and keys are ASCII allowlists. URL values undergo the URL
    // parser's scalar-string conversion later; opaque invalid values must not
    // make this reader stricter than JSON.parse's accepted syntax.
    let parsed = autorouter_core::js_json::JsDocument::parse(&bytes)
        .map_err(|_| "The AutoRouter organization policy must contain valid JSON.")?
        .to_serde_observation_lossy();
    Ok(Some(LoadedPolicy {
        path: path.to_owned(),
        values: validate_policy(&parsed)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn only_trusted_regular_files_and_directories_are_loaded() {
        let root = std::env::temp_dir().join(format!(
            "autorouter-rust-policy-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("policy.json");
        let uid = fs::metadata(&root).unwrap().uid();
        assert!(load_policy_at(&path, uid).unwrap().is_none());
        fs::write(&path, b"{\"allowed_evaluators\":[\"ollama\"]}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            load_policy_at(&path, uid).unwrap().unwrap().values["allowed_evaluators"],
            serde_json::json!(["ollama"])
        );
        // JSON.parse accepts lone surrogates and nonfinite numeric overflow.
        // These inputs reach the policy schema check instead of being reported
        // as malformed JSON; error text must not expose their values.
        for body in [
            r#"{"session_log_mode":"private-\ud800"}"#.to_owned(),
            r#"{"session_log_mode":1e400}"#.to_owned(),
            format!(
                "{{\"session_log_mode\":{}0{}}}",
                "[".repeat(500),
                "]".repeat(500)
            ),
        ] {
            fs::write(&path, body).unwrap();
            let error = load_policy_at(&path, uid).err().unwrap();
            assert_eq!(error, "Policy session_log_mode has an invalid value.");
        }
        fs::write(&path, b"{\"allowed_evaluators\":[\"ollama\"]}").unwrap();
        assert!(load_policy_at(&path, uid + 1).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();
        assert!(load_policy_at(&path, uid).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(load_policy_at(&path, uid).is_err());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_file(&path).unwrap();
        symlink(root.join("missing"), &path).unwrap();
        assert!(load_policy_at(&path, uid).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
