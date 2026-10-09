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
        let original =
            r#"{"env":{"SECRET":"synthetic-value"},"permissions":{"deny":["Read(/secret/**)"]}}"#;
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
        assert_eq!(
            fs::metadata(&result[1]).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
