//! Fixed-path, fail-closed organization policy loading.
use autorouter_core::policy::validate_policy;
use serde_json::Value;
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// A fail-closed policy error with the same stable category as the source API.
/// Messages are constructed only by this loader and its allowlisted validator;
/// underlying I/O errors, paths and policy contents are never retained.
#[derive(Clone, PartialEq, Eq)]
pub struct PolicyError {
    message: String,
}
impl PolicyError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
    pub const fn code(&self) -> &'static str {
        "AUTOROUTER_CONFIG_ERROR"
    }
    pub fn message(&self) -> &str {
        &self.message
    }
}
impl fmt::Display for PolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}
impl fmt::Debug for PolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PolicyError")
            .field("code", &self.code())
            .field("message", &self.message())
            .finish()
    }
}
impl std::error::Error for PolicyError {}

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

pub fn load_policy() -> Result<Option<LoadedPolicy>, PolicyError> {
    load_policy_at(default_policy_path(), 0)
}

// This injected adapter boundary is for isolated tests and embedders; product
// commands use load_policy(), with no environment override for its location.
pub fn load_policy_at(path: &Path, trusted_uid: u32) -> Result<Option<LoadedPolicy>, PolicyError> {
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
            return Err(PolicyError::new(format!(
                "Could not read the AutoRouter organization policy ({code}). Refusing to start."
            )));
        }
        Ok(_) => {}
    }
    for (target, kind) in [
        (path, "file"),
        (path.parent().unwrap_or_else(|| Path::new(".")), "directory"),
    ] {
        let metadata = fs::symlink_metadata(target).map_err(|_| {
            PolicyError::new(
                "Could not read the AutoRouter organization policy. Refusing to start.",
            )
        })?;
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
            return Err(PolicyError::new(format!(
                "The AutoRouter organization policy {kind} must be a regular {kind} owned by root and not writable by group or others. Refusing to start."
            )));
        }
    }
    let bytes = fs::read(path).map_err(|_| {
        PolicyError::new("Could not read the AutoRouter organization policy. Refusing to start.")
    })?;
    // Policy enums and keys are ASCII allowlists. URL values undergo the URL
    // parser's scalar-string conversion later; opaque invalid values must not
    // make this reader stricter than JSON.parse's accepted syntax.
    let parsed = autorouter_core::js_json::JsDocument::parse(&bytes)
        .map_err(|_| {
            PolicyError::new("The AutoRouter organization policy must contain valid JSON.")
        })?
        .to_serde_observation_lossy();
    Ok(Some(LoadedPolicy {
        path: path.to_owned(),
        values: validate_policy(&parsed).map_err(PolicyError::new)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn original_policy_rejections_preserve_stable_error_category_and_input_files() {
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        fn rejected(path: &Path, uid: u32, expected: &str) {
            let error = load_policy_at(path, uid)
                .err()
                .expect("policy fails closed");
            assert_eq!(error.code(), "AUTOROUTER_CONFIG_ERROR");
            assert_eq!(error.message(), expected);
            assert_eq!(error.to_string(), expected);
            assert!(std::error::Error::source(&error).is_none());
            let debug = format!("{error:?}");
            assert!(debug.contains("AUTOROUTER_CONFIG_ERROR"));
            for rendered in [error.message(), &error.to_string(), &debug] {
                assert!(!rendered.contains("PRIVATE_"));
                assert!(!rendered.contains(&path.to_string_lossy().to_string()));
            }
        }
        let root = Fixture(std::env::temp_dir().join(format!(
            "autorouter-policy-code-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
        fs::create_dir(&root.0).unwrap();
        fs::set_permissions(&root.0, fs::Permissions::from_mode(0o700)).unwrap();
        let uid = fs::metadata(&root.0).unwrap().uid();
        let trusted_file = "The AutoRouter organization policy file must be a regular file owned by root and not writable by group or others. Refusing to start.";
        let trusted_directory = "The AutoRouter organization policy directory must be a regular directory owned by root and not writable by group or others. Refusing to start.";
        // The nine original bad-policy fixtures, in the original order. These
        // exercise the actual filesystem loader, not just its JSON validator.
        let cases = [
            ("group-writable-file", "{}", 0o664, 0o755, trusted_file),
            (
                "world-writable-directory",
                "{}",
                0o644,
                0o777,
                trusted_directory,
            ),
            (
                "malformed-json",
                "PRIVATE_POLICY_TEXT",
                0o644,
                0o755,
                "The AutoRouter organization policy must contain valid JSON.",
            ),
            (
                "not-object",
                "[]",
                0o644,
                0o755,
                "The AutoRouter organization policy must be a JSON object.",
            ),
            (
                "unknown-key",
                r#"{"PRIVATE_KEY_NAME":1}"#,
                0o644,
                0o755,
                "The AutoRouter organization policy contains an unsupported key.",
            ),
            (
                "empty-allowlist",
                r#"{"allowed_evaluators":[]}"#,
                0o644,
                0o755,
                "Policy allowed_evaluators must be a nonempty list of: jev, ollama.",
            ),
            (
                "unknown-allowlist-value",
                r#"{"allowed_evaluators":["PRIVATE_VALUE"]}"#,
                0o644,
                0o755,
                "Policy allowed_evaluators must be a nonempty list of: jev, ollama.",
            ),
            (
                "bad-log-mode",
                r#"{"session_log_mode":"PRIVATE_VALUE"}"#,
                0o644,
                0o755,
                "Policy session_log_mode has an invalid value.",
            ),
            (
                "bad-url",
                r#"{"upstream_url":"PRIVATE_VALUE"}"#,
                0o644,
                0o755,
                "Policy upstream_url has an invalid value.",
            ),
        ];
        for (index, (label, body, file_mode, directory_mode, expected)) in cases.iter().enumerate()
        {
            let directory = root.0.join(format!("PRIVATE_{index}"));
            fs::create_dir(&directory).unwrap();
            fs::set_permissions(&directory, fs::Permissions::from_mode(*directory_mode)).unwrap();
            let path = directory.join("policy.json");
            fs::write(&path, body).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(*file_mode)).unwrap();
            rejected(&path, uid, expected);
            assert_eq!(fs::read(&path).unwrap(), body.as_bytes());
            assert_eq!(
                fs::symlink_metadata(&path).unwrap().mode() & 0o777,
                *file_mode
            );
            assert_eq!(
                fs::symlink_metadata(&directory).unwrap().mode() & 0o777,
                *directory_mode
            );
            assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
            eprintln!(
                "original policy rejection: {label}; code=AUTOROUTER_CONFIG_ERROR; unchanged=true"
            );
        }
        let owner_directory = root.0.join("PRIVATE_owner");
        fs::create_dir(&owner_directory).unwrap();
        fs::set_permissions(&owner_directory, fs::Permissions::from_mode(0o755)).unwrap();
        let path = owner_directory.join("policy.json");
        fs::write(&path, b"{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        rejected(&path, uid.checked_add(1).unwrap(), trusted_file);
        assert_eq!(fs::read(&path).unwrap(), b"{}");
        assert_eq!(fs::symlink_metadata(&path).unwrap().uid(), uid);
        eprintln!(
            "original policy rejection: wrong-owner; code=AUTOROUTER_CONFIG_ERROR; unchanged=true"
        );
        let link_directory = root.0.join("PRIVATE_link");
        fs::create_dir(&link_directory).unwrap();
        fs::set_permissions(&link_directory, fs::Permissions::from_mode(0o755)).unwrap();
        let target = root.0.join("PRIVATE_target.json");
        fs::write(&target, b"{}").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        let path = link_directory.join("policy.json");
        symlink(&target, &path).unwrap();
        rejected(&path, uid, trusted_file);
        assert_eq!(fs::read_link(&path).unwrap(), target);
        assert_eq!(fs::read(&target).unwrap(), b"{}");
        assert_eq!(fs::symlink_metadata(&target).unwrap().mode() & 0o777, 0o644);
        assert_eq!(fs::read_dir(&link_directory).unwrap().count(), 1);
        eprintln!(
            "original policy rejection: symlink; code=AUTOROUTER_CONFIG_ERROR; unchanged=true"
        );
    }

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
            assert!(!error.message().contains("PRIVATE_"));
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
            assert_eq!(
                error.message(),
                "Policy session_log_mode has an invalid value."
            );
        }
        fs::write(&path, b"{\"allowed_evaluators\":[\"ollama\"]}").unwrap();
        assert!(
            load_policy_at(&path, uid + 1)
                .err()
                .unwrap()
                .message()
                .contains("owned by root")
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();
        assert!(
            !load_policy_at(&path, uid)
                .err()
                .unwrap()
                .message()
                .contains("PRIVATE_")
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            !load_policy_at(&path, uid)
                .err()
                .unwrap()
                .message()
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
                .message()
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
