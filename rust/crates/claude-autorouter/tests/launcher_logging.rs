#![cfg(unix)]
mod support;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use support::{Home, Response, Server, output, quote};
const HAIKU: &str = "claude-haiku-4-5-20251001";
const SONNET: &str = "claude-sonnet-5";
fn payload(text: &str) -> Value {
    json!({"model":SONNET,"max_tokens":64,"stream":true,"system":"PRIVATE_EXECUTOR_SYSTEM","tools":[{"name":"Read","description":"PRIVATE_TOOL_SCHEMA","input_schema":{"type":"object"}}],"messages":[{"role":"user","content":text}]})
}
struct Fixture {
    home: Home,
    jev: Server,
    upstream: Server,
}
impl Fixture {
    fn new() -> Self {
        let home = Home::new();
        home.save(&json!({"AUTOROUTER_EVALUATOR":"jev"}));
        let jev = Server::new(|request| {
            assert_eq!(request.path, "/v1/systemone");
            assert_eq!(request.headers["authorization"], "Bearer synthetic-jev-key");
            Response::json(json!({"answers":{"tier":{"choice":"haiku","confidence":0.99}}}))
        });
        let upstream = Server::new(|request| {
            assert_eq!(request.path, "/v1/messages");
            assert_eq!(request.headers["x-api-key"], "synthetic-upstream-key");
            let model = request.json()["model"].clone();
            Response::sse(format!(
                "event: message_start\ndata: {}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n",
                json!({"type":"message_start","message":{"model":model}})
            ))
        });
        let requests = [
            (
                "session-alpha",
                "shared-agent",
                "alpha-prompt",
                "main",
                payload("LOG_ONLY_ALPHA_TASK"),
            ),
            (
                "session-beta",
                "shared-agent",
                "beta-prompt",
                "main",
                payload(&"😀".repeat(501)),
            ),
            (
                "session-alpha",
                "worker-agent",
                "worker-prompt",
                "subagent",
                payload("PRIVATE_AGENT_TASK"),
            ),
            (
                "session-alpha",
                "shared-agent",
                "review-prompt",
                "auxiliary",
                payload("PRIVATE_AUXILIARY_REVIEW"),
            ),
            (
                "session-alpha",
                "shared-agent",
                "final-prompt",
                "main",
                payload("LOG_ONLY_FINAL_ROW"),
            ),
        ];
        let mut script = String::from(
            "#!/bin/sh\nset -eu\n[ -z \"${TYPESAFE_API_KEY-}${AUTOROUTER_TOKEN-}\" ] || exit 71\n[ \"$ANTHROPIC_API_KEY\" != synthetic-upstream-key ] || exit 72\n[ \"${#ANTHROPIC_API_KEY}\" = 64 ] || exit 73\npids=''\n",
        );
        for (index, (session, agent, prompt, class, body)) in requests.iter().enumerate() {
            let body = home.write(
                &format!("request-{index}.json"),
                serde_json::to_vec(body).unwrap(),
            );
            let response = home.0.join(format!("response-{index}.txt"));
            if index == 4 {
                script.push_str("for pid in $pids; do wait \"$pid\"; done\n");
            }
            script.push_str(&format!("/usr/bin/curl --silent --show-error --fail --max-time 10 -H 'content-type: application/json' -H \"x-api-key: $ANTHROPIC_API_KEY\" -H 'x-claude-code-session-id: {session}' -H 'x-claude-code-agent-id: {agent}' -H 'x-claude-code-prompt-id: {prompt}' -H 'x-claude-code-request-class: {class}' --data-binary @{} \"$ANTHROPIC_BASE_URL/v1/messages\" --output {}",quote(&body),quote(&response)));
            if index < 4 {
                script.push_str(" &\npids=\"$pids $!\"\n");
            } else {
                script.push('\n');
            }
        }
        script.push_str(&format!(
            "/bin/cp \"$AUTOROUTER_STATUS_FILE\" {}\nprintf 'FAKE_CLAUDE_DONE\\n'\n",
            quote(&home.0.join("snapshot-copy.json"))
        ));
        home.claude(&script);
        Self {
            home,
            jev,
            upstream,
        }
    }
    fn run(&self, directory: Option<&Path>, mode: Option<&str>) -> std::process::Output {
        let mut command = self.home.command();
        command
            .arg("claude")
            .env("AUTOROUTER_AUTH_MODE", "api-key")
            .env("AUTOROUTER_CLIENT_PROFILE", "compatible")
            .env("ANTHROPIC_API_KEY", "synthetic-upstream-key")
            .env("TYPESAFE_API_KEY", "synthetic-jev-key")
            .env("AUTOROUTER_UPSTREAM_URL", self.upstream.url())
            .env(
                "AUTOROUTER_JEV_URL",
                format!("{}/v1/systemone", self.jev.url()),
            );
        if let Some(directory) = directory {
            command.env("AUTOROUTER_SESSION_LOG_DIR", directory);
        }
        if let Some(mode) = mode {
            command.env("AUTOROUTER_SESSION_LOG_MODE", mode);
        }
        let output = output(&mut command);
        self.jev.assert_clean();
        self.upstream.assert_clean();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"FAKE_CLAUDE_DONE\n");
        for index in 0..5 {
            assert!(
                fs::read_to_string(self.home.0.join(format!("response-{index}.txt")))
                    .unwrap()
                    .contains("message_stop")
            );
        }
        let snapshot = fs::read_to_string(self.home.0.join("snapshot-copy.json")).unwrap();
        for private in ["LOG_ONLY_", "PRIVATE_", "😀", "prompt_excerpt"] {
            assert!(!snapshot.contains(private));
        }
        assert!(
            !self
                .home
                .names()
                .iter()
                .any(|n| n.starts_with("autorouter-status-"))
        );
        self.jev.assert_clean();
        self.upstream.assert_clean();
        output
    }
}
fn log_files(directory: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().into_string().unwrap(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}
fn records(bytes: &[u8]) -> Vec<Value> {
    assert!(bytes.ends_with(b"\n"));
    std::str::from_utf8(bytes)
        .unwrap()
        .trim_end()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
#[test]
fn opt_in_launcher_logs_private_sessions_and_drains_final_outcomes_before_child_exit() {
    let fixture = Fixture::new();
    let directory = fixture.home.0.join("private logs");
    let first = fixture.run(Some(&directory), Some("prompts"));
    assert!(first.stderr.is_empty());
    assert_eq!(
        fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let original = log_files(&directory);
    assert_eq!(original.len(), 2);
    let mut sessions = BTreeSet::new();
    for (name, bytes) in &original {
        assert!(name.starts_with("autorouter-session-") && name.ends_with(".jsonl"));
        assert!(
            name.strip_suffix(".jsonl")
                .unwrap()
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        );
        assert!(!name.contains("session-alpha") && !name.contains("session-beta"));
        assert_eq!(
            fs::metadata(directory.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let rows = records(bytes);
        let decisions = rows
            .iter()
            .filter(|r| r["event"] == "decision")
            .collect::<Vec<_>>();
        let outcomes = rows
            .iter()
            .filter(|r| r["event"] == "outcome")
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), decisions.len() * 2);
        assert_eq!(outcomes.len(), decisions.len());
        let session = decisions[0]["session_id"].as_str().unwrap();
        sessions.insert(session.to_owned());
        for decision in &decisions {
            assert_eq!(decision["session_id"], session);
            assert_eq!(decision["schema_version"], 2);
            assert!(autorouter_core::telemetry_event::valid_timestamp(
                decision["timestamp"].as_str().unwrap()
            ));
            let id = decision["request_id"].as_str().unwrap();
            assert_eq!(id.len(), 36);
            assert!(
                id.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-')
            );
            assert_eq!(decision["requested_model"], SONNET);
            assert!(
                decision["decision_latency_ms"]
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && n >= 0.0)
            );
            assert_eq!(
                decision["selected_model"],
                if decision["request_class"] == "auxiliary" {
                    SONNET
                } else {
                    HAIKU
                }
            );
            if decision["request_class"] == "subagent" || decision["request_class"] == "auxiliary" {
                assert_eq!(decision["prompt_excerpt"], "");
            }
            let outcome = outcomes
                .iter()
                .find(|row| row["request_id"] == decision["request_id"])
                .unwrap();
            assert_eq!(outcome["status"], "completed");
            assert_eq!(outcome["confirmed_model"], decision["selected_model"]);
            assert_eq!(outcome["session_id"], decision["session_id"]);
            assert!(outcome.get("prompt_excerpt").is_none());
            assert_eq!(outcome["schema_version"], 2);
        }
        if session == "session-alpha" {
            assert_eq!(decisions.len(), 4);
            assert_eq!(
                decisions
                    .iter()
                    .find(|r| r["prompt_id"] == "alpha-prompt")
                    .unwrap()["prompt_excerpt"],
                "LOG_ONLY_ALPHA_TASK"
            );
            assert_eq!(decisions.last().unwrap()["prompt_id"], "final-prompt");
            assert_eq!(
                decisions.last().unwrap()["prompt_excerpt"],
                "LOG_ONLY_FINAL_ROW"
            );
            assert_eq!(
                decisions
                    .iter()
                    .find(|r| r["request_class"] == "auxiliary")
                    .unwrap()["source"],
                "passthrough"
            );
            assert_eq!(
                decisions
                    .iter()
                    .map(|r| r["agent_id"].as_str().unwrap())
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from(["shared-agent", "worker-agent"])
            );
        } else {
            assert_eq!(session, "session-beta");
            assert_eq!(decisions.len(), 1);
            assert_eq!(decisions[0]["prompt_excerpt"], "😀".repeat(500));
            assert_eq!(decisions[0]["prompt_truncated"], true);
        }
        let text = std::str::from_utf8(bytes).unwrap();
        for private in ["PRIVATE_", "synthetic-upstream-key", "synthetic-jev-key"] {
            assert!(!text.contains(private));
        }
    }
    assert_eq!(
        sessions,
        BTreeSet::from(["session-alpha".into(), "session-beta".into()])
    );
    let second = fixture.run(Some(&directory), Some("prompts"));
    assert!(second.stderr.is_empty());
    let after = log_files(&directory);
    assert_eq!(after.len(), 4);
    for (name, bytes) in original {
        assert_eq!(after[&name], bytes);
    }
    assert_eq!(fixture.upstream.calls().len(), 10);
}
#[test]
fn metadata_launcher_history_keeps_correlated_events_without_any_excerpt_fields() {
    let fixture = Fixture::new();
    let directory = fixture.home.0.join("metadata logs");
    let result = fixture.run(Some(&directory), Some("metadata"));
    assert!(result.stderr.is_empty());
    for bytes in log_files(&directory).values() {
        let rows = records(bytes);
        assert!(rows.iter().any(|r| r["event"] == "decision"));
        assert!(rows.iter().any(|r| r["event"] == "outcome"));
        let text = std::str::from_utf8(bytes).unwrap();
        for private in [
            "prompt_excerpt",
            "prompt_truncated",
            "LOG_ONLY_",
            "PRIVATE_",
            "😀",
        ] {
            assert!(!text.contains(private));
        }
    }
}
#[test]
fn unset_logging_and_empty_override_create_no_history_files() {
    let fixture = Fixture::new();
    let before = fixture.home.names();
    let first = fixture.run(None, None);
    assert!(first.stderr.is_empty());
    // The synthetic child writes only these explicit observation files.
    let after = fixture
        .home
        .names()
        .into_iter()
        .filter(|name| !name.starts_with("response-") && name != "snapshot-copy.json")
        .collect::<Vec<_>>();
    assert_eq!(after, before);
    let directory = fixture.home.0.join("must not exist");
    fixture
        .home
        .save(&json!({"AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_SESSION_LOG_DIR":directory}));
    let before = fixture.home.names();
    let second = fixture.run(Some(Path::new("")), None);
    assert!(second.stderr.is_empty());
    assert!(!directory.exists());
    assert_eq!(fixture.home.names(), before);
}
#[test]
fn unavailable_logging_warns_once_preserves_existing_file_and_keeps_forwarding() {
    let fixture = Fixture::new();
    let invalid = fixture
        .home
        .write("PRIVATE_LOG_LOCATION", b"existing unrelated file");
    let result = fixture.run(Some(&invalid), None);
    assert_eq!(result.stderr, b"AutoRouter session logging disabled.\n");
    assert_eq!(fs::read(&invalid).unwrap(), b"existing unrelated file");
    assert_eq!(fixture.upstream.calls().len(), 5);
    for private in [
        "LOG_ONLY_",
        "PRIVATE_",
        "😀",
        fixture.home.0.to_str().unwrap(),
    ] {
        assert!(!String::from_utf8_lossy(&result.stderr).contains(private));
    }
}
#[test]
fn launcher_keeps_default_stderr_empty_and_debug_events_private_while_status_savings_work() {
    let grep = ["/usr/bin/grep", "/bin/grep"]
        .into_iter()
        .find(|path| {
            fs::metadata(path).is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        })
        .expect("Synthetic launcher test requires an executable grep in /usr/bin or /bin");
    let grep = quote(Path::new(grep));
    let home = Home::new();
    home.save(&json!({"AUTOROUTER_EVALUATOR":"jev"}));
    let jev = Server::new(|request| {
        assert_eq!(
            request.headers["authorization"],
            "Bearer private-classifier-key"
        );
        assert_eq!(
            request.json()["state"]["original_task"],
            "private-prompt-content"
        );
        Response::json(json!({"answers":{"tier":{"choice":"sonnet","confidence":0.99}}}))
    });
    let upstream = Server::new(|request| {
        assert_eq!(request.headers["x-api-key"], "private-upstream-key");
        assert_eq!(request.json()["model"], SONNET);
        Response::sse(format!(
            "event: message_start\ndata: {}\n\nevent: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"delta\":{{\"text\":\"private-response-content\"}}}}\n\nevent: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":100}}}}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n",
            json!({"type":"message_start","message":{"model":SONNET,"usage":{"input_tokens":1000,"output_tokens":1}}})
        ))
    });
    let body=home.write("request.json",serde_json::to_vec(&json!({"model":HAIKU,"max_tokens":64,"stream":true,"messages":[{"role":"user","content":"private-prompt-content"}]})).unwrap());
    home.claude(&format!("#!/bin/sh\nset -eu\n[ \"$AUTOROUTER_EVALUATOR\" = jev ] || exit 71\n[ -z \"${{TYPESAFE_API_KEY-}}\" ] || exit 72\n[ \"${{#ANTHROPIC_API_KEY}}\" = 64 ] || exit 73\n[ \"$ANTHROPIC_API_KEY\" != private-upstream-key ] || exit 74\n[ \"$ANTHROPIC_MODEL\" = claude-haiku-4-5-20251001 ] || exit 75\n/usr/bin/curl --silent --show-error --fail --max-time 10 -H 'content-type: application/json' -H \"x-api-key: $ANTHROPIC_API_KEY\" -H 'x-claude-code-session-id: logging-session' -H 'x-claude-code-request-class: main' --data-binary @{} \"$ANTHROPIC_BASE_URL/v1/messages\" --output {}\ni=0\nwhile [ \"$i\" -lt 100 ]; do if {grep} -q '\"phase\":\"ready\"' \"$AUTOROUTER_STATUS_FILE\" && {grep} -q '\"requests\":1' \"$AUTOROUTER_STATUS_FILE\"; then break; fi; i=$((i+1)); /bin/sleep 0.01; done\n/bin/cp \"$AUTOROUTER_STATUS_FILE\" {}\nprintf 'Claude reply: private-response-content\\n'\n",quote(&body),quote(&home.0.join("reply.sse")),quote(&home.0.join("snapshot-copy.json"))));
    for debug in [false, true] {
        let mut command = home.command();
        command
            .arg("claude")
            .env("AUTOROUTER_AUTH_MODE", "api-key")
            .env("ANTHROPIC_API_KEY", "private-upstream-key")
            .env("TYPESAFE_API_KEY", "private-classifier-key")
            .env("AUTOROUTER_UPSTREAM_URL", upstream.url())
            .env("AUTOROUTER_JEV_URL", format!("{}/v1/systemone", jev.url()));
        if debug {
            command.env("AUTOROUTER_DEBUG", "1");
        }
        let result = output(&mut command);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(result.stdout, b"Claude reply: private-response-content\n");
        let stderr = String::from_utf8(result.stderr).unwrap();
        if debug {
            assert!(stderr.contains("AutoRouter listening"));
            for event in ["route", "upstream_response", "upstream_model"] {
                assert!(stderr.contains(&format!("\"event\":\"{event}\"")));
            }
            assert!(stderr.contains(&format!("\"model\":\"{SONNET}\"")));
        } else {
            assert!(stderr.is_empty());
        }
        for secret in [
            "private-upstream-key",
            "private-classifier-key",
            "private-prompt-content",
            "private-response-content",
        ] {
            assert!(!stderr.contains(secret));
        }
        let text = fs::read_to_string(home.0.join("snapshot-copy.json")).unwrap();
        assert!(!text.contains("private-"));
        let snapshot: Value = serde_json::from_str(&text).unwrap();
        let status = &snapshot["sessions"]["logging-session"];
        assert_eq!(status["phase"], "ready");
        assert_eq!(status["actual_model"], SONNET);
        assert_eq!(status["source"], "jev");
        let savings = &snapshot["savings"]["logging-session"];
        assert_eq!(savings["requests"], 1);
        assert_eq!(savings["unpriced_requests"], 0);
        assert_eq!(savings["actual_usd"], 0.003);
        assert_eq!(savings["baseline_usd"], 0.006);
        assert_eq!(savings["saved_usd"], 0.003);
        assert_eq!(savings["percent"].as_f64(), Some(50.0));
        assert!(
            fs::read_to_string(home.0.join("reply.sse"))
                .unwrap()
                .contains("private-response-content")
        );
    }
    assert_eq!(jev.calls().len(), 2);
    assert_eq!(upstream.calls().len(), 2);
}

#[test]
fn history_cli_default_line_bytes_accepts_the_ceiling_and_skips_only_the_larger_row() {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    mod record_contract {
        include!("../../autorouter-core/src/json_compare_macros.rs");
        contract_numeric_equal!(same_number);
        contract_json_equal!(pub(super) same_record_json, a, b => same_number(a, b));
    }

    const ID: &str = "autorouter-session-history-line-boundary";
    const MAIN: &str = r#"{"schema_version":2,"event":"decision","timestamp":"2026-10-18T00:00:00.000Z","request_id":"boundary-main","session_id":"synthetic-history-boundary","requested_model":"claude-opus-5-5","selected_model":"claude-sonnet-5-5","source":"jev","reason":"classified","decision_latency_ms":12,"prompt_excerpt":"Synthetic é😀 history boundary"}"#;
    const SENTINEL: &str = r#"{"schema_version":2,"event":"decision","timestamp":"2026-10-18T00:00:00.000Z","request_id":"boundary-sentinel","session_id":"synthetic-history-boundary","requested_model":"claude-opus-5-5","selected_model":"claude-sonnet-5-5","source":"jev","reason":"classified","decision_latency_ms":12,"prompt_excerpt":"Synthetic sentinel"}"#;
    let main: Value = serde_json::from_str(MAIN).unwrap();
    let sentinel: Value = serde_json::from_str(SENTINEL).unwrap();
    let mut normalized_main = main.clone();
    normalized_main["prompt_truncated"] = json!(false);
    let mut normalized_sentinel = sentinel.clone();
    normalized_sentinel["prompt_truncated"] = json!(false);
    let identity = |path: &Path| {
        let metadata = fs::symlink_metadata(path).unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        (
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.modified().unwrap(),
        )
    };
    for (line_bytes, accepted) in [(16_383, true), (16_384, true), (16_385, false)] {
        let home = Home::new();
        let directory = home.0.join("logs");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        home.save(&json!({"AUTOROUTER_SESSION_LOG_DIR":directory}));
        let config = home.config();
        let config_before = home.saved_bytes();
        let mut bytes = MAIN.as_bytes().to_vec();
        // JSON whitespace counts toward the raw UTF-8 line cap, before normalization.
        bytes.resize(line_bytes, b' ');
        assert_eq!(bytes.len(), line_bytes);
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), main);
        bytes.push(b'\n');
        bytes.extend_from_slice(SENTINEL.as_bytes());
        bytes.push(b'\n');
        let path = directory.join(format!("{ID}.jsonl"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(&bytes).unwrap();
        drop(file);
        let before = [identity(&path), identity(&config)];
        let names_before = home.names();
        let directory_before = fs::symlink_metadata(&directory).unwrap();
        assert!(directory_before.is_dir());
        assert_eq!(directory_before.permissions().mode() & 0o777, 0o700);
        let text = support::success(&output(
            home.command().args(["sessions", "show", ID, "--json"]),
        ));
        let report: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(report["schema_version"], 1);
        assert_eq!(report["type"], "session_history");
        assert_eq!(report["logging_enabled"], true);
        assert_eq!(
            report["limits"],
            json!({"maxFiles":100,"maxDirectoryEntries":10000,
            "maxFileBytes":4194304,"maxTotalBytes":16777216,"maxLineBytes":16384,
            "maxRecords":5000,"maxLines":10000})
        );
        let expected = if accepted {
            json!([normalized_main, normalized_sentinel])
        } else {
            json!([normalized_sentinel])
        };
        assert!(
            record_contract::same_record_json(&report["records"], &expected),
            "raw line bytes={line_bytes}"
        );
        let records = expected.as_array().unwrap().len();
        let summary = &report["summary"];
        assert_eq!(summary["id"], ID);
        assert_eq!(summary["session_id"], "synthetic-history-boundary");
        for field in ["requests", "decisions", "pending"] {
            assert_eq!(summary[field], json!(records));
        }
        assert_eq!(summary["outcomes"], 0);
        assert_eq!(summary["completed"], 0);
        assert_eq!(
            summary["coverage"],
            json!({"bytes_read":bytes.len(),"file_bytes":bytes.len(),
            "lines_read":2,"invalid_records":0,"oversized_lines":usize::from(!accepted),
            "mixed_session_records":0,"truncated":false,"incomplete_tail":false,
            "partial":!accepted,"legacy_decisions":0,"duplicate_records":0,"conflicting_requests":0})
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(home.saved_bytes(), config_before);
        assert_eq!([identity(&path), identity(&config)], before);
        assert_eq!(home.names(), names_before);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        let directory_after = fs::symlink_metadata(&directory).unwrap();
        assert!(directory_after.is_dir());
        assert_eq!(directory_after.permissions().mode() & 0o777, 0o700);
        assert_eq!(
            (directory_after.dev(), directory_after.ino()),
            (directory_before.dev(), directory_before.ino())
        );
        eprintln!(
            "history_line_boundary={}",
            json!({"line_bytes":line_bytes,
            "record_ids":report["records"].as_array().unwrap().iter()
                .map(|row| row["request_id"].clone()).collect::<Vec<_>>(),
            "coverage":summary["coverage"]})
        );
    }
}

// Default row/line boundaries use only local synthetic log inspection. The
// provider Fixture is intentionally not constructed by either control.
fn history_count_row(index: usize) -> String {
    format!(
        r#"{{"schema_version":2,"event":"decision","timestamp":"2026-10-18T00:00:00.000Z","request_id":"row-{index:04}","session_id":"synthetic-history-count","requested_model":"claude-opus-5-5","selected_model":"claude-sonnet-5-5"}}"#
    )
}
#[derive(Default)]
struct HistoryCountExpected {
    lines: usize,
    invalid: usize,
    mixed: usize,
    truncated: bool,
    duplicates: usize,
    requests: usize,
}
fn check_history_count_file(
    label: &str,
    bytes: &[u8],
    expected_rows: Vec<Value>,
    expected: HistoryCountExpected,
) {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    mod record_contract {
        include!("../../autorouter-core/src/json_compare_macros.rs");
        contract_numeric_equal!(same_number);
        contract_json_equal!(pub(super) same_record_json, a, b => same_number(a, b));
    }
    const ID: &str = "autorouter-session-history-count-boundary";
    let home = Home::new();
    let directory = home.0.join("logs");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    home.save(&json!({"AUTOROUTER_SESSION_LOG_DIR":directory}));
    let config_before = home.saved_bytes();
    let path = directory.join(format!("{ID}.jsonl"));
    assert!(bytes.len() < 4 * 1024 * 1024);
    assert!(
        bytes
            .split(|byte| *byte == b'\n')
            .all(|line| line.len() < 16384)
    );
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    file.write_all(bytes).unwrap();
    drop(file);
    let identity = |path: &Path| {
        let meta = fs::symlink_metadata(path).unwrap();
        assert!(meta.is_file());
        assert_eq!(meta.nlink(), 1);
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        (meta.dev(), meta.ino(), meta.len(), meta.modified().unwrap())
    };
    let before = [identity(&path), identity(&home.config())];
    let names = home.names();
    let dir_before = fs::symlink_metadata(&directory).unwrap();
    assert!(dir_before.is_dir());
    assert_eq!(dir_before.permissions().mode() & 0o777, 0o700);
    let result = output(home.command().args(["sessions", "show", ID, "--json"]));
    assert!(
        result.stdout.len() < 2 * 1024 * 1024,
        "fixture output cap: {label}"
    );
    let text = support::success(&result);
    let report: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["type"], "session_history");
    assert_eq!(report["logging_enabled"], true);
    assert_eq!(
        report["limits"],
        json!({"maxFiles":100,"maxDirectoryEntries":10000,
        "maxFileBytes":4194304,"maxTotalBytes":16777216,"maxLineBytes":16384,
        "maxRecords":5000,"maxLines":10000})
    );
    assert!(
        record_contract::same_record_json(&report["records"], &json!(expected_rows)),
        "{label}"
    );
    let summary = &report["summary"];
    assert_eq!(summary["id"], ID);
    assert_eq!(summary["session_id"], "synthetic-history-count");
    for field in ["requests", "decisions", "pending"] {
        assert_eq!(summary[field], json!(expected.requests), "{label}/{field}");
    }
    for field in [
        "outcomes",
        "completed",
        "failed",
        "cancelled",
        "unconfirmed",
        "outcome_only",
    ] {
        assert_eq!(summary[field], 0, "{label}/{field}");
    }
    assert_eq!(
        summary["selected_models"],
        json!({"claude-sonnet-5-5":expected.requests})
    );
    assert_eq!(
        summary["coverage"],
        json!({"bytes_read":bytes.len(),"file_bytes":bytes.len(),
        "lines_read":expected.lines,"invalid_records":expected.invalid,"oversized_lines":0,
        "mixed_session_records":expected.mixed,"truncated":expected.truncated,"incomplete_tail":false,
        "partial":expected.truncated || expected.invalid > 0 || expected.mixed > 0 || expected.duplicates > 0,
        "legacy_decisions":0,"duplicate_records":expected.duplicates,"conflicting_requests":0}),
        "{label}"
    );
    assert_eq!(fs::read(&path).unwrap().as_slice(), bytes);
    assert_eq!(home.saved_bytes(), config_before);
    assert_eq!([identity(&path), identity(&home.config())], before);
    assert_eq!(home.names(), names);
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    let dir_after = fs::symlink_metadata(&directory).unwrap();
    assert!(dir_after.is_dir());
    assert_eq!(dir_after.permissions().mode() & 0o777, 0o700);
    assert_eq!(
        (dir_after.dev(), dir_after.ino()),
        (dir_before.dev(), dir_before.ino())
    );
    eprintln!(
        "history_count_boundary={}",
        json!({"case":label,
        "records":report["records"].as_array().unwrap().len(),"requests":summary["requests"],
        "coverage":summary["coverage"]})
    );
}

#[test]
fn history_cli_default_record_cap_counts_accepted_rows_before_summary_deduplication() {
    for count in [4999, 5000, 5001] {
        let mut bytes = Vec::new();
        for index in 0..count {
            bytes.extend_from_slice(history_count_row(index).as_bytes());
            bytes.push(b'\n');
        }
        let accepted = count.min(5000);
        let rows = (0..accepted)
            .map(|index| serde_json::from_str(&history_count_row(index)).unwrap())
            .collect();
        check_history_count_file(
            &format!("records-{count}"),
            &bytes,
            rows,
            HistoryCountExpected {
                lines: accepted,
                requests: accepted,
                truncated: count > 5000,
                ..Default::default()
            },
        );
    }
    // Rejected lines do not consume accepted-row capacity. Put the foreign row
    // after the first valid row establishes the expected session identity.
    let mut bytes = b"!\n".to_vec();
    bytes.extend_from_slice(history_count_row(0).as_bytes());
    bytes.push(b'\n');
    bytes.extend_from_slice(
        history_count_row(9000)
            .replace("synthetic-history-count", "synthetic-foreign")
            .as_bytes(),
    );
    bytes.push(b'\n');
    for index in 1..5001 {
        bytes.extend_from_slice(history_count_row(index).as_bytes());
        bytes.push(b'\n');
    }
    let rows = (0..5000)
        .map(|index| serde_json::from_str(&history_count_row(index)).unwrap())
        .collect();
    check_history_count_file(
        "records-rejected-prefix",
        &bytes,
        rows,
        HistoryCountExpected {
            lines: 5002,
            invalid: 1,
            mixed: 1,
            requests: 5000,
            truncated: true,
            ..Default::default()
        },
    );
    // All duplicate decisions still occupy accepted record slots, even though
    // summary attribution counts one request. The following unique sentinel is
    // not admitted as a5001st row merely because the summary deduplicates.
    let repeated = history_count_row(0) + "\n";
    let mut bytes = repeated.repeat(5000).into_bytes();
    bytes.extend_from_slice(history_count_row(5000).as_bytes());
    bytes.push(b'\n');
    let row: Value = serde_json::from_str(&history_count_row(0)).unwrap();
    check_history_count_file(
        "records-duplicates",
        &bytes,
        vec![row; 5000],
        HistoryCountExpected {
            lines: 5000,
            requests: 1,
            duplicates: 4999,
            truncated: true,
            ..Default::default()
        },
    );
}

#[test]
fn history_cli_default_line_cap_stops_before_the_next_raw_line_without_record_cap_masking() {
    for count in [9999, 10000, 10001] {
        let mut bytes = (history_count_row(0) + "\n").into_bytes();
        for _ in 0..count - 2 {
            bytes.extend_from_slice(b"!\n");
        }
        bytes.extend_from_slice(history_count_row(1).as_bytes());
        bytes.push(b'\n');
        let accepted = if count > 10000 { 1 } else { 2 };
        let rows = (0..accepted)
            .map(|index| serde_json::from_str(&history_count_row(index)).unwrap())
            .collect();
        check_history_count_file(
            &format!("lines-{count}"),
            &bytes,
            rows,
            HistoryCountExpected {
                lines: count.min(10000),
                invalid: count - 2,
                requests: accepted,
                truncated: count > 10000,
                ..Default::default()
            },
        );
    }
}

// Public CLI numeric tokens are a separate contract from numeric Value equality.
// The synthetic history reader does not construct any provider fixtures.
#[test]
fn history_cli_json_preserves_source_number_tokens_for_strict_consumers() {
    use serde_json::value::RawValue;
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    type RawObject = BTreeMap<String, Box<RawValue>>;
    const ID: &str = "autorouter-session-history-number-tokens";
    const PROMPT: &str = "Synthetic 2.0 12.0 1e+21 -0 é😀 text";
    const CASES: &[(&str, &str, &str)] = &[
        ("positive-zero", "0", "0"),
        ("negative-zero", "-0", "0"),
        ("integral", "12", "12"),
        ("fraction", "12.5", "12.5"),
        ("fixed-small", "0.000001", "0.000001"),
        ("exponent-small", "0.0000001", "1e-7"),
        (
            "fixed-large",
            "100000000000000000000",
            "100000000000000000000",
        ),
        ("exponent-large", "1e21", "1e+21"),
        (
            "max-finite",
            "1.7976931348623157e308",
            "1.7976931348623157e+308",
        ),
        ("min-subnormal", "5e-324", "5e-324"),
        ("rounded-input", "9007199254740993", "9007199254740992"),
        (
            "shortest-large",
            "4.708356024711512e18",
            "4708356024711512000",
        ),
    ];
    let home = Home::new();
    let directory = home.0.join("logs");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    home.save(&json!({"AUTOROUTER_SESSION_LOG_DIR":directory}));
    let config_before = home.saved_bytes();
    let mut bytes = Vec::new();
    let mut expected_rows = Vec::new();
    for (index, (name, input, expected)) in CASES.iter().enumerate() {
        // Keep the original number text, including -0 and a rounded JS input.
        let schema = if index % 2 == 0 { "2" } else { "2e0" };
        let line = format!(
            r#"{{"schema_version":{schema},"event":"decision","timestamp":"2026-10-18T00:00:00.000Z","request_id":"number-{name}","session_id":"synthetic-history-numbers","requested_model":"claude-opus-5-5","selected_model":"claude-sonnet-5-5","source":"jev","reason":"classified","decision_latency_ms":{input},"prompt_excerpt":"{PROMPT}"}}"#
        );
        assert!(line.len() < 16384);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        let mut row: Value = serde_json::from_str(&line).unwrap();
        row["schema_version"] = json!(2);
        row["decision_latency_ms"] = serde_json::from_str(expected).unwrap();
        row["prompt_truncated"] = json!(false);
        expected_rows.push(row);
    }
    assert!(bytes.len() < 4 * 1024 * 1024);
    let path = directory.join(format!("{ID}.jsonl"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    file.write_all(&bytes).unwrap();
    drop(file);
    let identity = |path: &Path| {
        let metadata = fs::symlink_metadata(path).unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        (
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.modified().unwrap(),
        )
    };
    let before = [identity(&path), identity(&home.config())];
    let names = home.names();
    let mut failures = Vec::new();
    for operation in ["show", "list"] {
        let mut command = home.command();
        command.args(["sessions", operation]);
        if operation == "show" {
            command.arg(ID);
        }
        command.arg("--json");
        let result = output(&mut command);
        assert!(result.stdout.len() < 2 * 1024 * 1024);
        let text = support::success(&result);
        let report: Value = serde_json::from_str(&text).unwrap();
        let raw: RawObject = serde_json::from_str(&text).unwrap();
        assert_eq!(report["type"], "session_history");
        assert_eq!(report["logging_enabled"], true);
        assert_eq!(
            serde_json::from_str::<u8>(raw["schema_version"].get()).unwrap(),
            1
        );
        assert_eq!(
            report["limits"],
            json!({"maxFiles":100,"maxDirectoryEntries":10000,"maxFileBytes":4194304,
                "maxTotalBytes":16777216,"maxLineBytes":16384,"maxRecords":5000,"maxLines":10000})
        );
        let (summary, raw_summary) = if operation == "show" {
            let records: Vec<RawObject> = serde_json::from_str(raw["records"].get()).unwrap();
            assert_eq!(records.len(), CASES.len());
            for (index, row) in records.iter().enumerate() {
                let (name, _, expected) = CASES[index];
                let schema = row["schema_version"].get();
                // A typed integer consumer observes the lexical category.
                if serde_json::from_str::<u8>(schema).ok() != Some(2) || schema != "2" {
                    failures.push(format!(
                        "show/{name}: schema token {schema} rejected by u8/2"
                    ));
                }
                let latency = row["decision_latency_ms"].get();
                if latency != expected {
                    failures.push(format!(
                        "show/{name}: latency token {latency} != {expected}"
                    ));
                }
                assert_eq!(
                    serde_json::from_str::<String>(row["prompt_excerpt"].get()).unwrap(),
                    PROMPT
                );
            }
            if report["records"] != json!(expected_rows) {
                failures.push(
                    "show: complete normalized records differ from declared Source values".into(),
                );
            }
            (
                &report["summary"],
                serde_json::from_str::<RawObject>(raw["summary"].get()).unwrap(),
            )
        } else {
            let summaries: Vec<RawObject> = serde_json::from_str(raw["sessions"].get()).unwrap();
            assert_eq!(summaries.len(), 1);
            assert_eq!(report["coverage"]["partial"], false);
            (
                &report["sessions"][0],
                summaries.into_iter().next().unwrap(),
            )
        };
        assert_eq!(summary["id"], ID);
        for field in ["requests", "decisions", "pending"] {
            assert_eq!(summary[field], json!(CASES.len()));
        }
        for field in [
            "outcomes",
            "completed",
            "failed",
            "cancelled",
            "unconfirmed",
            "outcome_only",
        ] {
            assert_eq!(summary[field], 0);
        }
        assert_eq!(
            summary["coverage"],
            json!({"bytes_read":bytes.len(),"file_bytes":bytes.len(),
            "lines_read":CASES.len(),"invalid_records":0,"oversized_lines":0,"mixed_session_records":0,
            "truncated":false,"incomplete_tail":false,"partial":false,"legacy_decisions":0,
            "duplicate_records":0,"conflicting_requests":0})
        );
        let statistics: RawObject =
            serde_json::from_str(raw_summary["decision_latency_ms"].get()).unwrap();
        assert_eq!(
            serde_json::from_str::<usize>(statistics["samples"].get()).unwrap(),
            CASES.len()
        );
        for (field, expected) in [
            ("p50", "12"),
            ("p95", "1.7976931348623157e+308"),
            ("max", "1.7976931348623157e+308"),
        ] {
            if statistics[field].get() != expected {
                failures.push(format!(
                    "{operation}/{field}: {} != {expected}",
                    statistics[field].get()
                ));
            }
        }
    }
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(home.saved_bytes(), config_before);
    assert_eq!([identity(&path), identity(&home.config())], before);
    assert_eq!(home.names(), names);
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    eprintln!(
        "history_numeric_tokens={}",
        json!({"cases":CASES.len(),"operations":2,"mismatches":failures.len()})
    );
    assert!(failures.is_empty(), "{}", failures.join("; "));
}

// A real private file crosses only the default per-file byte ceiling. Every
// physical line is complete valid JSON and strictly below the line-byte limit.
#[test]
fn history_cli_default_file_bytes_bounds_the_read_before_the_final_sentinel_lf() {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    mod record_contract {
        include!("../../autorouter-core/src/json_compare_macros.rs");
        contract_numeric_equal!(same_number);
        contract_json_equal!(pub(super) same_record_json, a, b => same_number(a, b));
    }
    const CAP: usize = 4 * 1024 * 1024;
    const ID: &str = "autorouter-session-history-file-byte-boundary";
    for (delta, expected_hash) in [
        (
            -1isize,
            "d1a5eac2ec2f326e5239ba513fd26fc8db252d534b48a749c1a6d7606ef8f692",
        ),
        (
            0,
            "1296be5d053e64427fa269fccd0dcb0429f295d36826d93e99b4c5a1312c50b6",
        ),
        (
            1,
            "89dd12abbf40f805163bb153b071c6a58691a06364c4e840aa323e76f608904f",
        ),
    ] {
        let mut bytes = Vec::with_capacity(CAP + 1);
        for index in 0..257 {
            let line_bytes = match index {
                0 => 16_319usize.checked_add_signed(delta).unwrap(),
                256 => 16_383,
                _ => 16_319,
            };
            let start = bytes.len();
            bytes.extend_from_slice(history_count_row(index).as_bytes());
            assert!(bytes.len() - start < line_bytes);
            bytes.resize(start + line_bytes, b' ');
            bytes.push(b'\n');
        }
        assert_eq!(bytes.len(), CAP.checked_add_signed(delta).unwrap());
        assert!(bytes.len() < 16 * 1024 * 1024);
        assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 257);
        for (index, line) in bytes.split(|byte| *byte == b'\n').take(257).enumerate() {
            assert!(line.len() < 16_384);
            assert_eq!(
                serde_json::from_slice::<Value>(line).unwrap(),
                serde_json::from_str::<Value>(&history_count_row(index)).unwrap()
            );
        }
        let input_hash = format!("{:x}", Sha256::digest(&bytes));
        assert_eq!(input_hash, expected_hash);
        let truncated = delta > 0;
        let accepted = if truncated { 256 } else { 257 };
        let expected_rows: Vec<Value> = (0..accepted)
            .map(|index| serde_json::from_str(&history_count_row(index)).unwrap())
            .collect();
        let home = Home::new();
        let directory = home.0.join("logs");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        home.save(&json!({"AUTOROUTER_SESSION_LOG_DIR":directory}));
        let config_before = home.saved_bytes();
        let path = directory.join(format!("{ID}.jsonl"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(&bytes).unwrap();
        drop(file);
        let identity = |path: &Path| {
            let metadata = fs::symlink_metadata(path).unwrap();
            assert!(metadata.is_file());
            assert_eq!(metadata.nlink(), 1);
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            (
                metadata.dev(),
                metadata.ino(),
                metadata.len(),
                metadata.modified().unwrap(),
            )
        };
        let before = [identity(&path), identity(&home.config())];
        assert_eq!(before[0].2, bytes.len() as u64);
        let names = home.names();
        let dir_before = fs::symlink_metadata(&directory).unwrap();
        assert!(dir_before.is_dir());
        assert_eq!(dir_before.permissions().mode() & 0o777, 0o700);
        let result = output(home.command().args(["sessions", "show", ID, "--json"]));
        assert!(result.stdout.len() < 2 * 1024 * 1024);
        let text = support::success(&result);
        let report: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(report["schema_version"], 1);
        assert_eq!(report["type"], "session_history");
        assert_eq!(report["logging_enabled"], true);
        assert_eq!(
            report["limits"],
            json!({"maxFiles":100,"maxDirectoryEntries":10000,
            "maxFileBytes":4194304,"maxTotalBytes":16777216,"maxLineBytes":16384,
            "maxRecords":5000,"maxLines":10000})
        );
        assert!(record_contract::same_record_json(
            &report["records"],
            &json!(expected_rows)
        ));
        let records = report["records"].as_array().unwrap();
        assert_eq!(records.len(), accepted);
        assert_eq!(records.first().unwrap()["request_id"], "row-0000");
        assert_eq!(
            records.last().unwrap()["request_id"],
            if truncated { "row-0255" } else { "row-0256" }
        );
        assert_eq!(
            records.iter().any(|row| row["request_id"] == "row-0256"),
            !truncated
        );
        let summary = &report["summary"];
        assert_eq!(summary["id"], ID);
        assert_eq!(summary["session_id"], "synthetic-history-count");
        for field in ["requests", "decisions", "pending"] {
            assert_eq!(summary[field], json!(accepted), "{field}");
        }
        for field in [
            "outcomes",
            "completed",
            "failed",
            "cancelled",
            "unconfirmed",
            "outcome_only",
            "fallbacks",
        ] {
            assert_eq!(summary[field], 0, "{field}");
        }
        assert_eq!(
            summary["selected_models"],
            json!({"claude-sonnet-5-5":accepted})
        );
        assert_eq!(
            summary["coverage"],
            json!({"bytes_read":bytes.len().min(CAP),"file_bytes":bytes.len(),
            "lines_read":accepted,"invalid_records":0,"oversized_lines":0,"mixed_session_records":0,
            "truncated":truncated,"incomplete_tail":truncated,"partial":truncated,
            "legacy_decisions":0,"duplicate_records":0,"conflicting_requests":0})
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(home.saved_bytes(), config_before);
        assert_eq!([identity(&path), identity(&home.config())], before);
        assert_eq!(home.names(), names);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        let dir_after = fs::symlink_metadata(&directory).unwrap();
        assert!(dir_after.is_dir());
        assert_eq!(dir_after.permissions().mode() & 0o777, 0o700);
        assert_eq!(
            (dir_after.dev(), dir_after.ino()),
            (dir_before.dev(), dir_before.ino())
        );
        eprintln!(
            "history_file_boundary={}",
            json!({"file_bytes":bytes.len(),"records":accepted,
            "last_request_id":records.last().unwrap()["request_id"],"input_sha256":input_hash,
            "coverage":summary["coverage"]})
        );
    }
}

// All files stay below the per-file cap: only the aggregate default can shorten
// the last read. A sixth candidate separately witnesses pre-open exhaustion.
#[test]
fn history_cli_default_total_bytes_distinguishes_last_file_tail_from_next_file_exhaustion() {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    mod record_contract {
        include!("../../autorouter-core/src/json_compare_macros.rs");
        contract_numeric_equal!(same_number);
        contract_json_equal!(pub(super) same_json, a, b => same_number(a, b));
    }
    fn expected_summary(id: &str, records: usize, read: usize, size: usize) -> Value {
        let mut value: Value = serde_json::from_str(
            r#"{
            "id":"", "session_id":"synthetic-history-count",
            "started_at":"2026-10-18T00:00:00.000Z", "updated_at":"2026-10-18T00:00:00.000Z",
            "requests":0,"decisions":0,"outcomes":0,"completed":0,"failed":0,"cancelled":0,
            "pending":0,"unconfirmed":0,"outcome_only":0,
            "selected_models":{"claude-sonnet-5-5":0},"confirmed_models":{},"sources":{},
            "routing_reasons":{},"classifier_errors":{},"fallbacks":0,"fallback_rate":0,
            "decision_latency_ms":{"samples":0},"total_latency_ms":{"samples":0},
            "baseline_models":[],"mixed_baselines":false,"pricing_versions":[],
            "mixed_pricing_versions":false,"pricing_facts":[],"unversioned_outcomes":0,
            "savings":{"basis":"API-equivalent estimate, not subscription charges",
                "actual_usd":0,"baseline_usd":0,"saved_usd":0,"percent":0,
                "priced_requests":0,"unpriced_requests":0,"unpriced_reasons":{"missing_outcome":0}},
            "coverage":{}
        }"#,
        )
        .unwrap();
        value["id"] = json!(id);
        for field in ["requests", "decisions", "pending"] {
            value[field] = json!(records);
        }
        value["selected_models"]["claude-sonnet-5-5"] = json!(records);
        value["savings"]["unpriced_requests"] = json!(records);
        value["savings"]["unpriced_reasons"]["missing_outcome"] = json!(records);
        value["coverage"] = json!({"bytes_read":read,"file_bytes":size,"lines_read":records,
            "invalid_records":0,"oversized_lines":0,"mixed_session_records":0,
            "truncated":read<size,"incomplete_tail":read<size,"partial":read<size,
            "legacy_decisions":0,"duplicate_records":0,"conflicting_requests":0});
        value
    }
    const CAP: usize = 16 * 1024 * 1024;
    for (delta, exhaustion, expected_last_hash) in [
        (
            -1isize,
            false,
            "714e19e11f104d80aea3a9b5c32d9dc006ea9dc422d63332afdcd26deee8810b",
        ),
        (
            0,
            false,
            "de3825f098c50fe78460718cf2d1daebc8df1c1d3f99f92a3192282af9fa768e",
        ),
        (
            1,
            false,
            "9dcdd10e26df00811cdbaf6e1a30a4f18464da95f6472f7c3e2138a26bd3c08f",
        ),
        (
            0,
            true,
            "de3825f098c50fe78460718cf2d1daebc8df1c1d3f99f92a3192282af9fa768e",
        ),
    ] {
        let home = Home::new();
        let directory = home.0.join("logs");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        home.save(&json!({"AUTOROUTER_SESSION_LOG_DIR":directory}));
        let config_before = home.saved_bytes();
        let mut files = Vec::new();
        for suffix in (1..=5).rev() {
            let id = format!("autorouter-session-history-total-byte-{suffix}");
            let mut bytes = Vec::new();
            for row in 0..257 {
                let length = if suffix != 1 {
                    if row == 0 { 12_288 } else { 12_239 }
                } else {
                    match row {
                        0 => 16_315usize.checked_add_signed(delta).unwrap(),
                        256 => 16_383,
                        _ => 16_319,
                    }
                };
                let start = bytes.len();
                bytes.extend_from_slice(history_count_row(row).as_bytes());
                assert!(bytes.len() - start < length);
                bytes.resize(start + length, b' ');
                bytes.push(b'\n');
            }
            assert!(bytes.len() < 4 * 1024 * 1024);
            assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 257);
            for (row, line) in bytes.split(|byte| *byte == b'\n').take(257).enumerate() {
                assert!(line.len() < 16_384);
                assert_eq!(
                    serde_json::from_slice::<Value>(line).unwrap(),
                    serde_json::from_str::<Value>(&history_count_row(row)).unwrap()
                );
            }
            assert_eq!(
                format!("{:x}", Sha256::digest(&bytes)),
                if suffix == 1 {
                    expected_last_hash
                } else {
                    "f0eb9911ddc988fd94a094e6b6a24d2582a71c1378e3c75be8be5dcafe433b70"
                }
            );
            let path = directory.join(format!("{id}.jsonl"));
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .unwrap();
            file.write_all(&bytes).unwrap();
            drop(file);
            files.push((id, path, bytes));
        }
        assert_eq!(
            files.iter().map(|(_, _, bytes)| bytes.len()).sum::<usize>(),
            CAP.checked_add_signed(delta).unwrap()
        );
        if exhaustion {
            let id = "autorouter-session-history-total-byte-0".to_owned();
            let bytes = (history_count_row(0) + "\n").into_bytes();
            assert_eq!(bytes.len(), 215);
            assert_eq!(
                format!("{:x}", Sha256::digest(&bytes)),
                "b941492bfa80d830de6afdbb5fb23597190782522c950c66bc96345e92736e50"
            );
            let path = directory.join(format!("{id}.jsonl"));
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .unwrap();
            file.write_all(&bytes).unwrap();
            drop(file);
            files.push((id, path, bytes));
        }
        let identity = |path: &Path| {
            let metadata = fs::symlink_metadata(path).unwrap();
            assert!(metadata.is_file());
            assert_eq!(metadata.nlink(), 1);
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            (
                metadata.dev(),
                metadata.ino(),
                metadata.len(),
                metadata.modified().unwrap(),
            )
        };
        let before: Vec<_> = files
            .iter()
            .map(|(_, path, bytes)| {
                let value = identity(path);
                assert_eq!(value.2, bytes.len() as u64);
                value
            })
            .collect();
        let config_identity = identity(&home.config());
        let names = home.names();
        let dir_before = fs::symlink_metadata(&directory).unwrap();
        assert!(dir_before.is_dir());
        assert_eq!(dir_before.permissions().mode() & 0o777, 0o700);
        let mut expected: Value = serde_json::from_str(
            r#"{
            "schema_version":1,"type":"session_history","sessions":[],
            "limits":{"maxFiles":100,"maxDirectoryEntries":10000,"maxFileBytes":4194304,
                "maxTotalBytes":16777216,"maxLineBytes":16384,"maxRecords":5000,"maxLines":10000},
            "coverage":{},"logging_enabled":true
        }"#,
        )
        .unwrap();
        let mut remaining = CAP;
        let summaries: Vec<Value> = files
            .iter()
            .take(5)
            .map(|(id, _, bytes)| {
                let read = bytes.len().min(remaining);
                remaining -= read;
                let records = if read < bytes.len() { 256 } else { 257 };
                expected_summary(id, records, read, bytes.len())
            })
            .collect();
        expected["sessions"] = json!(summaries);
        expected["coverage"] = json!({"partial":delta>0 || exhaustion,"directory_entries":files.len(),
            "matching_files":files.len(),"skipped_files":usize::from(exhaustion),"unreadable_files":0,
            "bytes_read":CAP-remaining,"directory_scan_truncated":false,"byte_limit_reached":exhaustion});
        let result = output(home.command().args(["sessions", "list", "--json"]));
        assert!(result.stdout.len() < 2 * 1024 * 1024);
        let report: Value = serde_json::from_str(&support::success(&result)).unwrap();
        assert!(
            record_contract::same_json(&report, &expected),
            "delta={delta} exhaustion={exhaustion}: {report}"
        );
        eprintln!(
            "history_total_boundary={}",
            json!({"delta":delta,"exhaustion":exhaustion,"report":report})
        );
        if delta == 1 {
            // The same physical last file is complete when read independently:
            // only the earlier aggregate reads made list omit its final row.
            let (id, _, bytes) = files.last().unwrap();
            let rows: Vec<Value> = (0..257)
                .map(|row| serde_json::from_str(&history_count_row(row)).unwrap())
                .collect();
            let mut expected_show: Value = serde_json::from_str(
                r#"{
                "schema_version":1,"type":"session_history","summary":{},"records":[],
                "limits":{},"logging_enabled":true
            }"#,
            )
            .unwrap();
            expected_show["summary"] = expected_summary(id, 257, bytes.len(), bytes.len());
            expected_show["records"] = json!(rows);
            expected_show["limits"] = expected["limits"].clone();
            let result = output(home.command().args(["sessions", "show", id, "--json"]));
            assert!(result.stdout.len() < 2 * 1024 * 1024);
            let report: Value = serde_json::from_str(&support::success(&result)).unwrap();
            assert!(
                record_contract::same_json(&report, &expected_show),
                "independent last file: {report}"
            );
            eprintln!("history_total_last_file_show={report}");
        }
        for ((_, path, bytes), before) in files.iter().zip(before) {
            assert_eq!(fs::read(path).unwrap(), *bytes);
            assert_eq!(identity(path), before);
        }
        assert_eq!(home.saved_bytes(), config_before);
        assert_eq!(identity(&home.config()), config_identity);
        assert_eq!(home.names(), names);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), files.len());
        let mut entry_names: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        entry_names.sort();
        let mut expected_names: Vec<_> = files
            .iter()
            .map(|(_, path, _)| path.file_name().unwrap().to_owned())
            .collect();
        expected_names.sort();
        assert_eq!(entry_names, expected_names);
        let dir_after = fs::symlink_metadata(&directory).unwrap();
        assert!(dir_after.is_dir());
        assert_eq!(dir_after.permissions().mode() & 0o777, 0o700);
        assert_eq!(
            (dir_after.dev(), dir_after.ino()),
            (dir_before.dev(), dir_before.ino())
        );
    }
}

// Every recognized file is one small valid row. Only the default file-count
// selection ceiling can exclude an ID from the complete, ordered list report.
#[test]
fn history_cli_default_file_count_selects_the_100_highest_recognized_ids() {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    mod record_contract {
        include!("../../autorouter-core/src/json_compare_macros.rs");
        contract_numeric_equal!(same_number);
        contract_json_equal!(pub(super) same_json, a, b => same_number(a, b));
    }
    let summary_template: Value = serde_json::from_str(
        r#"{
  "id": "",
  "session_id": "synthetic-history-count",
  "started_at": "2026-10-18T00:00:00.000Z",
  "updated_at": "2026-10-18T00:00:00.000Z",
  "requests": 1,
  "decisions": 1,
  "outcomes": 0,
  "completed": 0,
  "failed": 0,
  "cancelled": 0,
  "pending": 1,
  "unconfirmed": 0,
  "outcome_only": 0,
  "selected_models": {
    "claude-sonnet-5-5": 1
  },
  "confirmed_models": {},
  "sources": {},
  "routing_reasons": {},
  "classifier_errors": {},
  "fallbacks": 0,
  "fallback_rate": 0,
  "decision_latency_ms": {
    "samples": 0
  },
  "total_latency_ms": {
    "samples": 0
  },
  "baseline_models": [],
  "mixed_baselines": false,
  "pricing_versions": [],
  "mixed_pricing_versions": false,
  "pricing_facts": [],
  "unversioned_outcomes": 0,
  "savings": {
    "basis": "API-equivalent estimate, not subscription charges",
    "actual_usd": 0,
    "baseline_usd": 0,
    "saved_usd": 0,
    "percent": 0,
    "priced_requests": 0,
    "unpriced_requests": 1,
    "unpriced_reasons": {
      "missing_outcome": 1
    }
  },
  "coverage": {
    "bytes_read": 215,
    "file_bytes": 215,
    "lines_read": 1,
    "invalid_records": 0,
    "oversized_lines": 0,
    "mixed_session_records": 0,
    "truncated": false,
    "incomplete_tail": false,
    "partial": false,
    "legacy_decisions": 0,
    "duplicate_records": 0,
    "conflicting_requests": 0
  }
}"#,
    )
    .unwrap();
    let bytes = (history_count_row(0) + "\n").into_bytes();
    assert_eq!(bytes.len(), 215);
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        "b941492bfa80d830de6afdbb5fb23597190782522c950c66bc96345e92736e50"
    );
    assert!(bytes.len() - 1 < 16_384);
    assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap(),
        serde_json::from_str::<Value>(&history_count_row(0)).unwrap()
    );
    for file_count in [99usize, 100, 101] {
        let home = Home::new();
        let directory = home.0.join("logs");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        home.save(&json!({"AUTOROUTER_SESSION_LOG_DIR":directory}));
        let config_before = home.saved_bytes();
        let mut files = Vec::new();
        for index in 0..file_count {
            let id = format!("autorouter-session-history-file-count-{index:03}");
            let path = directory.join(format!("{id}.jsonl"));
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .unwrap();
            file.write_all(&bytes).unwrap();
            drop(file);
            files.push((id, path));
        }
        assert!(file_count < 10_000);
        assert!(file_count * bytes.len() < 16 * 1024 * 1024);
        let identity = |path: &Path| {
            let metadata = fs::symlink_metadata(path).unwrap();
            assert!(metadata.is_file());
            assert_eq!(metadata.nlink(), 1);
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            (
                metadata.dev(),
                metadata.ino(),
                metadata.len(),
                metadata.modified().unwrap(),
            )
        };
        let before: Vec<_> = files
            .iter()
            .map(|(_, path)| {
                let value = identity(path);
                assert_eq!(value.2, bytes.len() as u64);
                value
            })
            .collect();
        let config_identity = identity(&home.config());
        let names = home.names();
        let dir_before = fs::symlink_metadata(&directory).unwrap();
        assert!(dir_before.is_dir());
        assert_eq!(dir_before.permissions().mode() & 0o777, 0o700);
        let mut expected: Value = serde_json::from_str(
            r#"{
            "schema_version":1,"type":"session_history","sessions":[],
            "limits":{"maxFiles":100,"maxDirectoryEntries":10000,"maxFileBytes":4194304,
                "maxTotalBytes":16777216,"maxLineBytes":16384,"maxRecords":5000,"maxLines":10000},
            "coverage":{},"logging_enabled":true
        }"#,
        )
        .unwrap();
        let summaries: Vec<Value> = files
            .iter()
            .rev()
            .take(100)
            .map(|(id, _)| {
                let mut summary = summary_template.clone();
                summary["id"] = json!(id);
                summary
            })
            .collect();
        let returned = file_count.min(100);
        assert_eq!(summaries.len(), returned);
        assert_eq!(
            summaries.first().unwrap()["id"],
            format!(
                "autorouter-session-history-file-count-{:03}",
                file_count - 1
            )
        );
        assert_eq!(
            summaries.last().unwrap()["id"],
            format!(
                "autorouter-session-history-file-count-{:03}",
                file_count.saturating_sub(100)
            )
        );
        expected["sessions"] = json!(summaries);
        expected["coverage"] = json!({"partial":file_count>100,"directory_entries":file_count,
            "matching_files":file_count,"skipped_files":file_count.saturating_sub(100),
            "unreadable_files":0,"bytes_read":returned*bytes.len(),
            "directory_scan_truncated":false,"byte_limit_reached":false});
        let result = output(home.command().args(["sessions", "list", "--json"]));
        assert!(result.stdout.len() < 2 * 1024 * 1024);
        let text = support::success(&result);
        assert!(!text.contains(home.0.to_str().unwrap()));
        assert!(!text.contains("AUTOROUTER_SESSION_LOG_DIR"));
        let report: Value = serde_json::from_str(&text).unwrap();
        assert!(
            record_contract::same_json(&report, &expected),
            "file_count={file_count}: {report}"
        );
        eprintln!(
            "history_file_count_boundary={}",
            json!({"files":file_count,"report":report})
        );
        for ((_, path), before) in files.iter().zip(before) {
            assert_eq!(fs::read(path).unwrap(), bytes);
            assert_eq!(identity(path), before);
        }
        assert_eq!(home.saved_bytes(), config_before);
        assert_eq!(identity(&home.config()), config_identity);
        assert_eq!(home.names(), names);
        let mut entry_names: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        entry_names.sort();
        let mut expected_names: Vec<_> = files
            .iter()
            .map(|(_, path)| path.file_name().unwrap().to_owned())
            .collect();
        expected_names.sort();
        assert_eq!(entry_names.len(), file_count);
        assert_eq!(entry_names, expected_names);
        let dir_after = fs::symlink_metadata(&directory).unwrap();
        assert!(dir_after.is_dir());
        assert_eq!(dir_after.permissions().mode() & 0o777, 0o700);
        assert_eq!(
            (dir_after.dev(), dir_after.ino()),
            (dir_before.dev(), dir_before.ino())
        );
    }
}

// Directory work is bounded before filename filtering. Unrecognized empty files
// isolate that ceiling without involving session selection or any byte budget.
#[test]
fn history_cli_default_directory_entries_counts_junk_before_filename_filtering() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    fn identity(path: &Path) -> (u64, u64, u64, std::time::SystemTime) {
        let metadata = fs::symlink_metadata(path).unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        (
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.modified().unwrap(),
        )
    }
    fn inventory(directory: &Path) -> BTreeMap<String, (u64, u64, u64, std::time::SystemTime)> {
        fs::read_dir(directory)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let path = entry.path();
                let value = identity(&path);
                assert_eq!(value.2, 0);
                assert!(fs::read(&path).unwrap().is_empty());
                (entry.file_name().into_string().unwrap(), value)
            })
            .collect()
    }
    for (entries, expected_hash) in [
        (
            9_999usize,
            "717882f366a48956dc5ee3b036915ef0c43f8e9f664b7aa5d1e3e421c09d825e",
        ),
        (
            10_000,
            "fdb57e3f0c71c2ce5f498613fe8906d3adae5b95281cf317430a0f80ba1e16ba",
        ),
        (
            10_001,
            "4bc4f830c8708f04541b1e109b098fecc12b7def01cd2a3fa30367d6fc10271a",
        ),
    ] {
        let home = Home::new();
        let directory = home.0.join("logs");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        home.save(&json!({"AUTOROUTER_SESSION_LOG_DIR":directory}));
        let config_before = home.saved_bytes();
        let config_identity = identity(&home.config());
        let expected_names: Vec<_> = (0..entries)
            .map(|index| format!("junk-{index:05}.tmp"))
            .collect();
        for name in &expected_names {
            assert!(!name.ends_with(".jsonl"));
            drop(
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(directory.join(name))
                    .unwrap(),
            );
        }
        let before = inventory(&directory);
        assert_eq!(before.len(), entries);
        assert_eq!(before.keys().cloned().collect::<Vec<_>>(), expected_names);
        let inventory_bytes = format!("{}\n", expected_names.join("\n"));
        assert_eq!(inventory_bytes.len(), entries * 15);
        let name_hash = format!("{:x}", Sha256::digest(inventory_bytes.as_bytes()));
        assert_eq!(name_hash, expected_hash);
        let home_names = home.names();
        let dir_before = fs::symlink_metadata(&directory).unwrap();
        assert!(dir_before.is_dir());
        assert_eq!(dir_before.permissions().mode() & 0o777, 0o700);
        let result = output(home.command().args(["sessions", "list", "--json"]));
        assert!(result.stdout.len() < 2 * 1024 * 1024);
        let report: Value = serde_json::from_str(&support::success(&result)).unwrap();
        let truncated = entries > 10_000;
        assert_eq!(
            report,
            json!({"schema_version":1,"type":"session_history","sessions":[],
            "limits":{"maxFiles":100,"maxDirectoryEntries":10000,"maxFileBytes":4194304,
                "maxTotalBytes":16777216,"maxLineBytes":16384,"maxRecords":5000,"maxLines":10000},
            "coverage":{"partial":truncated,"directory_entries":entries,"matching_files":0,
                "skipped_files":0,"unreadable_files":0,"bytes_read":0,
                "directory_scan_truncated":truncated,"byte_limit_reached":false},
            "logging_enabled":true})
        );
        assert_eq!(inventory(&directory), before);
        assert_eq!(home.saved_bytes(), config_before);
        assert_eq!(identity(&home.config()), config_identity);
        assert_eq!(home.names(), home_names);
        let dir_after = fs::symlink_metadata(&directory).unwrap();
        assert!(dir_after.is_dir());
        assert_eq!(dir_after.permissions().mode() & 0o777, 0o700);
        assert_eq!(
            (
                dir_after.dev(),
                dir_after.ino(),
                dir_after.modified().unwrap()
            ),
            (
                dir_before.dev(),
                dir_before.ino(),
                dir_before.modified().unwrap()
            )
        );
        eprintln!(
            "history_directory_boundary={}",
            json!({"entries":entries,
            "name_inventory_sha256":name_hash,"report":report})
        );
    }
}
