//! Frozen session-history.test.mjs #3, #4, #6 and #11 through the actual
//! command boundary. Saved settings remain synthetic and Keychain is forbidden.
use super::command;
use autorouter_core::auth::Environment;
use autorouter_core::savings::{PRICING_DATE, PRICING_SOURCE, PRICING_VERSION};
use autorouter_runtime::keychain::Keychain;
use autorouter_runtime::session_history::{HistoryOptions, read_session_history};
use autorouter_runtime::user_config::{ConfigContext, SaveOptions, save_user_config};
use serde_json::{Value, json};
use std::fs::{self, DirBuilder};
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::PathBuf;
use std::time::Duration;

struct NoSecrets;
impl Keychain for NoSecrets {
    fn available(&self) -> bool {
        panic!("History must not inspect Keychain availability")
    }
    async fn read(&mut self, _: &str) -> Result<Option<String>, String> {
        panic!("History must not read secrets")
    }
    async fn write(&mut self, _: &str, _: &str, _: &str) -> Result<(), String> {
        panic!("History must not write secrets")
    }
    async fn remove(&mut self, _: &str) -> Result<(), String> {
        panic!("History must not remove secrets")
    }
}

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
impl Fixture {
    fn environment(&self) -> Environment {
        Environment::from([(
            "AUTOROUTER_CONFIG".into(),
            self.0.join("config.json").into_os_string(),
        )])
    }
    fn context<'a>(&'a self, env: &'a Environment) -> ConfigContext<'a> {
        ConfigContext {
            env,
            cwd: &self.0,
            home: &self.0,
        }
    }
    async fn configure(&self, env: &Environment, extra: Value) {
        let mut settings = json!({"AUTOROUTER_SESSION_LOG_DIR":self.0.join("logs")});
        for (key, value) in extra.as_object().unwrap() {
            settings[key] = value.clone();
        }
        save_user_config(
            &settings,
            &self.context(env),
            &SaveOptions::default(),
            &mut NoSecrets,
        )
        .await
        .unwrap();
    }
    fn write(&self, rows: &[Value]) {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(self.0.join("logs").join(format!("{ID}.jsonl")))
            .unwrap();
        for row in rows {
            writeln!(file, "{row}").unwrap();
        }
    }
    async fn command(
        &self,
        env: &Environment,
        args: &[&str],
    ) -> crate::configuration::CommandOutput {
        tokio::time::timeout(
            Duration::from_secs(5),
            command(
                &args.iter().map(|arg| (*arg).into()).collect::<Vec<_>>(),
                &self.context(env),
                &mut NoSecrets,
            ),
        )
        .await
        .expect("bounded history command")
        .unwrap()
    }
    async fn show(&self) -> Value {
        tokio::time::timeout(
            Duration::from_secs(5),
            read_session_history(
                self.0.join("logs"),
                HistoryOptions {
                    id: Some(ID.into()),
                    ..HistoryOptions::default()
                },
            ),
        )
        .await
        .expect("bounded history read")
        .unwrap()
    }
}

#[tokio::test]
async fn original_history_list_and_show_retain_pricing_provenance_in_text_and_json() {
    let f = Fixture::new();
    let env = f.environment();
    f.configure(&env, json!({})).await;
    let mut unversioned = outcome("unversioned");
    // Undefined own properties are omitted by the original JSON.stringify.
    unversioned
        .as_object_mut()
        .unwrap()
        .remove("pricing_version");
    f.write(&[
        decision("current"),
        outcome("current"),
        changed(outcome("future"), json!({"pricing_version":"future.9"})),
        unversioned,
    ]);
    let config_before = fs::read(f.0.join("config.json")).unwrap();
    for operation in [vec!["list"], vec!["show", ID]] {
        let text = f.command(&env, &operation).await;
        assert!(text.success);
        assert!(text.lines.iter().any(|line| line.contains(&format!(
            "Recorded pricing versions (mixed): {PRICING_VERSION}, future.9"
        )))); // history#3:assert-1 (two operations)
        assert!(
            text.lines
                .join("\n")
                .contains("1 outcomes without a recorded version")
        ); // history#3:assert-2
        assert!(
            text.lines
                .join("\n")
                .contains("unknown_pricing_version (2)")
        ); // history#3:assert-3
        assert!(
            text.lines
                .iter()
                .any(|line| line.contains(&format!("reviewed {PRICING_DATE}: {PRICING_SOURCE}")))
        ); // history#3:assert-4
        let mut json_args = operation;
        json_args.push("--json");
        let output = f.command(&env, &json_args).await;
        assert!(output.success);
        assert_eq!(output.lines.len(), 1);
        let report: Value = serde_json::from_str(&output.lines[0]).unwrap();
        let summary = report.get("summary").unwrap_or(&report["sessions"][0]);
        assert_eq!(
            summary["pricing_versions"],
            json!([PRICING_VERSION, "future.9"])
        ); // history#3:assert-5
        assert_eq!(summary["mixed_pricing_versions"], true); // history#3:assert-6
        assert_eq!(summary["unversioned_outcomes"], 1); // history#3:assert-7
        assert_eq!(
            summary["pricing_facts"],
            json!([{"version":PRICING_VERSION,"date":PRICING_DATE,"source":PRICING_SOURCE}])
        ); // history#3:assert-8
        assert_eq!(summary["savings"]["priced_requests"], 1); // history#3:assert-9
    }
    assert_eq!(fs::read(f.0.join("config.json")).unwrap(), config_before);
}

#[tokio::test]
async fn original_history_fallback_denominator_uses_selections_then_outcome_only_zero() {
    let f = Fixture::new();
    let env = f.environment();
    f.configure(&env, json!({})).await;
    f.write(&[
        changed(
            decision("legacy-pending"),
            json!({"schema_version":1,"source":"fallback"}),
        ),
        changed(decision("fallback-completed"), json!({"source":"fallback"})),
        outcome("fallback-completed"),
        decision("evaluator-success"),
        changed(
            outcome("evaluator-success"),
            json!({"status":"error","http_status":429,"completion_confirmed":false}),
        ),
        changed(
            outcome("unselected-http-error"),
            json!({"status":"error","http_status":500,"completion_confirmed":false}),
        ),
    ]);
    let report = f.show().await;
    assert_eq!(report["summary"]["fallbacks"], 2); // history#4:assert-1
    assert_eq!(report["summary"]["decisions"], 3); // history#4:assert-2
    assert_eq!(report["summary"]["failed"], 2); // history#4:assert-3
    assert_eq!(report["summary"]["fallback_rate"].as_f64(), Some(2.0 / 3.0)); // history#4:assert-4
    let text = f.command(&env, &["show", ID]).await;
    assert!(text.success);
    assert!(
        text.lines
            .join("\n")
            .contains("Fallbacks: 2 of 3 selections (66.7%)")
    ); // history#4:assert-5
    f.write(&[changed(
        outcome("only-outcome"),
        json!({"pricing_version":"future.9","status":"error","completion_confirmed":false}),
    )]);
    let report = f.show().await;
    assert_eq!(report["summary"]["fallback_rate"].as_f64(), Some(0.0)); // history#4:assert-6
    assert_eq!(report["summary"]["fallbacks"], 0); // history#4:assert-7
    assert_eq!(report["summary"]["pricing_facts"], json!([])); // history#4:assert-8
}

#[tokio::test]
async fn original_history_command_sanitizes_controls_and_preserves_bounded_json_excerpts() {
    let f = Fixture::new();
    let env = f.environment();
    f.configure(
        &env,
        json!({"AUTOROUTER_JEV_URL":"invalid-inactive-settings","AUTOROUTER_EVALUATOR":"ollama"}),
    )
    .await;
    f.write(&[changed(decision("prompt"), json!({"prompt_excerpt":"\u{1b}[31m\u{202e}SECRET\n😀".repeat(100),"prompt_truncated":true}))]);
    let text = f.command(&env, &["show", ID]).await;
    assert!(text.success); // history#6:assert-1
    assert!(text.lines.iter().any(|line| line == &format!("ID: {ID}"))); // history#6:assert-2
    assert!(!text.lines.join("\n").contains('\u{1b}')); // history#6:assert-3
    assert!(!text.lines.join("\n").contains('\u{202e}')); // history#6:assert-4
    assert!(text.lines.join("\n").contains("0 confirmed completed")); // history#6:assert-5
    let output = f.command(&env, &["show", ID, "--json"]).await;
    assert!(output.success);
    assert_eq!(output.lines.len(), 1); // history#6:assert-6
    let report: Value = serde_json::from_str(&output.lines[0]).unwrap();
    let excerpt = report["records"][0]["prompt_excerpt"].as_str().unwrap();
    assert_eq!(excerpt.chars().count(), 500); // history#6:assert-7
    assert!(excerpt.contains('\u{1b}')); // history#6:assert-8
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        assert_eq!(
            fs::symlink_metadata(autorouter_runtime::policy::default_policy_path())
                .unwrap_err()
                .kind(),
            ErrorKind::NotFound,
            "Synthetic history command requires absent system policy"
        );
        let suffix = autorouter_runtime::random_hex::<12>().unwrap();
        let path = std::env::temp_dir().join(format!("autorouter-session-inspection-{suffix}"));
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        DirBuilder::new()
            .mode(0o700)
            .create(path.join("logs"))
            .unwrap();
        Self(path)
    }
    fn entries(&self) -> Vec<std::ffi::OsString> {
        let mut result = fs::read_dir(&self.0)
            .unwrap()
            .map(|row| row.unwrap().file_name())
            .collect::<Vec<_>>();
        result.sort();
        result
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn disabled_and_missing_history_inspection_creates_no_files_or_credentials() {
    let fixture = Fixture::new();
    let path = fixture.0.join("config.json");
    let env = Environment::from([("AUTOROUTER_CONFIG".into(), path.clone().into_os_string())]);
    let context = ConfigContext {
        env: &env,
        cwd: &fixture.0,
        home: &fixture.0,
    };
    let mut keychain = NoSecrets;
    let list = tokio::time::timeout(
        Duration::from_secs(2),
        command(&["list".into(), "--json".into()], &context, &mut keychain),
    )
    .await
    .expect("History list deadline")
    .unwrap();
    assert!(list.success);
    assert_eq!(list.lines.len(), 1);
    assert!(list.lines[0].len() < 4096);
    let report: Value = serde_json::from_str(&list.lines[0]).unwrap();
    assert_eq!(report["logging_enabled"], false);
    assert_eq!(report["sessions"], json!([]));
    let show = tokio::time::timeout(
        Duration::from_secs(2),
        command(
            &[
                "show".into(),
                "autorouter-session-20261005T120000Z-run-a".into(),
            ],
            &context,
            &mut keychain,
        ),
    )
    .await
    .expect("History show deadline")
    .unwrap();
    assert!(!show.success);
    assert_eq!(
        show.lines,
        ["Session logging is disabled. Set AUTOROUTER_SESSION_LOG_DIR to record future sessions."]
    );
    assert_eq!(fs::read_dir(fixture.0.join("logs")).unwrap().count(), 0);
    assert_eq!(fixture.entries(), ["logs"]);

    let missing = fixture.0.join("not-created");
    save_user_config(
        &json!({"AUTOROUTER_SESSION_LOG_DIR":missing}),
        &context,
        &SaveOptions::default(),
        &mut keychain,
    )
    .await
    .unwrap();
    let before = fs::read(&path).unwrap();
    let list = tokio::time::timeout(
        Duration::from_secs(2),
        command(&["list".into(), "--json".into()], &context, &mut keychain),
    )
    .await
    .expect("Missing history list deadline")
    .unwrap();
    assert!(list.success);
    assert_eq!(list.lines.len(), 1);
    assert!(list.lines[0].len() < 8192);
    let report: Value = serde_json::from_str(&list.lines[0]).unwrap();
    assert_eq!(report["coverage"]["directory_missing"], true);
    assert_eq!(report["sessions"], json!([]));
    assert_eq!(report["logging_enabled"], true);
    let absent = fs::metadata(&missing).unwrap_err();
    assert_eq!(absent.kind(), ErrorKind::NotFound);
    assert_eq!(absent.raw_os_error(), Some(nix::libc::ENOENT));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fixture.entries(), ["config.json", "logs"]);
    assert_eq!(fs::read_dir(fixture.0.join("logs")).unwrap().count(), 0);
    for (p, mode) in [(&fixture.0, 0o700), (&path, 0o600)] {
        let metadata = fs::symlink_metadata(p).unwrap();
        assert!(!metadata.file_type().is_symlink());
        assert_eq!(metadata.mode() & 0o777, mode);
        assert_eq!(metadata.uid(), nix::unistd::geteuid().as_raw());
    }
}

#[test]
fn history_json_float_tokens_follow_source_spelling() {
    let cases: &[(f64, &str)] = &[
        (0.0, "0"),
        (-0.0, "0"),
        (1.0, "1"),
        (-1.0, "-1"),
        (12.0, "12"),
        (12.5, "12.5"),
        (-12.5, "-12.5"),
        (1e-6, "0.000001"),
        (1e-7, "1e-7"),
        (-1e-7, "-1e-7"),
        (1e20, "100000000000000000000"),
        (1e21, "1e+21"),
        (f64::MAX, "1.7976931348623157e+308"),
        (f64::from_bits(1), "5e-324"),
        (2.2250738585072014e-308, "2.2250738585072014e-308"),
        (f64::from_bits(0x4340000000000000), "9007199254740992"),
        (4.708356024711512e18, "4708356024711512000"),
    ];
    for &(value, expected) in cases {
        let report = json!({"value":value});
        assert_eq!(
            super::render_history_json(&report).unwrap(),
            format!("{{\n  \"value\": {expected}\n}}"),
            "f64 bits={:016x}",
            value.to_bits()
        );
    }
}

#[test]
fn history_json_keeps_exact_integer_variants_and_pretty_structure() {
    // These are native integer-preservation guards, not JS arbitrary-integer claims.
    let cases = [
        (json!(9_007_199_254_740_991u64), "9007199254740991"),
        (json!(9_007_199_254_740_992u64), "9007199254740992"),
        (json!(9_007_199_254_740_993u64), "9007199254740993"),
        (json!(i64::MAX), "9223372036854775807"),
        (json!(u64::MAX), "18446744073709551615"),
        (json!(i64::MIN), "-9223372036854775808"),
    ];
    for (value, expected) in cases {
        assert_eq!(
            super::render_history_json(&json!({"value":value})).unwrap(),
            format!("{{\n  \"value\": {expected}\n}}")
        );
    }
    let report = json!({"text":"2.0 12.0 1e+21 -0 é😀\n","empty_array":[],"empty_object":{},
        "nested":[1.0,{"zero":-0.0,"flag":true,"nothing":null}]});
    assert_eq!(
        super::render_history_json(&report).unwrap(),
        r#"{
  "text": "2.0 12.0 1e+21 -0 é😀\n",
  "empty_array": [],
  "empty_object": {},
  "nested": [
    1,
    {
      "zero": 0,
      "flag": true,
      "nothing": null
    }
  ]
}"#
    );
}

// Synthetic caller-owned metadata; no huge/sparse files or filesystem I/O.
// Source ordinary Stats.size is Number; the native parser API keeps exact u64.
const HISTORY_SIZE_LOG_ID: &str = "autorouter-session-history-precision";
const HISTORY_SIZE_ROW: &[u8] = br#"{"schema_version":2,"event":"decision","timestamp":"2026-10-18T00:00:00.000Z","request_id":"precision-row","session_id":"synthetic-history-precision","requested_model":"claude-opus-5-5","selected_model":"claude-sonnet-5-5"}
"#;
struct HistorySizeCase {
    id: &'static str,
    size: u64,
    source_token: &'static str,
    bits: u64,
}
const HISTORY_SIZE_CASES: &[HistorySizeCase] = &[
    HistorySizeCase {
        id: "small-actual-length",
        size: 224u64,
        source_token: "224",
        bits: 0x406c000000000000,
    },
    HistorySizeCase {
        id: "safe-max",
        size: 9007199254740991u64,
        source_token: "9007199254740991",
        bits: 0x433fffffffffffff,
    },
    HistorySizeCase {
        id: "two53",
        size: 9007199254740992u64,
        source_token: "9007199254740992",
        bits: 0x4340000000000000,
    },
    HistorySizeCase {
        id: "two53-plus-one",
        size: 9007199254740993u64,
        source_token: "9007199254740992",
        bits: 0x4340000000000000,
    },
    HistorySizeCase {
        id: "two53-plus-two",
        size: 9007199254740994u64,
        source_token: "9007199254740994",
        bits: 0x4340000000000001,
    },
    HistorySizeCase {
        id: "two53-plus-three",
        size: 9007199254740995u64,
        source_token: "9007199254740996",
        bits: 0x4340000000000002,
    },
    HistorySizeCase {
        id: "two54-minus-one",
        size: 18014398509481983u64,
        source_token: "18014398509481984",
        bits: 0x4350000000000000,
    },
    HistorySizeCase {
        id: "shortest-large-exact-binary",
        size: 4708356024711512064u64,
        source_token: "4708356024711512000",
        bits: 0x43d055dc36f24000,
    },
    HistorySizeCase {
        id: "signed64-max",
        size: 9223372036854775807u64,
        source_token: "9223372036854776000",
        bits: 0x43e0000000000000,
    },
    HistorySizeCase {
        id: "u64-max-api-only",
        size: 18446744073709551615u64,
        source_token: "18446744073709552000",
        bits: 0x43f0000000000000,
    },
];

// Literal full Source/core report oracle for this one declared valid record.
// Only metadata size/partial change; no actual parser output supplies expectations.
fn history_size_expected_parse(file_bytes: Value, partial: bool) -> Value {
    // A fixed literal avoids json! macro recursion without raising crate limits.
    let mut expected: Value = serde_json::from_str(
        r#"{
        "summary": {
            "id": "autorouter-session-history-precision",
            "session_id": "synthetic-history-precision",
            "started_at": "2026-10-18T00:00:00.000Z",
            "updated_at": "2026-10-18T00:00:00.000Z",
            "requests": 1, "decisions": 1, "outcomes": 0,
            "completed": 0, "failed": 0, "cancelled": 0,
            "pending": 1, "unconfirmed": 0, "outcome_only": 0,
            "selected_models": {"claude-sonnet-5-5": 1},
            "confirmed_models": {}, "sources": {}, "routing_reasons": {},
            "classifier_errors": {}, "fallbacks": 0, "fallback_rate": 0,
            "decision_latency_ms": {"samples": 0},
            "total_latency_ms": {"samples": 0},
            "mixed_baselines": false, "baseline_models": [],
            "mixed_pricing_versions": false, "pricing_versions": [],
            "pricing_facts": [], "unversioned_outcomes": 0,
            "savings": {
                "basis": "API-equivalent estimate, not subscription charges",
                "actual_usd": 0, "baseline_usd": 0, "saved_usd": 0,
                "percent": 0, "priced_requests": 0, "unpriced_requests": 1,
                "unpriced_reasons": {"missing_outcome": 1}
            },
            "coverage": {
                "bytes_read": 224, "file_bytes": 224, "lines_read": 1,
                "invalid_records": 0, "oversized_lines": 0,
                "mixed_session_records": 0, "truncated": false,
                "incomplete_tail": false, "partial": false,
                "legacy_decisions": 0, "duplicate_records": 0,
                "conflicting_requests": 0
            }
        },
        "records": [{
            "schema_version": 2, "event": "decision",
            "timestamp": "2026-10-18T00:00:00.000Z",
            "request_id": "precision-row", "session_id": "synthetic-history-precision",
            "requested_model": "claude-opus-5-5", "selected_model": "claude-sonnet-5-5"
        }]
    }"#,
    )
    .unwrap();
    expected["summary"]["coverage"]["file_bytes"] = file_bytes;
    expected["summary"]["coverage"]["truncated"] = json!(partial);
    expected["summary"]["coverage"]["partial"] = json!(partial);
    expected
}

fn history_size_report(parsed: &Value, limits: &Value, show: bool, partial: bool) -> Value {
    if show {
        json!({"schema_version":1,"type":"session_history",
            "summary":parsed["summary"],"records":parsed["records"],"limits":limits})
    } else {
        json!({"schema_version":1,"type":"session_history",
            "sessions":[parsed["summary"]],"limits":limits,
            "coverage":{"partial":partial,"directory_entries":1,"matching_files":1,
                "skipped_files":0,"unreadable_files":0,"bytes_read":224,
                "directory_scan_truncated":false,"byte_limit_reached":false}})
    }
}

fn history_size_token(text: &str, show: bool) -> String {
    use serde_json::value::RawValue;
    use std::collections::BTreeMap;
    type RawObject = BTreeMap<String, Box<RawValue>>;
    let report: RawObject = serde_json::from_str(text).unwrap();
    let summary: RawObject = if show {
        serde_json::from_str(report["summary"].get()).unwrap()
    } else {
        let sessions: Vec<Box<RawValue>> = serde_json::from_str(report["sessions"].get()).unwrap();
        assert_eq!(sessions.len(), 1);
        serde_json::from_str(sessions[0].get()).unwrap()
    };
    let coverage: RawObject = serde_json::from_str(summary["coverage"].get()).unwrap();
    coverage["file_bytes"].get().into()
}

#[test]
fn history_file_byte_metadata_preserves_exact_native_u64_api() {
    use autorouter_core::session_history::{HistoryLimits, parse_session_with_locale};
    assert_eq!(HISTORY_SIZE_ROW.len(), 224);
    let bytes = HISTORY_SIZE_ROW.to_vec();
    let before = bytes.clone();
    for case in HISTORY_SIZE_CASES {
        let parsed = parse_session_with_locale(
            &bytes,
            case.size,
            HISTORY_SIZE_LOG_ID,
            &HistoryLimits::default(),
            "en-US",
        );
        assert_eq!(
            parsed["summary"]["coverage"]["file_bytes"].as_u64(),
            Some(case.size),
            "{}",
            case.id
        );
        assert_eq!(
            parsed["summary"]["coverage"]["file_bytes"]
                .as_f64()
                .unwrap()
                .to_bits(),
            case.bits,
            "{}",
            case.id
        );
        // Cross the unchanged generic writer, then use the same exact parser on
        // both sides. No in-memory Float/integer tolerance is introduced.
        let exact_text = super::render_history_json(&parsed).unwrap();
        let exact: Value = serde_json::from_str(&exact_text).unwrap();
        assert_eq!(
            exact,
            history_size_expected_parse(json!(case.size), case.size > 224),
            "{}",
            case.id
        );
        let raw = history_size_token(&exact_text, true);
        assert_eq!(raw, case.size.to_string(), "{}", case.id);
    }
    assert_eq!(bytes, before);
}

#[test]
fn history_file_byte_metadata_uses_source_tokens_only_in_enabled_reports() {
    use autorouter_core::session_history::{HistoryLimits, parse_session_with_locale};
    let limits = HistoryLimits::default();
    let limits_json = serde_json::to_value(&limits).unwrap();
    let expected_limits = json!({"maxFiles":100,"maxDirectoryEntries":10000,
        "maxFileBytes":4194304,"maxTotalBytes":16777216,"maxLineBytes":16384,
        "maxRecords":5000,"maxLines":10000});
    assert_eq!(limits_json, expected_limits);
    let mut failures = Vec::new();
    let mut observations = 0;
    for case in HISTORY_SIZE_CASES {
        let partial = case.size > 224;
        let parsed = parse_session_with_locale(
            HISTORY_SIZE_ROW,
            case.size,
            HISTORY_SIZE_LOG_ID,
            &limits,
            "en-US",
        );
        assert_eq!(
            parsed["summary"]["coverage"]["file_bytes"].as_u64(),
            Some(case.size)
        );
        let source_size: Value = serde_json::from_str(case.source_token).unwrap();
        let expected_parsed = history_size_expected_parse(source_size, partial);
        for show in [true, false] {
            let operation = if show { "show" } else { "list" };
            let report = history_size_report(&parsed, &limits_json, show, partial);
            let input_before = report.clone();
            let text = super::render_enabled_history_json(report).unwrap();
            assert!(text.is_ascii() && text.len() <= 16384);
            let actual: Value = serde_json::from_str(&text).unwrap();
            let mut expected =
                history_size_report(&expected_parsed, &expected_limits, show, partial);
            expected["logging_enabled"] = json!(true);
            let token = history_size_token(&text, show);
            let full_report_equal = actual == expected;
            eprintln!(
                "history_file_bytes_observation={}",
                json!({
                    "id":case.id,"operation":operation,"native_u64_decimal":parsed["summary"]["coverage"]["file_bytes"].as_u64().unwrap().to_string(),
                    "report_json":text
                })
            );
            observations += 1;
            if token != case.source_token || !full_report_equal {
                failures.push(format!(
                    "{}/{operation}: {token} != {}; full_report_equal={full_report_equal}",
                    case.id, case.source_token
                ));
            }
            assert_eq!(
                parsed["summary"]["coverage"]["file_bytes"].as_u64(),
                Some(case.size)
            );
            assert!(input_before.get("logging_enabled").is_none());
        }
    }
    assert_eq!(observations, 20);
    eprintln!(
        "history_file_bytes_result={}",
        json!({"cases":10,"operations":2,"observations":20,"mismatches":failures.len()})
    );
    assert!(failures.is_empty(), "{}", failures.join("; "));
}

#[test]
fn history_file_byte_projection_preserves_other_integer_paths_and_generic_writer() {
    let size = 9_007_199_254_740_993u64;
    let report = json!({"schema_version":1,"type":"session_history",
        "summary":{"coverage":{"file_bytes":size,"bytes_read":size},"unrelated":size},
        "records":[{"file_bytes":size,"coverage":{"file_bytes":size}}],
        "file_bytes":size,"outside":[{"coverage":{"file_bytes":size}}]});
    let original = report.clone();
    let generic = super::render_history_json(&report).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&generic).unwrap(), original);
    assert_eq!(history_size_token(&generic, true), "9007199254740993");
    let projected = super::render_enabled_history_json(report).unwrap();
    let mut expected = original.clone();
    expected["logging_enabled"] = json!(true);
    expected["summary"]["coverage"]["file_bytes"] = json!(9_007_199_254_740_992u64);
    assert_eq!(serde_json::from_str::<Value>(&projected).unwrap(), expected);
    assert_eq!(history_size_token(&projected, true), "9007199254740992");
    assert_eq!(serde_json::from_str::<Value>(&generic).unwrap(), original);
    // Aggregate list coverage is not per-file summary metadata.
    let list = json!({"sessions":[{"coverage":{"file_bytes":size,"bytes_read":size}},
        {"coverage":{"file_bytes":224}}],"coverage":{"file_bytes":size,"bytes_read":size}});
    let before = list.clone();
    let actual: Value =
        serde_json::from_str(&super::render_enabled_history_json(list).unwrap()).unwrap();
    let mut expected = before.clone();
    expected["logging_enabled"] = json!(true);
    expected["sessions"][0]["coverage"]["file_bytes"] = json!(9_007_199_254_740_992u64);
    assert_eq!(actual, expected);
    assert_eq!(
        serde_json::from_str::<Value>(&super::render_history_json(&before).unwrap()).unwrap(),
        before
    );
}
