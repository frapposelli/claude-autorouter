//! Exact original status-settings #4/#6 file schedules. These tests use owned
//! synthetic files and the ordinary settings API, without touching user config.
use autorouter_runtime::status_settings::add_status_line_settings;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const CASES: &str = include_str!("../../../parity/cases/status-settings-file-contracts.jsonl");
const CAPTURE: &str =
    include_str!("../../../parity/cases/status-settings-file-contracts.capture.json");
const MARKER: &str = "/__synthetic_settings_root__";
const CORPUS_HASH: &str = "587458346495d5811880fb577f9e5f7856139b99696a67e5210ca3b4b28891ae";
static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

fn hash(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}

fn rows() -> Vec<Value> {
    CASES
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "autorouter-settings-native-{}-{}",
            std::process::id(),
            NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        Self(fs::canonicalize(directory).unwrap())
    }
    fn actual(&self, value: &str) -> String {
        value.replace(MARKER, self.0.to_str().unwrap())
    }
    fn normalized(&self, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(text.replace(self.0.to_str().unwrap(), MARKER)),
            Value::Array(values) => {
                Value::Array(values.iter().map(|v| self.normalized(v)).collect())
            }
            Value::Object(values) => Value::Object(
                values
                    .iter()
                    .map(|(k, v)| (k.clone(), self.normalized(v)))
                    .collect(),
            ),
            _ => value.clone(),
        }
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn frozen_original_definitions_and_all_four_source_assertions_are_retained() {
    assert_eq!(hash(CASES), CORPUS_HASH);
    assert_eq!(
        hash(CAPTURE),
        "b3a6b173b2923b2b82fb328944331f4e89bcfa2b7373bca93899bea4ae622a6f"
    );
    let capture: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(capture["corpus_sha256"], CORPUS_HASH);
    assert_eq!(capture["callbacks_executed"], 2);
    assert_eq!(capture["static_assertions"], 4);
    assert_eq!(capture["expanded_assertions"], 4);
    assert_eq!(capture["provider_calls_performed"], false);
    assert_eq!(capture["user_configuration_touched"], false);
    let sources = capture["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0]["path"], "test/status-settings.test.mjs");
    assert_eq!(
        sources[0]["sha256"],
        "ad02fc85d4fcd9790556781438b88ef038f1e6d508f712b68fd26ba2dd1c0f16"
    );
    for source in sources {
        assert_eq!(
            hash(source["source"].as_str().unwrap()),
            source["sha256"].as_str().unwrap()
        );
    }
    let definitions = capture["definitions"].as_array().unwrap();
    let fixtures = rows();
    assert_eq!(definitions.len(), 2);
    assert_eq!(fixtures.len(), 2);
    let expected = [
        (
            4,
            "b49fa2ed767c505380fea6cb0c8a44e4a19bd849af4f5ab922f18eb024bcb1a3",
            "311b97bab61a51aee5f877404d5c05c5b4503ddd2a31c62159dc055d80b688d0",
        ),
        (
            6,
            "2bad70357b126f354b6340d24cf0f7bee87d8417ee23cc95cc7dc51f7af40041",
            "276ae14946d94503ff3668800ed68281c1de9f9a2d5c1e17fbc77aa87ef5c5fd",
        ),
    ];
    for ((definition, row), (number, inventory_hash, call_hash)) in
        definitions.iter().zip(&fixtures).zip(expected)
    {
        assert_eq!(definition["number"], number);
        assert_eq!(definition["definition_sha256"], inventory_hash);
        assert_eq!(definition["ast_call_sha256"], call_hash);
        let statement = definition["statement"].as_str().unwrap();
        assert!(sources[0]["source"].as_str().unwrap().contains(statement));
        assert_eq!(hash(format!("{statement}\n\n")), inventory_hash);
        assert_eq!(hash(statement.strip_suffix(';').unwrap()), call_hash);
        assert_eq!(
            hash(statement),
            definition["statement_sha256"].as_str().unwrap()
        );
        assert_eq!(row["source_test"], definition["id"]);
        assert_eq!(row["source_name"], definition["name"]);
        let assertions = definition["assertions"].as_array().unwrap();
        assert_eq!(assertions.len(), 2);
        let ids: Vec<Value> = assertions
            .iter()
            .map(|assertion| {
                assert_eq!(assertion["executions"], 1);
                assert_eq!(
                    hash(assertion["expression"].as_str().unwrap()),
                    assertion["sha256"].as_str().unwrap()
                );
                assertion["id"].clone()
            })
            .collect();
        assert_eq!(row["assertion_ids"], json!(ids));
    }
}

fn replay(number: u64) -> (Value, Value) {
    let row = rows()
        .into_iter()
        .find(|row| row["source_test"] == format!("test/status-settings.test.mjs#{number}"))
        .unwrap();
    let scratch = Scratch::new();
    let cwd = PathBuf::from(scratch.actual(row["cwd"].as_str().unwrap()));
    let source = PathBuf::from(scratch.actual(row["source_path"].as_str().unwrap()));
    let output = PathBuf::from(scratch.actual(row["output_directory"].as_str().unwrap()));
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::create_dir_all(&output).unwrap();
    let input = row["input_utf8"].as_str().unwrap();
    fs::write(&source, input.as_bytes()).unwrap();
    let args: Vec<String> = row["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| scratch.actual(arg.as_str().unwrap()))
        .collect();
    if number == 4 {
        assert!(
            !Path::new(&args[1]).is_absolute(),
            "Original #4 supplies a relative filename"
        );
        assert_eq!(
            args[1],
            "../temporary/autorouter-settings-FIXTURE/original/config.json"
        );
        assert_eq!(fs::canonicalize(cwd.join(&args[1])).unwrap(), source);
    } else {
        assert!(Path::new(&args[1]).is_absolute());
        assert!(args[1].contains("autorouter-settings-[literal]*?-FIXTURE"));
    }
    let executable = scratch.0.join("bin/autorouter");
    let returned = add_status_line_settings(&args, &output, &cwd, &executable).unwrap();
    assert_eq!(scratch.normalized(&json!(returned)), row["output_args"]);
    let generated: Value = serde_json::from_slice(&fs::read(&returned[1]).unwrap()).unwrap();
    // Original #4 assert-1 and #6 assert-1 compare the complete permission object.
    assert_eq!(
        scratch.normalized(&generated["permissions"]),
        row["generated_permissions"]
    );
    assert_eq!(
        generated["statusLine"],
        json!({
            "type":"command", "command":format!("'{}' statusline", executable.to_str().unwrap()),
            "padding":0, "refreshInterval":1
        })
    );
    let common = json!({"type":generated["statusLine"]["type"], "padding":generated["statusLine"]["padding"], "refreshInterval":generated["statusLine"]["refreshInterval"]});
    assert_eq!(common, row["generated_status_common"]);
    assert_eq!(
        fs::metadata(&returned[1]).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(row["generated_mode"], 0o600);
    // Original #4 assert-2 checks parsed original permissions; full byte identity
    // is an additional preservation check, also applied to the original #6 file.
    let original_after = fs::read(&source).unwrap();
    assert_eq!(original_after, input.as_bytes());
    assert_eq!(
        original_after,
        row["source_utf8_after"].as_str().unwrap().as_bytes()
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&original_after).unwrap(),
        serde_json::from_str::<Value>(input).unwrap()
    );
    (row, scratch.normalized(&generated))
}

#[test]
fn original_relative_file_retains_allow_ask_deny_default_mode_and_unchanged_source() {
    let (row, generated) = replay(4);
    assert_eq!(generated["permissions"]["allow"][2], "Read");
    assert_eq!(generated["permissions"]["allow"][3], "Bash(node --test)");
    assert_eq!(generated["permissions"]["defaultMode"], "dontAsk");
    assert_eq!(
        row["assertion_ids"],
        json!(["status-settings#4:assert-1", "status-settings#4:assert-2"])
    );
}

#[test]
fn original_literal_glob_directory_cannot_broaden_edit_or_read_anchors() {
    let (_, generated) = replay(6);
    // Original #6 assert-2 requires literal bracket/star/question-mark escaping.
    assert!(
        generated["permissions"]["allow"][0]
            .as_str()
            .unwrap()
            .contains("\\[literal\\]\\*\\?")
    );
    assert_eq!(
        generated["permissions"]["deny"].as_array().unwrap().len(),
        1
    );
}
