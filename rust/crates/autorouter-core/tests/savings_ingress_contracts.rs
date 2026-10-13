//! Exact savings #8/#17/#18; explicit nonfinite/getter/owned-facts boundaries.
use autorouter_core::{
    js_json::{JsDocument, JsNode},
    savings::{PRICING_DATE, PRICING_SOURCE, PRICING_VERSION, SavingsTracker, pricing_facts},
    telemetry_event::normalization_projection,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};

const CASES: &str = include_str!("../../../parity/cases/savings-ingress-contracts.jsonl");
const CAPTURE: &str = include_str!("../../../parity/cases/savings-ingress-contracts.capture.json");
fn rows(number: usize) -> Vec<Value> {
    assert_eq!(
        format!("{:x}", Sha256::digest(CASES.as_bytes())),
        "3cc2f2ba9351a046d5ae65e7ecb9355bf31b737c049af2a98fdd057336f1c682"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(CAPTURE.as_bytes())),
        "9d8b6ae71aa4dec98560a50b81cff73b5d3d5e873a7aa7621629ef133bf4216b"
    );
    let metadata: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(metadata["expanded_assertions"], 46);
    assert_eq!(metadata["static_assertions"], 12);
    assert_eq!(metadata["cases"], 20);
    CASES
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|r| r["source_test"] == format!("test/savings.test.mjs#{number}"))
        .collect()
}
fn save(name: &str, value: &Value) {
    if let Some(path) = std::env::var_os("AUTOROUTER_SAVINGS_INGRESS_REPORT_DIR") {
        let bytes = serde_json::to_vec_pretty(value).unwrap();
        assert!(bytes.len() < 512 * 1024);
        std::fs::write(Path::new(&path).join(format!("native-{name}.json")), bytes).unwrap();
    }
}
// Same guarded numeric representation comparator as xtask/src/parity.rs.
fn numeric_equal(a: &serde_json::Number, b: &serde_json::Number) -> bool {
    if a == b {
        return true;
    }
    if !a.is_f64() && !b.is_f64() {
        return false;
    }
    let (Some(left), Some(right)) = (a.as_f64(), b.as_f64()) else {
        return false;
    };
    if left != right {
        return false;
    }
    if a.is_f64() && b.is_f64() {
        return true;
    }
    let integer = if a.is_f64() { b } else { a };
    if let Some(value) = integer.as_u64() {
        (0.0..18_446_744_073_709_551_616.0).contains(&left) && left as u64 == value
    } else if let Some(value) = integer.as_i64() {
        (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&left)
            && left as i64 == value
    } else {
        false
    }
}
fn equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => numeric_equal(a, b),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len() && a.iter().all(|(k, a)| b.get(k).is_some_and(|b| equal(a, b)))
        }
        _ => a == b,
    }
}
fn tagged_kind(value: &Value) -> Option<&str> {
    value.get("$js_type").and_then(Value::as_str)
}
fn finite(
    value: &Value,
    path: &mut Vec<String>,
    omitted: &mut Vec<Vec<String>>,
) -> Result<Option<Value>, &'static str> {
    match tagged_kind(value) {
        Some("undefined") => {
            omitted.push(path.clone());
            return Ok(None);
        }
        Some("nonfinite") => return Err("nonfinite"),
        Some("accessor") => return Err("accessor"),
        Some(_) => panic!("unknown source tag"),
        None => {}
    }
    match value {
        Value::Object(fields) => {
            let mut map = serde_json::Map::new();
            for (k, v) in fields {
                path.push(k.clone());
                let item = finite(v, path, omitted)?;
                path.pop();
                if let Some(v) = item {
                    map.insert(k.clone(), v);
                }
            }
            Ok(Some(map.into()))
        }
        Value::Array(items) => {
            let mut out = Vec::new();
            for (i, v) in items.iter().enumerate() {
                path.push(i.to_string());
                out.push(finite(v, path, omitted)?.expect("source has no undefined array slots"));
                path.pop();
            }
            Ok(Some(out.into()))
        }
        _ => Ok(Some(value.clone())),
    }
}
fn raw_nonfinite(value: &Value) -> Option<String> {
    match tagged_kind(value) {
        Some("undefined") => return None,
        Some("nonfinite") => {
            assert_eq!(value["value"], "+Infinity");
            return Some("1e400".into());
        }
        Some("accessor") => panic!("an accessor is not JSON"),
        Some(_) => panic!("unknown tag"),
        None => {}
    }
    Some(match value {
        Value::Object(fields) => format!(
            "{{{}}}",
            fields
                .iter()
                .filter_map(|(key, v)| raw_nonfinite(v)
                    .map(|v| format!("{}:{v}", serde_json::to_string(key).unwrap())))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(|v| raw_nonfinite(v).unwrap())
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => value.to_string(),
    })
}
fn replay(row: &Value) -> (SavingsTracker, Vec<Value>) {
    let mut tracker = SavingsTracker::default();
    assert!(equal(&tracker.snapshot(), &row["initial_snapshot"]));
    let mut records = Vec::new();
    for (index, step) in row["steps"].as_array().unwrap().iter().enumerate() {
        let mut record = json!({"step":index,"operation":step["operation"]});
        match step["operation"].as_str().unwrap() {
            "update" => {
                let mut omitted = Vec::new();
                let result = finite(&step["tagged_event"], &mut Vec::new(), &mut omitted);
                match result {
                    Ok(Some(event)) => {
                        let before = event.clone();
                        tracker.update(&event);
                        assert_eq!(event, before);
                        record["actual_input"] = event;
                        record["undefined_paths_omitted_at_typed_boundary"] = json!(omitted);
                        record["invoked_update"] = json!(true);
                    }
                    Err("nonfinite") => {
                        let raw = raw_nonfinite(&step["tagged_event"]).unwrap();
                        assert!(serde_json::from_str::<Value>(&raw).is_err());
                        assert!(serde_json::Number::from_f64(f64::INFINITY).is_none());
                        let document = JsDocument::parse(raw.as_bytes()).unwrap();
                        let usage = document.get(document.root(), "usage").unwrap();
                        let output = document.get(usage, "output_tokens").unwrap();
                        let Some(JsNode::Number(number)) = document.node(output) else {
                            panic!("nonfinite remains a Number in the authoritative document");
                        };
                        assert!(number.is_infinite() && number.is_sign_positive());
                        let projected = normalization_projection(&document);
                        assert_eq!(projected["usage"]["output_tokens"], "\0");
                        assert!(!projected["usage"]["output_tokens"].is_null());
                        tracker.update(&projected);
                        record["actual_input"] = projected;
                        record["invoked_update"] = json!(true);
                        record["nonfinite_boundary"] = json!({"raw_native_document":raw,"authoritative_number_is_positive_infinity":true,"ordinary_serde_rejected":true,"number_constructor_rejected":true,"existing_schema_projection":"invalid string sentinel; never null"});
                    }
                    Err("accessor") => {
                        assert_eq!(step["tagged_event"]["event"]["$js_type"], "accessor");
                        // No source getter is replaced by an invented native call.
                        // The separate compile control proves this host object
                        // cannot be passed to the public &Value update API.
                        record["invoked_update"] = json!(false);
                        record["accessor_boundary"] = json!({"source":step["tagged_event"],"native":"Value API excludes executable properties; actual compile rejection qualified separately"});
                    }
                    _ => panic!("unsupported source input"),
                }
            }
            "snapshot" => {}
            "clear" => tracker.clear(),
            _ => panic!("unknown operation"),
        }
        let snapshot = tracker.snapshot();
        assert!(
            equal(&snapshot, &step["expected_snapshot"]),
            "{} step{index} actual={snapshot:?} expected={:?}",
            row["id"],
            step["expected_snapshot"]
        );
        record["actual_snapshot"] = snapshot;
        records.push(record);
    }
    (tracker, records)
}
struct Assertions {
    number: usize,
    order: Vec<usize>,
}
impl Assertions {
    fn new(number: usize) -> Self {
        Self {
            number,
            order: Vec::new(),
        }
    }
    fn check(&mut self, id: usize, passed: bool) {
        self.order.push(id);
        assert!(passed, "savings#{}:assert{id}", self.number);
    }
    fn finish(self) {
        let capture: Value = serde_json::from_str(CAPTURE).unwrap();
        let definition = capture["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["number"] == self.number)
            .unwrap();
        let mut counts = BTreeMap::new();
        for n in &self.order {
            *counts.entry(*n).or_insert(0usize) += 1;
        }
        for (i, site) in definition["assertions"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            assert_eq!(
                counts.get(&(i + 1)),
                Some(&(site["executions"].as_u64().unwrap() as usize))
            );
        }
        assert_eq!(
            counts.len(),
            definition["assertions"].as_array().unwrap().len()
        );
        save(
            &format!("{}-assertion-order", self.number),
            &json!({"order":self.order}),
        );
    }
}
#[test]
fn original_08_all_eighteen_absent_malformed_counter_and_contradictory_ttl_variants() {
    let rows = rows(8);
    assert_eq!(rows.len(), 18);
    let mut assertions = Assertions::new(8);
    let mut reports = Vec::new();
    for row in rows {
        let (tracker, records) = replay(&row);
        let current = tracker.snapshot();
        assertions.check(1, current["session-a"]["unpriced_requests"] == 1);
        assertions.check(2, current["session-a"]["actual_usd"].as_f64() == Some(0.0));
        reports.push(json!({"case":row["id"],"records":records}));
    }
    save(
        "8",
        &json!({"cases":reports,"nonfinite_scope":"The original Infinity argument is retained tagged; native typed JSON rejects it and the existing authoritative-document schema guard preserves invalidity via its documented sentinel. This is an explicit ingress adaptation, not a direct nonfinite Value call."}),
    );
    assertions.finish();
}
#[test]
fn original_17_null_unknown_terminal_clear_and_same_request_reuse_with_explicit_getter_boundary() {
    let row = rows(17).pop().unwrap();
    let mut assertions = Assertions::new(17);
    let result = std::panic::catch_unwind(|| replay(&row));
    assertions.check(1, result.is_ok());
    let (tracker, records) = result.unwrap();
    assertions.check(2, records[3]["actual_snapshot"] == json!({}));
    assertions.check(3, records[11]["actual_snapshot"] == json!({}));
    assertions.check(4, tracker.snapshot()["session-a"]["requests"] == 1);
    assert_eq!(
        records[1]["invoked_update"], false,
        "no fake replacement of source getter"
    );
    save(
        "17",
        &json!({"records":records,"source_does_not_throw_boundary":"Native runtime replay executes representable null/unknown terminal inputs; the source getter invocation is excluded by public Value types and proved by actual positive/negative consumer compilation."}),
    );
    assertions.finish();
}
#[test]
fn original_18_complete_dated_table_and_detached_mutations_preserve_internal_price_constants() {
    let row = rows(18).pop().unwrap();
    let mut assertions = Assertions::new(18);
    let original = pricing_facts();
    assert!(equal(&original, &row["pricing_before"]));
    assertions.check(1, original["version"] == PRICING_VERSION);
    assertions.check(2, original["date"] == PRICING_DATE);
    assertions.check(3, original["source"] == PRICING_SOURCE);
    assertions.check(4, original["unit"] == "cents_per_million_tokens");
    assert_eq!(original["currency"], "USD");
    assert_eq!(original["models"].as_object().unwrap().len(), 6);
    let mut detached = original.clone();
    detached["models"]["claude-haiku-4-5-20251001"]["input"] = json!(0);
    assert_eq!(detached["models"]["claude-haiku-4-5-20251001"]["input"], 0);
    assertions.check(5, equal(&pricing_facts(), &row["pricing_after"]));
    detached["models"]["private"] = json!({});
    assert!(detached["models"].get("private").is_some());
    assertions.check(6, equal(&pricing_facts(), &row["pricing_after"]));
    for rates in detached["models"].as_object_mut().unwrap().values_mut() {
        if let Some(fields) = rates.as_object_mut() {
            for value in fields.values_mut() {
                *value = json!(0);
            }
        }
    }
    for key in ["version", "date", "source", "currency", "unit"] {
        detached[key] = json!("synthetic modified copy");
    }
    assert!(equal(&pricing_facts(), &original));
    let mut tracker = SavingsTracker::default();
    for event in [
        json!({"event":"request_start"}),
        json!({"event":"upstream_model","model":"claude-haiku-4-5-20251001"}),
        json!({"event":"upstream_usage","usage":{"input_tokens":1000000,"output_tokens":1000000}}),
        json!({"event":"request_complete"}),
    ] {
        let mut event = event;
        event["request_id"] = json!("immutable-price-request");
        event["session_id"] = json!("immutable-price-session");
        tracker.update(&event);
    }
    let snapshot = tracker.snapshot();
    assert_eq!(
        snapshot["immutable-price-session"]["actual_usd"].as_f64(),
        Some(6.0)
    );
    assert_eq!(
        snapshot["immutable-price-session"]["baseline_usd"].as_f64(),
        Some(24.0)
    );
    assert_eq!(
        snapshot["immutable-price-session"]["pricing_version"],
        PRICING_VERSION
    );
    assert_eq!(
        snapshot["immutable-price-session"]["pricing_date"],
        PRICING_DATE
    );
    assert_eq!(
        snapshot["immutable-price-session"]["pricing_source"],
        PRICING_SOURCE
    );
    save(
        "18",
        &json!({"facts_before":original,"mutated_detached_value":detached,"facts_after":pricing_facts(),"priced_after_detached_mutation":snapshot,"immutable_api_boundary":"Source exports a frozen object and both writes throw TypeError; native exports a fresh owned Value whose writes succeed locally and cannot affect immutable private pricing constants or future facts/estimates. No runtime TypeError equivalence claimed."}),
    );
    assertions.finish();
}

// Actual native consumer compilation establishes the API boundary. An invented
// runtime callback is never substituted for the original throwing JS getter.
#[test]
fn native_consumer_controls_accept_plain_value_and_reject_host_getters_and_private_rates() {
    use std::fs;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let directory = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let mut libraries: Vec<_> = fs::read_dir(&directory)
        .unwrap()
        .map(Result::unwrap)
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("libautorouter_core-") && n.ends_with(".rlib"))
        })
        .filter(|e| e.file_type().unwrap().is_file())
        .collect();
    assert!(!libraries.is_empty() && libraries.len() <= 32);
    libraries.sort_by_key(|e| e.metadata().unwrap().modified().unwrap());
    let library = libraries.last().unwrap().path();
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "autorouter-savings-consumer-{}",
        std::process::id()
    )));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&scratch.0)
        .unwrap();
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if !matches!(self.0.try_wait(), Ok(Some(_))) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }
    let mut reports = Vec::new();
    for (id, source, expected_code) in [
        (
            "plain_value",
            "use autorouter_core::savings::{SavingsTracker,pricing_facts,PRICING_VERSION}; pub fn accept(){let mut tracker=SavingsTracker::default();let facts=pricing_facts();tracker.update(&facts);let _=PRICING_VERSION;}",
            None,
        ),
        (
            "host_getter",
            "use autorouter_core::savings::SavingsTracker; struct GetterEvent{event:fn()->String} pub fn rejected(){let event=GetterEvent{event:||panic!(\"synthetic private getter\")};SavingsTracker::default().update(&event);}",
            Some("E0308"),
        ),
        (
            "private_rates",
            "pub fn rejected(){let _=autorouter_core::savings::HAIKU_45;}",
            Some("E0603"),
        ),
    ] {
        let input = scratch.0.join(format!("{id}.rs"));
        fs::write(&input, source).unwrap();
        let errors = scratch.0.join(format!("{id}.jsonl"));
        let output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&errors)
            .unwrap();
        let mut child = OwnedChild(
            Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
                .args([
                    "--crate-type=lib",
                    "--edition=2024",
                    "--emit=metadata",
                    "--error-format=json",
                ])
                .arg("--extern")
                .arg(format!("autorouter_core={}", library.display()))
                .arg("-L")
                .arg(format!("dependency={}", directory.display()))
                .arg("--out-dir")
                .arg(&scratch.0)
                .arg(&input)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(output)
                .spawn()
                .unwrap(),
        );
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() > Duration::from_secs(30)
                || fs::metadata(&errors).unwrap().len() > 1024 * 1024
            {
                let _ = child.0.kill();
                let _ = child.0.wait();
                panic!("Synthetic native compile control exceeded its deadline or output bound");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let bytes = fs::read(&errors).unwrap();
        assert!(bytes.len() <= 1024 * 1024);
        let diagnostics: Vec<Value> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        match expected_code {
            None => assert!(
                status.success(),
                "positive consumer must compile: {diagnostics:?}"
            ),
            Some(code) => {
                assert!(!status.success());
                assert!(
                    diagnostics
                        .iter()
                        .any(|d| d["level"] == "error" && d["code"]["code"] == code),
                    "exact API diagnostic {code}: {diagnostics:?}"
                );
            }
        }
        reports.push(json!({"id":id,"source":source,"success":status.success(),"expected_code":expected_code,"diagnostics":diagnostics}));
    }
    save(
        "consumer-controls",
        &json!({"controls":reports,"library_file_sha256":format!("{:x}",Sha256::digest(fs::read(library).unwrap())),"limits":{"deadline_seconds":30,"diagnostic_bytes":1024*1024},"migration_scope":"Public update accepts ordinary Value data and does not admit a host getter object. Private immutable Prices constants cannot be accessed by an external consumer; pricing_facts returns independently mutable copies."}),
    );
}
