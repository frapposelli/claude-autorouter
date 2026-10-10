//! Complete policy.test.mjs #6/#7 operations through actual config/setup APIs.
//! Only private per-call dependencies replace the fixed policy path and prompt.
use super::{SecretPrompt, SetupDependencies, setup_with_dependencies};
use crate::configuration::command_with_policy_for_test;
use autorouter_core::auth::Environment;
use autorouter_runtime::keychain::Keychain;
use autorouter_runtime::policy::{LoadedPolicy, load_policy_at};
use autorouter_runtime::user_config::{
    ConfigContext, LoadOptions, LoadedConfig, SaveOptions, environment_json, load_with_policy,
    save_user_config,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::ffi::OsString;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const CORPUS: &str = include_str!("../../../parity/cases/policy-command-contracts.jsonl");
const CAPTURE: &str = include_str!("../../../parity/cases/policy-command-contracts.capture.json");
const CORPUS_SHA: &str = "583db6243796f84bb540036a6f7e1f62d64ce7a816114cabe936d6ca316c8307";
const CAPTURE_SHA: &str = "309b48464cff27408afe26e730b41e55687566b7ba45e54031fe0651e76a49f6";
const TOKEN: &str = "<fixture-root>";
const POLICY_ERROR: &str =
    "AUTOROUTER_EVALUATOR is not permitted by the AutoRouter organization policy. Allowed: ollama.";

struct NoSecrets;
impl Keychain for NoSecrets {
    fn available(&self) -> bool {
        panic!("Unexpected Keychain availability lookup")
    }
    async fn read(&mut self, _: &str) -> Result<Option<String>, String> {
        panic!("Unexpected Keychain read")
    }
    async fn write(&mut self, _: &str, _: &str, _: &str) -> Result<(), String> {
        panic!("Unexpected Keychain write")
    }
    async fn remove(&mut self, _: &str) -> Result<(), String> {
        panic!("Unexpected Keychain removal")
    }
}
#[derive(Default)]
struct NoPrompt {
    calls: usize,
}
impl SecretPrompt for NoPrompt {
    async fn read(&mut self, _: &str) -> Result<String, String> {
        self.calls += 1;
        Err("Synthetic unexpected prompt".into())
    }
}

fn cases() -> Vec<Value> {
    assert!(CORPUS.len() < 1024 * 1024 && CAPTURE.len() < 1024 * 1024);
    assert_eq!(
        format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
        CORPUS_SHA
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(CAPTURE.as_bytes())),
        CAPTURE_SHA
    );
    let capture: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(capture["static_assertions"], 12);
    assert_eq!(capture["expanded_assertions"], 11);
    assert_eq!(capture["definitions"][1]["assertions"][4]["executions"], 0);
    let cases: Vec<Value> = CORPUS
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(cases.len(), 2);
    assert_eq!(cases[0]["source_test"], "test/policy.test.mjs#6");
    assert_eq!(cases[1]["source_test"], "test/policy.test.mjs#7");
    assert_eq!(cases[0]["operations"].as_array().unwrap().len(), 4);
    assert_eq!(cases[1]["operations"].as_array().unwrap().len(), 5);
    cases
}
fn strings(value: &Value, from: &str, to: &str) -> Value {
    match value {
        Value::String(text) => json!(text.replace(from, to)),
        Value::Array(rows) => Value::Array(rows.iter().map(|row| strings(row, from, to)).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), strings(value, from, to)))
                .collect(),
        ),
        _ => value.clone(),
    }
}
struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new(initial: &Value) -> Self {
        let mut random = [0u8; 12];
        getrandom::fill(&mut random).unwrap();
        let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let root = std::env::temp_dir().join(format!("autorouter-policy-command-{suffix}"));
        DirBuilder::new().mode(0o700).create(&root).unwrap();
        let fixture = Self { root };
        let rows = initial.as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], json!({"path":".","kind":"directory","mode":448}));
        assert_eq!(rows[1], json!({"path":"etc","kind":"directory","mode":493}));
        assert_eq!(rows[2]["path"], "etc/policy.json");
        assert_eq!(rows[2]["kind"], "file");
        assert_eq!(rows[2]["mode"], 420);
        DirBuilder::new()
            .mode(0o755)
            .create(fixture.root.join("etc"))
            .unwrap();
        fs::set_permissions(fixture.root.join("etc"), fs::Permissions::from_mode(0o755)).unwrap();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(fixture.policy_path())
            .unwrap();
        file.write_all(rows[2]["content"].as_str().unwrap().as_bytes())
            .unwrap();
        file.set_permissions(fs::Permissions::from_mode(0o644))
            .unwrap();
        assert_eq!(fixture.snapshot(), *initial);
        fixture
    }
    fn policy_path(&self) -> PathBuf {
        self.root.join("etc/policy.json")
    }
    fn config_path(&self) -> PathBuf {
        self.root.join("config.json")
    }
    fn policy(&self) -> Result<Option<LoadedPolicy>, String> {
        load_policy_at(&self.policy_path(), fs::metadata(&self.root).unwrap().uid())
    }
    fn env(&self) -> Environment {
        Environment::from([(
            "AUTOROUTER_CONFIG".into(),
            self.config_path().into_os_string(),
        )])
    }
    fn context<'a>(&'a self, env: &'a Environment) -> ConfigContext<'a> {
        ConfigContext {
            env,
            cwd: &self.root,
            home: &self.root,
        }
    }
    fn expand(&self, value: &Value) -> Value {
        strings(value, TOKEN, self.root.to_str().unwrap())
    }
    fn normalize(&self, value: &Value) -> Value {
        strings(value, self.root.to_str().unwrap(), TOKEN)
    }
    fn snapshot(&self) -> Value {
        fn visit(root: &Path, relative: &str, depth: usize, rows: &mut Vec<Value>) {
            assert!(depth <= 2 && rows.len() < 16);
            let path = if relative == "." {
                root.to_owned()
            } else {
                root.join(relative)
            };
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(!metadata.file_type().is_symlink());
            assert!(metadata.is_dir() || metadata.is_file());
            let mut row = json!({"path":relative,"kind":if metadata.is_file() {"file"} else {"directory"},"mode":metadata.mode() & 0o777});
            if metadata.is_file() {
                assert!(metadata.len() <= 65536);
                let content = fs::read_to_string(&path).unwrap();
                assert_eq!(content.len() as u64, metadata.len());
                row["content"] = json!(content);
            }
            rows.push(row);
            if metadata.is_dir() {
                let mut names: Vec<_> = fs::read_dir(path)
                    .unwrap()
                    .map(|row| row.unwrap().file_name().into_string().unwrap())
                    .collect();
                names.sort();
                for name in names {
                    visit(
                        root,
                        &if relative == "." {
                            name
                        } else {
                            format!("{relative}/{name}")
                        },
                        depth + 1,
                        rows,
                    );
                }
            }
        }
        let mut rows = Vec::new();
        visit(&self.root, ".", 0, &mut rows);
        self.normalize(&json!(rows))
    }
    fn cleanup(self) {
        fs::remove_dir_all(&self.root).unwrap();
        assert!(!self.root.exists());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn args(value: &Value) -> Vec<OsString> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().into())
        .collect()
}
fn env(value: &Value) -> Environment {
    value
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| (key.into(), value.as_str().unwrap().into()))
        .collect()
}
fn loaded(value: LoadedConfig) -> Value {
    json!({"env":environment_json(&value.env),"values":value.values,"path":value.path,"exists":value.exists,
        "revision":value.revision,"secretStore":value.secret_store,"keychainSecrets":value.keychain_secrets,
        "unavailableSecrets":value.unavailable_secrets,"policy":value.policy,"policyPath":value.policy_path,"policyLocked":value.policy_locked})
}
// Exact finite JavaScript Number comparison; no tolerance or integer rounding.
fn same(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .zip(b.as_f64())
            .is_some_and(|(a, b)| a.is_finite() && b.is_finite() && a == b),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, value)| b.get(key).is_some_and(|other| same(value, other)))
        }
        _ => left == right,
    }
}
fn comparable(mut row: Value, reference: bool) -> Value {
    if row["status"] == "error" && reference {
        assert_eq!(row["error"]["name"], "Error");
        assert_eq!(row["error"]["code"], "AUTOROUTER_CONFIG_ERROR");
        row["error"].as_object_mut().unwrap().remove("name");
        row["error"].as_object_mut().unwrap().remove("code");
    }
    if row["operation"] == "config" && row["input"]["args"][0] == "show" && row["status"] == "ok" {
        let lines = row["output"].as_array().unwrap();
        assert_eq!(lines.len(), 1);
        row["output"] = json!([serde_json::from_str::<Value>(lines[0].as_str().unwrap()).unwrap()]);
    }
    row
}
fn transcripts_match(actual: &[Value], expected: &[Value]) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(a, b)| same(&comparable(a.clone(), false), &comparable(b.clone(), true)))
}
async fn replay(case: &Value) -> Vec<Value> {
    let fixture = Fixture::new(&case["operations"][0]["before"]);
    let mut actual = Vec::new();
    let mut keychain = NoSecrets;
    let mut prompt = NoPrompt::default();
    for expected in case["operations"].as_array().unwrap() {
        let input = fixture.expand(&expected["input"]);
        let environment = env(&input["env"]);
        let context = fixture.context(&environment);
        let mut row = json!({"operation":expected["operation"],"input":expected["input"],"before":fixture.snapshot(),"output":[],"prompts":[],"status":"pending"});
        if expected["operation"] != "save" {
            assert_eq!(input["policy"]["path"], json!(fixture.policy_path()));
            assert_eq!(input["policy"]["trusted_owner"], "fixture_uid");
        }
        let mut output = Vec::new();
        let result: Result<Value, String> = match expected["operation"].as_str().unwrap() {
            "save" => save_user_config(
                &input["values"],
                &context,
                &SaveOptions::default(),
                &mut keychain,
            )
            .await
            .map(|path| json!(path)),
            "load" => {
                let policy = fixture.policy().unwrap();
                load_with_policy(
                    &context,
                    &LoadOptions::default(),
                    &mut keychain,
                    policy.as_ref(),
                )
                .await
                .map(loaded)
            }
            "config" => {
                command_with_policy_for_test(&args(&input["args"]), &context, &mut keychain, || {
                    fixture.policy()
                })
                .await
                .map(|value| {
                    output = value.lines;
                    json!(value.success)
                })
            }
            "setup" => setup_with_dependencies(
                &args(&input["args"]),
                &context,
                &mut keychain,
                &CancellationToken::new(),
                &mut |line| output.push(line),
                cfg!(target_os = "macos"),
                SetupDependencies {
                    prompt: &mut prompt,
                    policy_loader: || fixture.policy(),
                },
            )
            .await
            .map(|()| Value::Null),
            _ => panic!("Unknown finite operation"),
        };
        row["after"] = fixture.snapshot();
        row["output"] = json!(output);
        assert_eq!(prompt.calls, 0);
        match result {
            Ok(value) => {
                row["status"] = json!("ok");
                row["result"] = value;
            }
            Err(error) => {
                row["status"] = json!("error");
                row["error"] = json!({"message":error});
            }
        }
        actual.push(fixture.normalize(&row));
    }
    assert_eq!(fixture.snapshot(), case["final_files"]);
    fixture.cleanup();
    actual
}
async fn bounded_replay(case: &Value) -> Vec<Value> {
    tokio::time::timeout(Duration::from_secs(5), replay(case))
        .await
        .expect("Bounded local policy operations")
}

#[tokio::test]
async fn original_6_policy_report_and_rejected_edit_preserve_file() {
    let case = cases().remove(0);
    let actual = bounded_replay(&case).await;
    assert!(
        transcripts_match(&actual, case["operations"].as_array().unwrap()),
        "Full original #6 transcript differs: {actual:#?}"
    );
    assert_eq!(actual[1]["result"], true, "policy#6:assert-1");
    let report: Value = serde_json::from_str(actual[1]["output"][0].as_str().unwrap()).unwrap();
    assert_eq!(
        report["settings"]["AUTOROUTER_SESSION_LOG_MODE"]["source"], "policy",
        "policy#6:assert-2"
    );
    assert_eq!(
        report["settings"]["AUTOROUTER_SESSION_LOG_MODE"]["value"], "metadata",
        "policy#6:assert-3"
    );
    assert_eq!(
        report["policy_path"],
        format!("{TOKEN}/etc/policy.json"),
        "policy#6:assert-4"
    );
    assert!(
        actual[2]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not permitted"),
        "policy#6:assert-5"
    );
    assert_eq!(actual[2]["before"], actual[2]["after"], "policy#6:assert-6");
    assert_eq!(actual[3]["result"], true, "policy#6:assert-7");
}
#[tokio::test]
async fn original_7_repair_and_setup_rejection_keep_original_choices() {
    let case = cases().remove(1);
    let actual = bounded_replay(&case).await;
    assert!(
        transcripts_match(&actual, case["operations"].as_array().unwrap()),
        "Full original #7 transcript differs: {actual:#?}"
    );
    assert!(
        actual[1]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not permitted"),
        "policy#7:assert-1"
    );
    assert_eq!(actual[2]["result"], true, "policy#7:assert-2");
    assert_eq!(
        actual[3]["result"]["env"]["AUTOROUTER_EVALUATOR"], "ollama",
        "policy#7:assert-3"
    );
    assert!(
        actual[4]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not permitted"),
        "policy#7:assert-4"
    );
    // Original #7:assert-5 is the untaken assert.fail('No prompt') guard.
    assert_eq!(actual[4]["prompts"], json!([]));
    assert_eq!(actual[4]["before"], actual[4]["after"]);
}
#[tokio::test]
async fn forged_transcripts_do_not_pass_full_observation_checks() {
    for case in cases() {
        let actual = bounded_replay(&case).await;
        let expected = case["operations"].as_array().unwrap();
        assert!(transcripts_match(&actual, expected));
        let mut bad = actual.clone();
        bad.pop();
        assert!(!transcripts_match(&bad, expected));
        let mut bad = actual.clone();
        bad.swap(0, 1);
        assert!(!transcripts_match(&bad, expected));
        let mut bad = actual.clone();
        bad[0]["result"] = json!("wrong path");
        assert!(!transcripts_match(&bad, expected));
        let mut bad = actual.clone();
        bad[0]["after"][1]["content"] = json!("forged file bytes");
        assert!(!transcripts_match(&bad, expected));
        let mut bad = actual.clone();
        let error = bad.iter_mut().find(|row| row["status"] == "error").unwrap();
        error["error"]["message"] = json!("wrong error");
        assert!(!transcripts_match(&bad, expected));
        let mut bad = actual.clone();
        bad.last_mut().unwrap()["output"] = json!(["forged output"]);
        assert!(!transcripts_match(&bad, expected));
    }
    assert!(!same(&json!(9001), &json!(9002)));
    assert!(!same(&json!({"present":null}), &json!({})));
}

#[tokio::test]
async fn missing_jev_key_still_rejects_policy_before_prompt() {
    let fixture = Fixture::new(&cases()[1]["operations"][0]["before"]);
    let environment = fixture.env();
    let mut prompt = NoPrompt::default();
    let mut output = Vec::new();
    let before = fixture.snapshot();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        setup_with_dependencies(
            &args(&json!(["--evaluator", "jev", "--force"])),
            &fixture.context(&environment),
            &mut NoSecrets,
            &CancellationToken::new(),
            &mut |line| output.push(line),
            cfg!(target_os = "macos"),
            SetupDependencies {
                prompt: &mut prompt,
                policy_loader: || fixture.policy(),
            },
        ),
    )
    .await
    .unwrap();
    assert_eq!(result, Err(POLICY_ERROR.into()));
    assert_eq!(prompt.calls, 0);
    assert_eq!(fixture.snapshot(), before);
    assert_eq!(
        output,
        [
            "AutoRouter sends bounded prompt excerpts to TypeSafe Jev and complete requests to Anthropic."
        ]
    );
    fixture.cleanup();
}
#[tokio::test]
async fn loader_error_precedes_config_reads_keychain_and_prompt() {
    let fixture = Fixture::new(&cases()[1]["operations"][0]["before"]);
    let environment = fixture.env();
    let count = Cell::new(0);
    // Invalid bytes would cause a different error if config were read first.
    // A valid Keychain document would invoke NoSecrets if policy were delayed.
    for contents in ["NOT JSON", "{\"AUTOROUTER_SECRET_STORE\":\"keychain\"}"] {
        fs::write(fixture.config_path(), contents).unwrap();
        for arguments in [
            json!(["show", "--json"]),
            json!(["set", "AUTOROUTER_PORT", "9001"]),
        ] {
            let result = command_with_policy_for_test(
                &args(&arguments),
                &fixture.context(&environment),
                &mut NoSecrets,
                || {
                    count.set(count.get() + 1);
                    Err("Synthetic policy loader failed".into())
                },
            )
            .await;
            if arguments[0] == "show" {
                let output = result.ok().unwrap();
                assert!(!output.success);
                assert_eq!(output.lines.len(), 1);
                assert_eq!(
                    serde_json::from_str::<Value>(&output.lines[0]).unwrap(),
                    json!({"schema_version":1,"valid":false,"error":"Synthetic policy loader failed"})
                );
            } else {
                assert_eq!(result.err().unwrap(), "Synthetic policy loader failed");
            }
        }
        let mut prompt = NoPrompt::default();
        let mut output = Vec::new();
        let result = setup_with_dependencies(
            &args(&json!(["--evaluator", "jev", "--force"])),
            &fixture.context(&environment),
            &mut NoSecrets,
            &CancellationToken::new(),
            &mut |line| output.push(line),
            cfg!(target_os = "macos"),
            SetupDependencies {
                prompt: &mut prompt,
                policy_loader: || {
                    count.set(count.get() + 1);
                    Err("Synthetic policy loader failed".into())
                },
            },
        )
        .await;
        assert_eq!(result, Err("Synthetic policy loader failed".into()));
        assert_eq!(prompt.calls, 0);
        assert!(output.is_empty());
        assert_eq!(fs::read_to_string(fixture.config_path()).unwrap(), contents);
    }
    assert_eq!(count.get(), 6);
    fixture.cleanup();
}
#[tokio::test]
async fn cancelled_setup_bypasses_policy_loader_and_all_other_dependencies() {
    let fixture = Fixture::new(&cases()[1]["operations"][0]["before"]);
    let environment = fixture.env();
    fs::write(fixture.config_path(), "NOT JSON").unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let mut prompt = NoPrompt::default();
    let mut output = Vec::new();
    let result = setup_with_dependencies(
        &args(&json!(["--evaluator", "jev", "--force"])),
        &fixture.context(&environment),
        &mut NoSecrets,
        &cancellation,
        &mut |line| output.push(line),
        cfg!(target_os = "macos"),
        SetupDependencies {
            prompt: &mut prompt,
            policy_loader: || -> Result<Option<LoadedPolicy>, String> {
                panic!("Cancelled setup must not load policy")
            },
        },
    )
    .await;
    assert_eq!(result, Err("Setup cancelled".into()));
    assert_eq!(prompt.calls, 0);
    assert!(output.is_empty());
    assert_eq!(
        fs::read_to_string(fixture.config_path()).unwrap(),
        "NOT JSON"
    );
    fixture.cleanup();
}
#[tokio::test]
async fn invalid_command_arguments_are_rejected_before_policy_loader() {
    let fixture = Fixture::new(&cases()[1]["operations"][0]["before"]);
    let environment = fixture.env();
    for arguments in [
        json!(["show", "--unknown"]),
        json!(["set", "UNSUPPORTED", "1"]),
        json!(["set", "TYPESAFE_API_KEY", "forbidden-argument"]),
    ] {
        let result = command_with_policy_for_test(
            &args(&arguments),
            &fixture.context(&environment),
            &mut NoSecrets,
            || -> Result<Option<LoadedPolicy>, String> {
                panic!("Invalid arguments must precede policy loading")
            },
        )
        .await;
        assert!(result.is_err());
    }
    assert!(!fixture.config_path().exists());
    fixture.cleanup();
}
