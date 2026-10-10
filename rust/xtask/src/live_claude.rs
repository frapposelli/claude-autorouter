//! Isolated Claude stream driver. Private answers are discarded before reporting.
use crate::tool_process::{self, InputAction, RunOptions};
use autorouter_core::auth::Environment;
use autorouter_core::js_json::JsDocument;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
pub const TEST_COMMAND: &str = "cargo test --offline --quiet";
pub const SOURCE: &str = "merge-intervals.rs";
pub const TEST: &str = "merge-intervals.test.rs";
fn push_unique(values: &mut Vec<Value>, value: Value) {
    if !values.contains(&value) {
        values.push(value);
    }
}
fn integer(value: &Value) -> bool {
    value
        .as_f64()
        .is_some_and(|n| n.is_finite() && n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0)
}
fn safe_enum(value: Option<&Value>) -> Value {
    match value {
        None => json!("absent"),
        Some(Value::Null) => Value::Null,
        Some(Value::String(s))
            if [
                "global",
                "us",
                "not_available",
                "standard",
                "message",
                "compaction",
                "advisor_message",
                "fallback_message",
            ]
            .contains(&s.as_str()) =>
        {
            json!(s)
        }
        _ => json!("other"),
    }
}
fn call_metadata(name: &str, input: &Value, cwd: &Path) -> Value {
    let tool = if ["Read", "Edit", "Bash"].contains(&name) {
        name
    } else {
        "other"
    };
    if tool == "Bash" {
        let command = input["command"].as_str().unwrap_or("").trim();
        json!({"tool":tool,"command_category":if command=="pwd"{"working_directory_lookup"}else if command==TEST_COMMAND{"exact_fixture_test"}else if command.contains(TEST_COMMAND){"fixture_test_with_other_shell_text"}else{"other"}})
    } else {
        let path = input["file_path"].as_str().unwrap_or("");
        let is = |name: &str| {
            path == name || path == format!("./{name}") || Path::new(path) == cwd.join(name)
        };
        json!({"tool":tool,"target_category":if is(SOURCE){"fixture_source"}else if is(TEST){"fixture_test"}else{"other"}})
    }
}
struct Stream<'a> {
    cwd: &'a Path,
    prompts: &'a [Value],
    sent: usize,
    buffer: Vec<u8>,
    results: Vec<String>,
    assistant_models: Vec<Value>,
    tools: Vec<Value>,
    usage: Value,
    failures: Vec<Value>,
    shapes: Vec<Value>,
    calls: Vec<(String, Value)>,
    outcomes: BTreeMap<String, bool>,
    denials: Vec<(String, Value)>,
    denied: usize,
    parse_errors: usize,
}
impl<'a> Stream<'a> {
    fn new(cwd: &'a Path, prompts: &'a [Value]) -> Self {
        Self {
            cwd,
            prompts,
            sent: 0,
            buffer: Vec::new(),
            results: Vec::new(),
            assistant_models: Vec::new(),
            tools: Vec::new(),
            usage: json!({}),
            failures: Vec::new(),
            shapes: Vec::new(),
            calls: Vec::new(),
            outcomes: BTreeMap::new(),
            denials: Vec::new(),
            denied: 0,
            parse_errors: 0,
        }
    }
    fn next(&mut self) -> InputAction {
        if self.sent >= self.prompts.len() {
            return InputAction {
                close: true,
                ..Default::default()
            };
        }
        let bytes = format!(
            "{}\n",
            json!({"type":"user","message":{"role":"user","content":self.prompts[self.sent]}})
        )
        .into_bytes();
        self.sent += 1;
        InputAction {
            bytes,
            close: false,
        }
    }
    fn chunk(&mut self, bytes: &[u8]) -> InputAction {
        self.buffer.extend_from_slice(bytes);
        let mut actions = InputAction::default();
        while let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<_> = self.buffer.drain(..=end).collect();
            let action = self.event(&line);
            actions.bytes.extend(action.bytes);
            actions.close |= action.close;
        }
        actions
    }
    fn event(&mut self, line: &[u8]) -> InputAction {
        if line.iter().all(u8::is_ascii_whitespace) {
            return InputAction::default();
        }
        let Ok(doc) = JsDocument::parse(line) else {
            self.parse_errors += 1;
            return InputAction::default();
        };
        let value = doc.to_serde_observation_lossy();
        match value["type"].as_str() {
            Some("assistant") => {
                if value["message"]["model"].is_string() {
                    push_unique(
                        &mut self.assistant_models,
                        value["message"]["model"].clone(),
                    );
                }
                if let Some(usage) = value["message"]
                    .get("usage")
                    .filter(|u| u.is_object() || u.is_array())
                {
                    let iterations = match usage.get("iterations") {
                        Some(Value::Array(items)) => json!(
                            items
                                .iter()
                                .take(10)
                                .map(|item| {
                                    let mut entry = json!({"type":safe_enum(item.get("type"))});
                                    for key in [
                                        "input_tokens",
                                        "output_tokens",
                                        "cache_creation_input_tokens",
                                        "cache_read_input_tokens",
                                    ] {
                                        if let Some(v) = item.get(key).filter(|v| integer(v)) {
                                            entry[key] = v.clone();
                                        }
                                    }
                                    entry
                                })
                                .collect::<Vec<_>>()
                        ),
                        Some(Value::Null) => Value::Null,
                        _ => json!("absent"),
                    };
                    self.shapes.push(json!({"inference_geo":safe_enum(usage.get("inference_geo")),"iterations":iterations}));
                }
                for block in value["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_use" {
                        let name = block["name"].as_str().unwrap_or("");
                        if ["Read", "Edit", "Bash"].contains(&name) {
                            push_unique(&mut self.tools, json!(name));
                        }
                        let id = block["id"].as_str().unwrap_or("").to_owned();
                        let call = call_metadata(name, &block["input"], self.cwd);
                        if let Some((_, old)) = self.calls.iter_mut().find(|(key, _)| *key == id) {
                            *old = call;
                        } else {
                            self.calls.push((id, call));
                        }
                    }
                }
            }
            Some("user") => {
                for block in value["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_result" {
                        self.outcomes.insert(
                            block["tool_use_id"].as_str().unwrap_or("").into(),
                            block["is_error"] == true,
                        );
                    }
                }
            }
            Some("result") => {
                self.results
                    .push(value["result"].as_str().unwrap_or("").into());
                if value["is_error"] == true {
                    let subtype = value["subtype"].as_str().unwrap_or("");
                    self.failures.push(json!(if [
                        "error_max_turns",
                        "error_during_execution",
                        "error_max_budget_usd"
                    ]
                    .contains(&subtype)
                    {
                        subtype
                    } else {
                        "claude_result_error"
                    }));
                }
                for denial in value["permission_denials"].as_array().into_iter().flatten() {
                    self.denied += 1;
                    self.denials.push((
                        denial["tool_use_id"].as_str().unwrap_or("").into(),
                        call_metadata(
                            denial["tool_name"].as_str().unwrap_or(""),
                            &denial["tool_input"],
                            self.cwd,
                        ),
                    ));
                }
                if let Some(models) = value["modelUsage"].as_object() {
                    for (name, usage) in models {
                        let mut sanitized = json!({});
                        for (key, value) in usage.as_object().into_iter().flatten() {
                            let lower = key.to_ascii_lowercase();
                            if ["tokens", "cost", "requests"]
                                .iter()
                                .any(|term| lower.contains(term))
                                && value.is_number()
                            {
                                sanitized[key] = value.clone();
                            }
                        }
                        self.usage[name] = sanitized;
                    }
                }
                if self.sent < self.prompts.len() && value["is_error"] != true {
                    return self.next();
                }
                return InputAction {
                    close: true,
                    ..Default::default()
                };
            }
            _ => {}
        }
        InputAction::default()
    }
    fn finish(mut self, mut process: Value) -> (Value, Vec<String>) {
        if !self.buffer.iter().all(u8::is_ascii_whitespace) {
            let bytes = std::mem::take(&mut self.buffer);
            self.event(&bytes);
        }
        if process["spawn_error"] == true {
            self.failures.push(json!("claude_spawn_error"));
        }
        let outcomes: Vec<_> = self
            .calls
            .iter()
            .map(|(id, call)| {
                let mut row = call.clone();
                row["result_received"] = json!(self.outcomes.contains_key(id));
                row["is_error"] = json!(self.outcomes.get(id));
                row["permission_denied"] = json!(self.denials.iter().any(|(key, _)| key == id));
                row
            })
            .collect();
        let mut successful = Vec::new();
        for row in &outcomes {
            if row["result_received"] == true
                && row["is_error"] != true
                && row["permission_denied"] != true
            {
                push_unique(&mut successful, row["tool"].clone());
            }
        }
        process.as_object_mut().unwrap().remove("stdout_bytes");
        for key in ["response_received", "controlled_stop", "spawn_error"] {
            process.as_object_mut().unwrap().remove(key);
        }
        let mut report = json!({"result_count":self.results.len(),"assistant_models":self.assistant_models,"tools_used":self.tools,"permission_denials":self.denied,"parse_errors":self.parse_errors,"failures":self.failures,"tool_outcomes":outcomes,"provider_usage_shapes":self.shapes,"successful_tools":successful,"denied_tool_details":self.denials.into_iter().map(|(_,v)|v).collect::<Vec<_>>(),"client_model_usage_estimate":self.usage});
        report
            .as_object_mut()
            .unwrap()
            .extend(process.as_object().unwrap().clone());
        (report, self.results)
    }
}
pub fn arguments(
    options: &Value,
    scenario: &Value,
    system: Option<&Path>,
    max_turns: bool,
) -> Vec<String> {
    let mut args = [
        "--print",
        "--safe-mode",
        "--restricted",
        "--setting-sources",
        "",
        "--strict-mcp-config",
        "--mcp-config",
        "{\"mcpServers\":{}}",
        "--no-session-persistence",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--permission-mode",
        "dontAsk",
        "--no-chrome",
        "--tools",
        if scenario["tools"] == true {
            "Read,Edit,Bash"
        } else {
            ""
        },
    ]
    .map(str::to_owned)
    .to_vec();
    if max_turns {
        args.extend(["--max-turns".into(), "12".into()]);
    }
    if let Some(model) = options["model"].as_str() {
        args.extend(["--model".into(), model.into()]);
    }
    if let Some(system) = system {
        args.extend([
            "--append-system-prompt-file".into(),
            system.to_string_lossy().into_owned(),
        ]);
    }
    if scenario["tools"] == true {
        args.extend([
            "--allowedTools".into(),
            "Read".into(),
            format!("Edit(./{SOURCE})"),
            format!("Bash({TEST_COMMAND})"),
        ]);
    }
    args
}
pub async fn run(
    cwd: &Path,
    env: Environment,
    scenario: &Value,
    options: &Value,
    system: Option<&Path>,
    max_turns: bool,
    cancel: &CancellationToken,
) -> (Value, Vec<String>) {
    let prompts = scenario["prompts"].as_array().unwrap();
    let mut stream = Stream::new(cwd, prompts);
    let initial = stream.next();
    let response = CancellationToken::new();
    let report = tool_process::run_child(
        Command::new("claude")
            .args(arguments(options, scenario, system, max_turns))
            .current_dir(cwd)
            .env_clear()
            .envs(env),
        RunOptions {
            timeout: Duration::from_millis(options["timeoutMs"].as_u64().unwrap()),
            grace: Duration::from_secs(2),
            max_stdout: Some(8 * 1024 * 1024),
            interactive: false,
            response: &response,
            cancel,
            initial,
        },
        |chunk| stream.chunk(chunk),
    )
    .await;
    stream.finish(report)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn continuation_waits_for_success_and_reports_only_allowlisted_metadata() {
        let prompts = vec![json!("first-private"), json!("second-private")];
        let mut stream = Stream::new(Path::new("/tmp/fixture"), &prompts);
        assert!(
            String::from_utf8(stream.next().bytes)
                .unwrap()
                .contains("first-private")
        );
        let action=stream.chunk(b"{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"secret-id\",\"name\":\"Bash\",\"input\":{\"command\":\"secret command\"}}]}}\n{\"type\":\"result\",\"result\":\"secret answer\",\"is_error\":false}\n");
        assert!(
            String::from_utf8(action.bytes)
                .unwrap()
                .contains("second-private")
        );
        let action=stream.chunk(b"{\"type\":\"result\",\"result\":\"secret answer2\",\"is_error\":true,\"subtype\":\"secret error\"}\n");
        assert!(action.close);
        let (report, answers) = stream.finish(json!({}));
        assert_eq!(answers.len(), 2);
        assert!(!report.to_string().contains("secret"));
        assert_eq!(report["failures"], json!(["claude_result_error"]));
    }
    #[test]
    fn arguments_preserve_isolation_and_scope_permissions_to_native_fixture() {
        let args = arguments(&json!({}), &json!({"tools":true}), None, true);
        for flag in [
            "--safe-mode",
            "--restricted",
            "--strict-mcp-config",
            "--no-session-persistence",
        ] {
            assert!(args.contains(&flag.into()));
        }
        assert!(args.contains(&format!("Edit(./{SOURCE})")));
        assert!(args.contains(&format!("Bash({TEST_COMMAND})")));
        assert!(!args.iter().any(|s| s.contains("node ")));
    }
}
