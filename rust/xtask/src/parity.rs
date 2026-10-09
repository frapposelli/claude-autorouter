use autorouter_core::fixture;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const MAX_INPUT: u64 = 64 * 1024 * 1024;
const MAX_LINE: usize = 8 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(60);

fn identifier(value: Option<&Value>) -> bool {
    value.and_then(Value::as_str).is_some_and(|text| {
        !text.is_empty()
            && text.len() <= 160
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
    })
}

pub fn parse_lines(bytes: &[u8], inputs: bool) -> Result<Vec<Value>, String> {
    if bytes.len() as u64 > MAX_INPUT {
        return Err("Fixture input exceeds byte limit".into());
    }
    let mut rows = Vec::new();
    let mut ids = HashSet::new();
    let content = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    for line in content.split(|byte| *byte == b'\n') {
        if line.is_empty() || line.len() > MAX_LINE {
            return Err("Fixture line is empty or exceeds byte limit".into());
        }
        let row: Value = serde_json::from_slice(line).map_err(|_| "Invalid fixture JSON")?;
        if !identifier(row.get("id")) || !identifier(row.get("op")) {
            return Err("Invalid fixture identity or operation".into());
        }
        if inputs && row.get("input").is_none() {
            return Err("Missing fixture input".into());
        }
        if !inputs && (row.get("result").is_some() == row.get("error").is_some()) {
            return Err("Fixture response needs exactly one result or error".into());
        }
        if !ids.insert(row["id"].as_str().unwrap().to_owned()) {
            return Err("Duplicate fixture identity".into());
        }
        rows.push(row);
    }
    if rows.is_empty() {
        return Err("No fixture cases supplied".into());
    }
    Ok(rows)
}

fn numeric_equal(a: &serde_json::Number, b: &serde_json::Number) -> bool {
    if a == b {
        return true;
    }
    // JSON numbers such as 1 and 1.0 denote the same value. Do not round two
    // distinct large integers through f64 and hide a safe-integer regression.
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
    // Exactly representable integers can have different JSON number encodings.
    // Require an exact round-trip and reject saturating conversion boundaries.
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

fn difference(expected: &Value, actual: &Value, path: &str) -> Option<String> {
    match (expected, actual) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, value) in a {
                let field = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                let Some(other) = b.get(key) else {
                    return Some(field);
                };
                if let Some(found) = difference(value, other, &field) {
                    return Some(found);
                }
            }
            b.keys()
                .find(|key| !a.contains_key(*key))
                .map(|key| format!("{path}/{}", key.replace('~', "~0").replace('/', "~1")))
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                return Some(format!("{path}/length"));
            }
            a.iter()
                .zip(b)
                .enumerate()
                .find_map(|(index, (left, right))| {
                    difference(left, right, &format!("{path}/{index}"))
                })
        }
        (Value::Number(a), Value::Number(b)) if numeric_equal(a, b) => None,
        _ if expected == actual => None,
        _ => Some(path.to_owned()),
    }
}

pub fn compare(cases: &[Value], reference: &[Value], candidate: &[Value]) -> Value {
    let mut failures = Vec::new();
    if reference.len() != cases.len() || candidate.len() != cases.len() {
        failures.push(json!({"reason":"response_count", "expected":cases.len(), "reference":reference.len(), "candidate":candidate.len()}));
    }
    let mut passed = 0;
    for (index, case) in cases.iter().enumerate() {
        let (Some(expected), Some(actual)) = (reference.get(index), candidate.get(index)) else {
            continue;
        };
        if expected["id"] != case["id"]
            || actual["id"] != case["id"]
            || expected["op"] != case["op"]
            || actual["op"] != case["op"]
        {
            failures.push(json!({"id":case["id"], "op":case["op"], "reason":"response_identity"}));
        } else if expected["error"] == "Unknown fixture operation"
            || actual["error"] == "Unknown fixture operation"
        {
            failures.push(
                json!({"id":case["id"], "op":case["op"], "reason":"unimplemented_operation"}),
            );
        } else if let Some(pointer) = captured_difference(case, expected) {
            failures.push(json!({"id":case["id"], "op":case["op"], "reason":"captured_reference_result", "pointer":pointer}));
        } else if let Some(pointer) = difference(expected, actual, "") {
            failures.push(json!({"id":case["id"], "op":case["op"], "reason":"different_result", "pointer":pointer}));
        } else {
            passed += 1;
        }
    }
    json!({"passed":failures.is_empty() && !cases.is_empty(), "cases":cases.len(), "matched":passed, "failures":failures})
}

fn captured_difference(case: &Value, reference: &Value) -> Option<String> {
    let captured = case.get("node_expected")?;
    match reference.get("result") {
        Some(result) if reference.get("error").is_none() => difference(captured, result, "/result"),
        _ => Some("/result".into()),
    }
}

fn serialize_lines(rows: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, row).expect("JSON value is serializable");
        bytes.push(b'\n');
    }
    bytes
}

fn resolve(root: &Path, value: &str) -> PathBuf {
    root.join(value)
}

fn response(case: &Value, root: &Path) -> Value {
    if case["op"] != "response_observer" {
        return fixture::response(case, root);
    }
    match crate::observer_fixture::execute(&case["input"]) {
        Ok(result) => json!({"id":case["id"],"op":case["op"],"result":result}),
        Err(error) => json!({"id":case["id"],"op":case["op"],"error":error}),
    }
}

pub fn run(args: &[String], root: &Path) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("fixture") if args.len() == 1 => {
            let mut bytes = Vec::new();
            std::io::stdin().take(MAX_INPUT + 1).read_to_end(&mut bytes).map_err(|_| "Cannot read fixture stdin")?;
            let cases = parse_lines(&bytes, true)?;
            let result: Vec<_> = cases.iter().map(|case| response(case, root)).collect();
            std::io::stdout().write_all(&serialize_lines(&result)).map_err(|_| "Cannot write fixture results")?;
            Ok(())
        }
        Some("parity") if args.get(1).map(String::as_str) == Some("--all") => all(&args[2..], root),
        Some("parity") => parity(&args[1..], root),
        Some("freeze-reference") if args.len() == 1 => {
            let files = crate::reference::freeze(root, &root.join("artifacts/rust-rewrite/reference"))?;
            println!("{}", json!({"frozen_files":files,"reference":"artifacts/rust-rewrite/reference"}));
            Ok(())
        }
        _ => Err("Usage: cargo xtask parity [--cases PATH] [--report PATH] [--reference-root PATH] [--candidate PATH]\n       cargo xtask fixture < CASES.jsonl\nAll paths are relative to the repository root; candidate executables implement the fixture subcommand.".into()),
    }
}

fn parity(args: &[String], root: &Path) -> Result<(), String> {
    let mut cases_path = root.join("rust/parity/cases/core.jsonl");
    let mut reference_root = root.join("artifacts/rust-rewrite/reference");
    let mut explicit_reference = false;
    let mut candidate_path = None;
    let mut report_path = None;
    let mut index = 0;
    while index < args.len() {
        let value = args.get(index + 1).ok_or("Missing parity option value")?;
        match args[index].as_str() {
            "--cases" => cases_path = resolve(root, value),
            "--reference-root" => {
                reference_root = resolve(root, value);
                explicit_reference = true;
            }
            "--candidate" => candidate_path = Some(resolve(root, value)),
            "--report" => report_path = Some(resolve(root, value)),
            _ => return Err("Unknown parity option".into()),
        }
        index += 2;
    }
    if !explicit_reference {
        crate::reference::freeze(root, &reference_root)?;
    }
    let bytes = crate::process::read_bounded(&cases_path, MAX_INPUT)?;
    let mut cases = parse_lines(&bytes, true)?;
    for case in &mut cases {
        if case["op"] == "read_config" {
            let cwd = case["input"]
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or(".");
            case["input"]["cwd"] = json!(root.join(cwd));
        }
    }
    let execution_bytes = serialize_lines(&cases);
    let mut reference = Command::new("node");
    reference
        .arg(root.join("scripts/rust-reference.mjs"))
        .arg("--root")
        .arg(&reference_root)
        .current_dir(&reference_root);
    let reference_rows = parse_lines(
        &crate::process::capture(&mut reference, &execution_bytes, TIMEOUT)?,
        false,
    )?;
    let candidate_rows = if let Some(path) = candidate_path {
        let mut candidate = Command::new(path);
        candidate.arg("fixture").current_dir(root);
        parse_lines(
            &crate::process::capture(&mut candidate, &execution_bytes, TIMEOUT)?,
            false,
        )?
    } else {
        cases.iter().map(|case| response(case, root)).collect()
    };
    let baseline: Value = serde_json::from_slice(&crate::process::read_bounded(
        &root.join("rust/parity/baseline.json"),
        MAX_INPUT,
    )?)
    .map_err(|_| "Invalid baseline manifest")?;
    let mut report = compare(&cases, &reference_rows, &candidate_rows);
    report["schema_version"] = json!(1);
    report["kind"] = json!("rust_pure_function_differential");
    report["baseline_commit"] = baseline["baseline_commit"].clone();
    report["cases_sha256"] = json!(format!("{:x}", Sha256::digest(&bytes)));
    report["scope"] = json!(
        "Only the supplied pure fixture cases. Gateway, full CLI, lifecycle, privacy, packaging, performance and provider quality remain separate pending gates."
    );
    if let Some(path) = report_path {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| "Cannot create parity report directory")?;
        }
        let mut formatted = serde_json::to_vec_pretty(&report).expect("JSON value is serializable");
        formatted.push(b'\n');
        std::fs::write(path, formatted).map_err(|_| "Cannot write parity report")?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("JSON value is serializable")
    );
    if report["passed"] == true {
        Ok(())
    } else {
        Err("Rust differential parity failed".into())
    }
}

/// Deterministic, provider-free suite. Every invocation owns a new directory;
/// individual reports remain available even when a later family fails.
fn all(args: &[String], root: &Path) -> Result<(), String> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "Invalid clock")?
        .as_nanos();
    let mut output = root.join(format!("artifacts/rust-rewrite/parity-all-{nonce}"));
    let mut reference = root.join("artifacts/rust-rewrite/reference");
    let mut explicit_reference = false;
    let mut candidate = None;
    for pair in args.chunks(2) {
        let value = pair.get(1).ok_or("Missing parity --all option value")?;
        match pair[0].as_str() {
            "--output" => output = root.join(value),
            "--reference-root" => {
                reference = root.join(value);
                explicit_reference = true;
            }
            "--candidate" => candidate = Some(root.join(value)),
            _ => return Err("Unknown parity --all option".into()),
        }
    }
    if !explicit_reference {
        crate::reference::freeze(root, &reference)?;
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(|_| "Cannot create parity parent")?;
    }
    std::fs::create_dir(&output).map_err(|_| "Parity --all output must be a new directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "Cannot protect parity output")?;
    }
    let families = [
        ("core", "cases/core.jsonl", "checked"),
        ("observer", "cases/observer.jsonl", "checked"),
        ("guards-json", "cases/guards-json.jsonl", "checked"),
        (
            "guards-router-json",
            "cases/guards-router-json.jsonl",
            "checked",
        ),
        ("json-exhaustive", "generate-json.mjs", "exhaustive"),
        ("compatibility", "generate-compatibility.mjs", "stdout"),
        ("router", "generate-router.mjs", "stdout"),
        ("validation-json", "generate-validation-json.mjs", "stdout"),
        (
            "redaction-adversarial",
            "generate-redaction.mjs",
            "adversarial",
        ),
        (
            "telemetry-savings",
            "generate-telemetry-savings.mjs",
            "stdout",
        ),
        ("prompt-state", "generate-prompt-state.mjs", "stdout"),
        (
            "evaluation-report",
            "generate-evaluation-report.mjs",
            "stdout",
        ),
        ("status", "generate-status.mjs", "file"),
        ("history", "generate-history.mjs", "file"),
        ("history-locales", "generate-history-locales.mjs", "file"),
        ("config-auth", "generate-config-auth.mjs", "file"),
        ("telemetry-json", "generate-telemetry-json.mjs", "file"),
    ];
    let mut summaries = Vec::new();
    let mut passed = true;
    for (name, source, mode) in families {
        let cases = output.join(format!("{name}.jsonl"));
        let report = output.join(format!("{name}.report.json"));
        let source = root.join("rust/parity").join(source);
        let result = (|| {
            let source_bytes = crate::process::read_bounded(&source, MAX_INPUT)?;
            if mode == "checked" {
                std::fs::write(&cases, &source_bytes).map_err(|_| "Cannot copy checked fixture")?;
            } else {
                let mut command = Command::new("node");
                command.arg(&source).current_dir(root);
                match mode {
                    "file" => {
                        command.arg(&cases);
                    }
                    "exhaustive" => {
                        command.args(["--exhaustive", "--output"]).arg(&cases);
                    }
                    "adversarial" => {
                        command.arg("--adversarial");
                    }
                    _ => {}
                }
                let stdout = crate::process::capture(&mut command, &[], TIMEOUT)?;
                if mode == "stdout" || mode == "adversarial" {
                    std::fs::write(&cases, stdout)
                        .map_err(|_| "Cannot retain generated fixtures")?;
                }
            }
            let mut options = vec![
                "--cases".into(),
                cases.to_string_lossy().into_owned(),
                "--report".into(),
                report.to_string_lossy().into_owned(),
                "--reference-root".into(),
                reference.to_string_lossy().into_owned(),
            ];
            if let Some(candidate) = &candidate {
                options.extend([
                    "--candidate".into(),
                    candidate.to_string_lossy().into_owned(),
                ]);
            }
            let comparison = parity(&options, root);
            let report_bytes = crate::process::read_bounded(&report, MAX_INPUT)?;
            let value: Value = serde_json::from_slice(&report_bytes)
                .map_err(|_| "Invalid generated parity report")?;
            Ok::<_, String>((
                comparison,
                json!({"family":name,"source":source.strip_prefix(root).unwrap_or(&source),"source_sha256":format!("{:x}",Sha256::digest(&source_bytes)),"report_sha256":format!("{:x}",Sha256::digest(&report_bytes)),"cases_sha256":value["cases_sha256"],"cases":value["cases"],"matched":value["matched"],"passed":value["passed"]}),
            ))
        })();
        match result {
            Ok((comparison, summary)) => {
                passed &= comparison.is_ok();
                summaries.push(summary);
            }
            Err(error) => {
                passed = false;
                summaries.push(json!({"family":name,"passed":false,"error":error}));
            }
        }
        let summary = json!({"schema_version":1,"kind":"deterministic_parity_suite","passed":passed && summaries.len()==families.len(),"complete":summaries.len()==families.len(),"families":summaries,"scope":"Checked and generated pure-function/observer fixture parity only. Executable HTTP, native lifecycle, archive installation, platform qualification, timing and live-provider quality are separate gates."});
        std::fs::write(
            output.join("suite.json"),
            serde_json::to_vec_pretty(&summary).unwrap(),
        )
        .map_err(|_| "Cannot retain parity suite report")?;
    }
    println!(
        "{}",
        json!({"parity_suite":output,"passed":passed,"families":summaries.len()})
    );
    if passed {
        Ok(())
    } else {
        Err(
            "Deterministic parity suite failed; retained per-family reports identify failures"
                .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case() -> Value {
        json!({"id":"tier", "op":"target_compatibility", "input":{}})
    }
    fn result(value: Value) -> Value {
        json!({"id":"tier", "op":"target_compatibility", "result":value})
    }

    #[test]
    fn matching_adapter_failures_cannot_replace_a_captured_baseline_result() {
        let mut captured = case();
        captured["node_expected"] = json!({"compatible":true});
        for response in [
            result(json!({"compatible":false})),
            json!({"id":"tier","op":"target_compatibility","error":"synthetic-private-error"}),
        ] {
            let report = compare(
                std::slice::from_ref(&captured),
                std::slice::from_ref(&response),
                std::slice::from_ref(&response),
            );
            assert_eq!(report["passed"], false);
            assert_eq!(report["matched"], 0);
            assert_eq!(report["failures"][0]["reason"], "captured_reference_result");
            assert!(!report.to_string().contains("synthetic-private-error"));
        }
        for value in [json!({"compatible":true}), Value::Null] {
            captured["node_expected"] = value.clone();
            let response = result(value);
            assert_eq!(
                compare(
                    std::slice::from_ref(&captured),
                    std::slice::from_ref(&response),
                    std::slice::from_ref(&response)
                )["passed"],
                true
            );
        }
    }

    #[test]
    fn exact_large_integer_double_encoding_matches_without_rounding_neighbors() {
        let number = |value: Value| value.as_number().unwrap().clone();
        assert!(numeric_equal(
            &number(json!(9_007_199_254_740_992_u64)),
            &number(json!(9_007_199_254_740_992.0_f64))
        ));
        assert!(!numeric_equal(
            &number(json!(9_007_199_254_740_993_u64)),
            &number(json!(9_007_199_254_740_992.0_f64))
        ));
        assert!(!numeric_equal(
            &number(json!(u64::MAX)),
            &number(json!(18_446_744_073_709_551_616.0_f64))
        ));
        assert!(numeric_equal(
            &number(json!(i64::MIN)),
            &number(json!(-9_223_372_036_854_775_808.0_f64))
        ));
    }

    #[test]
    fn mutation_of_model_guard_or_permission_is_detected_without_payloads() {
        let expected = result(
            json!({"model":"claude-opus-5-5","reason":"tool_turn_pinned","permission":"deny"}),
        );
        for (field, value) in [
            ("model", "claude-haiku-4-5"),
            ("reason", "new_task"),
            ("permission", "allow"),
        ] {
            let mut changed = expected.clone();
            changed["result"][field] = json!(value);
            let report = compare(&[case()], std::slice::from_ref(&expected), &[changed]);
            assert_eq!(report["passed"], false);
            assert_eq!(report["failures"][0]["pointer"], format!("/result/{field}"));
            assert!(!report.to_string().contains(value));
        }
    }

    #[test]
    fn omitted_extra_reordered_and_duplicate_cases_cannot_pass() {
        let response = result(json!(true));
        assert_eq!(
            compare(&[case()], std::slice::from_ref(&response), &[])["passed"],
            false
        );
        assert_eq!(
            compare(
                &[case()],
                std::slice::from_ref(&response),
                &[response.clone(), response.clone()]
            )["passed"],
            false
        );
        let mut wrong = response.clone();
        wrong["id"] = json!("another-case");
        assert_eq!(compare(&[case()], &[response], &[wrong])["passed"], false);
        assert!(parse_lines(b"{\"id\":\"a\",\"op\":\"b\",\"input\":null}\n{\"id\":\"a\",\"op\":\"b\",\"input\":null}\n", true).is_err());
        assert!(parse_lines(b"", true).is_err());
    }

    #[test]
    fn missing_null_bytes_and_large_integer_differences_remain_visible() {
        assert!(difference(&json!({}), &json!({"value":null}), "").is_some());
        assert!(difference(&json!("line\r\n"), &json!("line\n"), "").is_some());
        assert!(
            difference(
                &json!(9_007_199_254_740_992_u64),
                &json!(9_007_199_254_740_993_u64),
                ""
            )
            .is_some()
        );
        assert!(difference(&json!(1), &json!(1.0), "").is_none());
    }

    #[test]
    fn matching_unimplemented_operations_are_not_functional_parity() {
        let row =
            json!({"id":"tier","op":"target_compatibility","error":"Unknown fixture operation"});
        assert_eq!(
            compare(
                &[case()],
                std::slice::from_ref(&row),
                std::slice::from_ref(&row)
            )["passed"],
            false
        );
    }

    #[test]
    fn malformed_or_ambiguous_envelopes_are_rejected() {
        for line in [
            b"{}".as_slice(),
            b"{\"id\":\"a\",\"op\":\"b\"}",
            b"{\"id\":\"a\",\"op\":\"b\",\"result\":null,\"error\":\"failed\"}",
        ] {
            assert!(parse_lines(line, false).is_err());
        }
    }
}
