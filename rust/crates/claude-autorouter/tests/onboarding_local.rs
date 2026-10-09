#![cfg(unix)]
mod support;
use autorouter_core::config::{DEFAULT_OLLAMA_MODEL, read_config};
use serde_json::{Value, json};
use std::fs;
use std::io::Read;
use std::process::Command;
use std::sync::{Arc, Mutex};
use support::{
    HiddenInput, Home, Request, Response, Server, hidden_inputs, hidden_secret, output, quote,
    signalled_output, success,
};

fn diagnostic_reply(request: &Request, model: &str, collapsed: bool) -> Response {
    for header in ["authorization", "x-api-key", "x-autorouter-token"] {
        assert!(!request.headers.contains_key(header));
    }
    let body = match request.path.as_str() {
        "/api/version" => json!({"version":"0.35.0"}),
        "/api/tags" | "/api/ps" => json!({"models":[{"name":model}]}),
        "/api/show" => {
            assert_eq!(request.json()["model"], model);
            json!({"details":{"parameter_size":"0.8B"}})
        }
        "/v1/systemone" => {
            let body = request.json();
            assert_eq!(body["model"], model);
            assert_eq!(body["keep_alive"], "5m");
            let cases = autorouter_runtime::local_diagnostic::local_diagnostic_cases();
            let tier = if body["state"]["current_task"] == "Return the literal word ready." {
                "haiku"
            } else {
                let fixture = cases
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["prompt"] == body["state"]["current_task"])
                    .expect("Only packaged synthetic tasks may reach local diagnosis");
                if collapsed {
                    "sonnet"
                } else {
                    fixture["expected"].as_str().unwrap()
                }
            };
            json!({"model":model,"answers":{"tier":{"type":"choice","choice":tier,"confidence":1,
                "probabilities":{"haiku":u8::from(tier=="haiku"),"sonnet":u8::from(tier=="sonnet"),"opus":u8::from(tier=="opus")}}},
                "usage":{"input_tokens":900,"output_tokens":1}})
        }
        _ => panic!("Local diagnosis reached an unexpected endpoint"),
    };
    Response::json(body)
}

fn diagnostic_home(model: &str) -> Home {
    let home = Home::new();
    home.save(&json!({"AUTOROUTER_AUTH_MODE":"api-key","AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_OLLAMA_MODEL":model}));
    home.claude(&format!(
        "#!/bin/sh\nprintf unexpected > {}\nexit 93\n",
        quote(&home.0.join("unexpected-claude"))
    ));
    home
}

#[test]
fn local_diagnostic_cli_bypasses_cloud_keys_and_claude_auth_and_emits_one_json_record() {
    for collapsed in [false, true] {
        let home = diagnostic_home("tev1:0.8b");
        let before = fs::read(home.config()).unwrap();
        let server = Server::new(move |request| diagnostic_reply(request, "tev1:0.8b", collapsed));
        let result = output(
            home.command()
                .args(["doctor", "--evaluate-local", "--json"])
                .env("AUTOROUTER_OLLAMA_URL", server.url()),
        );
        assert_eq!(result.status.code(), Some(i32::from(collapsed)));
        assert!(
            result.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let text = std::str::from_utf8(&result.stdout).unwrap();
        assert!(text.ends_with('\n'));
        assert_eq!(
            text.lines().count(),
            1,
            "Progress must not contaminate JSON output"
        );
        let report: Value = serde_json::from_str(text).unwrap();
        assert_eq!(report["schema_version"], 1);
        assert_eq!(report["type"], "local_evaluator_diagnostic");
        assert_eq!(report["evaluator_model"], "tev1:0.8b");
        assert_eq!(report["passed"], !collapsed);
        assert_eq!(report["gates"]["rubric"]["passed"], !collapsed);
        for gate in ["preflight", "startup", "cases", "evaluator"] {
            assert_eq!(report["gates"][gate]["passed"], true);
        }
        for key in ["paid_provider_calls", "downloads", "models_unloaded"] {
            assert_eq!(report[key], 0);
        }
        assert_eq!(report["configuration_changed"], false);
        assert_eq!(
            report["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["case"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "literal",
                "array-length",
                "bounded-feature",
                "distributed-fencing",
                "new-mechanical-task",
                "new-difficult-task"
            ]
        );
        assert!(
            report["rows"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["current_task_matches"] == true)
        );
        assert_eq!(
            server
                .paths()
                .iter()
                .filter(|path| *path == "/v1/systemone")
                .count(),
            7
        );
        assert_eq!(
            server
                .paths()
                .iter()
                .filter(|path| *path == "/api/ps")
                .count(),
            7
        );
        assert!(!home.0.join("unexpected-claude").exists());
        assert_eq!(fs::read(home.config()).unwrap(), before);
        server.assert_clean();
    }
}

#[test]
fn local_diagnostic_cli_rejects_jev_without_io_or_claude_authentication() {
    let home = diagnostic_home("tev1:0.8b");
    home.save(&json!({"AUTOROUTER_EVALUATOR":"jev"}));
    let before = fs::read(home.config()).unwrap();
    let server = Server::new(|_| panic!("Rejected Jev diagnosis must perform no HTTP requests"));
    let result = output(
        home.command()
            .args(["doctor", "--evaluate-local", "--json"])
            .env(
                "AUTOROUTER_JEV_URL",
                format!("{}/v1/systemone", server.url()),
            )
            .env("AUTOROUTER_OLLAMA_URL", server.url()),
    );
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stderr.is_empty());
    assert_eq!(
        std::str::from_utf8(&result.stdout).unwrap().lines().count(),
        1
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap(),
        json!({
            "schema_version":1,"type":"local_evaluator_diagnostic","passed":false,
            "error":{"code":"configuration_error","message":"Local evaluation requires AUTOROUTER_EVALUATOR=ollama. It does not call Jev or Anthropic."}
        })
    );
    assert!(server.calls().is_empty());
    assert!(!home.0.join("unexpected-claude").exists());
    assert_eq!(fs::read(home.config()).unwrap(), before);
}

#[test]
fn local_diagnostic_cli_cancellation_after_request_closes_transport_without_a_report() {
    use nix::sys::signal::Signal;
    use std::sync::mpsc;
    use std::time::Duration;
    for signal in [Signal::SIGINT, Signal::SIGTERM] {
        for json_output in [false, true] {
            let home = diagnostic_home("tev1:0.8b");
            let before = fs::read(home.config()).unwrap();
            let (ready_tx, ready_rx) = mpsc::channel();
            let (closed_tx, closed_rx) = mpsc::channel();
            let server = Server::with_connection(move |request, stream| {
                let response = diagnostic_reply(request, "tev1:0.8b", false);
                if request.path != "/v1/systemone" {
                    return Some(response);
                }
                ready_tx.send(()).unwrap();
                let closed = match stream.read(&mut [0; 1]) {
                    Ok(0) => true,
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => true,
                    _ => false,
                };
                closed_tx.send(closed).unwrap();
                None
            });
            let mut command = home.command();
            command
                .args(["doctor", "--evaluate-local"])
                .env("AUTOROUTER_OLLAMA_URL", server.url());
            if json_output {
                command.arg("--json");
            }
            let result = signalled_output(&mut command, &ready_rx, signal);
            assert_eq!(result.status.code(), Some(1));
            assert_eq!(result.stderr, b"Evaluation cancelled\n");
            if json_output {
                assert!(result.stdout.is_empty());
            } else {
                assert_eq!(
                    String::from_utf8(result.stdout).unwrap(),
                    "Checking the local evaluator; no Claude authentication or cloud requests are used.\nPreparing the local model with a separate 60-second startup deadline…\n"
                );
            }
            assert!(closed_rx.recv_timeout(Duration::from_secs(2)).unwrap());
            assert_eq!(
                server.paths(),
                [
                    "/api/version",
                    "/api/tags",
                    "/api/show",
                    "/api/ps",
                    "/api/show",
                    "/v1/systemone"
                ]
            );
            assert!(!home.0.join("unexpected-claude").exists());
            assert_eq!(fs::read(home.config()).unwrap(), before);
            server.assert_clean();
        }
    }
}
struct State {
    model: String,
    installed: bool,
    config_at_warm: Vec<Option<Vec<u8>>>,
}
struct Local {
    home: Home,
    server: Server,
    state: Arc<Mutex<State>>,
}
impl Local {
    fn new(model: &str) -> Self {
        let home = Home::new();
        let path = home.config();
        let state = Arc::new(Mutex::new(State {
            model: model.into(),
            installed: true,
            config_at_warm: Vec::new(),
        }));
        let observed = state.clone();
        let server = Server::new(move |request| {
            let mut state = observed.lock().unwrap();
            assert!(!request.headers.contains_key("authorization"));
            match request.path.as_str() {
                "/api/version" => Response::json(json!({"version":"0.35.0"})),
                "/api/tags" => Response::json(
                    json!({"models":if state.installed{json!([{"name":state.model}])}else{json!([])}}),
                ),
                "/api/show" => {
                    assert_eq!(request.json()["model"], state.model);
                    Response::json(json!({"details":{"parameter_size":"1.7B"}}))
                }
                "/v1/systemone" => {
                    assert_eq!(request.json()["model"], state.model);
                    state.config_at_warm.push(fs::read(&path).ok());
                    Response::json(
                        json!({"model":state.model,"answers":{"tier":{"type":"choice","choice":"haiku","probabilities":{"haiku":1,"sonnet":0,"opus":0},"confidence":1}},"usage":{"input_tokens":200,"output_tokens":1}}),
                    )
                }
                _ => panic!("Unexpected synthetic local operation"),
            }
        });
        let auth = home.0.join("claude-calls.txt");
        home.claude(&format!("#!/bin/sh\nset -eu\n[ -z \"${{TYPESAFE_API_KEY-}}${{ANTHROPIC_API_KEY-}}${{ANTHROPIC_AUTH_TOKEN-}}${{CLAUDE_CODE_OAUTH_TOKEN-}}${{AUTOROUTER_CONFIG-}}${{ANTHROPIC_BASE_URL-}}\" ] || exit 71\nprintf '%s\\n' \"$*\" >> {}\nif [ \"$#\" = 1 ] && [ \"$1\" = --version ]; then printf '%s (Claude Code)\\n' \"${{SYNTHETIC_CLAUDE_VERSION-2.1.285}}\"\nelif [ \"$#\" = 3 ] && [ \"$1\" = auth ] && [ \"$2\" = status ] && [ \"$3\" = --json ]; then printf '{{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"PRIVATE@example.invalid\",\"token\":\"PRIVATE_AUTH_TOKEN\"}}\\n'\nelse exit 72; fi\n",quote(&auth)));
        Self {
            home,
            server,
            state,
        }
    }
    fn command(&self) -> Command {
        let mut c = self.home.command();
        c.env("AUTOROUTER_OLLAMA_URL", self.server.url());
        c
    }
    fn model(&self, model: &str) {
        self.state.lock().unwrap().model = model.into();
    }
    fn reset(&self) {
        self.server.clear();
        self.state.lock().unwrap().config_at_warm.clear();
        let _ = fs::remove_file(self.home.0.join("claude-calls.txt"));
    }
    fn auth_calls(&self) -> Vec<String> {
        fs::read_to_string(self.home.0.join("claude-calls.txt"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
    fn saved_base(&self) -> Value {
        json!({"AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_CLIENT_PROFILE":"compatible","AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_OLLAMA_MODEL":DEFAULT_OLLAMA_MODEL,"AUTOROUTER_OLLAMA_URL":self.server.url(),"AUTOROUTER_SECRET_STORE":"file"})
    }
}

#[test]
fn ollama_subscription_setup_preloads_before_saving_and_doctor_is_read_only() {
    let local = Local::new(DEFAULT_OLLAMA_MODEL);
    let result = output(
        local
            .command()
            .args(["setup", "--evaluator", "ollama"])
            .env("TYPESAFE_API_KEY", "unused-jev-key")
            .env("ANTHROPIC_API_KEY", "unused-anthropic-key"),
    );
    let text = success(&result);
    assert_eq!(local.home.saved(), local.saved_base());
    assert_eq!(
        local.server.paths(),
        [
            "/api/version",
            "/api/tags",
            "/api/show",
            "/api/show",
            "/v1/systemone"
        ]
    );
    assert_eq!(local.state.lock().unwrap().config_at_warm, [None]);
    assert!(local.auth_calls().is_empty());
    let warm = local.server.calls().last().unwrap().json();
    assert_eq!(
        warm["state"]["current_task"],
        "Return the literal word ready."
    );
    assert_eq!(warm["questions"]["tier"]["type"], "choice");
    assert!(warm.get("options").is_none());
    assert!(warm.get("messages").is_none());
    assert!(text.contains("locally with Ollama"));
    assert!(
        text.contains("Local evaluator: nimble:9b-q4_K_M; routing deadline 30000 ms per request")
    );
    assert!(!text.contains("unused-"));
    for call in local.server.calls() {
        assert!(!String::from_utf8_lossy(&call.body).contains("unused-"));
    }
    let before = fs::read(local.home.config()).unwrap();
    local.reset();
    let result = output(local.command().arg("doctor"));
    let text = success(&result);
    assert_eq!(
        local.server.paths(),
        ["/api/version", "/api/tags", "/api/show"]
    );
    assert_eq!(local.auth_calls(), ["--version", "auth status --json"]);
    assert!(text.contains("Local Ollama model available"));
    assert!(text.contains("routing deadline 30000 ms per request"));
    assert!(text.contains("classification speed and accuracy are not tested"));
    assert!(!text.contains("PRIVATE"));
    assert_eq!(fs::read(local.home.config()).unwrap(), before);
    local.reset();
    local.state.lock().unwrap().installed = false;
    let result = output(local.command().arg("doctor"));
    assert!(!result.status.success());
    assert!(result.stderr.is_empty());
    assert!(String::from_utf8_lossy(&result.stdout).contains("--pull"));
    assert_eq!(local.server.paths(), ["/api/version", "/api/tags"]);
    assert_eq!(fs::read(local.home.config()).unwrap(), before);
}
#[test]
fn ollama_api_setup_prompts_only_anthropic_preserves_keepalive_and_accepts_custom_model() {
    let local = Local::new(DEFAULT_OLLAMA_MODEL);
    let result = hidden_secret(
        local
            .command()
            .args(["setup", "--evaluator", "ollama", "--auth-mode", "api-key"])
            .env("AUTOROUTER_OLLAMA_KEEP_ALIVE", "10m"),
        "ANTHROPIC_API_KEY",
        "PRIVATE_ANTHROPIC_KEY",
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&result.stderr)
            .matches("(hidden):")
            .count(),
        1
    );
    assert!(!String::from_utf8_lossy(&result.stderr).contains("TYPESAFE_API_KEY"));
    assert!(!String::from_utf8_lossy(&result.stdout).contains("PRIVATE"));
    let saved = local.home.saved();
    assert_eq!(saved["AUTOROUTER_OLLAMA_MODEL"], DEFAULT_OLLAMA_MODEL);
    assert_eq!(saved["ANTHROPIC_API_KEY"], "PRIVATE_ANTHROPIC_KEY");
    assert!(saved.get("TYPESAFE_API_KEY").is_none());
    assert_eq!(saved["AUTOROUTER_OLLAMA_URL"], local.server.url());
    assert_eq!(
        local.server.calls().last().unwrap().json()["keep_alive"],
        "10m"
    );
    local.model("custom-router:latest");
    local.reset();
    success(&output(local.command().args([
        "setup",
        "--force",
        "--evaluator",
        "ollama",
        "--ollama-model",
        "custom-router:latest",
    ])));
    assert_eq!(
        local.home.saved()["AUTOROUTER_OLLAMA_MODEL"],
        "custom-router:latest"
    );
    assert_eq!(
        local.home.saved()["ANTHROPIC_API_KEY"],
        "PRIVATE_ANTHROPIC_KEY"
    );
    assert!(local.auth_calls().is_empty());
}
#[test]
fn forced_model_deadlines_and_doctor_runtime_overrides_preserve_saved_values() {
    let local = Local::new("tev1:4b");
    let result = output(
        local
            .command()
            .args([
                "setup",
                "--evaluator",
                "ollama",
                "--ollama-model",
                "tev1:4b",
            ])
            .env("AUTOROUTER_OLLAMA_TIMEOUT_MS", "2400"),
    );
    assert!(success(&result).contains("tev1:4b; routing deadline 2400 ms per request"));
    assert_eq!(local.home.saved()["AUTOROUTER_OLLAMA_TIMEOUT_MS"], "2400");
    local.model(DEFAULT_OLLAMA_MODEL);
    let result = output(
        local
            .command()
            .args([
                "setup",
                "--force",
                "--evaluator",
                "ollama",
                "--ollama-model",
                DEFAULT_OLLAMA_MODEL,
            ])
            .env("AUTOROUTER_OLLAMA_TIMEOUT_MS", "8000"),
    );
    assert!(success(&result).contains("nimble:9b-q4_K_M; routing deadline 8000 ms per request"));
    let saved = local.home.saved();
    assert_eq!(saved["AUTOROUTER_OLLAMA_MODEL"], DEFAULT_OLLAMA_MODEL);
    assert_eq!(saved["AUTOROUTER_OLLAMA_TIMEOUT_MS"], "8000");
    assert_eq!(
        read_config(&saved, false, &local.home.0)
            .unwrap()
            .ollama_timeout_ms,
        8000
    );
    let mut override_env = saved.clone();
    override_env["AUTOROUTER_OLLAMA_TIMEOUT_MS"] = json!("600");
    assert_eq!(
        read_config(&override_env, false, &local.home.0)
            .unwrap()
            .ollama_timeout_ms,
        600
    );
    local.model("tev1:4b");
    success(&output(
        local
            .command()
            .args([
                "setup",
                "--force",
                "--evaluator",
                "ollama",
                "--ollama-model",
                "tev1:4b",
            ])
            .env("AUTOROUTER_OLLAMA_TIMEOUT_MS", "7000"),
    ));
    let before = fs::read(local.home.config()).unwrap();
    local.reset();
    let result = output(
        local
            .command()
            .arg("doctor")
            .env("SYNTHETIC_CLAUDE_VERSION", "2.1.284")
            .env("AUTOROUTER_OLLAMA_TIMEOUT_MS", "9000"),
    );
    assert!(success(&result).contains("tev1:4b; routing deadline 9000 ms per request"));
    assert_eq!(
        local.server.paths(),
        ["/api/version", "/api/tags", "/api/show"]
    );
    assert_eq!(local.auth_calls(), ["--version", "auth status --json"]);
    assert_eq!(local.home.saved()["AUTOROUTER_OLLAMA_TIMEOUT_MS"], "7000");
    assert_eq!(fs::read(local.home.config()).unwrap(), before);
}
#[test]
fn explicit_zero_deadline_overrides_environment_and_doctor_never_warms() {
    let local = Local::new("tev1:4b");
    let result = output(
        local
            .command()
            .args([
                "setup",
                "--evaluator",
                "ollama",
                "--ollama-model",
                "tev1:4b",
                "--ollama-timeout-ms",
                "0",
            ])
            .env("AUTOROUTER_OLLAMA_TIMEOUT_MS", "1500"),
    );
    let text = success(&result);
    assert_eq!(local.home.saved()["AUTOROUTER_OLLAMA_TIMEOUT_MS"], "0");
    assert_eq!(
        read_config(&local.home.saved(), false, &local.home.0)
            .unwrap()
            .ollama_timeout_ms,
        0
    );
    assert!(text.contains("tev1:4b; routing deadline disabled."));
    assert!(!text.contains("deadline 0 ms"));
    assert_eq!(
        local
            .server
            .paths()
            .iter()
            .filter(|p| p.as_str() == "/v1/systemone")
            .count(),
        1
    );
    local.reset();
    let text = success(&output(
        local
            .command()
            .arg("doctor")
            .env("SYNTHETIC_CLAUDE_VERSION", "2.1.284"),
    ));
    assert!(text.contains("tev1:4b; routing deadline disabled."));
    assert_eq!(
        local.server.paths(),
        ["/api/version", "/api/tags", "/api/show"]
    );
    let text = success(&output(
        local
            .command()
            .args([
                "setup",
                "--force",
                "--evaluator",
                "ollama",
                "--ollama-timeout-ms",
                "2200",
            ])
            .env("AUTOROUTER_OLLAMA_TIMEOUT_MS", "0"),
    ));
    assert_eq!(local.home.saved()["AUTOROUTER_OLLAMA_TIMEOUT_MS"], "2200");
    assert!(text.contains("routing deadline 2200 ms per request"));
}
#[test]
fn failed_ollama_setup_and_custom_model_repair_leave_existing_configuration_intact() {
    let local = Local::new(DEFAULT_OLLAMA_MODEL);
    let original = json!({"AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_CLIENT_PROFILE":"compatible","AUTOROUTER_EVALUATOR":"jev","TYPESAFE_API_KEY":"original-key","AUTOROUTER_SECRET_STORE":"file"});
    local.home.save(&original);
    let before = fs::read(local.home.config()).unwrap();
    local.state.lock().unwrap().installed = false;
    let result = output(
        local
            .command()
            .args(["setup", "--force", "--evaluator", "ollama"]),
    );
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("--pull"));
    assert_eq!(local.server.paths(), ["/api/version", "/api/tags"]);
    assert_eq!(fs::read(local.home.config()).unwrap(), before);
    for args in [
        vec!["--pull"],
        vec!["--evaluator", "unknown"],
        vec!["--evaluator", "ollama", "--ollama-preset"],
        vec!["--evaluator", "ollama", "--ollama-model"],
    ] {
        let fresh = Local::new(DEFAULT_OLLAMA_MODEL);
        let result = output(
            fresh
                .command()
                .arg("setup")
                .args(args)
                .env("AUTOROUTER_EVALUATOR", "jev"),
        );
        assert!(!result.status.success());
        assert!(fresh.server.calls().is_empty());
        assert!(!fresh.home.config().exists());
        assert_eq!(fs::read(local.home.config()).unwrap(), before);
    }
    let custom = json!({"AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_OLLAMA_MODEL":"team/router:v2","AUTOROUTER_OLLAMA_URL":local.server.url(),"AUTOROUTER_SECRET_STORE":"file"});
    local.home.save(&custom);
    local.model("team/router:v2");
    local.reset();
    let result = output(
        local
            .command()
            .arg("doctor")
            .env("SYNTHETIC_CLAUDE_VERSION", "2.1.999"),
    );
    assert!(!result.status.success());
    assert!(result.stderr.is_empty());
    let text = String::from_utf8(result.stdout).unwrap();
    assert!(text.contains("setup --evaluator ollama --ollama-model team/router:v2 --pull --force"));
    assert!(text.contains("2.1.284") && text.contains("2.1.285"));
    assert!(text.contains("does not certify its full compatibility"));
    assert_eq!(local.server.paths(), ["/api/version", "/api/tags"]);
    assert_eq!(local.home.saved(), custom);
}
#[test]
fn forced_ollama_updates_keep_all_unrelated_saved_settings_and_runtime_overrides_ephemeral() {
    let local = Local::new("tev1:4b-q4_K_M");
    let saved = json!({"AUTOROUTER_AUTH_MODE":"api-key","AUTOROUTER_CLIENT_PROFILE":"native","AUTOROUTER_EVALUATOR":"ollama","ANTHROPIC_API_KEY":"saved-anthropic","TYPESAFE_API_KEY":"saved-jev","AUTOROUTER_TOKEN":"saved-private-token","AUTOROUTER_OLLAMA_MODEL":"tev1:4b-q4_K_M","AUTOROUTER_OLLAMA_TIMEOUT_MS":"0","AUTOROUTER_OLLAMA_URL":local.server.url(),"AUTOROUTER_SECRET_STORE":"file","AUTOROUTER_PORT":"8123","AUTOROUTER_MIN_CONFIDENCE":"0.9","AUTOROUTER_SONNET_MODEL":"claude-sonnet-5-5","ENABLE_TOOL_SEARCH":"auto:5","AUTOROUTER_STATUSLINE":"0","AUTOROUTER_DEBUG":"1","CLAUDE_CODE_STOP_HOOK_BLOCK_CAP":"2","AUTOROUTER_SESSION_LOG_DIR":local.home.0.join("logs"),"AUTOROUTER_SESSION_LOG_MODE":"metadata"});
    local.home.save(&saved);
    let mut without_overrides = saved.clone();
    without_overrides["AUTOROUTER_OLLAMA_TIMEOUT_MS"] = json!("8000");
    success(&output(local.command().args([
        "setup",
        "--force",
        "--ollama-timeout-ms",
        "8000",
    ])));
    assert_eq!(local.home.saved(), without_overrides);
    assert!(local.server.paths().iter().any(|path| path == "/api/show"));
    let saved = json!({"AUTOROUTER_AUTH_MODE":"api-key","AUTOROUTER_CLIENT_PROFILE":"native","AUTOROUTER_EVALUATOR":"ollama","ANTHROPIC_API_KEY":"saved-anthropic","TYPESAFE_API_KEY":"saved-jev","AUTOROUTER_TOKEN":"saved-private-token","AUTOROUTER_OLLAMA_MODEL":"tev1:4b-q4_K_M","AUTOROUTER_OLLAMA_TIMEOUT_MS":"0","AUTOROUTER_OLLAMA_URL":local.server.url(),"AUTOROUTER_SECRET_STORE":"file","AUTOROUTER_PORT":"8123","AUTOROUTER_DEBUG":"0","AUTOROUTER_STATUSLINE":"1","AUTOROUTER_SESSION_LOG_MODE":"metadata"});
    local.home.save(&saved);
    local.reset();
    let before = fs::read(local.home.config()).unwrap();
    let text = success(&output(
        local
            .command()
            .args(["setup", "--force", "--ollama-timeout-ms", "8000"])
            .env("AUTOROUTER_AUTH_MODE", "subscription")
            .env("AUTOROUTER_CLIENT_PROFILE", "auto")
            .env("AUTOROUTER_EVALUATOR", "jev")
            .env("ANTHROPIC_API_KEY", "temporary-anthropic")
            .env("TYPESAFE_API_KEY", "temporary-jev")
            .env("AUTOROUTER_TOKEN", "temporary-private-token")
            .env("AUTOROUTER_PORT", "9123")
            .env("AUTOROUTER_DEBUG", "1")
            .env("AUTOROUTER_STATUSLINE", "0")
            .env("AUTOROUTER_SESSION_LOG_MODE", "prompts")
            .env("AUTOROUTER_OLLAMA_MODEL", DEFAULT_OLLAMA_MODEL)
            .env("AUTOROUTER_OLLAMA_TIMEOUT_MS", "200"),
    ));
    let mut expected = saved;
    expected["AUTOROUTER_OLLAMA_TIMEOUT_MS"] = json!("8000");
    assert_eq!(local.home.saved(), expected);
    assert!(!text.contains("temporary-"));
    let calls = local.server.calls();
    assert!(calls.iter().any(|r| r.path == "/api/show"));
    assert!(
        calls
            .iter()
            .filter(|r| r.path == "/api/show")
            .all(|r| r.json()["model"] == "tev1:4b-q4_K_M")
    );
    assert_eq!(local.state.lock().unwrap().config_at_warm, [Some(before)]);
    assert!(!local.home.0.join("logs").exists());
}
#[test]
fn explicitly_selected_backend_and_auth_use_environment_values_without_overwriting_unrelated_preferences()
 {
    let local = Local::new("tev1:0.8b");
    let saved = json!({"AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_CLIENT_PROFILE":"native","AUTOROUTER_EVALUATOR":"jev","TYPESAFE_API_KEY":"saved-jev","ANTHROPIC_API_KEY":"saved-anthropic","AUTOROUTER_PORT":"8123","AUTOROUTER_SECRET_STORE":"file"});
    local.home.save(&saved);
    let command = || {
        let mut c = local.command();
        c.env("AUTOROUTER_OLLAMA_MODEL", "tev1:0.8b")
            .env("TYPESAFE_API_KEY", "new-selected-jev")
            .env("ANTHROPIC_API_KEY", "new-selected-anthropic")
            .env("AUTOROUTER_PORT", "9123")
            .env("AUTOROUTER_CLIENT_PROFILE", "auto")
            .env("AUTOROUTER_JEV_MODEL", "jev-explicit-model")
            .env("AUTOROUTER_MIN_CONFIDENCE", "0.85");
        c
    };
    success(&output(command().args([
        "setup",
        "--force",
        "--evaluator",
        "ollama",
        "--auth-mode",
        "api-key",
    ])));
    let mut expected = saved;
    expected["AUTOROUTER_EVALUATOR"] = json!("ollama");
    expected["AUTOROUTER_AUTH_MODE"] = json!("api-key");
    expected["AUTOROUTER_OLLAMA_MODEL"] = json!("tev1:0.8b");
    expected["AUTOROUTER_OLLAMA_URL"] = json!(local.server.url());
    expected["ANTHROPIC_API_KEY"] = json!("new-selected-anthropic");
    assert_eq!(local.home.saved(), expected);
    local.reset();
    success(&output(command().args([
        "setup",
        "--force",
        "--evaluator",
        "jev",
    ])));
    let actual = local.home.saved();
    assert_eq!(actual["TYPESAFE_API_KEY"], "new-selected-jev");
    assert_eq!(actual["AUTOROUTER_JEV_MODEL"], "jev-explicit-model");
    assert_eq!(actual["AUTOROUTER_MIN_CONFIDENCE"], "0.85");
    assert_eq!(actual["AUTOROUTER_PORT"], "8123");
    assert_eq!(actual["AUTOROUTER_CLIENT_PROFILE"], "native");
    assert!(local.server.calls().is_empty());
}
#[test]
fn invalid_stop_cap_admission_matrix_rejects_before_prompts_network_or_persistence() {
    const ERROR: &str =
        "requires a nonnegative safe integer (0 disables the Stop-hook continuation cap)";
    let rejected = |local: &Local, command: &mut Command, source: &str, scenario: &str| {
        let before = fs::read(local.home.config()).ok();
        let names = local.home.names();
        let result = output(command);
        assert_eq!(result.status.code(), Some(1), "{scenario}");
        assert!(
            result.stdout.is_empty(),
            "Prompt/disclosure before {scenario}"
        );
        assert_eq!(
            std::str::from_utf8(&result.stderr).unwrap(),
            format!("{source} {ERROR}\n"),
            "{scenario}"
        );
        assert_eq!(fs::read(local.home.config()).ok(), before, "{scenario}");
        assert_eq!(local.home.names(), names, "{scenario}");
        assert!(local.server.calls().is_empty(), "Provider call: {scenario}");
        assert!(local.auth_calls().is_empty(), "Claude call: {scenario}");
    };
    let mut scenarios = 0;
    for evaluator in ["jev", "ollama"] {
        let local = Local::new(DEFAULT_OLLAMA_MODEL);
        // Both possible providers stay on loopback even if admission regresses.
        // No keys are supplied: reaching the first hidden prompt is a failure.
        let command = || {
            let mut command = local.command();
            command.env(
                "AUTOROUTER_JEV_URL",
                format!("{}/v1/systemone", local.server.url()),
            );
            command
        };
        for value in [
            None,
            Some(""),
            Some(" "),
            Some("--pull"),
            Some("-1"),
            Some("1.5"),
            Some("2.0"),
            Some("1e2"),
            Some("1e-999"),
            Some("Infinity"),
            Some("9007199254740992"),
        ] {
            rejected(
                &local,
                command()
                    .args(["setup", "--evaluator", evaluator, "--stop-hook-block-cap"])
                    .args(value),
                "--stop-hook-block-cap",
                &format!("{evaluator} CLI {value:?}"),
            );
            assert!(!local.home.config().exists());
            scenarios += 1;
        }
        // OS environment values are strings. The baseline's injected null/false
        // objects are separate embedding adaptations, not aliases for these cases.
        for value in ["", " ", "-1", "1.5", "1e2", "9007199254740992"] {
            rejected(
                &local,
                command()
                    .args(["setup", "--evaluator", evaluator])
                    .env("CLAUDE_CODE_STOP_HOOK_BLOCK_CAP", value),
                "CLAUDE_CODE_STOP_HOOK_BLOCK_CAP",
                &format!("{evaluator} environment {value:?}"),
            );
            assert!(!local.home.config().exists());
            scenarios += 1;
        }
    }
    let local = Local::new(DEFAULT_OLLAMA_MODEL);
    success(&output(
        local
            .command()
            .args(["setup", "--evaluator", "jev"])
            .env(
                "AUTOROUTER_JEV_URL",
                format!("{}/v1/systemone", local.server.url()),
            )
            .env("TYPESAFE_API_KEY", "synthetic-saved-jev-key"),
    ));
    assert_eq!(local.home.saved()["AUTOROUTER_EVALUATOR"], "jev");
    rejected(
        &local,
        local
            .command()
            .args([
                "setup",
                "--force",
                "--evaluator",
                "ollama",
                "--stop-hook-block-cap",
                "",
            ])
            .env("CLAUDE_CODE_STOP_HOOK_BLOCK_CAP", ""),
        "--stop-hook-block-cap",
        "Invalid forced evaluator change preserves the exact saved bytes",
    );
    scenarios += 1;
    assert_eq!(scenarios, 35);
}

#[test]
fn stop_cap_cli_precedence_normalization_and_doctor_overrides_work_for_both_evaluators() {
    for evaluator in ["jev", "ollama"] {
        let local = Local::new(DEFAULT_OLLAMA_MODEL);
        let command = || {
            let mut c = local.command();
            c.env("TYPESAFE_API_KEY", "synthetic-jev-key")
                .env("CLAUDE_CODE_STOP_HOOK_BLOCK_CAP", "9");
            c
        };
        let text = success(&output(command().args([
            "setup",
            "--evaluator",
            evaluator,
            "--stop-hook-block-cap",
            "0002",
        ])));
        let saved = local.home.saved();
        assert_eq!(saved["CLAUDE_CODE_STOP_HOOK_BLOCK_CAP"], "2");
        assert_eq!(saved["AUTOROUTER_EVALUATOR"], evaluator);
        assert_eq!(
            read_config(&saved, false, &local.home.0)
                .unwrap()
                .stop_hook_block_cap,
            Some(2)
        );
        let mut env = saved;
        env["CLAUDE_CODE_STOP_HOOK_BLOCK_CAP"] = json!("0");
        assert_eq!(
            read_config(&env, false, &local.home.0)
                .unwrap()
                .stop_hook_block_cap,
            Some(0)
        );
        assert!(text.contains("Claude Stop/SubagentStop cap: 2 continuations without tool use"));
        let before = fs::read(local.home.config()).unwrap();
        let text = success(&output(
            local
                .command()
                .arg("doctor")
                .env("CLAUDE_CODE_STOP_HOOK_BLOCK_CAP", "0"),
        ));
        assert!(text.contains("Claude Stop/SubagentStop continuation cap disabled (0)"));
        assert_eq!(fs::read(local.home.config()).unwrap(), before);
        let text = success(&output(command().args([
            "setup",
            "--force",
            "--evaluator",
            evaluator,
            "--stop-hook-block-cap",
            "0",
        ])));
        assert_eq!(local.home.saved()["CLAUDE_CODE_STOP_HOOK_BLOCK_CAP"], "0");
        assert!(text.contains("Claude Stop/SubagentStop continuation cap disabled (0)"));
        if evaluator == "ollama" {
            assert_eq!(
                local
                    .server
                    .paths()
                    .iter()
                    .filter(|p| p.as_str() == "/v1/systemone")
                    .count(),
                2
            )
        } else {
            assert!(local.server.calls().is_empty())
        }
    }
    for (raw, expected) in [("0002", "2"), ("0000", "0")] {
        let local = Local::new(DEFAULT_OLLAMA_MODEL);
        success(&output(
            local
                .command()
                .args(["setup", "--evaluator", "jev"])
                .env("TYPESAFE_API_KEY", "synthetic-jev-key")
                .env("CLAUDE_CODE_STOP_HOOK_BLOCK_CAP", raw),
        ));
        assert_eq!(
            local.home.saved()["CLAUDE_CODE_STOP_HOOK_BLOCK_CAP"],
            expected
        );
        assert!(local.server.calls().is_empty());
    }
}

#[test]
fn stop_cap_doctor_reports_absent_saved_and_runtime_values_without_inference() {
    let local = Local::new(DEFAULT_OLLAMA_MODEL);
    success(&output(
        local
            .command()
            .args(["setup", "--evaluator", "jev"])
            .env("TYPESAFE_API_KEY", "synthetic-jev-key"),
    ));
    let text = success(&output(local.command().arg("doctor")));
    assert!(!text.contains("Stop/SubagentStop"));
    success(&output(
        local
            .command()
            .args(["setup", "--force", "--stop-hook-block-cap", "2"])
            .env("TYPESAFE_API_KEY", "synthetic-jev-key"),
    ));
    let saved = fs::read(local.home.config()).unwrap();
    let text = success(&output(local.command().arg("doctor")));
    assert!(text.contains("Claude Stop/SubagentStop cap: 2 continuations without tool use"));
    let text = success(&output(
        local
            .command()
            .arg("doctor")
            .env("CLAUDE_CODE_STOP_HOOK_BLOCK_CAP", "0"),
    ));
    assert!(text.contains("Claude Stop/SubagentStop continuation cap disabled (0)"));
    assert_eq!(fs::read(local.home.config()).unwrap(), saved);
    assert_eq!(
        local.auth_calls(),
        [
            "--version",
            "auth status --json",
            "--version",
            "auth status --json",
            "--version",
            "auth status --json"
        ]
    );
    assert!(local.server.calls().is_empty());
}

#[test]
fn environment_only_ollama_doctor_checks_availability_without_warming_or_creating_config() {
    let local = Local::new(DEFAULT_OLLAMA_MODEL);
    let command = || {
        let mut command = local.command();
        command
            .arg("doctor")
            .env_remove("AUTOROUTER_CONFIG")
            .env("AUTOROUTER_EVALUATOR", "ollama")
            .env("SYNTHETIC_CLAUDE_VERSION", "2.1.284")
            .env("AUTOROUTER_AUTH_MODE", "subscription");
        command
    };
    let text = success(&output(&mut command()));
    assert!(text.contains("absent; using environment"));
    assert!(text.contains("Local Ollama model available"));
    assert!(text.contains("nimble:9b-q4_K_M; routing deadline 30000 ms per request"));
    assert!(text.contains("classification speed and accuracy are not tested"));
    assert_eq!(
        local.server.paths(),
        ["/api/version", "/api/tags", "/api/show"]
    );
    assert_eq!(local.auth_calls(), ["--version", "auth status --json"]);
    assert!(!local.home.config().exists());
    assert!(!local.home.0.join("claude-autorouter").exists());
    local.reset();
    local.state.lock().unwrap().installed = false;
    let result = output(&mut command());
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stdout).contains("--pull"));
    assert!(result.stderr.is_empty());
    assert_eq!(local.server.paths(), ["/api/version", "/api/tags"]);
    assert!(!local.home.config().exists());
    assert!(!local.home.0.join("claude-autorouter").exists());
}

#[test]
fn jev_auto_doctor_reports_routing_profile_without_permission_eligibility_claims() {
    let local = Local::new(DEFAULT_OLLAMA_MODEL);
    success(&output(
        local
            .command()
            .args(["setup", "--evaluator", "jev", "--client-profile", "auto"])
            .env("TYPESAFE_API_KEY", "synthetic-jev-key"),
    ));
    let before = fs::read(local.home.config()).unwrap();
    let text = success(&output(local.command().arg("doctor")));
    assert!(text.contains("Auto-compatible profile: Sonnet/Opus task routing"));
    assert!(text.contains("Claude controls permission-mode availability and safety checks"));
    for claim in [
        "auto mode available",
        "auto mode is available",
        "auto mode enabled",
        "auto mode is enabled",
        "auto permission mode available",
        "auto permission mode is available",
        "auto permission mode enabled",
        "auto permission mode is enabled",
    ] {
        assert!(!text.to_ascii_lowercase().contains(claim));
    }
    assert_eq!(local.auth_calls(), ["--version", "auth status --json"]);
    assert_eq!(fs::read(local.home.config()).unwrap(), before);
    assert!(local.server.calls().is_empty());
}

#[test]
fn saved_subscription_doctor_scrubs_all_credentials_and_uses_the_exact_auth_sequence() {
    let local = Local::new(DEFAULT_OLLAMA_MODEL);
    success(&output(
        local
            .command()
            .args(["setup", "--evaluator", "jev"])
            .env("TYPESAFE_API_KEY", "jev-secret"),
    ));
    let before = fs::read(local.home.config()).unwrap();
    let result = output(
        local
            .command()
            .arg("doctor")
            .env("ANTHROPIC_API_KEY", "stale-api-secret")
            .env("ANTHROPIC_AUTH_TOKEN", "stale-token")
            .env("CLAUDE_CODE_OAUTH_TOKEN", "stale-oauth")
            .env("SYNTHETIC_CLAUDE_VERSION", "2.1.284"),
    );
    let text = success(&result);
    assert_eq!(local.auth_calls(), ["--version", "auth status --json"]);
    for secret in [
        "PRIVATE@example.invalid",
        "PRIVATE_AUTH_TOKEN",
        "jev-secret",
        "stale-token",
        "stale-api-secret",
        "stale-oauth",
    ] {
        assert!(!text.contains(secret));
    }
    assert!(local.server.calls().is_empty());
    assert_eq!(fs::read(local.home.config()).unwrap(), before);
}

#[test]
fn doctor_missing_login_provider_conflicts_and_child_errors_fail_without_private_diagnostics() {
    let local = Local::new(DEFAULT_OLLAMA_MODEL);
    success(&output(
        local
            .command()
            .args(["setup", "--evaluator", "jev"])
            .env("TYPESAFE_API_KEY", "jev-secret"),
    ));
    for status in [
        json!({"loggedIn":false}),
        json!({"loggedIn":true,"authMethod":"api_key"}),
    ] {
        local.home.claude(&format!("#!/bin/sh\nif [ \"$1\" = --version ]; then printf '2.1.284\\n'; else printf '%s\\n' '{}'; fi\n",status));
        let result = output(local.command().arg("doctor"));
        assert!(!result.status.success());
        assert!(result.stderr.is_empty());
    }
    local
        .home
        .claude("#!/bin/sh\nprintf 'private-error-secret\\n' >&2\nexit 29\n");
    let result = output(local.command().arg("doctor"));
    assert!(!result.status.success());
    assert!(result.stderr.is_empty());
    assert!(!String::from_utf8_lossy(&result.stdout).contains("private-error-secret"));
    local.home.claude("#!/bin/sh\nif [ \"$1\" = --version ]; then printf '2.1.284\\n'; else printf '{\"loggedIn\":true,\"authMethod\":\"claude.ai\"}\\n'; fi\n");
    let result = output(
        local
            .command()
            .arg("doctor")
            .env("CLAUDE_CODE_USE_VERTEX", "1"),
    );
    assert!(!result.status.success());
    assert!(result.stderr.is_empty());
    assert!(local.server.calls().is_empty());
}

#[test]
fn environment_only_api_doctor_checks_only_claude_version_and_never_creates_config() {
    let local = Local::new(DEFAULT_OLLAMA_MODEL);
    let text = success(&output(
        local
            .command()
            .arg("doctor")
            .env_remove("AUTOROUTER_CONFIG")
            .env("AUTOROUTER_EVALUATOR", "jev")
            .env("TYPESAFE_API_KEY", "jev-secret")
            .env("ANTHROPIC_API_KEY", "api-secret")
            .env("SYNTHETIC_CLAUDE_VERSION", "2.1.284"),
    ));
    assert_eq!(local.auth_calls(), ["--version"]);
    assert!(text.contains("absent; using environment"));
    assert!(text.contains("key validity") && text.contains("not tested"));
    assert!(local.server.calls().is_empty());
    assert!(!local.home.config().exists());
}

#[test]
fn logging_setup_resolves_cli_directory_and_disables_without_creating_history() {
    let local = Local::new(DEFAULT_OLLAMA_MODEL);
    let directory = fs::canonicalize(&local.home.0)
        .unwrap()
        .join("decision logs");
    let ignored = local.home.0.join("ignored logs");
    let mut command = local.home.command();
    command
        .args([
            "setup",
            "--session-log-dir",
            "./decision logs",
            "--session-log-mode",
            "prompts",
        ])
        .env("AUTOROUTER_EVALUATOR", "jev")
        .env("AUTOROUTER_JEV_URL", local.server.url())
        .env("TYPESAFE_API_KEY", "synthetic-jev-key")
        .env("AUTOROUTER_SESSION_LOG_DIR", &ignored);
    let environment = command
        .get_envs()
        .map(|(key, value)| (key.to_os_string(), value.map(std::ffi::OsStr::to_os_string)))
        .collect::<Vec<_>>();
    let text = success(&output(&mut command));
    assert!(text.contains("Session decision logs enabled"));
    assert_eq!(
        command
            .get_envs()
            .map(|(key, value)| (key.to_os_string(), value.map(std::ffi::OsStr::to_os_string)))
            .collect::<Vec<_>>(),
        environment
    );
    let before = fs::read(local.home.config()).unwrap();
    let saved = local.home.saved();
    assert_eq!(saved["AUTOROUTER_SESSION_LOG_DIR"], json!(directory));
    assert_eq!(saved["AUTOROUTER_SESSION_LOG_MODE"], "prompts");
    assert_eq!(
        read_config(&saved, false, &local.home.0)
            .unwrap()
            .session_log_dir,
        Some(directory.to_str().unwrap().to_owned())
    );
    let mut disabled = saved;
    disabled["AUTOROUTER_SESSION_LOG_DIR"] = json!("");
    assert!(
        read_config(&disabled, false, &local.home.0)
            .unwrap()
            .session_log_dir
            .is_none()
    );
    assert!(!directory.exists());
    assert!(!ignored.exists());
    assert!(local.server.calls().is_empty());
    assert!(local.auth_calls().is_empty());

    let text = success(&output(local.home.command().arg("doctor")));
    assert!(text.contains("Session decision logs enabled"));
    assert_eq!(local.auth_calls(), ["--version", "auth status --json"]);
    assert!(local.server.calls().is_empty());
    assert!(!directory.exists());
    assert_eq!(fs::read(local.home.config()).unwrap(), before);
    let text = success(&output(
        local
            .home
            .command()
            .arg("doctor")
            .env("AUTOROUTER_SESSION_LOG_DIR", ""),
    ));
    assert!(!text.contains("Session decision logs enabled"));
    assert_eq!(fs::read(local.home.config()).unwrap(), before);

    success(&output(
        local
            .home
            .command()
            .args(["setup", "--force", "--session-log-dir", ""])
            .env("AUTOROUTER_SESSION_LOG_DIR", &ignored),
    ));
    assert_eq!(local.home.saved()["AUTOROUTER_SESSION_LOG_DIR"], "");
    assert!(!directory.exists());
    assert!(!ignored.exists());
    assert!(local.server.calls().is_empty());
}

#[test]
fn setup_missing_keys_prompt_in_order_and_reject_blank_cancelled_or_nonterminal_input() {
    let local = Local::new(DEFAULT_OLLAMA_MODEL);
    let command = || {
        let mut command = local.home.command();
        command
            .env("AUTOROUTER_EVALUATOR", "jev")
            .env("AUTOROUTER_JEV_URL", local.server.url());
        command
    };
    let result = hidden_inputs(
        command().args(["setup", "--auth-mode", "api-key"]),
        &[
            ("TYPESAFE_API_KEY", HiddenInput::Secret("jev-secret")),
            ("ANTHROPIC_API_KEY", HiddenInput::Secret("")),
        ],
    );
    assert!(!result.status.success());
    let visible = String::from_utf8_lossy(&result.stderr);
    assert_eq!(visible.matches("(hidden): ").count(), 2);
    assert!(
        visible.find("TYPESAFE_API_KEY (hidden): ").unwrap()
            < visible.find("ANTHROPIC_API_KEY (hidden): ").unwrap()
    );
    assert!(visible.contains("ANTHROPIC_API_KEY must be a nonempty, single-line key"));
    assert!(!String::from_utf8_lossy(&result.stdout).contains("jev-secret"));
    assert!(!local.home.config().exists());

    let result = hidden_inputs(
        command().arg("setup"),
        &[("TYPESAFE_API_KEY", HiddenInput::Cancel)],
    );
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Setup cancelled"));
    assert!(!local.home.config().exists());

    let result = output(command().args(["setup", "--key", "private-secret-sentinel"]));
    assert!(!result.status.success());
    assert!(!String::from_utf8_lossy(&result.stderr).contains("private-secret-sentinel"));
    assert!(!String::from_utf8_lossy(&result.stdout).contains("private-secret-sentinel"));
    assert!(!local.home.config().exists());

    let result = output(command().arg("setup"));
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("environment for noninteractive setup")
    );
    assert!(!local.home.config().exists());
    assert!(local.server.calls().is_empty());
    assert!(local.auth_calls().is_empty());
}

#[test]
fn replacement_discards_old_backend_auth_preferences_and_prompts_only_for_jev() {
    let local = Local::new(DEFAULT_OLLAMA_MODEL);
    local.home.save(&json!({
        "AUTOROUTER_AUTH_MODE":"api-key",
        "AUTOROUTER_EVALUATOR":"ollama",
        "AUTOROUTER_CLIENT_PROFILE":"native",
        "ANTHROPIC_API_KEY":"discarded-secret",
        "TYPESAFE_API_KEY":"old-secret",
        "AUTOROUTER_PORT":"8123",
        "AUTOROUTER_OLLAMA_MODEL":"tev1:4b",
        "CLAUDE_CODE_STOP_HOOK_BLOCK_CAP":"2"
    }));
    let result = hidden_secret(
        local
            .home
            .command()
            .args(["setup", "--replace"])
            .env("AUTOROUTER_EVALUATOR", "jev")
            .env("AUTOROUTER_JEV_URL", local.server.url()),
        "TYPESAFE_API_KEY",
        "replacement-secret",
    );
    assert!(result.status.success());
    let visible = String::from_utf8_lossy(&result.stderr);
    assert_eq!(visible.matches("(hidden): ").count(), 1);
    assert!(!visible.contains("ANTHROPIC_API_KEY"));
    assert_eq!(
        local.home.saved(),
        json!({
            "AUTOROUTER_AUTH_MODE":"subscription",
            "AUTOROUTER_CLIENT_PROFILE":"compatible",
            "AUTOROUTER_EVALUATOR":"jev",
            "TYPESAFE_API_KEY":"replacement-secret",
            "AUTOROUTER_SECRET_STORE":"file",
            "AUTOROUTER_JEV_URL":local.server.url()
        })
    );
    assert!(local.server.calls().is_empty());
    assert!(local.auth_calls().is_empty());
    for secret in ["discarded-secret", "old-secret", "replacement-secret"] {
        assert!(!String::from_utf8_lossy(&result.stdout).contains(secret));
        assert!(!visible.contains(secret));
    }
}

#[test]
fn invalid_setup_settings_fail_before_keys_and_cancelled_replacement_preserves_bytes() {
    let local = Local::new(DEFAULT_OLLAMA_MODEL);
    let command = || {
        let mut command = local.home.command();
        command
            .env("AUTOROUTER_EVALUATOR", "jev")
            .env("AUTOROUTER_JEV_URL", local.server.url());
        command
    };
    for (key, value) in [
        ("AUTOROUTER_PORT", ""),
        ("AUTOROUTER_JEV_URL", "private-invalid-url"),
        ("AUTOROUTER_DEBUG", "false"),
    ] {
        let result = output(command().arg("setup").env(key, value));
        assert!(!result.status.success());
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(error.contains(key));
        assert!(!error.contains("environment for noninteractive setup"));
        assert!(!error.contains("(hidden):"));
        assert!(!error.contains("private-invalid-url"));
        assert!(!String::from_utf8_lossy(&result.stdout).contains("private-invalid-url"));
        assert!(!local.home.config().exists());
        assert!(local.server.calls().is_empty());
        assert!(local.auth_calls().is_empty());
    }
    local.home.save(&json!({
        "AUTOROUTER_AUTH_MODE":"subscription",
        "TYPESAFE_API_KEY":"preserved-secret",
        "AUTOROUTER_PORT":"8123"
    }));
    let before = fs::read(local.home.config()).unwrap();
    let result = hidden_inputs(
        command().args(["setup", "--replace"]),
        &[("TYPESAFE_API_KEY", HiddenInput::Cancel)],
    );
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Setup cancelled"));
    assert_eq!(fs::read(local.home.config()).unwrap(), before);
    assert!(local.server.calls().is_empty());
    assert!(local.auth_calls().is_empty());
}

#[test]
fn default_ollama_failure_saves_nothing_and_only_implicit_selection_suggests_jev() {
    let home = Home::new();
    let server = Server::disconnecting();
    for explicit in [false, true] {
        server.clear();
        let mut command = home.command();
        command
            .arg("setup")
            .env("AUTOROUTER_OLLAMA_URL", server.url());
        if explicit {
            command.args(["--evaluator", "ollama"]);
        }
        let result = output(&mut command);
        assert!(!result.status.success());
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(error.contains("Cannot reach local Ollama"));
        assert_eq!(error.contains("setup --evaluator jev"), !explicit);
        assert!(!error.contains("environment for noninteractive setup"));
        assert!(!error.contains("(hidden):"));
        assert!(!error.contains("TYPESAFE_API_KEY"));
        assert!(!home.config().exists());
        assert_eq!(server.paths(), ["/api/version"]);
        assert_eq!(server.calls()[0].method, "GET");
    }
}

#[test]
fn prompt_fixture_closes_descendants_holding_output_after_the_leader_exits() {
    let home = Home::new();
    let script = r#"
        (
            trap '' HUP
            printf ready > "$1/started"
            while [ ! -f "$1/release" ]; do /bin/sleep 0.01; done
            printf escaped > "$1/escaped"
        ) &
        while [ ! -f "$1/started" ]; do /bin/sleep 0.01; done
        printf completed
        exit 23
    "#;
    let mut command = Command::new("/bin/sh");
    command
        .env_clear()
        .args(["-c", script, "synthetic-prompt-fixture"])
        .arg(&home.0);
    let started = std::time::Instant::now();
    let result = hidden_inputs(&mut command, &[]);
    assert_eq!(result.status.code(), Some(23));
    assert_eq!(result.stdout, b"completed");
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    home.write("release", b"released");
    std::thread::sleep(std::time::Duration::from_millis(40));
    assert!(!home.0.join("escaped").exists());
}
