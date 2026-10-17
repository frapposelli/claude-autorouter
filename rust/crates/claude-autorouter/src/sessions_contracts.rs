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
        let mut random = [0u8; 12];
        getrandom::fill(&mut random).unwrap();
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
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
