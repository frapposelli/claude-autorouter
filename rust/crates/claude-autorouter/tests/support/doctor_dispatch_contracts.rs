#![cfg(unix)]
//! Actual doctor dispatch for frozen onboarding #31/#32 observable obligations.
//! Local fixtures only; injected JS reports/Error identity are explicit API migrations.
use crate::support::{Home, Request, Response, Server, output, quote, signalled_output};
use autorouter_core::config::read_config;
use nix::sys::signal::Signal;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    process::Output,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
const SOURCE_MODEL: &str = "tev1:0.8b";
const CUSTOM_MODEL: &str = "team/router:doctor-v2";
const STARTUP: &str = "Return the literal word ready.";
fn save(id: &str, value: Value) {
    if let Some(directory) = std::env::var_os("AUTOROUTER_DOCTOR_DISPATCH_REPORT_DIR") {
        let raw = serde_json::to_vec_pretty(&value).unwrap();
        assert!(raw.len() <= 1024 * 1024);
        let path = Path::new(&directory).join(format!("native-{id}.json"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        file.write_all(&raw).unwrap();
    }
}
fn home(model: &str, trap: bool) -> Home {
    let home = Home::new();
    home.save(&json!({"AUTOROUTER_AUTH_MODE":"api-key","AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_OLLAMA_MODEL":model}));
    assert!(
        read_config(&home.saved(), false, &home.0)
            .unwrap()
            .anthropic_key
            .is_none()
    );
    assert!(
        read_config(&home.saved(), false, &home.0)
            .unwrap()
            .jev_key
            .is_none()
    );
    if trap {
        home.claude(&format!(
            "#!/bin/sh\nprintf unexpected >> {}\nexit 93\n",
            quote(&home.0.join("claude-invocation"))
        ));
    } else {
        assert!(!home.0.join("claude").exists());
    }
    for executable in ["node", "security", "ollama"] {
        assert!(!home.0.join(executable).exists());
    }
    home
}
fn request_row(request: &Request) -> Value {
    assert!(request.body.len() <= 65536);
    json!({"path":request.path,"method":request.method,"headers":request.headers,"body_utf8":std::str::from_utf8(&request.body).unwrap()})
}
fn reply(request: &Request, model: &str, collapsed: bool) -> Response {
    for header in ["authorization", "x-api-key", "x-autorouter-token"] {
        assert!(!request.headers.contains_key(header));
    }
    let body = match request.path.as_str() {
        "/api/version" => {
            assert_eq!(request.method, "GET");
            json!({"version":"0.35.0"})
        }
        "/api/tags" | "/api/ps" => {
            assert_eq!(request.method, "GET");
            json!({"models":[{"name":model}]})
        }
        "/api/show" => {
            assert_eq!(request.method, "POST");
            assert_eq!(request.json()["model"], model);
            json!({"details":{"parameter_size":"0.8B"}})
        }
        "/v1/systemone" => {
            assert_eq!(request.method, "POST");
            let body = request.json();
            assert_eq!(body["model"], model);
            assert_eq!(body["keep_alive"], "5m");
            let fixtures = autorouter_runtime::local_diagnostic::local_diagnostic_cases();
            let tier = if body["state"]["current_task"] == STARTUP {
                "haiku"
            } else {
                let fixture = fixtures
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|fixture| fixture["prompt"] == body["state"]["current_task"])
                    .expect("Only packaged synthetic tasks");
                if collapsed {
                    "sonnet"
                } else {
                    fixture["expected"].as_str().unwrap()
                }
            };
            json!({"model":model,"answers":{"tier":{"type":"choice","choice":tier,"confidence":1,"probabilities":{"haiku":u8::from(tier=="haiku"),"sonnet":u8::from(tier=="sonnet"),"opus":u8::from(tier=="opus")}}},"usage":{"input_tokens":900,"output_tokens":1}})
        }
        _ => panic!("Unexpected diagnostic endpoint"),
    };
    Response::json(body)
}
fn output_row(result: &Output) -> Value {
    json!({"exit_code":result.status.code(),"stdout_utf8":std::str::from_utf8(&result.stdout).unwrap(),"stderr_utf8":std::str::from_utf8(&result.stderr).unwrap()})
}
fn executable_row() -> Value {
    let executable = std::env::var_os("AUTOROUTER_TEST_EXECUTABLE")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_claude-autorouter").into());
    let raw = fs::read(&executable).unwrap();
    json!({"path":executable.to_string_lossy(),"bytes":raw.len(),"sha256":format!("{:x}",Sha256::digest(raw))})
}
fn check_unchanged(home: &Home, before: &[u8], names: &[String]) {
    assert_eq!(fs::read(home.config()).unwrap(), before);
    assert_eq!(home.names(), names);
    assert!(!home.0.join("claude-invocation").exists());
}
fn json_diagnosis(id: &str, model: &str, collapsed: bool, trap: bool) {
    let home = home(model, trap);
    let before = fs::read(home.config()).unwrap();
    let names = home.names();
    let wire = Arc::new(Mutex::new(Vec::new()));
    let recorded = wire.clone();
    let requested_model = model.to_owned();
    let server = Server::new(move |request| {
        let response = reply(request, &requested_model, collapsed);
        let mut calls = recorded.lock().unwrap();
        assert!(calls.len() < 32);
        calls.push(json!({"request":request_row(request),"response_status":response.status,"response_content_type":response.kind,"response_utf8":std::str::from_utf8(&response.body).unwrap()}));
        response
    });
    let result = output(
        home.command()
            .args(["doctor", "--evaluate-local", "--json"])
            .env("AUTOROUTER_OLLAMA_URL", server.url()),
    );
    assert_eq!(result.status.code(), Some(i32::from(collapsed)));
    assert!(result.stderr.is_empty());
    let stdout = std::str::from_utf8(&result.stdout).unwrap();
    assert!(stdout.ends_with('\n'));
    assert_eq!(
        stdout.lines().count(),
        1,
        "Progress must not contaminate JSON"
    );
    let report: Value = serde_json::from_str(stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["type"], "local_evaluator_diagnostic");
    assert_eq!(report["evaluator_model"], model);
    assert_eq!(report["passed"], !collapsed);
    assert_eq!(report["gates"]["rubric"]["passed"], !collapsed);
    for gate in ["preflight", "startup", "cases", "evaluator"] {
        assert_eq!(report["gates"][gate]["passed"], true);
    }
    for key in ["paid_provider_calls", "downloads", "models_unloaded"] {
        assert_eq!(report[key], 0);
    }
    assert_eq!(report["configuration_changed"], false);
    let rows = report["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 6);
    assert_eq!(
        rows.iter()
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
    assert!(rows.iter().all(|row| row["current_task_matches"] == true));
    let paths = server.paths();
    assert_eq!(paths.iter().filter(|p| *p == "/v1/systemone").count(), 7);
    assert_eq!(paths.iter().filter(|p| *p == "/api/ps").count(), 7);
    assert!(paths.iter().all(|path| {
        [
            "/api/version",
            "/api/tags",
            "/api/show",
            "/api/ps",
            "/v1/systemone",
        ]
        .contains(&path.as_str())
    }));
    check_unchanged(&home, &before, &names);
    server.assert_clean();
    save(
        id,
        json!({"id":id,"source_definition":31,"model":model,"collapsed":collapsed,"claude_executable_present":trap,"isolated_path_utf8":home.0.to_string_lossy(),"saved_utf8":std::str::from_utf8(&before).unwrap(),"saved_after_utf8":String::from_utf8(fs::read(home.config()).unwrap()).unwrap(),"config_credential_facts":{"anthropic_key_absent":true,"jev_key_absent":true,"mode":"api-key","secret_store":"file"},"executable":executable_row(),"output":output_row(&result),"report":report,"wire":wire.lock().unwrap().clone(),"claude_marker_absent":true,"home_names_before":names,"home_names_after":home.names()}),
    );
}
#[test]
fn original31_actual_cli_without_claude_and_json_progress_isolation() {
    for (model, label) in [
        (SOURCE_MODEL, "source-model"),
        (CUSTOM_MODEL, "custom-model"),
    ] {
        for collapsed in [false, true] {
            json_diagnosis(
                &format!(
                    "original31-{label}-{}",
                    if collapsed { "failed" } else { "passed" }
                ),
                model,
                collapsed,
                false,
            );
        }
    }
    json_diagnosis("original31-claude-trap", SOURCE_MODEL, true, true);
}
#[test]
fn original32_cloud_backend_rejection_performs_zero_requests_and_claude_work() {
    for trap in [false, true] {
        let home = home(SOURCE_MODEL, trap);
        home.save(&json!({"AUTOROUTER_EVALUATOR":"jev"}));
        let before = fs::read(home.config()).unwrap();
        let names = home.names();
        let server =
            Server::new(|_| panic!("Cloud local-diagnostic rejection must not perform HTTP"));
        let result = output(
            home.command()
                .args(["doctor", "--evaluate-local", "--json"])
                .env("AUTOROUTER_OLLAMA_URL", server.url())
                .env(
                    "AUTOROUTER_JEV_URL",
                    format!("{}/v1/systemone", server.url()),
                ),
        );
        assert_eq!(result.status.code(), Some(1));
        assert!(result.stderr.is_empty());
        let stdout = std::str::from_utf8(&result.stdout).unwrap();
        assert!(stdout.ends_with('\n'));
        assert_eq!(stdout.lines().count(), 1);
        let report: Value = serde_json::from_str(stdout).unwrap();
        assert_eq!(
            report,
            json!({"schema_version":1,"type":"local_evaluator_diagnostic","passed":false,"error":{"code":"configuration_error","message":"Local evaluation requires AUTOROUTER_EVALUATOR=ollama. It does not call Jev or Anthropic."}})
        );
        assert!(server.calls().is_empty());
        check_unchanged(&home, &before, &names);
        server.assert_clean();
        let id = if trap {
            "original32-cloud-claude-trap"
        } else {
            "original32-cloud-no-claude"
        };
        save(
            id,
            json!({"id":id,"source_definition":32,"executable":executable_row(),"output":output_row(&result),"report":report,"claude_executable_present":trap,"claude_marker_absent":true,"wire":[],"saved_utf8":std::str::from_utf8(&before).unwrap(),"saved_after_utf8":String::from_utf8(fs::read(home.config()).unwrap()).unwrap(),"home_names_before":names,"home_names_after":home.names()}),
        );
    }
}
#[test]
fn original32_actual_dispatch_cancels_stalled_headers_and_response_body() {
    for signal in [Signal::SIGINT, Signal::SIGTERM] {
        for json_output in [false, true] {
            for partial_body in [false, true] {
                let home = home(SOURCE_MODEL, false);
                let before = fs::read(home.config()).unwrap();
                let names = home.names();
                let (ready_tx, ready_rx) = mpsc::channel();
                let (closed_tx, closed_rx) = mpsc::channel();
                let wire = Arc::new(Mutex::new(Vec::new()));
                let recorded = wire.clone();
                let server = Server::with_connection(move |request, stream| {
                    let response = reply(request, SOURCE_MODEL, false);
                    if request.path != "/v1/systemone" {
                        recorded.lock().unwrap().push(json!({"request":request_row(request),"response_status":response.status,"response_utf8":std::str::from_utf8(&response.body).unwrap()}));
                        return Some(response);
                    }
                    assert_eq!(request.json()["state"]["current_task"], STARTUP);
                    let prefix = if partial_body {
                        &response.body[..1]
                    } else {
                        &[]
                    };
                    let header = if partial_body {
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            response.body.len()
                        )
                    } else {
                        String::new()
                    };
                    if partial_body {
                        stream.write_all(header.as_bytes()).unwrap();
                        stream.write_all(prefix).unwrap();
                        stream.flush().unwrap();
                    }
                    recorded.lock().unwrap().push(json!({"request":request_row(request),"response_stalled":true,"partial_body":partial_body,"response_header_utf8":header,"actual_response_prefix_utf8":std::str::from_utf8(prefix).unwrap(),"declared_response_bytes":response.body.len(),"planned_response_utf8":std::str::from_utf8(&response.body).unwrap()}));
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
                    assert_eq!(result.stdout,"Checking the local evaluator; no Claude authentication or cloud requests are used.\nPreparing the local model with a separate 60-second startup deadline…\n".as_bytes());
                }
                let connection_closed = closed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                assert!(connection_closed);
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
                check_unchanged(&home, &before, &names);
                server.assert_clean();
                let id = format!(
                    "original32-cancel-{}-{}-{}",
                    if signal == Signal::SIGINT {
                        "sigint"
                    } else {
                        "sigterm"
                    },
                    if json_output { "json" } else { "text" },
                    if partial_body { "body" } else { "headers" }
                );
                save(
                    &id,
                    json!({"id":id,"source_definition":32,"executable":executable_row(),"output":output_row(&result),"signal":format!("{signal:?}"),"json":json_output,"partial_body":partial_body,"ready_after_complete_request":true,"response_connection_closed":connection_closed,"wire":wire.lock().unwrap().clone(),"saved_utf8":std::str::from_utf8(&before).unwrap(),"saved_after_utf8":String::from_utf8(fs::read(home.config()).unwrap()).unwrap(),"claude_executable_present":false,"claude_marker_absent":true,"home_names_before":names,"home_names_after":home.names(),"bounded_process_owner":"support::signalled_output observes an unreaped leader, cleans owned group, joins bounded output, and reaps."}),
                );
            }
        }
    }
}
