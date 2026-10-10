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
        assert_eq!(
            default_policy_path(),
            Path::new(if cfg!(target_os = "macos") {
                "/Library/Application Support/claude-autorouter/policy.json"
            } else {
                "/etc/claude-autorouter/policy.json"
            })
        );
        assert!(load_policy_at(&path, uid).unwrap().is_none());
        fs::write(&path, br#"{"allowed_evaluators":["ollama","ollama"],"session_log_mode":"metadata","upstream_url":"https://api.anthropic.com"}"#).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            load_policy_at(&path, uid).unwrap().unwrap().values,
            serde_json::json!({"allowed_evaluators":["ollama"],"session_log_mode":"metadata","upstream_url":"https://api.anthropic.com"})
        );
        for body in [
            "PRIVATE_POLICY_TEXT",
            "[]",
            r#"{"PRIVATE_KEY_NAME":1}"#,
            r#"{"allowed_evaluators":[]}"#,
            r#"{"allowed_evaluators":["PRIVATE_VALUE"]}"#,
            r#"{"session_log_mode":"PRIVATE_VALUE"}"#,
            r#"{"upstream_url":"PRIVATE_VALUE"}"#,
        ] {
            fs::write(&path, body).unwrap();
            let error = load_policy_at(&path, uid)
                .err()
                .expect("invalid policy rejected");
            assert!(!error.contains("PRIVATE_"));
        }
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
        assert!(
            load_policy_at(&path, uid + 1)
                .err()
                .unwrap()
                .contains("owned by root")
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();
        assert!(
            !load_policy_at(&path, uid)
                .err()
                .unwrap()
                .contains("PRIVATE_")
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            !load_policy_at(&path, uid)
                .err()
                .unwrap()
                .contains("PRIVATE_")
        );
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_file(&path).unwrap();
        symlink(root.join("missing"), &path).unwrap();
        assert!(load_policy_at(&path, uid).is_err());
        fs::remove_file(&path).unwrap();
        let target = root.join("target.json");
        fs::write(&target, "{}").unwrap();
        symlink(&target, &path).unwrap();
        assert!(
            load_policy_at(&path, uid)
                .err()
                .unwrap()
                .contains("regular file")
        );
        assert_eq!(fs::read(&target).unwrap(), b"{}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn allowlists_cover_both_defaults_and_explicit_choices_while_locks_preserve_the_caller() {
        use autorouter_core::policy::apply_policy;
        use serde_json::json;
        let policy = json!({"allowed_evaluators":["ollama"],"allowed_auth_modes":["subscription"],"session_log_mode":"metadata","upstream_url":"https://api.anthropic.com"});
        assert!(
            apply_policy(
                &json!({"AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_AUTH_MODE":"subscription"}),
                &policy,
                true
            )
            .unwrap_err()
            .contains("AUTOROUTER_EVALUATOR is not permitted")
        );
        assert!(
            apply_policy(&json!({}), &policy, true)
                .unwrap_err()
                .contains("AUTOROUTER_AUTH_MODE is not permitted")
        );
        assert!(
            apply_policy(
                &json!({"AUTOROUTER_AUTH_MODE":"subscription"}),
                &json!({"allowed_evaluators":["jev"]}),
                true
            )
            .unwrap_err()
            .contains("AUTOROUTER_EVALUATOR")
        );
        let env = json!({"AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_SESSION_LOG_MODE":"prompts","AUTOROUTER_UPSTREAM_URL":"https://evil.example","KEEP":"1"});
        let before = env.clone();
        assert_eq!(
            apply_policy(&env, &policy, true).unwrap(),
            json!({"env":{"AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_SESSION_LOG_MODE":"metadata","AUTOROUTER_UPSTREAM_URL":"https://api.anthropic.com","KEEP":"1"},"locked":["AUTOROUTER_SESSION_LOG_MODE","AUTOROUTER_UPSTREAM_URL"]})
        );
        assert_eq!(env, before);
        assert!(apply_policy(&json!({"AUTOROUTER_EVALUATOR":"jev"}), &policy, false).is_ok());
    }
}
