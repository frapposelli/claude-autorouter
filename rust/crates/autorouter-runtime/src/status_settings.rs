//! A per-launch settings overlay. Original files and permission anchors remain
//! intact. Opaque extension values retain JavaScript JSON/UTF-16 semantics.
use autorouter_core::js_json::{JsDocument, JsNode, JsString, NodeId};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
const SETTINGS_ERROR: &str = "Could not read --settings as a JSON object; set AUTOROUTER_STATUSLINE=0 to pass it directly to Claude";
const SANDBOX_ERROR: &str =
    "Cannot safely relocate source-relative sandbox paths; keep the original Claude settings";
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
pub fn status_line_settings(executable: &Path) -> serde_json::Value {
    serde_json::json!({"type":"command","command":format!("{} statusline",shell_quote(&executable.to_string_lossy())),"padding":0,"refreshInterval":1})
}
fn field_path(doc: &JsDocument, keys: &[&str]) -> Option<NodeId> {
    let mut node = doc.root();
    for key in keys {
        node = doc.get(node, key)?;
    }
    Some(node)
}
fn absolute_or_home(value: &JsString) -> bool {
    value.units().starts_with(&[b'/' as u16])
        || value.units().starts_with(&[b'~' as u16, b'/' as u16])
}
fn check_sandbox(doc: &JsDocument) -> Result<(), String> {
    for key in ["allowRead", "allowWrite", "denyRead", "denyWrite"] {
        if let Some(JsNode::Array(paths)) =
            field_path(doc, &["sandbox", "filesystem", key]).and_then(|n| doc.node(n))
        {
            for path in paths {
                if doc.string(*path).is_some_and(|s| !absolute_or_home(s)) {
                    return Err(SANDBOX_ERROR.into());
                }
            }
        }
    }
    if let Some(JsNode::Array(files)) =
        field_path(doc, &["sandbox", "credentials", "files"]).and_then(|n| doc.node(n))
    {
        for file in files {
            if doc
                .get(*file, "path")
                .and_then(|n| doc.string(n))
                .is_some_and(|s| !absolute_or_home(s))
            {
                return Err(SANDBOX_ERROR.into());
            }
        }
    }
    Ok(())
}
fn rebase_permissions(doc: &mut JsDocument, source: &Path) -> Result<(), String> {
    let Some(permissions) = doc.get(doc.root(), "permissions") else {
        return Ok(());
    };
    if !matches!(doc.node(permissions), Some(JsNode::Object(_))) {
        return Ok(());
    };
    let raw = source.to_string_lossy();
    let anchor = raw.strip_suffix('/').unwrap_or(&raw);
    let mut escaped = String::new();
    for c in anchor.chars() {
        if matches!(c, '\\' | '*' | '?' | '[' | ']') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    let prefix: Vec<u16> = format!("/{escaped}").encode_utf16().collect();
    for kind in ["allow", "ask", "deny"] {
        let Some(array) = doc.get(permissions, kind) else {
            continue;
        };
        let Some(JsNode::Array(rules)) = doc.node(array) else {
            continue;
        };
        let mut output = Vec::new();
        for rule in rules {
            let rewritten = doc.string(*rule).and_then(|text| {
                let units = text.units();
                let opening = if units.starts_with(&"Read(/".encode_utf16().collect::<Vec<_>>())
                    || units.starts_with(&"Edit(/".encode_utf16().collect::<Vec<_>>())
                {
                    5
                } else {
                    return None;
                };
                if units.last() != Some(&(b')' as u16))
                    || units.get(opening + 1) == Some(&(b'/' as u16))
                {
                    return None;
                };
                let mut result = units[..opening].to_vec();
                result.extend(&prefix);
                result.extend(&units[opening..]);
                Some(JsString::from_utf16(result).stringify())
            });
            output.push(rewritten.unwrap_or_else(|| doc.stringify_node(*rule)));
        }
        doc.set_field_json(
            permissions,
            kind,
            format!("[{}]", output.join(",")).as_bytes(),
        )
        .map_err(|_| SETTINGS_ERROR)?;
    }
    Ok(())
}
/// Pure relocation transform, exposed separately for exact byte fixtures.
pub fn overlay_json(bytes: &[u8], source: &Path, executable: &Path) -> Result<Vec<u8>, String> {
    let mut document = JsDocument::parse(bytes).map_err(|_| SETTINGS_ERROR)?;
    if !matches!(document.node(document.root()), Some(JsNode::Object(_))) {
        return Err(SETTINGS_ERROR.into());
    }
    check_sandbox(&document)?;
    rebase_permissions(&mut document, source)?;
    let settings = status_line_settings(executable).to_string();
    document
        .set_root_field_json("statusLine", settings.as_bytes())
        .map_err(|_| SETTINGS_ERROR)?;
    Ok(document.stringify().into_bytes())
}
pub fn add_status_line_settings(
    args: &[String],
    directory: &Path,
    cwd: &Path,
    executable: &Path,
) -> Result<Vec<String>, String> {
    let mut forwarded = Vec::new();
    let mut supplied: Option<&str> = None;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            forwarded.extend(args[index..].iter().cloned());
            break;
        }
        if arg == "--settings" {
            index += 1;
            let value = args
                .get(index)
                .ok_or("--settings requires a JSON object or settings file")?;
            if value.is_empty() || value.starts_with("--") {
                return Err("--settings requires a JSON object or settings file".into());
            }
            supplied = Some(value);
        } else if let Some(value) = arg.strip_prefix("--settings=") {
            supplied = Some(value);
        } else {
            forwarded.push(arg.clone());
        }
        index += 1;
    }
    let mut source = cwd.to_owned();
    let bytes = if let Some(supplied) = supplied {
        if autorouter_core::config::js_trim(supplied).starts_with('{') {
            supplied.as_bytes().to_vec()
        } else {
            let path = autorouter_core::config::resolve_path(cwd, Path::new(supplied));
            let metadata = fs::metadata(&path).map_err(|_| SETTINGS_ERROR)?;
            if !metadata.is_file() || metadata.len() > 2 * 1024 * 1024 {
                return Err(SETTINGS_ERROR.into());
            }
            let mut bytes = Vec::new();
            fs::File::open(&path)
                .map_err(|_| SETTINGS_ERROR)?
                .take(2 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| SETTINGS_ERROR)?;
            if bytes.len() > 2 * 1024 * 1024 {
                return Err(SETTINGS_ERROR.into());
            }
            source = path.parent().unwrap_or(cwd).to_owned();
            bytes
        }
    } else {
        b"{}".to_vec()
    };
    let bytes = overlay_json(&bytes, &source, executable)?;
    let path = directory.join("claude-settings.json");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .map_err(|_| "Could not write temporary Claude settings")?;
    file.write_all(&bytes)
        .map_err(|_| "Could not write temporary Claude settings")?;
    let mut result = vec!["--settings".into(), path.to_string_lossy().into_owned()];
    result.extend(forwarded);
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct TestDirectory(std::path::PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            let mut nonce = [0u8; 8];
            getrandom::fill(&mut nonce).unwrap();
            let path = std::env::temp_dir().join(format!(
                "autorouter-settings-test-{:x}",
                u64::from_le_bytes(nonce)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn repeated_inline_settings_use_the_last_value_and_invalid_inputs_stay_private() {
        let directory = TestDirectory::new();
        let executable = Path::new("/synthetic/autorouter");
        let args = [
            "--settings",
            r#"{"model":"old"}"#,
            r#"--settings={"model":"new"}"#,
            "-p",
            "hi",
        ]
        .map(str::to_owned);
        let forwarded =
            add_status_line_settings(&args, &directory.0, &directory.0, executable).unwrap();
        assert_eq!(&forwarded[2..], &["-p", "hi"]);
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(&forwarded[1]).unwrap()).unwrap();
        assert_eq!(saved["model"], "new");
        for args in [
            vec!["--settings"],
            vec!["--settings", r#"{"secret":"synthetic-PRIVATE""#],
            vec!["--settings", "missing.json"],
        ] {
            let args: Vec<_> = args.into_iter().map(str::to_owned).collect();
            let error = add_status_line_settings(&args, &directory.0, &directory.0, executable)
                .unwrap_err();
            assert!(error.contains("--settings"));
            assert!(!error.contains("PRIVATE"));
            let unchanged: serde_json::Value =
                serde_json::from_slice(&fs::read(&forwarded[1]).unwrap()).unwrap();
            assert_eq!(unchanged, saved);
        }
    }

    #[test]
    fn inline_permissions_keep_cwd_anchors_and_all_other_pattern_forms() {
        let directory = TestDirectory::new();
        let cwd = Path::new("/synthetic/project");
        let unchanged = [
            "Read(//private/secrets/**)",
            "Edit(~/Documents/**)",
            "Read(./.env)",
            "Edit(src/**)",
            "Read(!.env)",
            "Write(/legacy/**)",
            "Bash(cat /tmp/file)",
        ];
        let mut permissions = serde_json::Map::new();
        for (kind, anchored) in [
            ("allow", "Read(/public/**)"),
            ("ask", "Edit(/review/**)"),
            ("deny", "Read(/secrets/**)"),
        ] {
            permissions.insert(
                kind.into(),
                json!(
                    std::iter::once(anchored)
                        .chain(unchanged)
                        .collect::<Vec<_>>()
                ),
            );
        }
        let supplied = format!("--settings={}", json!({"permissions":permissions}));
        let forwarded = add_status_line_settings(
            &[supplied],
            &directory.0,
            cwd,
            Path::new("/synthetic/autorouter"),
        )
        .unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(&forwarded[1]).unwrap()).unwrap();
        for (kind, anchored) in [
            ("allow", "Read(//synthetic/project/public/**)"),
            ("ask", "Edit(//synthetic/project/review/**)"),
            ("deny", "Read(//synthetic/project/secrets/**)"),
        ] {
            assert_eq!(
                saved["permissions"][kind],
                json!(
                    std::iter::once(anchored)
                        .chain(unchanged)
                        .collect::<Vec<_>>()
                )
            );
        }
    }

    #[test]
    fn absolute_and_home_sandbox_paths_are_preserved_verbatim() {
        let directory = TestDirectory::new();
        let sandbox = json!({
            "enabled":true,
            "filesystem":{
                "allowRead":["/public"],"allowWrite":["~/build"],
                "denyRead":["/private/secrets"],"denyWrite":["~/.ssh"]
            },
            "credentials":{"files":[
                {"path":"~/.aws/credentials","mode":"deny"},
                {"path":"/private/credential","mode":"mask"}
            ]}
        });
        let forwarded = add_status_line_settings(
            &["--settings".into(), json!({"sandbox":sandbox}).to_string()],
            &directory.0,
            &directory.0,
            Path::new("/synthetic/autorouter"),
        )
        .unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(&forwarded[1]).unwrap()).unwrap();
        assert_eq!(saved["sandbox"], sandbox);
    }

    #[test]
    fn relative_sandbox_rejection_preserves_inline_and_file_security_settings() {
        let directory = TestDirectory::new();
        let mut variants = Vec::new();
        for kind in ["allowRead", "allowWrite", "denyRead", "denyWrite"] {
            variants.push(json!({"sandbox":{"filesystem":{kind:["./synthetic-PRIVATE"]}}}));
        }
        variants.push(json!({"sandbox":{"credentials":{"files":[{"path":"../synthetic-PRIVATE","mode":"deny"}]}}}));
        let source = directory.0.join("original.json");
        for settings in variants {
            let original = settings.to_string();
            fs::write(&source, &original).unwrap();
            for supplied in [original.clone(), source.to_string_lossy().into_owned()] {
                let error = add_status_line_settings(
                    &["--settings".into(), supplied],
                    &directory.0,
                    &directory.0,
                    Path::new("/synthetic/autorouter"),
                )
                .unwrap_err();
                assert_eq!(error, SANDBOX_ERROR);
                assert!(!error.contains("PRIVATE"));
                assert_eq!(fs::read_to_string(&source).unwrap(), original);
                assert!(!directory.0.join("claude-settings.json").exists());
            }
        }
    }

    #[test]
    fn permission_anchors_and_unknown_utf16_are_preserved() {
        let source = Path::new("/private/synthetic[literal]*?/");
        let bytes=br#"{"opaque":"\ud800","permissions":{"allow":["Read(/src/**)","Read(//private/**)","Edit(~/safe/**)","Bash(node --test)"],"ask":["Edit(/review/**)"],"deny":["Read(/secret/**)"],"defaultMode":"dontAsk"},"statusLine":{"command":"old"}}"#;
        let result = String::from_utf8(
            overlay_json(
                bytes,
                source,
                Path::new("/synthetic/a ' $HOME `x`/autorouter"),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(result.contains("\\ud800"));
        assert!(result.contains(r"Read(//private/synthetic\\[literal\\]\\*\\?/src/**)"));
        assert!(result.contains("Read(//private/**)"));
        assert!(result.contains("Edit(~/safe/**)"));
        assert!(result.contains("dontAsk"));
        assert!(!result.contains("\"old\""));
    }
    #[test]
    fn relative_sandbox_paths_decline_without_echoing_secrets() {
        for key in ["allowRead", "allowWrite", "denyRead", "denyWrite"] {
            let bytes = json!({"sandbox":{"filesystem":{key:["./synthetic-secret"]}}}).to_string();
            assert_eq!(
                overlay_json(
                    bytes.as_bytes(),
                    Path::new("/cwd"),
                    Path::new("/bin/autorouter")
                )
                .unwrap_err(),
                SANDBOX_ERROR
            );
        }
        let bytes = json!({"sandbox":{"credentials":{"files":[{"path":"../secret"}]}}}).to_string();
        assert_eq!(
            overlay_json(
                bytes.as_bytes(),
                Path::new("/cwd"),
                Path::new("/bin/autorouter")
            )
            .unwrap_err(),
            SANDBOX_ERROR
        );
    }
    #[test]
    fn shell_metacharacters_are_literal() {
        let value = "a b ' c $HOME $(echo wrong) `echo wrong`";
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", &format!("printf %s {}", shell_quote(value))])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), value);
    }
    #[test]
    fn overlay_file_is_private_and_original_is_unchanged() {
        use std::os::unix::fs::PermissionsExt;
        let directory =
            std::env::temp_dir().join(format!("autorouter-overlay-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir(&directory).unwrap();
        let source = directory.join("original.json");
        let original = r#"{"env":{"SECRET":"synthetic-value"},"permissions":{"allow":["Read"]},"statusLine":{"type":"command","command":"old-command"}}"#;
        fs::write(&source, original).unwrap();
        let args = vec![
            "--settings".into(),
            source.to_string_lossy().into_owned(),
            "--model".into(),
            "sonnet".into(),
            "--".into(),
            "--settings=literal".into(),
        ];
        let result = add_status_line_settings(
            &args,
            &directory,
            Path::new("/"),
            Path::new("/native/autorouter"),
        )
        .unwrap();
        assert_eq!(&result[2..], &args[2..]);
        assert!(!result.join(" ").contains("synthetic-value"));
        assert_eq!(fs::read_to_string(source).unwrap(), original);
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(&result[1]).unwrap()).unwrap();
        let original_value: serde_json::Value = serde_json::from_str(original).unwrap();
        assert_eq!(saved["env"], original_value["env"]);
        assert_eq!(saved["permissions"], original_value["permissions"]);
        assert_eq!(saved["statusLine"]["refreshInterval"], 1);
        assert_ne!(saved["statusLine"]["command"], "old-command");
        assert_eq!(
            fs::metadata(&result[1]).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn default_settings_file_bytes_preserve_security_fields_at_the_bound_and_reject_only_larger_files()
     {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        const CAP: usize = 2 * 1024 * 1024;
        const ORIGINAL: &str = r#"{"permissions":{"allow":["Read(/public/**)"],"ask":["Edit(/review/**)"],"deny":["Read(/secrets/**)"],"defaultMode":"dontAsk"},"env":{"SYNTHETIC_CANARY":"PRIVATE_SETTINGS_BOUNDARY"},"opaque":{"values":["retained",0,true]},"statusLine":{"type":"command","command":"old"}}"#;
        const PREVIOUS: &[u8] = b"PREVIOUS_SYNTHETIC_OVERLAY\n";
        let original_value: serde_json::Value = serde_json::from_str(ORIGINAL).unwrap();
        for (delta, inline) in [(-1isize, false), (0, false), (1, false), (1, true)] {
            let directory = TestDirectory::new();
            fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o700)).unwrap();
            let source_directory = directory.0.join("source[literal]*?");
            let output_directory = directory.0.join("overlay");
            for path in [&source_directory, &output_directory] {
                fs::create_dir(path).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            let mut original = ORIGINAL.as_bytes().to_vec();
            original.resize(CAP.checked_add_signed(delta).unwrap(), b' ');
            assert_eq!(original.len(), CAP.checked_add_signed(delta).unwrap());
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&original).unwrap(),
                original_value
            );
            let source = source_directory.join("original.json");
            fs::write(&source, &original).unwrap();
            fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
            let overlay = output_directory.join("claude-settings.json");
            fs::write(&overlay, PREVIOUS).unwrap();
            fs::set_permissions(&overlay, fs::Permissions::from_mode(0o600)).unwrap();
            let identity = |path: &Path| {
                let value = fs::symlink_metadata(path).unwrap();
                assert!(value.is_file());
                assert_eq!(value.nlink(), 1);
                assert_eq!(value.permissions().mode() & 0o777, 0o600);
                (
                    value.dev(),
                    value.ino(),
                    value.len(),
                    value.modified().unwrap(),
                )
            };
            let source_before = identity(&source);
            let overlay_before = identity(&overlay);
            let supplied = if inline {
                String::from_utf8(original.clone()).unwrap()
            } else {
                source.to_string_lossy().into_owned()
            };
            let args = [
                "--settings".into(),
                supplied,
                "--literal".into(),
                "forwarded".into(),
                "--".into(),
                "--settings=literal".into(),
            ];
            let result = add_status_line_settings(
                &args,
                &output_directory,
                &directory.0,
                Path::new("/synthetic/autorouter"),
            );
            let accepted = inline || delta <= 0;
            if accepted {
                let forwarded = result.unwrap();
                assert_eq!(&forwarded[2..], &args[2..]);
                assert_eq!(forwarded[0], "--settings");
                assert_eq!(Path::new(&forwarded[1]), overlay);
                assert!(!forwarded.join(" ").contains("PRIVATE_SETTINGS_BOUNDARY"));
                let actual: serde_json::Value =
                    serde_json::from_slice(&fs::read(&overlay).unwrap()).unwrap();
                let anchor = if inline {
                    &directory.0
                } else {
                    &source_directory
                };
                let escaped = anchor
                    .to_str()
                    .unwrap()
                    .replace('\\', "\\\\")
                    .replace('*', "\\*")
                    .replace('?', "\\?")
                    .replace('[', "\\[")
                    .replace(']', "\\]");
                let mut expected = original_value.clone();
                expected["permissions"]["allow"] =
                    serde_json::json!([format!("Read(/{escaped}/public/**)")]);
                expected["permissions"]["ask"] =
                    serde_json::json!([format!("Edit(/{escaped}/review/**)")]);
                expected["permissions"]["deny"] =
                    serde_json::json!([format!("Read(/{escaped}/secrets/**)")]);
                expected["statusLine"] = serde_json::json!({"type":"command",
                    "command":"'/synthetic/autorouter' statusline","padding":0,"refreshInterval":1});
                assert_eq!(actual, expected);
                let after = identity(&overlay);
                assert_eq!((after.0, after.1), (overlay_before.0, overlay_before.1));
            } else {
                let error = result.unwrap_err();
                assert_eq!(error, SETTINGS_ERROR);
                assert!(!error.contains("PRIVATE_SETTINGS_BOUNDARY"));
                assert!(!error.contains(source.to_str().unwrap()));
                assert_eq!(fs::read(&overlay).unwrap(), PREVIOUS);
                assert_eq!(identity(&overlay), overlay_before);
            }
            assert_eq!(fs::read(&source).unwrap(), original);
            assert_eq!(identity(&source), source_before);
            assert_eq!(fs::read_dir(&source_directory).unwrap().count(), 1);
            assert_eq!(fs::read_dir(&output_directory).unwrap().count(), 1);
            eprintln!(
                "settings_file_boundary={}",
                serde_json::json!({"bytes":original.len(),
                "inline_API_control":inline,"accepted":accepted,"source_unchanged":true,
                "permission_fields_preserved_or_previous_overlay_unchanged":true})
            );
        }
    }
}
