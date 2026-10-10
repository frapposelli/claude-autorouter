//! Complete frozen history-reader schedules, using actual private files and the
//! public asynchronous reader. Source definitions 1, 2, 5, 7, 8, 9 and 10 retain
//! their original inputs, operation order and assertions. No provider calls.
use autorouter_core::savings::PRICING_VERSION;
use autorouter_core::session_history::HistoryLimits;
use autorouter_runtime::session_history::{HistoryOptions, read_session_history};
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::Duration;

const ID: &str = "autorouter-session-20261005T120000Z-run-a";
const STAMP: &str = "2026-10-05T12:00:00.000Z";

fn decision(request: &str) -> Value {
    json!({"schema_version":2,"event":"decision","timestamp":STAMP,
        "request_id":request,"session_id":"session-a",
        "requested_model":"claude-haiku-4-5-20251001",
        "selected_model":"claude-haiku-4-5-20251001","source":"jev",
        "reason":"classified","decision_latency_ms":12,"prompt_excerpt":"Synthetic task"})
}
fn outcome(request: &str) -> Value {
    json!({"schema_version":2,"event":"outcome","timestamp":STAMP,
        "request_id":request,"session_id":"session-a","status":"completed",
        "http_status":200,"completion_confirmed":true,
        "confirmed_model":"claude-haiku-4-5-20251001","baseline_model":"claude-opus-5-5",
        "pricing_version":PRICING_VERSION,"usage_complete":true,
        "usage":{"input_tokens":1000,"output_tokens":100},"total_latency_ms":600})
}
fn changed(mut row: Value, fields: Value) -> Value {
    for (key, value) in fields.as_object().unwrap() {
        row[key] = value.clone();
    }
    row
}
fn omitted(mut row: Value, key: &str) -> Value {
    row.as_object_mut().unwrap().remove(key);
    row
}
struct Fixture {
    root: PathBuf,
    directory: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let mut random = [0; 12];
        getrandom::fill(&mut random).unwrap();
        let root = std::env::temp_dir().join(format!(
            "autorouter-history-contract-{}",
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let directory = root.join("logs");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        Self { root, directory }
    }
    fn write(&self, rows: &[Value], name: &str) {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(self.directory.join(format!("{name}.jsonl")))
            .unwrap();
        for row in rows {
            writeln!(file, "{row}").unwrap();
        }
    }
    fn append(&self, bytes: &[u8]) {
        OpenOptions::new()
            .append(true)
            .open(self.directory.join(format!("{ID}.jsonl")))
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }
    async fn show(&self, limits: HistoryLimits) -> Value {
        read(&self.directory, Some(ID), limits).await.unwrap()
    }
    async fn list(&self, limits: HistoryLimits) -> Value {
        read(&self.directory, None, limits).await.unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
async fn read(directory: &Path, id: Option<&str>, limits: HistoryLimits) -> Result<Value, String> {
    tokio::time::timeout(
        Duration::from_secs(5),
        read_session_history(
            directory.to_owned(),
            HistoryOptions {
                id: id.map(str::to_owned),
                limits,
            },
        ),
    )
    .await
    .expect("bounded local history read")
}

#[tokio::test]
async fn original_history_correlates_selections_outcomes_and_only_covered_usage() {
    let f = Fixture::new();
    f.write(&[
        decision("success"), outcome("success"), decision("failed"),
        changed(outcome("failed"), json!({"status":"error","http_status":429,"completion_confirmed":false})),
        changed(decision("pending"), json!({"source":"fallback","classifier_error":"timeout","reason":"classifier_error","selected_model":"claude-sonnet-5-5","decision_latency_ms":1500})),
        changed(decision("legacy"), json!({"schema_version":1})),
        omitted(changed(outcome("invalid"), json!({"status":"error","http_status":400,"completion_confirmed":false})), "confirmed_model"),
        decision("unfinished"), changed(outcome("unfinished"), json!({"completion_confirmed":false})),
        changed(outcome("cancelled"), json!({"status":"cancelled","completion_confirmed":false})),
    ], ID);
    let result = f.show(HistoryLimits::default()).await;
    let summary = &result["summary"];
    assert_eq!(summary["id"], ID); // history#1:assert-1
    assert_eq!(summary["session_id"], "session-a"); // history#1:assert-2
    let counts: Vec<_> = [
        "requests",
        "decisions",
        "outcomes",
        "completed",
        "failed",
        "cancelled",
        "pending",
        "unconfirmed",
        "outcome_only",
    ]
    .map(|field| summary[field].as_u64().unwrap())
    .into();
    assert_eq!(counts, [7, 5, 5, 1, 2, 1, 2, 1, 2]); // history#1:assert-3
    assert_eq!(summary["fallbacks"], 1); // history#1:assert-4
    assert_eq!(summary["fallback_rate"].as_f64(), Some(0.2)); // history#1:assert-5
    assert_eq!(summary["classifier_errors"], json!({"timeout":1})); // history#1:assert-6
    assert_eq!(
        summary["decision_latency_ms"],
        // JavaScript has one Number type; native measured percentiles are f64.
        // These exact values require no tolerance or rounding.
        json!({"samples":5,"p50":12.0,"p95":1500.0,"max":1500.0})
    ); // history#1:assert-7
    assert_eq!(summary["savings"]["priced_requests"], 1); // history#1:assert-8
    assert_eq!(summary["savings"]["unpriced_requests"], 6); // history#1:assert-9
    assert_eq!(summary["savings"]["saved_usd"].as_f64(), Some(0.0045)); // history#1:assert-10
    assert_eq!(summary["savings"]["unpriced_reasons"]["missing_outcome"], 2); // history#1:assert-11
    assert_eq!(summary["pricing_versions"], json!([PRICING_VERSION])); // history#1:assert-12
    assert_eq!(summary["mixed_pricing_versions"], false); // history#1:assert-13
    assert_eq!(summary["unversioned_outcomes"], 0); // history#1:assert-14
    assert_eq!(summary["coverage"]["legacy_decisions"], 1); // history#1:assert-15
    assert_eq!(summary["coverage"]["partial"], false); // history#1:assert-16
    assert_eq!(
        result["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["request_id"] == "legacy")
            .unwrap()["schema_version"]
            .as_f64(),
        Some(1.0)
    ); // history#1:assert-17
}

#[tokio::test]
async fn original_history_keeps_mixed_provenance_and_deduplicates_recorded_outcomes() {
    let f = Fixture::new();
    let one = outcome("one");
    f.write(
        &[
            decision("one"),
            one.clone(),
            one,
            changed(outcome("two"), json!({"baseline_model":"claude-opus-5"})),
            changed(outcome("future"), json!({"pricing_version":"future.9"})),
            omitted(outcome("missing-baseline"), "baseline_model"),
        ],
        ID,
    );
    let result = f.show(HistoryLimits::default()).await;
    let summary = &result["summary"];
    assert_eq!(summary["requests"], 4); // history#2:assert-1
    assert_eq!(summary["savings"]["priced_requests"], 2); // history#2:assert-2
    assert_eq!(
        summary["savings"]["unpriced_reasons"]["unknown_pricing_version"],
        1
    ); // history#2:assert-3
    assert_eq!(
        summary["savings"]["unpriced_reasons"]["unknown_baseline"],
        1
    ); // history#2:assert-4
    assert_eq!(
        summary["baseline_models"],
        json!(["claude-opus-5", "claude-opus-5-5"])
    ); // history#2:assert-5
    assert_eq!(summary["mixed_baselines"], true); // history#2:assert-6
    assert_eq!(
        summary["pricing_versions"],
        json!([PRICING_VERSION, "future.9"])
    ); // history#2:assert-7
    assert_eq!(summary["mixed_pricing_versions"], true); // history#2:assert-8
    assert_eq!(summary["coverage"]["duplicate_records"], 1); // history#2:assert-9
    assert_eq!(summary["coverage"]["partial"], true); // history#2:assert-10
}

#[tokio::test]
async fn original_history_legacy_and_metadata_never_invent_success_or_private_fields() {
    let f = Fixture::new();
    f.write(&[
        changed(decision("legacy"), json!({"schema_version":1,"body":{"private":"PRIVATE_BODY"},"headers":{"authorization":"PRIVATE_AUTH"}})),
        omitted(decision("metadata"), "prompt_excerpt"),
    ], ID);
    let result = f.show(HistoryLimits::default()).await;
    assert_eq!(result["summary"]["completed"], 0); // history#5:assert-1
    assert_eq!(result["summary"]["pending"], 2); // history#5:assert-2
    assert_eq!(result["summary"]["confirmed_models"], json!({})); // history#5:assert-3
    assert_eq!(result["summary"]["savings"]["priced_requests"], 0); // history#5:assert-4
    assert!(!result.to_string().contains("PRIVATE_")); // history#5:assert-5
}

#[tokio::test]
async fn original_history_exposes_record_line_byte_and_incomplete_tail_limits() {
    let f = Fixture::new();
    f.write(&[decision("one"), decision("two"), outcome("one")], ID);
    let result = f
        .show(HistoryLimits {
            max_records: 1,
            ..HistoryLimits::default()
        })
        .await;
    assert_eq!(result["records"].as_array().unwrap().len(), 1); // history#7:assert-1
    assert_eq!(result["summary"]["coverage"]["truncated"], true); // history#7:assert-2
    let result = f
        .show(HistoryLimits {
            max_lines: 1,
            ..HistoryLimits::default()
        })
        .await;
    assert_eq!(result["summary"]["coverage"]["partial"], true); // history#7:assert-3
    let result = f
        .show(HistoryLimits {
            max_file_bytes: 30,
            ..HistoryLimits::default()
        })
        .await;
    assert_eq!(result["summary"]["coverage"]["bytes_read"], 30); // history#7:assert-4
    assert_eq!(result["summary"]["coverage"]["truncated"], true); // history#7:assert-5
    assert_eq!(result["records"].as_array().unwrap().len(), 0); // history#7:assert-6
    f.append(b"{\"unfinished\":");
    let result = f.show(HistoryLimits::default()).await;
    assert_eq!(result["records"].as_array().unwrap().len(), 3); // history#7:assert-7
    assert_eq!(result["summary"]["coverage"]["incomplete_tail"], true); // history#7:assert-8
    assert_eq!(result["summary"]["coverage"]["partial"], true); // history#7:assert-9
}

#[tokio::test]
async fn original_history_counts_skipped_oversized_malformed_foreign_and_future_rows() {
    let f = Fixture::new();
    f.write(
        &[
            decision("good"),
            changed(decision("foreign"), json!({"session_id":"session-other"})),
            changed(decision("future"), json!({"schema_version":99})),
        ],
        ID,
    );
    f.append(format!("{}\nPRIVATE_MALFORMED_JSON\n", "x".repeat(17000)).as_bytes());
    let result = f.show(HistoryLimits::default()).await;
    assert_eq!(result["records"].as_array().unwrap().len(), 1); // history#8:assert-1
    assert_eq!(result["summary"]["coverage"]["mixed_session_records"], 1); // history#8:assert-2
    assert_eq!(result["summary"]["coverage"]["invalid_records"], 2); // history#8:assert-3
    assert_eq!(result["summary"]["coverage"]["oversized_lines"], 1); // history#8:assert-4
    assert_eq!(result["summary"]["coverage"]["partial"], true); // history#8:assert-5
    assert!(!result.to_string().contains("PRIVATE_")); // history#8:assert-6
}

#[tokio::test]
async fn original_history_lists_newest_ids_with_aggregate_byte_and_directory_bounds() {
    let f = Fixture::new();
    for suffix in ["a", "b", "c"] {
        f.write(&[decision(suffix)], &format!("autorouter-session-{suffix}"));
    }
    let result = f
        .list(HistoryLimits {
            max_files: 2,
            ..HistoryLimits::default()
        })
        .await;
    let ids: Vec<_> = result["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|session| session["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["autorouter-session-c", "autorouter-session-b"]); // history#9:assert-1
    assert_eq!(result["coverage"]["partial"], true); // history#9:assert-2
    assert_eq!(result["coverage"]["skipped_files"], 1); // history#9:assert-3
    let result = f
        .list(HistoryLimits {
            max_total_bytes: 100,
            ..HistoryLimits::default()
        })
        .await;
    assert_eq!(result["coverage"]["bytes_read"], 100); // history#9:assert-4
    assert_eq!(result["coverage"]["byte_limit_reached"], true); // history#9:assert-5
    assert_eq!(result["coverage"]["partial"], true); // history#9:assert-6
    let result = f
        .list(HistoryLimits {
            max_directory_entries: 1,
            ..HistoryLimits::default()
        })
        .await;
    assert_eq!(result["coverage"]["directory_scan_truncated"], true); // history#9:assert-7
    let mut files: Vec<_> = fs::read_dir(&f.directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            "autorouter-session-a.jsonl",
            "autorouter-session-b.jsonl",
            "autorouter-session-c.jsonl"
        ]
    ); // history#9:assert-8
}

#[tokio::test]
async fn original_history_rejects_traversal_symlinks_and_hardlinks_without_target_changes() {
    let f = Fixture::new();
    let target = f.root.join("PRIVATE_TARGET.jsonl");
    let content = format!(
        "{}\n",
        changed(
            decision("private"),
            json!({"prompt_excerpt":"PRIVATE_TARGET_CONTENT"})
        )
    );
    fs::write(&target, &content).unwrap();
    let log = f.directory.join(format!("{ID}.jsonl"));
    symlink(&target, &log).unwrap();
    let error = read(&f.directory, Some(ID), HistoryLimits::default())
        .await
        .unwrap_err();
    assert!(!error.contains("PRIVATE")); // history#10:assert-1
    let result = f.list(HistoryLimits::default()).await;
    assert_eq!(result["sessions"].as_array().unwrap().len(), 0); // history#10:assert-2
    assert_eq!(result["coverage"]["skipped_files"], 1); // history#10:assert-3
    let link = f.root.join("directory-link");
    symlink(&f.directory, &link).unwrap();
    let error = read(&link, None, HistoryLimits::default())
        .await
        .unwrap_err();
    assert!(error.contains("symbolic link")); // history#10:assert-4
    for attempted in [
        "../PRIVATE_TARGET".to_owned(),
        "/PRIVATE_TARGET".to_owned(),
        format!("{ID}.jsonl"),
        "x\nPRIVATE_TARGET".to_owned(),
    ] {
        let error = read(&f.directory, Some(&attempted), HistoryLimits::default())
            .await
            .unwrap_err();
        assert!(!error.contains("PRIVATE")); // history#10:assert-5 (four original attempts)
    }
    fs::remove_file(&log).unwrap();
    fs::hard_link(&target, &log).unwrap();
    let result = f.list(HistoryLimits::default()).await;
    assert_eq!(result["sessions"].as_array().unwrap().len(), 0); // history#10:assert-6
    assert_eq!(result["coverage"]["unreadable_files"], 1); // history#10:assert-7
    assert!(
        fs::read_to_string(&target)
            .unwrap()
            .contains("PRIVATE_TARGET_CONTENT")
    ); // history#10:assert-8
    // Stronger native boundary: every target byte remains unchanged.
    assert_eq!(fs::read_to_string(target).unwrap(), content);
}
