//! Gateway-only descriptor snapshots without recording filenames or targets.
//! Collection is outside the measured interval; snapshots are not atomic peaks.
use serde_json::{Value, json};
#[path = "benchmark_libproc.rs"]
mod libproc;
pub(super) const LIBPROC_DESIGN: &str = libproc::DESIGN;
pub(super) const LIBPROC_IMPLEMENTATION: &str = libproc::IMPLEMENTATION;
pub(super) struct Collector {
    helper: Option<libproc::Collector>,
    pub(super) identity: Value,
}
impl Collector {
    pub(super) async fn prepare(libproc: bool) -> Result<Self, String> {
        if !libproc {
            return Ok(Self {
                helper: None,
                identity: Value::Null,
            });
        }
        if cfg!(target_os = "linux") {
            return Ok(Self {
                helper: None,
                identity: json!({"method":"proc_pid_fd_numeric_entries","libproc_companion_selected":true,"linux_method_unchanged":true}),
            });
        }
        let helper =
            libproc::Collector::prepare(&tokio_util::sync::CancellationToken::new()).await?;
        let identity = helper.identity.clone();
        Ok(Self {
            helper: Some(helper),
            identity,
        })
    }
    pub(super) async fn snapshot(&self, pid: u32) -> Value {
        if let Some(helper) = &self.helper {
            helper
                .snapshot(pid, &tokio_util::sync::CancellationToken::new())
                .await
        } else {
            snapshot(pid).await
        }
    }
}

const MAX_ENTRIES: usize = 131072;
const MAX_OUTPUT: usize = 1024 * 1024;

fn parse_lsof(bytes: &[u8], pid: u32) -> Option<u64> {
    if bytes.len() > MAX_OUTPUT || !bytes.ends_with(b"\n") {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.lines();
    if lines.next()? != format!("p{pid}") {
        return None;
    }
    let mut entries = std::collections::BTreeSet::new();
    for (index, line) in lines.enumerate() {
        if index >= MAX_ENTRIES {
            return None;
        }
        let fd = line.strip_prefix('f')?;
        // lsof also emits pseudo-descriptors for executable mappings and cwd;
        // those are not open numeric descriptors. Unknown records fail closed.
        if matches!(fd, "cwd" | "rtd" | "txt" | "mem") {
            continue;
        }
        if fd.is_empty() || !fd.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let number = fd.parse::<u32>().ok()?;
        if !entries.insert(number) {
            return None;
        }
    }
    u64::try_from(entries.len()).ok()
}

fn result(source: &str, observed: Option<u64>, verified: bool, status: &str) -> Value {
    json!({
        "method": source,
        "count": if verified { observed } else { None },
        "observed_count": observed,
        "verified": verified,
        "status": status,
        "scope": "numeric gateway descriptors at one non-atomic instant; no targets or filenames retained"
    })
}

pub(super) async fn snapshot(pid: u32) -> Value {
    if cfg!(target_os = "linux") {
        // The measured child remains owned and unreaped while this path runs.
        // Only enumerate names; do not read symlink targets or private paths.
        let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            return result("proc_pid_fd_numeric_entries", None, false, "unavailable");
        };
        let mut count = 0u64;
        for (index, entry) in entries.enumerate() {
            let Ok(entry) = entry else {
                return result("proc_pid_fd_numeric_entries", None, false, "read_failed");
            };
            if index >= MAX_ENTRIES {
                return result("proc_pid_fd_numeric_entries", None, false, "entry_limit");
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return result("proc_pid_fd_numeric_entries", None, false, "invalid_entry");
            };
            if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_digit()) {
                return result("proc_pid_fd_numeric_entries", None, false, "invalid_entry");
            }
            count += 1;
        }
        return result(
            "proc_pid_fd_numeric_entries",
            Some(count),
            true,
            "collected",
        );
    }
    if cfg!(target_os = "macos") {
        use crate::tool_process::{InputAction, RunOptions, run_child};
        use std::time::Duration;
        use tokio_util::sync::CancellationToken;
        let response = CancellationToken::new();
        let cancel = CancellationToken::new();
        let mut bytes = Vec::new();
        let report = run_child(
            tokio::process::Command::new("/usr/sbin/lsof")
                .args(["-n", "-P", "-a", "-p", &pid.to_string(), "-F", "pf"])
                .env_clear(),
            RunOptions {
                timeout: Duration::from_secs(2),
                grace: Duration::from_millis(100),
                max_stdout: Some(MAX_OUTPUT),
                interactive: false,
                response: &response,
                cancel: &cancel,
                initial: InputAction {
                    close: true,
                    ..Default::default()
                },
            },
            |part| {
                bytes.extend_from_slice(part);
                InputAction::default()
            },
        )
        .await;
        let completed = report["exit_code"] == 0
            && report["timed_out"] == false
            && report["output_limit_exceeded"] == false;
        let observed = completed.then(|| parse_lsof(&bytes, pid)).flatten();
        let verified = observed.is_some() && report["stderr_bytes"] == 0;
        return result(
            "lsof_pid_numeric_fields",
            observed,
            verified,
            if !completed {
                "collection_failed"
            } else if observed.is_none() {
                "invalid_or_ambiguous_fields"
            } else if !verified {
                "diagnostics_may_indicate_incomplete_collection"
            } else {
                "collected"
            },
        );
    }
    result("unsupported_os", None, false, "unavailable")
}

pub(super) fn delta(before: &Value, after: &Value) -> Option<i64> {
    if before["verified"] != true || after["verified"] != true {
        return None;
    }
    let before = i64::try_from(before["count"].as_u64()?).ok()?;
    let after = i64::try_from(after["count"].as_u64()?).ok()?;
    after.checked_sub(before)
}

/// Validation retains collection outcomes, never numerical resource values.
pub(super) fn validation_only(row: &mut Value) {
    if let Some(observations) = row["open_file_descriptors"].as_object_mut() {
        observations.remove("postload_minus_warmup");
        for key in ["idle", "after_warmup", "postload"] {
            if let Some(observation) = observations.get_mut(key).and_then(Value::as_object_mut) {
                observation.remove("count");
                observation.remove("observed_count");
            }
        }
    }
    row.as_object_mut().unwrap().remove("postload_rss_bytes");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fields_are_numeric_and_pid_scoped_without_file_targets() {
        assert_eq!(
            parse_lsof(b"p41\nfcwd\nftxt\nftxt\nf0\nf1\nf17\n", 41),
            Some(3)
        );
        for bytes in [
            &b"p42\nf0\n"[..],
            &b"p41\nf0\np42\nf1\n"[..],
            &b"p41\nf0\nf0\n"[..],
            &b"p41\nf0r\n"[..],
            &b"p41\nfNOFD\n"[..],
            &b"p41\nf0\nn/private/file\n"[..],
            &b"p41\nf1"[..],
            &b"p41\nf4294967296\n"[..],
            &b""[..],
        ] {
            assert_eq!(parse_lsof(bytes, 41), None, "{bytes:?}");
        }
        assert_eq!(parse_lsof(&vec![b'x'; MAX_OUTPUT + 1], 41), None);
    }
    #[test]
    fn recovery_needs_two_verified_counts_and_retains_growth() {
        let before = result("synthetic", Some(10), true, "collected");
        let after = result("synthetic", Some(13), true, "collected");
        assert_eq!(delta(&before, &after), Some(3));
        let warning = result("synthetic", Some(13), false, "warning");
        assert!(warning["count"].is_null());
        assert_eq!(warning["observed_count"], 13);
        assert_eq!(delta(&before, &warning), None);
        assert_eq!(delta(&Value::Null, &after), None);
    }
    #[test]
    fn validation_strips_resources_but_keeps_collection_failure() {
        let mut row = json!({
            "open_file_descriptors": {
                "idle": result("synthetic", Some(10), true, "collected"),
                "after_warmup": result("synthetic", Some(13), false, "warning"),
                "postload": result("synthetic", None, false, "unavailable"),
                "postload_minus_warmup": 3,
                "acceptance_qualified": false
            },
            "postload_rss_bytes": 100
        });
        validation_only(&mut row);
        assert_eq!(
            row["open_file_descriptors"]["after_warmup"]["verified"],
            false
        );
        assert_eq!(
            row["open_file_descriptors"]["postload"]["status"],
            "unavailable"
        );
        let text = row.to_string();
        assert!(!text.contains("observed_count") && !text.contains("\"count\""));
        assert!(!text.contains("postload_minus_warmup") && !text.contains("postload_rss_bytes"));
    }
    #[tokio::test]
    async fn os_snapshot_has_no_names_and_never_promotes_unverified_counts() {
        let row = snapshot(std::process::id()).await;
        assert!(row["status"].is_string());
        assert!(!row.to_string().contains("/private/") && !row.to_string().contains("/proc/"));
        if row["verified"] == true {
            assert!(row["count"].as_u64().unwrap() >= 3);
        } else {
            assert!(row["count"].is_null());
        }
    }
}
