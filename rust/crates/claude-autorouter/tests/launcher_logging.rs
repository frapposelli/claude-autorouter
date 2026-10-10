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
    home.claude(&format!("#!/bin/sh\nset -eu\n[ \"$AUTOROUTER_EVALUATOR\" = jev ] || exit 71\n[ -z \"${{TYPESAFE_API_KEY-}}\" ] || exit 72\n[ \"${{#ANTHROPIC_API_KEY}}\" = 64 ] || exit 73\n[ \"$ANTHROPIC_API_KEY\" != private-upstream-key ] || exit 74\n[ \"$ANTHROPIC_MODEL\" = claude-haiku-4-5-20251001 ] || exit 75\n/usr/bin/curl --silent --show-error --fail --max-time 10 -H 'content-type: application/json' -H \"x-api-key: $ANTHROPIC_API_KEY\" -H 'x-claude-code-session-id: logging-session' -H 'x-claude-code-request-class: main' --data-binary @{} \"$ANTHROPIC_BASE_URL/v1/messages\" --output {}\ni=0\nwhile [ \"$i\" -lt 100 ]; do if /usr/bin/grep -q '\"phase\":\"ready\"' \"$AUTOROUTER_STATUS_FILE\" && /usr/bin/grep -q '\"requests\":1' \"$AUTOROUTER_STATUS_FILE\"; then break; fi; i=$((i+1)); /bin/sleep 0.01; done\n/bin/cp \"$AUTOROUTER_STATUS_FILE\" {}\nprintf 'Claude reply: private-response-content\\n'\n",quote(&body),quote(&home.0.join("reply.sse")),quote(&home.0.join("snapshot-copy.json"))));
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
