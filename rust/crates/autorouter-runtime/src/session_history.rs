//! Read-only bounded session inspection. Files are opened without following
//! links or blocking on special devices, and directory identity is checked
//! around enumeration and opens. Historical logs are never repriced silently.
use autorouter_core::session_history::{
    HistoryLimits, locale_from_environment, parse_session_with_locale, valid_id,
};
use nix::fcntl::OFlag;
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
#[derive(Default)]
pub struct HistoryOptions {
    pub id: Option<String>,
    pub limits: HistoryLimits,
}
fn same_directory(root: &Path, identity: &fs::Metadata) -> io::Result<()> {
    let current = fs::symlink_metadata(root)?;
    if !current.is_dir()
        || current.file_type().is_symlink()
        || current.dev() != identity.dev()
        || current.ino() != identity.ino()
    {
        return Err(io::Error::other(
            "Session log directory changed or is not a regular directory.",
        ));
    }
    Ok(())
}
fn read_session(
    root: &Path,
    identity: &fs::Metadata,
    id: &str,
    limits: &HistoryLimits,
    remaining: usize,
) -> io::Result<Value> {
    same_directory(root, identity)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags((OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK).bits())
        .open(root.join(format!("{id}.jsonl")))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(io::Error::other(
            "Session log is not a regular private file.",
        ));
    }
    same_directory(root, identity)?;
    let capacity = metadata
        .len()
        .min(limits.max_file_bytes as u64)
        .min(remaining as u64) as usize;
    let mut bytes = vec![0u8; capacity];
    let mut read = 0;
    while read < capacity {
        match file.read(&mut bytes[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    bytes.truncate(read);
    let env = serde_json::Value::Object(
        ["LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .filter_map(|key| {
                std::env::var_os(key).map(|value| (key.into(), json!(value.to_string_lossy())))
            })
            .collect(),
    );
    Ok(parse_session_with_locale(
        &bytes,
        metadata.len(),
        id,
        limits,
        &locale_from_environment(&env),
    ))
}
pub async fn read_session_history(
    directory: PathBuf,
    options: HistoryOptions,
) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || read_history(&directory, &options))
        .await
        .map_err(|_| "Could not read the session log directory.".to_owned())?
}
fn read_history(root: &Path, options: &HistoryOptions) -> Result<Value, String> {
    let limits = &options.limits;
    if options.id.as_deref().is_some_and(|id| !valid_id(id)) {
        return Err("Use an exact session ID from sessions list.".into());
    }
    if autorouter_core::config::js_trim(&root.to_string_lossy()).is_empty() {
        return Err("A regular session log directory is required.".into());
    }
    let identity = match fs::symlink_metadata(root) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound && options.id.is_none() => {
            return Ok(
                json!({"schema_version":1,"type":"session_history","sessions":[],"limits":limits,"coverage":{"partial":false,"directory_missing":true}}),
            );
        }
        Err(_) => return Err("Could not read the session log directory.".into()),
    };
    if !identity.is_dir() || identity.file_type().is_symlink() {
        return Err(
            "Session logs must be read from a regular directory, not a symbolic link.".into(),
        );
    }
    if let Some(id) = &options.id {
        let result=read_session(root,&identity,id,limits,limits.max_total_bytes).map_err(|_|"Could not read that session as a regular log file. Use sessions list for available IDs.")?;
        return Ok(
            json!({"schema_version":1,"type":"session_history","summary":result["summary"],"records":result["records"],"limits":limits}),
        );
    }
    let (mut entries, mut matching, mut skipped, mut unreadable, mut bytes_read) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut truncated, mut byte_limit) = (false, false);
    let mut ids = Vec::new();
    let directory =
        fs::read_dir(root).map_err(|_| "Could not safely list the session log directory.")?;
    for entry in directory {
        let entry = entry.map_err(|_| "Could not safely list the session log directory.")?;
        entries += 1;
        if entries > limits.max_directory_entries {
            truncated = true;
            break;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(candidate) = name.strip_suffix(".jsonl").filter(|id| valid_id(id)) else {
            continue;
        };
        if !entry
            .file_type()
            .map_err(|_| "Could not safely list the session log directory.")?
            .is_file()
        {
            skipped += 1;
            continue;
        }
        matching += 1;
        ids.push(candidate.to_owned());
        ids.sort_by(|a, b| b.cmp(a));
        ids.truncate(limits.max_files);
    }
    same_directory(root, &identity)
        .map_err(|_| "Could not safely list the session log directory.")?;
    let mut sessions = Vec::new();
    for id in ids {
        if bytes_read >= limits.max_total_bytes {
            byte_limit = true;
            break;
        }
        match read_session(
            root,
            &identity,
            &id,
            limits,
            limits.max_total_bytes - bytes_read,
        ) {
            Ok(result) => {
                bytes_read += result["summary"]["coverage"]["bytes_read"]
                    .as_u64()
                    .unwrap_or(0) as usize;
                sessions.push(result["summary"].clone());
            }
            Err(_) => unreadable += 1,
        }
    }
    skipped += matching.saturating_sub(sessions.len() + unreadable);
    let partial = truncated
        || byte_limit
        || skipped > 0
        || unreadable > 0
        || sessions.iter().any(|s| s["coverage"]["partial"] == true);
    Ok(
        json!({"schema_version":1,"type":"session_history","sessions":sessions,"limits":limits,"coverage":{"partial":partial,"directory_entries":entries,"matching_files":matching,"skipped_files":skipped,"unreadable_files":unreadable,"bytes_read":bytes_read,"directory_scan_truncated":truncated,"byte_limit_reached":byte_limit}}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    fn root() -> PathBuf {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).unwrap();
        let path = std::env::temp_dir().join(format!(
            "autorouter-history-test-{:x}",
            u64::from_le_bytes(bytes)
        ));
        fs::create_dir(&path).unwrap();
        path
    }
    fn decision() -> Value {
        json!({"schema_version":2,"event":"decision","timestamp":"2026-10-05T12:00:00.000Z","request_id":"r","requested_model":"claude-opus-5-5","selected_model":"claude-haiku-4-5"})
    }
    #[tokio::test]
    async fn rejects_links_special_files_and_nonregular_directories() {
        let root = root();
        let target = root.join("private-target");
        fs::write(&target, format!("{}\n", decision())).unwrap();
        symlink(&target, root.join("autorouter-session-link.jsonl")).unwrap();
        fs::hard_link(&target, root.join("autorouter-session-hardlink.jsonl")).unwrap();
        nix::unistd::mkfifo(
            &root.join("autorouter-session-pipe.jsonl"),
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        for id in [
            "autorouter-session-link",
            "autorouter-session-hardlink",
            "autorouter-session-pipe",
        ] {
            let error = read_session_history(
                root.clone(),
                HistoryOptions {
                    id: Some(id.into()),
                    ..HistoryOptions::default()
                },
            )
            .await
            .unwrap_err();
            assert!(error.contains("regular log file"));
            assert!(!error.contains("private-target"));
        }
        let report = read_session_history(root.clone(), HistoryOptions::default())
            .await
            .unwrap();
        assert_eq!(report["sessions"].as_array().unwrap().len(), 0);
        assert_eq!(report["coverage"]["skipped_files"], 2);
        assert_eq!(report["coverage"]["unreadable_files"], 1);
        assert!(
            fs::read_to_string(target)
                .unwrap()
                .contains("requested_model")
        );
        let link = root.with_extension("link");
        symlink(&root, &link).unwrap();
        assert!(
            read_session_history(link.clone(), HistoryOptions::default())
                .await
                .unwrap_err()
                .contains("symbolic link")
        );
        fs::remove_file(link).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn file_record_directory_and_byte_bounds_are_visible_without_modifying_logs() {
        let root = root();
        let line = format!("{}\n", decision());
        for id in ["a", "b", "c"] {
            fs::write(
                root.join(format!("autorouter-session-{id}.jsonl")),
                line.repeat(3),
            )
            .unwrap();
        }
        let limits = HistoryLimits::from_overrides(&json!({"maxFiles":2,"maxRecords":1})).unwrap();
        let report = read_session_history(
            root.clone(),
            HistoryOptions {
                limits,
                ..HistoryOptions::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(report["sessions"].as_array().unwrap().len(), 2);
        assert_eq!(report["sessions"][0]["id"], "autorouter-session-c");
        assert_eq!(report["coverage"]["partial"], true);
        assert_eq!(report["coverage"]["skipped_files"], 1);
        assert_eq!(
            fs::read_to_string(root.join("autorouter-session-a.jsonl")).unwrap(),
            line.repeat(3)
        );
        let limits = HistoryLimits::from_overrides(&json!({"maxTotalBytes":line.len()+1})).unwrap();
        let report = read_session_history(
            root.clone(),
            HistoryOptions {
                limits,
                ..HistoryOptions::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(report["coverage"]["byte_limit_reached"], true);
        assert_eq!(report["sessions"][0]["coverage"]["incomplete_tail"], true);
        fs::remove_dir_all(root).unwrap();
    }
}
