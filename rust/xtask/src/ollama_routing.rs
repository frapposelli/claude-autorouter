//! Explicit integration through the production router and installed local model.
use crate::evaluation::{digest, environment, runtime, write_report};
use autorouter_core::config::{
    Evaluator, RouterConfig, read_config, validate_ollama_endpoint, validate_ollama_model,
};
use autorouter_core::js_json::{JsDocument, JsNode};
use autorouter_core::prompt_state::build_ollama_state_document;
use autorouter_core::router::RouteOptions;
use autorouter_runtime::bounded_json::read_response_document;
use autorouter_runtime::evaluator::ollama_questions;
use autorouter_runtime::http_client::{HttpTransport, NativeHttpClient};
use autorouter_runtime::ollama_setup::{SetupError, SetupOptions, setup_ollama};
use autorouter_runtime::router::Router;
use bytes::Bytes;
use http_body_util::Full;
use hyper::Request;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
const TIERS: &[&str] = &["haiku", "sonnet", "opus"];
/// Tool callers retain setup error identity; CLI diagnostics display only the safe message.
#[derive(Debug)]
pub struct RoutingTestError {
    // Native tool callers can inspect identity; the CLI intentionally prints only message.
    #[allow(dead_code)]
    pub code: &'static str,
    pub message: String,
}
impl std::fmt::Display for RoutingTestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for RoutingTestError {}
impl From<String> for RoutingTestError {
    fn from(message: String) -> Self {
        Self {
            code: "ROUTING_TEST_ERROR",
            message,
        }
    }
}
impl From<&str> for RoutingTestError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
impl From<SetupError> for RoutingTestError {
    fn from(error: SetupError) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }
}
fn identity(model: &str) -> String {
    let model = model.strip_prefix("registry.ollama.ai/").unwrap_or(model);
    let model = model.strip_prefix("library/").unwrap_or(model);
    if model.rsplit('/').next().unwrap_or_default().contains(':') {
        model.into()
    } else {
        format!("{model}:latest")
    }
}
pub fn parse_args(args: &[String]) -> Result<Value, String> {
    let mut out = json!({});
    let mut i = 0;
    while i < args.len() {
        let flag = &args[i];
        if flag == "--help" {
            out["help"] = json!(true);
            i += 1;
            continue;
        }
        if !["--model", "--timeout-ms", "--output"].contains(&flag.as_str())
            || args
                .get(i + 1)
                .is_none_or(|v| v.is_empty() || v.starts_with("--"))
        {
            return Err("Unknown or incomplete option; use --help.".into());
        }
        out[&flag[2..]] = json!(args[i + 1]);
        i += 2;
    }
    if out["help"] != true && out.get("model").is_none() {
        return Err("--model is required; select one installed local model explicitly.".into());
    }
    Ok(out)
}
fn fixtures(bytes: &[u8]) -> Result<Value, String> {
    let cases: Value =
        serde_json::from_slice(bytes).map_err(|_| "Invalid checked-in routing fixtures.")?;
    let items = cases
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or("Invalid checked-in routing fixtures.")?;
    for (i, item) in items.iter().enumerate() {
        if !item["id"].as_str().is_some_and(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        }) || !item["expected"]
            .as_str()
            .is_some_and(|s| TIERS.contains(&s))
            || !item["prompt"].is_string()
            || items[..i].iter().any(|p| p["id"] == item["id"])
        {
            return Err("Invalid checked-in routing fixtures.".into());
        }
    }
    Ok(cases)
}
pub fn request(item: &Value, config: &RouterConfig) -> JsDocument {
    let mut messages = item["history"].as_array().cloned().unwrap_or_default();
    messages.push(json!({"role":"user","content":[{"type":"text","text":"<system-reminder>SYNTHETIC_REMINDER_ONLY: environment and session metadata. This background is unrelated to the current task.</system-reminder>"},{"type":"text","text":"<available-deferred-tools>SYNTHETIC_REMINDER_ONLY: no tools are enabled.</available-deferred-tools>"},{"type":"text","text":item["prompt"]}]}));
    JsDocument::parse(json!({"model":config.models.haiku,"max_tokens":32000,"stream":false,"thinking":{"type":"disabled"},"tools":[],"system":[{"type":"text","text":"Synthetic coding assistant guidance: explain changes clearly, inspect relevant code, and verify the requested behavior. ".repeat(110)}],"messages":messages}).to_string().as_bytes()).expect("synthetic request")
}
async fn resident_models<T: HttpTransport>(
    transport: &T,
    config: &RouterConfig,
    cancel: &CancellationToken,
) -> Result<Vec<String>, String> {
    let token = cancel.child_token();
    let _guard = token.clone().drop_guard();
    let action = async {
        let response = transport
            .request(
                Request::get(format!("{}/api/ps", config.ollama_endpoint))
                    .body(Full::new(Bytes::new()))
                    .map_err(|_| "Cannot inspect resident models on local Ollama.")?,
            )
            .await
            .map_err(|_| "Cannot inspect resident models on local Ollama.")?;
        if !response.status().is_success() {
            return Err("Cannot inspect resident models on local Ollama.".into());
        }
        let (parts, body) = response.into_parts();
        let doc = read_response_document(body, &parts.headers, 1024 * 1024, &token)
            .await
            .map_err(|_| "Ollama returned an invalid or oversized resident model list.")?;
        let Some(JsNode::Array(items)) = doc.get(doc.root(), "models").and_then(|id| doc.node(id))
        else {
            return Err("Ollama returned an invalid resident model list.".into());
        };
        items
            .iter()
            .map(|id| {
                doc.get(*id, "name")
                    .filter(|id| !matches!(doc.node(*id), Some(JsNode::Null)))
                    .or_else(|| doc.get(*id, "model"))
                    .and_then(|id| doc.string(id))
                    .and_then(|s| s.to_scalar())
                    .ok_or_else(|| "Ollama returned an invalid resident model list.".into())
            })
            .collect()
    };
    tokio::select! {biased;_=cancel.cancelled()=>Err("Routing test cancelled.".into()),_=tokio::time::sleep(Duration::from_secs(5))=>Err("Cannot inspect resident models on local Ollama.".into()),r=action=>r}
}
pub async fn run_tests<T: HttpTransport + 'static>(
    transport: Arc<T>,
    config: &RouterConfig,
    fixture: &[u8],
    cancel: &CancellationToken,
    write: &mut impl FnMut(String),
) -> Result<Value, RoutingTestError> {
    if config.evaluator != Evaluator::Ollama {
        return Err("This test requires the local Ollama evaluator.".into());
    }
    validate_ollama_endpoint(&config.ollama_endpoint)?;
    validate_ollama_model(&config.ollama_model)?;
    let cases = fixtures(fixture)?;
    let resident = resident_models(transport.as_ref(), config, cancel).await?;
    if resident
        .iter()
        .any(|m| identity(m) != identity(&config.ollama_model))
    {
        return Err("Other Ollama models are resident. Rerun when only the selected model or no model is loaded; this test leaves existing models running.".into());
    }
    let start = Instant::now();
    setup_ollama(
        transport.as_ref(),
        config,
        cancel,
        &SetupOptions::default(),
        write,
    )
    .await?;
    let mut report = json!({"type":"ollama_router_integration","timestamp":autorouter_runtime::server_events::timestamp(),"evaluator_model":config.ollama_model,"timeout_ms":config.ollama_timeout_ms,"protocol":"/v1/systemone","fixture_sha256":digest(fixture),"questions_sha256":digest(ollama_questions().to_string().as_bytes()),"resident_before":!resident.is_empty(),"warmup_ms":start.elapsed().as_millis(),"paid_provider_calls":0,"downloads":0,"model_unloaded_by_test":false,"keep_alive":config.ollama_keep_alive,"rows":[]});
    for item in cases.as_array().unwrap() {
        if cancel.is_cancelled() {
            return Err("Routing test cancelled.".into());
        }
        let body = Arc::new(request(item, config));
        let state = build_ollama_state_document(&body, config.ollama_state_chars);
        let router = Router::new(transport.clone(), config.clone());
        let decision = router
            .route(
                body,
                RouteOptions {
                    scope: format!("synthetic-{}", item["id"].as_str().unwrap()),
                    request_class: "main".into(),
                    ..Default::default()
                },
                &hyper::HeaderMap::new(),
                cancel,
                "",
            )
            .await
            .map_err(|_| "Routing test cancelled.")?;
        router.shutdown();
        let expected_model = match item["expected"].as_str().unwrap() {
            "haiku" => &config.models.haiku,
            "sonnet" => &config.models.sonnet,
            _ => &config.models.opus,
        };
        let category = if decision["source"] == "fallback" {
            "fallback"
        } else if decision["source"] != "ollama" {
            "unexpected_source"
        } else if decision["classified_tier"] != item["expected"] {
            "classification_mismatch"
        } else if decision["model"] != *expected_model || decision["reason"] != "classified" {
            "guard_override"
        } else {
            "pass"
        };
        let matches = state.current_task.to_well_formed() == item["prompt"].as_str().unwrap();
        let mut row = json!({"case":item["id"],"expected":item["expected"],"selected_model":decision["model"],"state_bytes":state.stringify().len(),"current_task_matches":matches,"result":if matches{category}else{"task_extraction_mismatch"}});
        for key in [
            "classified_tier",
            "source",
            "reason",
            "classifier_error",
            "classifier_status",
            "latency_ms",
        ] {
            if let Some(value) = decision.get(key) {
                row[key] = value.clone();
            }
        }
        write(format!(
            "{}: {}; evaluator={}, selected={}, source={}, reason={}{}, {}ms",
            item["id"].as_str().unwrap(),
            row["result"].as_str().unwrap(),
            row["classified_tier"].as_str().unwrap_or("none"),
            row["selected_model"].as_str().unwrap_or(""),
            row["source"].as_str().unwrap_or(""),
            row["reason"].as_str().unwrap_or(""),
            row["classifier_error"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|error| format!(", error={error}"))
                .unwrap_or_default(),
            row["latency_ms"]
        ));
        report["rows"].as_array_mut().unwrap().push(row);
    }
    let rows = report["rows"].as_array().unwrap();
    let missing: Vec<_> = TIERS
        .iter()
        .filter(|t| {
            !rows
                .iter()
                .any(|r| r["result"] == "pass" && r["classified_tier"] == **t)
        })
        .collect();
    let passed = missing.is_empty() && rows.iter().all(|r| r["result"] == "pass");
    report["missing_tiers"] = json!(missing);
    report["passed"] = json!(passed);
    Ok(report)
}
pub fn run(args: &[String], root: &Path) -> Result<bool, String> {
    let options = parse_args(args)?;
    if options["help"] == true {
        println!(
            "Usage: cargo xtask test-ollama-routing --model TAG [--timeout-ms N] [--output PATH]\nRuns synthetic requests through the production router and an already installed local Ollama model. Warms once, tests all tiers and new turns. No downloads, Jev/Anthropic calls, configuration changes or explicit model unload. Refuses unrelated resident models."
        );
        return Ok(true);
    }
    let environment = environment();
    let mut env = json!({"AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_OLLAMA_MODEL":validate_ollama_model(options["model"].as_str().unwrap())?});
    for key in [
        "AUTOROUTER_OLLAMA_URL",
        "AUTOROUTER_OLLAMA_TIMEOUT_MS",
        "AUTOROUTER_OLLAMA_KEEP_ALIVE",
    ] {
        if let Some(v) = environment.get(key) {
            env[key] = v.clone();
        }
    }
    if let Some(v) = options.get("timeout-ms") {
        env["AUTOROUTER_OLLAMA_TIMEOUT_MS"] = v.clone();
    }
    let config = read_config(
        &env,
        false,
        &std::env::current_dir().map_err(|_| "Cannot resolve working directory")?,
    )?;
    let fixture = std::fs::read(root.join("test/fixtures/ollama-integration.json"))
        .map_err(|_| "Cannot read checked-in routing fixtures.")?;
    let report = runtime()?.block_on(async {
        let transport =
            Arc::new(NativeHttpClient::new().map_err(|_| "Cannot construct HTTP transport")?);
        let signals = crate::tool_process::Signals::new()?;
        run_tests(transport, &config, &fixture, &signals.token, &mut |line| {
            println!("{line}")
        })
        .await
        .map_err(|error| error.message)
    })?;
    if let Some(output) = options["output"].as_str() {
        write_report(Path::new(output), &report, true)?;
        println!("Saved {output}");
    }
    println!(
        "{}: {}/{} cases; missing tiers: {}.",
        if report["passed"] == true {
            "PASS"
        } else {
            "FAIL"
        },
        report["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["result"] == "pass")
            .count(),
        report["rows"].as_array().unwrap().len(),
        report["missing_tiers"]
    );
    Ok(report["passed"] == true)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requires_explicit_model_and_validates_all_checked_in_shapes() {
        assert!(parse_args(&[]).is_err());
        assert!(parse_args(&["--help".into()]).is_ok());
        assert!(fixtures(br#"[{"id":"good","expected":"haiku","prompt":"a"},{"id":"good","expected":"opus","prompt":"b"}]"#).is_err());
        assert_eq!(
            identity("registry.ollama.ai/library/nimble"),
            "nimble:latest"
        );
    }
    #[test]
    fn synthetic_wrapper_extracts_exact_task() {
        let config = read_config(&json!({}), false, Path::new("/tmp")).unwrap();
        let body = request(&json!({"prompt":"Simple synthetic task"}), &config);
        assert_eq!(
            build_ollama_state_document(&body, 3000)
                .current_task
                .to_well_formed(),
            "Simple synthetic task"
        );
    }
}

#[cfg(test)]
#[path = "ollama_routing_contracts.rs"]
mod contracts;
