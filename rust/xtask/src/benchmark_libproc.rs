//! Optional SDK-backed macOS collector. No private descriptor details retained.
use crate::tool_process::{InputAction, RunOptions, Scratch, run_child};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const C_SOURCE: &str = include_str!("../fixtures/descriptor-collector.c");
pub(super) const DESIGN: &str = include_str!("../../parity/descriptor-benchmark-v1.json");
pub(super) const IMPLEMENTATION: &str =
    include_str!("../../parity/descriptor-implementation-v1.json");
const METHOD: &str = "libproc_pid_numeric_records_v1";
const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_COMPILER_FILE: u64 = 512 * 1024 * 1024;
const STATUSES: &[&str] = &[
    "invalid_arguments",
    "invalid_pid",
    "sdk_size_overflow",
    "process_unavailable",
    "list_unavailable",
    "entry_limit",
    "misaligned_hint",
    "allocation_failed",
    "enumeration_unstable",
    "reported_overflow",
    "misaligned_records",
    "invalid_records",
    "process_identity_changed",
];
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn file_sha(path: &Path, maximum: u64) -> Result<String, String> {
    let mut file =
        std::fs::File::open(path).map_err(|_| "Cannot identify descriptor compiler input")?;
    let metadata = file
        .metadata()
        .map_err(|_| "Cannot inspect descriptor compiler input")?;
    if !metadata.is_file() || metadata.len() > maximum {
        return Err("Descriptor compiler input exceeds bound".into());
    }
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| "Cannot hash descriptor compiler input")?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > maximum {
            return Err("Descriptor compiler input grew beyond bound".into());
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn unsuccessful(status: &str) -> Value {
    json!({"schema_version":1,"method":METHOD,"status":status,"verified":false,"count":null})
}
fn parse(bytes: &[u8]) -> Option<Value> {
    if bytes.len() > MAX_OUTPUT
        || !bytes.ends_with(b"\n")
        || bytes[..bytes.len() - 1].contains(&b'\n')
        || bytes.contains(&b'\r')
    {
        return None;
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Record {
        schema_version: u8,
        method: String,
        status: String,
        verified: bool,
        count: Option<u64>,
    }
    // Derived struct deserialization rejects duplicate known fields, unlike
    // JSON.parse-like observation objects. Exact required keys are checked too.
    let record: Record = serde_json::from_slice(bytes).ok()?;
    if record.schema_version != 1
        || record.method != METHOD
        || (record.verified && (record.status != "collected" || record.count.is_none()))
    {
        return None;
    }
    let value: Value = serde_json::from_slice(bytes).ok()?;
    if value.as_object()?.len() != 5 || value["schema_version"] != 1 || value["method"] != METHOD {
        return None;
    }
    for field in ["schema_version", "method", "status", "verified", "count"] {
        if !value.as_object()?.contains_key(field) {
            return None;
        }
    }
    match value["verified"].as_bool()? {
        true if value["status"] == "collected"
            && value["count"].as_u64().is_some_and(|count| count <= 131072) =>
        {
            Some(value)
        }
        false if value["count"].is_null() && STATUSES.contains(&value["status"].as_str()?) => {
            Some(value)
        }
        _ => None,
    }
}
async fn bounded(
    command: &mut tokio::process::Command,
    timeout: Duration,
    cancel: &CancellationToken,
) -> (Value, Vec<u8>) {
    // Only the explicitly owned compiler scratch path may survive clearing.
    let temporary = command
        .as_std()
        .get_envs()
        .find(|(key, _)| *key == "TMPDIR")
        .and_then(|(_, value)| value.map(std::ffi::OsStr::to_owned));
    command.env_clear();
    if let Some(temporary) = temporary {
        command.env("TMPDIR", temporary);
    }
    let response = CancellationToken::new();
    let mut bytes = Vec::new();
    let report = run_child(
        command,
        RunOptions {
            timeout,
            grace: Duration::from_millis(100),
            max_stdout: Some(MAX_OUTPUT),
            interactive: false,
            response: &response,
            cancel,
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
    (report, bytes)
}
fn complete(report: &Value) -> bool {
    report["exit_code"] == 0
        && report["timed_out"] == false
        && report["output_limit_exceeded"] == false
        && report["cancelled"] == false
        && report["stderr_bytes"] == 0
}
async fn tool(
    command: &mut tokio::process::Command,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, String> {
    let (report, bytes) = bounded(command, Duration::from_secs(30), cancel).await;
    if !complete(&report) {
        return Err("Descriptor compiler/SDK identification failed or produced diagnostics".into());
    }
    Ok(bytes)
}
fn one_line(bytes: &[u8]) -> Result<&str, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "Invalid descriptor tool output")?;
    let text = text
        .strip_suffix('\n')
        .ok_or("Incomplete descriptor tool output")?;
    if text.is_empty() || text.contains(['\n', '\r', '\0']) {
        return Err("Ambiguous descriptor tool output".into());
    }
    Ok(text)
}
pub(super) struct Collector {
    _scratch: Scratch,
    binary: PathBuf,
    pub(super) identity: Value,
}
impl Collector {
    pub(super) async fn prepare(cancel: &CancellationToken) -> Result<Self, String> {
        Self::compile(cancel, false).await
    }
    async fn compile(cancel: &CancellationToken, test: bool) -> Result<Self, String> {
        Self::compile_source(cancel, test, C_SOURCE).await
    }
    async fn compile_source(
        cancel: &CancellationToken,
        test: bool,
        contents: &str,
    ) -> Result<Self, String> {
        if cancel.is_cancelled() {
            return Err("Descriptor compilation cancelled before admission".into());
        }
        if !cfg!(target_os = "macos") {
            return Err("libproc-v1 requires macOS".into());
        }
        let scratch = Scratch::new("descriptor-collector")?;
        let compiler = tool(
            tokio::process::Command::new("/usr/bin/xcrun")
                .env("TMPDIR", &scratch.0)
                .args(["--find", "clang"]),
            cancel,
        )
        .await?;
        let compiler = PathBuf::from(one_line(&compiler)?);
        let sdk = tool(
            tokio::process::Command::new("/usr/bin/xcrun")
                .env("TMPDIR", &scratch.0)
                .args(["--sdk", "macosx", "--show-sdk-path"]),
            cancel,
        )
        .await?;
        let sdk = PathBuf::from(one_line(&sdk)?);
        if !compiler.is_absolute() || !sdk.is_absolute() {
            return Err("Descriptor compiler/SDK path is not absolute".into());
        }
        let sdk_version = tool(
            tokio::process::Command::new("/usr/bin/xcrun")
                .env("TMPDIR", &scratch.0)
                .args(["--sdk", "macosx", "--show-sdk-version"]),
            cancel,
        )
        .await?;
        let sdk_version = one_line(&sdk_version)?.to_owned();
        let version = tool(
            tokio::process::Command::new(&compiler)
                .env("TMPDIR", &scratch.0)
                .arg("--version"),
            cancel,
        )
        .await?;
        let version = std::str::from_utf8(&version)
            .map_err(|_| "Invalid descriptor compiler version")?
            .lines()
            .next()
            .filter(|line| !line.is_empty())
            .ok_or("Missing descriptor compiler version")?
            .to_owned();
        let compiler_sha = file_sha(&compiler, MAX_COMPILER_FILE)?;
        let libproc_header = sdk.join("usr/include/libproc.h");
        let records_header = sdk.join("usr/include/sys/proc_info.h");
        let libproc_sha = file_sha(&libproc_header, 1024 * 1024)?;
        let records_sha = file_sha(&records_header, 1024 * 1024)?;
        if contents.len() > 1024 * 1024 {
            return Err("Descriptor source exceeds bound".into());
        }
        let source = scratch.file("descriptor-collector.c", contents.as_bytes())?;
        let binary = scratch.0.join("descriptor-collector");
        let mut flags = vec![
            "-std=c11",
            "-Os",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-mmacosx-version-min=11.0",
            "-UAUTOROUTER_FD_TEST",
        ];
        if test {
            flags.push("-DAUTOROUTER_FD_TEST=1");
        }
        let mut command = tokio::process::Command::new(&compiler);
        command.env("TMPDIR", &scratch.0).current_dir(&scratch.0);
        command
            .args(&flags)
            .arg("-isysroot")
            .arg(&sdk)
            .arg(&source)
            .arg("-o")
            .arg(&binary);
        let output = tool(&mut command, cancel).await?;
        if !output.is_empty() {
            return Err("Descriptor compiler produced unexpected output".into());
        }
        if compiler_sha != file_sha(&compiler, MAX_COMPILER_FILE)?
            || libproc_sha != file_sha(&libproc_header, 1024 * 1024)?
            || records_sha != file_sha(&records_header, 1024 * 1024)?
        {
            return Err("Descriptor compiler/SDK inputs changed during compilation".into());
        }
        let identity = json!({"method":METHOD,"compiled_before_services":true,"test_backend":test,
            "source_sha256":sha(contents.as_bytes()),"helper_sha256":file_sha(&binary,1024*1024)?,
            "compiler_sha256":compiler_sha,"compiler_version":version,"compiler_flags":flags,"compiler_hash_maximum_bytes":MAX_COMPILER_FILE,"compiler_hash_buffer_bytes":65536,
            "sdk_version":sdk_version,"sdk_headers":{"libproc.h":libproc_sha,"sys/proc_info.h":records_sha},"inputs_identity_checked_before_and_after":true,
            "design_protocol_sha256":sha(DESIGN.as_bytes()),"implementation_protocol_sha256":sha(IMPLEMENTATION.as_bytes())});
        Ok(Self {
            _scratch: scratch,
            binary,
            identity,
        })
    }
    pub(super) async fn snapshot(&self, pid: u32, cancel: &CancellationToken) -> Value {
        if cancel.is_cancelled() {
            return unsuccessful("collection_cancelled");
        }
        let (report, bytes) = bounded(
            tokio::process::Command::new(&self.binary).arg(pid.to_string()),
            Duration::from_secs(2),
            cancel,
        )
        .await;
        Self::result(&report, &bytes)
    }
    fn result(report: &Value, bytes: &[u8]) -> Value {
        if !complete(report) {
            return unsuccessful(if report["cancelled"] == true {
                "collection_cancelled"
            } else if report["timed_out"] == true {
                "collection_timed_out"
            } else if report["output_limit_exceeded"] == true {
                "collection_output_limit"
            } else if report["stderr_bytes"]
                .as_u64()
                .is_some_and(|bytes| bytes != 0)
            {
                "collection_diagnostics"
            } else {
                "collection_failed"
            });
        }
        parse(bytes).unwrap_or_else(|| unsuccessful("invalid_or_ambiguous_record"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn records_are_exact_bounded_and_private_free() {
        let valid = json!({"schema_version":1,"method":METHOD,"status":"collected","verified":true,"count":3});
        let bytes = |value: &Value| {
            let mut bytes = serde_json::to_vec(value).unwrap();
            bytes.push(b'\n');
            bytes
        };
        assert_eq!(parse(&bytes(&valid)), Some(valid.clone()));
        for (field, bad) in [
            ("count", json!(131073)),
            ("count", json!(-1)),
            ("count", json!(3.5)),
            ("count", json!(null)),
            ("verified", json!(false)),
            ("status", json!("unknown")),
            ("schema_version", json!(2)),
            ("private_path", json!("synthetic forbidden")),
        ] {
            let mut value = valid.clone();
            value[field] = bad;
            assert!(parse(&bytes(&value)).is_none(), "{field}");
        }
        let failure = unsuccessful("process_unavailable");
        assert_eq!(parse(&bytes(&failure)), Some(failure));
        assert!(parse(&serde_json::to_vec(&valid).unwrap()).is_none());
        let mut extra = bytes(&valid);
        extra.push(b'\n');
        assert!(parse(&extra).is_none());
        assert!(parse(&vec![b'x'; MAX_OUTPUT + 1]).is_none());
        assert!(parse(b"{\"schema_version\":1,\"method\":\"libproc_pid_numeric_records_v1\",\"status\":\"collected\",\"verified\":true,\"count\":1,\"count\":2}\n").is_none());
        let report = json!({"exit_code":0,"timed_out":false,"output_limit_exceeded":false,"cancelled":false,"stderr_bytes":1});
        assert_eq!(
            Collector::result(&report, &bytes(&valid))["status"],
            "collection_diagnostics"
        );
        assert!(Collector::result(&report, &bytes(&valid))["count"].is_null());
    }
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn sdk_helper_handles_real_open_close_exit_and_bounded_races() {
        use std::process::Stdio;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let cancel = CancellationToken::new();
        let collector = Collector::compile(&cancel, true).await.unwrap();
        let production = Collector::prepare(&cancel).await.unwrap();
        assert_eq!(production.identity["test_backend"], false);
        assert!(
            Collector::compile_source(&cancel, false, "#error Synthetic compiler failure\n")
                .await
                .is_err()
        );
        for argument in [
            "0",
            "-1",
            "non-numeric",
            "2147483648",
            "184467440737095516160",
        ] {
            let (report, bytes) = bounded(
                tokio::process::Command::new(&production.binary).arg(argument),
                Duration::from_secs(2),
                &cancel,
            )
            .await;
            assert_eq!(Collector::result(&report, &bytes)["status"], "invalid_pid");
        }
        let mut child = tokio::process::Command::new(&collector.binary)
            .arg("--target")
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(2), stdout.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(line, "ready\n");
        let before = production.snapshot(pid, &cancel).await;
        assert_eq!(before["verified"], true, "{before}");
        stdin.write_all(b"o").await.unwrap();
        line.clear();
        tokio::time::timeout(Duration::from_secs(2), stdout.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(line, "opened\n");
        let open = production.snapshot(pid, &cancel).await;
        assert_eq!(
            open["count"].as_u64().unwrap(),
            before["count"].as_u64().unwrap() + 2
        );
        stdin.write_all(b"c").await.unwrap();
        line.clear();
        tokio::time::timeout(Duration::from_secs(2), stdout.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(line, "closed\n");
        assert_eq!(
            production.snapshot(pid, &cancel).await["count"],
            before["count"]
        );
        stdin.write_all(b"q").await.unwrap();
        child.wait().await.unwrap();
        let exited = production.snapshot(pid, &cancel).await;
        assert_eq!(exited["verified"], false);
        assert!(exited["count"].is_null());
        for (scenario, status) in [
            ("identity_changed", "process_identity_changed"),
            ("negative", "invalid_records"),
            ("duplicate", "invalid_records"),
            ("overflow_hint", "entry_limit"),
            ("integer_overflow_hint", "entry_limit"),
            ("misaligned_hint", "misaligned_hint"),
            ("reported_overflow", "reported_overflow"),
            ("misaligned_records", "misaligned_records"),
            ("unstable_growth", "enumeration_unstable"),
        ] {
            let (report, bytes) = bounded(
                tokio::process::Command::new(&collector.binary).args(["1", scenario]),
                Duration::from_secs(2),
                &cancel,
            )
            .await;
            let result = Collector::result(&report, &bytes);
            assert_eq!(result["status"], status, "{scenario}");
            assert!(result["count"].is_null());
        }
        let (report, bytes) = bounded(
            tokio::process::Command::new(&collector.binary).args(["1", "growth_then_stable"]),
            Duration::from_secs(2),
            &cancel,
        )
        .await;
        assert_eq!(Collector::result(&report, &bytes)["count"], 2);
        let stopping = CancellationToken::new();
        let response = CancellationToken::new();
        let mut bytes = Vec::new();
        let report = run_child(
            tokio::process::Command::new(&collector.binary)
                .args(["1", "blocked"])
                .env_clear(),
            RunOptions {
                timeout: Duration::from_secs(2),
                grace: Duration::from_millis(100),
                max_stdout: Some(MAX_OUTPUT),
                interactive: false,
                response: &response,
                cancel: &stopping,
                initial: InputAction {
                    close: true,
                    ..Default::default()
                },
            },
            |part| {
                bytes.extend_from_slice(part);
                if bytes == b"blocked\n" {
                    stopping.cancel();
                }
                InputAction::default()
            },
        )
        .await;
        assert_eq!(bytes, b"blocked\n");
        assert_eq!(report["cancelled"], true);
        assert_eq!(
            Collector::result(&report, &bytes)["status"],
            "collection_cancelled"
        );
        assert!(report["exit_signal"].is_string());
        let (report, bytes) = bounded(
            tokio::process::Command::new(&collector.binary).args(["1", "blocked"]),
            Duration::from_secs(2),
            &cancel,
        )
        .await;
        assert_eq!(bytes, b"blocked\n");
        assert_eq!(report["timed_out"], true);
        assert_eq!(
            Collector::result(&report, &bytes)["status"],
            "collection_timed_out"
        );
        let already = CancellationToken::new();
        already.cancel();
        assert!(production.snapshot(std::process::id(), &already).await["count"].is_null());
        assert!(Collector::prepare(&already).await.is_err());
        let (report, bytes) = bounded(
            &mut tokio::process::Command::new("/nonexistent/synthetic-collector"),
            Duration::from_secs(2),
            &cancel,
        )
        .await;
        assert_eq!(
            Collector::result(&report, &bytes)["status"],
            "collection_failed"
        );
        let production_path = production._scratch.0.clone();
        let test_path = collector._scratch.0.clone();
        drop(production);
        drop(collector);
        assert!(!production_path.exists() && !test_path.exists());
    }
}
