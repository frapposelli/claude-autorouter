//! Explicit JSON-config versus OS-environment setup input migration.
//! These are real public admissions/compiler controls, not null/false CLI calls.
#[path = "../../autorouter-core/tests/support/consumer_compiler.rs"]
mod consumer_compiler;
use consumer_compiler::CompileContext;

use autorouter_core::{auth::Environment, config::read_config};
use autorouter_runtime::user_config::environment_json;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::path::Path;

const KEY: &str = "CLAUDE_CODE_STOP_HOOK_BLOCK_CAP";
const ERROR: &str = "CLAUDE_CODE_STOP_HOOK_BLOCK_CAP requires a nonnegative safe integer (0 disables the Stop-hook continuation cap)";

fn save(name: &str, value: &Value) {
    if let Some(directory) = std::env::var_os("AUTOROUTER_ONBOARDING_ENVIRONMENT_REPORT_DIR") {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let bytes = serde_json::to_vec_pretty(value).unwrap();
        assert!(bytes.len() <= 2 * 1024 * 1024);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(Path::new(&directory).join(format!("native-{name}.json")))
            .unwrap();
        file.write_all(&bytes).unwrap();
    }
}

#[test]
fn public_json_configuration_rejects_four_original_raw_null_false_values() {
    let mut rows = Vec::new();
    for evaluator in ["jev", "ollama"] {
        for value in [Value::Null, Value::Bool(false)] {
            let env =
                json!({"AUTOROUTER_EVALUATOR":evaluator,"CLAUDE_CODE_STOP_HOOK_BLOCK_CAP":value});
            let before = env.clone();
            let error = read_config(&env, false, Path::new("/synthetic-owned-cwd"))
                .err()
                .unwrap();
            assert_eq!(error, ERROR);
            assert_eq!(env, before);
            rows.push(
                json!({"evaluator":evaluator,"raw_environment":env,"error":error,
                "entrypoint":"autorouter_core::config::read_config",
                "not_a_native_setup_call":true}),
            );
        }
    }
    assert_eq!(rows.len(), 4);
    save(
        "raw-admission",
        &json!({"rows":rows,"scope":"Public pure JSON configuration admission; no setup/filesystem/provider/Keychain/secret prompt is invoked. Native setup receives a separate typed OS environment."}),
    );
}

#[test]
fn legal_os_strings_and_absence_remain_distinct_from_raw_null_false() {
    let mut rows = Vec::new();
    for evaluator in ["jev", "ollama"] {
        for (raw, expected) in [(None, None), (Some("0002"), Some(2)), (Some("0"), Some(0))] {
            let mut env = Environment::new();
            env.insert("AUTOROUTER_EVALUATOR".into(), evaluator.into());
            if let Some(raw) = raw {
                env.insert(KEY.into(), OsString::from(raw));
            }
            let before = env.clone();
            // Use the same public conversion as setup's ConfigContext. No
            // invalid value is coerced into a string or silently omitted.
            let value = environment_json(&env);
            let configuration =
                read_config(&value, false, Path::new("/synthetic-owned-cwd")).unwrap();
            assert_eq!(configuration.stop_hook_block_cap, expected);
            assert_eq!(env, before);
            assert_eq!(value.get(KEY).is_none(), raw.is_none());
            rows.push(json!({"evaluator":evaluator,"input":value,"accepted_cap":expected}));
        }
        let string_false =
            json!({"AUTOROUTER_EVALUATOR":evaluator,"CLAUDE_CODE_STOP_HOOK_BLOCK_CAP":"false"});
        assert_eq!(
            read_config(&string_false, false, Path::new("/synthetic-owned-cwd"))
                .err()
                .unwrap(),
            ERROR
        );
    }
    assert_eq!(rows.len(), 6);
    save(
        "legal-inputs",
        &json!({"rows":rows,"string_false_rejections":2}),
    );
}

#[test]
fn external_os_environment_consumers_accept_strings_and_reject_value_bool_option() {
    use std::fs;
    use std::io::Read;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    use std::process::Stdio;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    struct Owner(std::process::Child);
    impl Drop for Owner {
        fn drop(&mut self) {
            if matches!(self.0.try_wait(), Ok(None)) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }
    let context =
        CompileContext::for_test(env!("CARGO_PKG_NAME"), "onboarding_environment_contracts");
    let library = context.library();
    let scratch = Scratch(std::env::temp_dir().join(format!(
            "autorouter-environment-consumer-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&scratch.0)
        .unwrap();
    let mut reports = Vec::new();
    for evaluator in ["jev", "ollama"] {
        for (id, operand, expected) in [
            ("absent", None, None),
            ("string", Some("OsString::from(\"0002\")"), None),
            (
                "value_null",
                Some("JsDocument::parse(b\"null\").unwrap().to_serde_observation_lossy()"),
                Some("Value"),
            ),
            (
                "value_false",
                Some("JsDocument::parse(b\"false\").unwrap().to_serde_observation_lossy()"),
                Some("Value"),
            ),
            ("boolean_false", Some("false"), Some("bool")),
            (
                "optional_absence",
                Some("None::<OsString>"),
                Some("Option<OsString>"),
            ),
        ] {
            let name = format!("{evaluator}_{id}");
            let insert = operand
                .map(|operand| format!("env.insert(OsString::from(\"{KEY}\"), {operand});"))
                .unwrap_or_default();
            let source = format!(
                "use autorouter_core::auth::Environment; use autorouter_core::js_json::JsDocument; use std::ffi::OsString; pub fn boundary(){{let mut env=Environment::new();env.insert(OsString::from(\"AUTOROUTER_EVALUATOR\"),OsString::from(\"{evaluator}\"));{insert}let _=env;}}\n"
            );
            let input = scratch.0.join(format!("{name}.rs"));
            fs::write(&input, &source).unwrap();
            let output = scratch.0.join(format!("{name}.jsonl"));
            let diagnostics = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&output)
                .unwrap();
            let mut child = Owner(
                context
                    .command()
                    .arg("--out-dir")
                    .arg(&scratch.0)
                    .arg(&input)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(diagnostics)
                    .spawn()
                    .unwrap(),
            );
            let began = Instant::now();
            let status = loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    break status;
                }
                if began.elapsed() > Duration::from_secs(30)
                    || fs::metadata(&output).unwrap().len() > 1024 * 1024
                {
                    let _ = child.0.kill();
                    let _ = child.0.wait();
                    panic!("Environment consumer exceeded compiler bounds");
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            let mut bytes = Vec::new();
            fs::File::open(&output)
                .unwrap()
                .take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .unwrap();
            assert!(bytes.len() <= 1024 * 1024);
            let diagnostics: Vec<Value> = std::str::from_utf8(&bytes)
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
            match expected {
                None => assert!(
                    status.success(),
                    "legal typed environment must compile: {diagnostics:?}"
                ),
                Some(found) => {
                    assert!(!status.success());
                    assert!(
                        diagnostics.iter().any(|d| d["level"] == "error"
                            && d["code"]["code"] == "E0308"
                            && d["spans"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .any(|s| s["is_primary"] == true
                                    && s["label"]
                                        .as_str()
                                        .is_some_and(|label| label.contains("OsString")
                                            && label.contains(found)))),
                        "specific Environment insertion diagnostic required: {diagnostics:?}"
                    );
                }
            }
            reports.push(json!({"id":name,"source":source,"accepted":status.success(),"expected_found_type":expected,"diagnostics":diagnostics}));
        }
    }
    assert_eq!(reports.len(), 12);
    save(
        "compile-controls",
        &json!({"controls":reports,"core_rlib_sha256":format!("{:x}",Sha256::digest(fs::read(library).unwrap())),"core_rlib_path":library,"provenance":"Local hash-bound consumer API evidence; exact executed-test Cargo dependency context, target, host search path and tokenized rustflags are replayed. This is not source-to-binary release attestation."}),
    );
}
