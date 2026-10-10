//! Opt-in subscription validation with private, disposable native fixtures.
use crate::context_probe::{array, field_raw, now_ms, raw, string};
use crate::evaluation::{digest, environment, profile, runtime};
use crate::live_claude;
use crate::tool_process::{self, Scratch, Signals};
use autorouter_core::auth::build_claude_env;
use autorouter_core::config::{Evaluator, RouterConfig, read_config, require_keys};
use autorouter_core::evaluation_report::{
    create_evaluation_policy, evaluate_live_case, evaluate_live_report, model_tier, profile_tier,
};
use autorouter_core::js_json::{JsDocument, JsNode};
use autorouter_core::prompt_state::build_state_document;
use autorouter_core::router::{RouteDecision, RouteOptions, context_size_bytes};
use autorouter_core::status_state::StatusState;
use autorouter_core::statusline::render_status_line;
use autorouter_runtime::evaluator::EvaluationError;
use autorouter_runtime::http_client::{HttpError, HttpTransport, NativeHttpClient};
use autorouter_runtime::response_observer::CompletionEvidence;
use autorouter_runtime::router::Router;
use autorouter_runtime::server::{Gateway, GatewayRouter};
use autorouter_runtime::server_events::EventSinks;
use bytes::Bytes;
use http_body_util::Full;
use hyper::{HeaderMap, Request, Response};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
const CASES_JSON: &str = include_str!("../fixtures/live-cases-v2.json");
const IMPLEMENTATION: &str = include_str!("../fixtures/merge-intervals.rs");
const TEST_SOURCE: &str = include_str!("../fixtures/merge-intervals.test.rs");
const MANIFEST: &str = include_str!("../fixtures/repair-cargo.toml");
#[path = "live_reference.rs"]
mod reference_gateway;
pub fn cases() -> Value {
    serde_json::from_str::<Value>(CASES_JSON).expect("packaged live fixtures")["cases"].clone()
}
fn expected(scenario: &Value) -> Value {
    scenario
        .get("expectedClassifiedTiers")
        .or_else(|| scenario.get("expectedClassifiedTier"))
        .or_else(|| scenario.get("expectedTier"))
        .cloned()
        .unwrap_or(Value::Null)
}
fn fingerprint(cases: &Value) -> String {
    let rows:Vec<_>=cases.as_object().unwrap().iter().map(|(name,scenario)|json!({"name":name,"prompts":scenario["prompts"],"expected":expected(scenario)})).collect();
    digest(json!(rows).to_string().as_bytes())
}
fn js_number(value: &str) -> f64 {
    let value = autorouter_core::config::js_trim(value);
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ] {
        if let Some(digits) = value.strip_prefix(prefix) {
            return u64::from_str_radix(digits, radix)
                .map(|v| v as f64)
                .unwrap_or(f64::NAN);
        }
    }
    if value.is_empty() {
        0.0
    } else {
        value.parse().unwrap_or(f64::NAN)
    }
}
pub fn parse_args(args: &[String]) -> Result<Value, String> {
    let cases = cases();
    let mut options = json!({"engine":"native","cases":cases.as_object().unwrap().iter().filter(|(_,v)|v["optIn"]!=true).map(|(k,_)|k).collect::<Vec<_>>(),"timeoutMs":120000});
    let mut i = 0;
    while i < args.len() {
        let flag = &args[i];
        match flag.as_str() {
            "--help" => options["help"] = json!(true),
            "--no-thinking" => options["noThinking"] = json!(true),
            "--simulate-evaluator-outage" | "--simulate-jev-outage" => {
                options["simulateJevOutage"] = json!(true)
            }
            "--case" | "--timeout-ms" | "--model" | "--client-model" | "--engine" => {
                let value = args
                    .get(i + 1)
                    .filter(|v| !v.is_empty())
                    .ok_or("Use --help for supported options")?;
                match flag.as_str() {
                    "--engine" => {
                        if !["native", "node-reference"].contains(&value.as_str()) {
                            return Err("--engine must be native or node-reference".into());
                        }
                        options["engine"] = json!(value);
                    }
                    "--case" => {
                        options["cases"] = json!(value.split(',').collect::<Vec<_>>());
                        options["selectedCases"] = json!(true);
                    }
                    "--timeout-ms" => {
                        let timeout = js_number(value);
                        if !timeout.is_finite()
                            || timeout.fract() != 0.0
                            || !(1000.0..=120000.0).contains(&timeout)
                        {
                            return Err(
                                "--timeout-ms must be an integer between 1000 and 120000".into()
                            );
                        }
                        options["timeoutMs"] = json!(timeout as u64);
                    }
                    _ => options["model"] = json!(value),
                }
                i += 1;
            }
            _ => return Err("Use --help for supported options".into()),
        }
        i += 1;
    }
    if options["cases"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| cases.get(v.as_str().unwrap()).is_none())
    {
        return Err("Unknown live-test case".into());
    }
    Ok(options)
}
fn large_context() -> String {
    format!(
        "This is a synthetic context-capacity validation fixture. The repeated words between the padding markers are irrelevant data. Answer the user normally and do not repeat the padding.\n<synthetic-padding>\n{}\n</synthetic-padding>\n",
        "x ".repeat(220000)
    )
}
fn reminder() -> String {
    format!(
        "<system-reminder>\nSynthetic tool catalog metadata, unrelated to the user task.\n{}\n</system-reminder>",
        "Synthetic catalog entry for a tool that is not enabled. ".repeat(4000)
    )
}
fn expected_result(name: &str, answers: &[String]) -> bool {
    let first = answers.first().map(String::as_str).unwrap_or("");
    match name {
        "simple" | "large_context" | "example_haiku" => answers.len() == 1 && first.trim() == "0",
        "medium" => answers.len() == 1 && first.contains("ORDER=A,D,B,C"),
        "difficult" => {
            answers.len() == 1
                && first.contains("VERDICT=UNSAFE")
                && first.to_ascii_lowercase().contains("atomic")
                && first.contains("10")
                && first.contains("11")
        }
        "coding" | "example_sonnet" => answers.len() == 1 && !first.is_empty(),
        "example_opus" => answers.len() == 1 && first.to_ascii_lowercase().contains("atomic"),
        "continuation" => {
            answers.len() == 2
                && first.trim() == "ACK"
                && answers[1].trim() == "ROUTER_CONTINUATION_47"
        }
        "thinking_continuation" => {
            answers.len() == 2
                && first.contains("VERDICT=UNSAFE")
                && answers[1].trim() == "ROUTER_THINKING_83"
        }
        _ => false,
    }
}
fn field(doc: &JsDocument, node: usize, key: &str) -> String {
    string(doc, doc.get(node, key))
}
pub fn metadata(doc: &JsDocument, options: &RouteOptions) -> Value {
    let messages = array(doc, doc.get(doc.root(), "messages"));
    let tools = array(doc, doc.get(doc.root(), "tools"));
    let roles: Vec<_> = messages.iter().map(|&n| field(doc, n, "role")).collect();
    let blocks: Vec<_> = messages
        .iter()
        .flat_map(|&n| array(doc, doc.get(n, "content")))
        .copied()
        .collect();
    let mut types = Vec::new();
    for &tool in tools {
        if let Some(node) = doc.get(tool, "type") {
            let kind = raw(doc, Some(node));
            if !matches!(kind.as_str(), "null" | "false" | "0" | "\"\"") {
                let parsed: Value = serde_json::from_str(&kind).unwrap_or(Value::Null);
                if !types.contains(&parsed) {
                    types.push(parsed);
                }
            }
        }
    }
    let thinking = doc
        .get(doc.root(), "thinking")
        .map(|n| field(doc, n, "type"))
        .unwrap_or_default();
    let mut report = json!({"requested_model":field(doc,doc.root(),"model"),"request_class":if ["main","compaction","auxiliary"].contains(&options.request_class.as_str()){options.request_class.as_str()}else if options.request_class.is_empty(){"unspecified"}else{"other"},"message_count":messages.len(),"message_roles":roles,"request_bytes":doc.stringify().len(),"system_bytes":field_raw(doc,"system").len(),"tools_bytes":if doc.get(doc.root(),"tools").is_some(){field_raw(doc,"tools").len()}else{2},"messages_bytes":field_raw(doc,"messages").len(),"has_system_messages":roles.contains(&"system".into()),"thinking_type":if ["enabled","adaptive","disabled"].contains(&thinking.as_str()){thinking.as_str()}else{"unspecified"},"thinking_history_count":blocks.iter().filter(|&&n|["thinking","redacted_thinking"].contains(&field(doc,n,"type").as_str())).count(),"tool_result_count":blocks.iter().filter(|&&n|field(doc,n,"type")=="tool_result").count(),"tool_count":tools.len(),"typed_tools":types,"has_output_effort":doc.get(doc.root(),"output_config").and_then(|n|doc.get(n,"effort")).is_some_and(|n|!matches!(doc.node(n),Some(JsNode::Null|JsNode::Bool(false)))) ,"has_context_management":doc.get(doc.root(),"context_management").is_some_and(|n|!matches!(doc.node(n),Some(JsNode::Null|JsNode::Bool(false))))});
    if let Some(node) = doc.get(doc.root(), "max_tokens") {
        report["max_tokens"] =
            serde_json::from_str(&doc.stringify_node(node)).unwrap_or(Value::Null);
    }
    report
}
struct ClassifierTransport {
    native: Arc<NativeHttpClient>,
    outage: bool,
}
impl HttpTransport for ClassifierTransport {
    type ResponseBody = hyper::body::Incoming;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        if self.outage {
            Err(HttpError::Network)
        } else {
            self.native.request(request).await
        }
    }
}
struct LiveRouter {
    inner: Router<ClassifierTransport>,
    scenario: Value,
    routes: Arc<Mutex<Vec<Value>>>,
}
impl LiveRouter {
    fn transform(&self, document: Arc<JsDocument>, class: &str) -> Arc<JsDocument> {
        if self.scenario["syntheticReminder"] != true
            || ["compaction", "auxiliary"].contains(&class)
        {
            return document;
        }
        let mut doc = (*document).clone();
        let first = array(&doc, doc.get(doc.root(), "messages"))
            .iter()
            .copied()
            .find(|&n| field(&doc, n, "role") == "user");
        if let Some(first) = first {
            let content = doc.get(first, "content");
            let mut chunks = vec![json!({"type":"text","text":reminder()}).to_string()];
            if content.and_then(|n| doc.string(n)).is_some() {
                chunks.push(format!(
                    "{{\"type\":\"text\",\"text\":{}}}",
                    raw(&doc, content)
                ));
            } else {
                chunks.extend(array(&doc, content).iter().map(|&n| doc.stringify_node(n)));
            }
            let _ = doc.set_field_json(
                first,
                "content",
                format!("[{}]", chunks.join(",")).as_bytes(),
            );
        }
        Arc::new(doc)
    }
    async fn observe(
        &self,
        doc: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancel: &CancellationToken,
        search: &str,
    ) -> Result<RouteDecision, EvaluationError> {
        let mut evidence = metadata(&doc, &options);
        let state = build_state_document(&doc, 12000);
        let expected = if let Some(tiers) = self.scenario["expectedClassifiedTiers"].as_array() {
            self.scenario["prompts"]
                .as_array()
                .unwrap()
                .iter()
                .position(|p| p.as_str() == Some(&state.current_task.to_well_formed()))
                .and_then(|i| tiers.get(i))
                .cloned()
        } else {
            self.scenario
                .get("expectedClassifiedTier")
                .or_else(|| self.scenario.get("expectedTier"))
                .cloned()
        };
        if let Some(expected) = expected {
            evidence["expected_classified_tier"] = expected;
        }
        if self.scenario["syntheticReminder"] == true {
            evidence["context_guard_bytes"] =
                json!(context_size_bytes(&doc, &field(&doc, doc.root(), "model")));
            let escaped = self.scenario["prompts"][0].to_string();
            evidence["classifier_contains_example"] =
                json!(state.stringify().contains(&escaped[1..escaped.len() - 1]));
        }
        let result = self
            .inner
            .route_exact(doc, options, headers, cancel, search)
            .await?;
        evidence
            .as_object_mut()
            .unwrap()
            .extend(result.decision.as_object().unwrap().clone());
        self.routes.lock().unwrap().push(evidence);
        Ok(result)
    }
}
impl GatewayRouter for LiveRouter {
    fn transform_document_for_request(&self, doc: Arc<JsDocument>, class: &str) -> Arc<JsDocument> {
        self.transform(doc, class)
    }
    async fn route(
        &self,
        doc: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancel: &CancellationToken,
        search: &str,
    ) -> Result<Value, EvaluationError> {
        self.observe(doc, options, headers, cancel, search)
            .await
            .map(|r| r.decision)
    }
    async fn route_exact(
        &self,
        doc: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancel: &CancellationToken,
        search: &str,
    ) -> Result<RouteDecision, EvaluationError> {
        self.observe(doc, options, headers, cancel, search).await
    }
    fn complete(&self, id: &str, evidence: &Value) -> bool {
        self.inner.complete(id, evidence)
    }
    fn complete_exact(&self, id: &str, evidence: &CompletionEvidence) -> bool {
        self.inner.complete_exact(id, evidence)
    }
    fn shutdown(&self) {
        self.inner.shutdown();
    }
    async fn close(&self) {
        self.inner.close().await;
    }
}
struct Evidence {
    state: StatusState,
    observations: Vec<Value>,
    upstream_models: Vec<Value>,
    statuses: Vec<Value>,
    usage: Vec<Value>,
    errors: Vec<Value>,
}
impl Evidence {
    fn status(&mut self, event: &Value) {
        self.state.update(event, now_ms());
        if event["event"] == "upstream_usage" {
            self.usage.push(event["usage"].clone());
        }
        if let Some(id) = event["request_id"].as_str() {
            let index = self
                .observations
                .iter()
                .position(|v| v["request_id"] == id)
                .unwrap_or_else(|| {
                    self.observations.push(json!({"request_id":id}));
                    self.observations.len() - 1
                });
            let observed = &mut self.observations[index];
            match event["event"].as_str() {
                Some("route") => {
                    for (out, key) in [
                        ("requested_model", "requested_model"),
                        ("selected_model", "model"),
                        ("source", "source"),
                    ] {
                        observed[out] = event[key].clone();
                    }
                    observed["classified_tier"] =
                        event.get("classified_tier").cloned().unwrap_or(Value::Null);
                    observed["confirmed_model"] = Value::Null;
                }
                Some("upstream_model") => observed["confirmed_model"] = event["model"].clone(),
                Some("upstream_response") => observed["http_status"] = event["status"].clone(),
                Some("request_complete" | "request_error" | "request_cancelled") => {
                    observed["outcome"] = event["event"].clone()
                }
                _ => {}
            }
        }
    }
    fn log(&mut self, event: &Value) {
        match event["event"].as_str() {
            Some("upstream_model") if event["model"].is_string() => {
                if !self.upstream_models.contains(&event["model"]) {
                    self.upstream_models.push(event["model"].clone());
                }
            }
            Some("upstream_response") => self.statuses.push(event["status"].clone()),
            Some("proxy_error" | "invalid_request_shape") => self
                .errors
                .push(json!({"event":event["event"],"status":event["status"]})),
            _ => {}
        }
    }
}
fn family(model: &Value) -> String {
    let model = model.as_str().unwrap_or("");
    let lower = model.to_ascii_lowercase();
    ["haiku", "sonnet", "opus"]
        .into_iter()
        .find(|tier| lower.contains(tier))
        .unwrap_or(model)
        .to_owned()
}
fn all_model(models: &[Value], tier: &str) -> bool {
    !models.is_empty() && models.iter().all(|m| family(m) == tier)
}
fn checks(
    name: &str,
    scenario: &Value,
    options: &Value,
    config: &RouterConfig,
    run_answers: (&Value, &[String]),
    routes: &[Value],
    evidence: &Evidence,
) -> Value {
    let (run, answers) = run_answers;
    let outage = options["simulateJevOutage"] == true;
    let mut checks = json!({"claude_success":run["exit_code"]==0&&run["timed_out"]!=true&&run["output_limit_exceeded"]!=true&&run["failures"].as_array().is_some_and(Vec::is_empty),"expected_result":expected_result(name,answers),"requests_reached_router":!routes.is_empty(),"upstream_model_evidence":!evidence.observations.is_empty()&&evidence.observations.iter().all(|row|model_tier(row["confirmed_model"].as_str().unwrap_or("")).is_some()&&model_tier(row["confirmed_model"].as_str().unwrap_or(""))==model_tier(row["selected_model"].as_str().unwrap_or(""))),"no_upstream_api_errors":!evidence.statuses.is_empty()&&evidence.statuses.iter().all(|s|s.as_f64().is_some_and(|s|(200.0..300.0).contains(&s))),"no_proxy_errors":evidence.errors.is_empty(),"no_permission_denials":run["permission_denials"]==0});
    if name == "thinking_continuation" && !outage {
        checks["first_request_routed_to_opus"] = json!(
            routes
                .first()
                .is_some_and(|r| family(&r["model"]) == "opus")
        );
        checks["thinking_history_present"] = json!(routes.iter().skip(1).any(|r| {
            r["thinking_history_count"]
                .as_f64()
                .is_some_and(|n| n > 0.0)
        }));
        if profile(config) == "auto" {
            checks["auto_thinking_uses_supported_models"] = json!(
                routes
                    .iter()
                    .all(|r| ["sonnet", "opus"].contains(&family(&r["model"]).as_str()))
            );
        } else {
            checks["thinking_continuation_pinned"] = json!(
                routes
                    .iter()
                    .skip(1)
                    .any(|r| r["reason"] == "thinking_history" && family(&r["model"]) == "opus")
            );
            checks["actual_upstream_stayed_opus"] =
                json!(all_model(&evidence.upstream_models, "opus"));
        }
    }
    let main: Vec<_> = routes
        .iter()
        .filter(|r| ["main", "unspecified"].contains(&r["request_class"].as_str().unwrap_or("")))
        .collect();
    if name == "large_context" {
        if profile(config) == "compatible" {
            checks["compatible_client_requested_haiku"] = json!(
                !main.is_empty()
                    && main
                        .iter()
                        .all(|r| family(&r["requested_model"]) == "haiku")
            );
        }
        checks["large_system_context_reached_router"] =
            json!(
                main.iter()
                    .any(|r| r["system_bytes"].as_f64().unwrap_or(0.0)
                        >= large_context().len() as f64
                        && r["system_bytes"].as_f64().unwrap_or(0.0)
                            > r["request_bytes"].as_f64().unwrap_or(0.0) * 0.9
                        && r["messages_bytes"].as_f64().unwrap_or(f64::INFINITY) < 10000.0)
            );
        checks["capacity_guard_selected_sonnet"] = json!(
            !main.is_empty()
                && main.iter().all(|r| family(&r["model"]) == "sonnet"
                    && (profile(config) != "compatible"
                        || outage
                        || r["reason"] == "context_capacity"))
        );
        checks["actual_upstream_used_sonnet"] =
            json!(all_model(&evidence.upstream_models, "sonnet"));
        checks["provider_input_exceeds_haiku_window"] = json!(evidence.usage.iter().any(|u| {
            [
                "input_tokens",
                "cache_creation_input_tokens",
                "cache_read_input_tokens",
            ]
            .iter()
            .filter_map(|k| {
                u[k].as_f64()
                    .filter(|n| n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0)
            })
            .sum::<f64>()
                > 200000.0
        }));
    }
    if let Some(tier) = scenario["expectedTier"].as_str().filter(|_| !outage) {
        let tier = profile_tier(tier, profile(config));
        checks["expected_tier"] =
            json!(!main.is_empty() && main.iter().all(|r| family(&r["model"]) == tier));
        checks["actual_expected_model"] = json!(all_model(&evidence.upstream_models, tier));
        checks["actual_prompt_reached_classifier"] = json!(
            main.iter()
                .all(|r| r["classifier_contains_example"] == true)
        );
        checks["large_reminder_fixture"] = json!(main.iter().all(|r| {
            r["context_guard_bytes"]
                .as_f64()
                .is_some_and(|n| n > 150000.0)
        }));
        if tier == "haiku" {
            checks["counted_context_fits_haiku"] = json!(main.iter().all(|r| {
                r["context_check"] == "within_budget"
                    && r["counted_input_tokens"]
                        .as_f64()
                        .is_some_and(|n| n <= 190000.0)
            }));
        }
    }
    if outage {
        let evaluated: Vec<_> = routes
            .iter()
            .filter(|r| r["source"] != "passthrough")
            .collect();
        checks["classifier_outage_fell_back"] =
            json!(!evaluated.is_empty() && evaluated.iter().all(|r| r["source"] == "fallback"));
        checks["fallback_retained_capability_floor"] =
            json!(evaluated.iter().all(|r| family(&r["model"])
                == if family(&r["requested_model"]) == "opus" {
                    "opus"
                } else {
                    "sonnet"
                }));
    }
    checks
}
async fn independent_tests(dir: &Scratch, cancel: &CancellationToken) -> (bool, bool) {
    let preserved = std::fs::read(dir.0.join(live_claude::TEST))
        .is_ok_and(|b| b == TEST_SOURCE.as_bytes())
        && std::fs::read(dir.0.join("Cargo.toml")).is_ok_and(|b| b == MANIFEST.as_bytes());
    if dir.file(live_claude::TEST, TEST_SOURCE.as_bytes()).is_err()
        || dir.file("Cargo.toml", MANIFEST.as_bytes()).is_err()
    {
        return (preserved, false);
    }
    let (result, _) = tool_process::capture(
        "cargo",
        &["test", "--offline", "--quiet"],
        &dir.0,
        Duration::from_secs(10),
        cancel,
    )
    .await;
    (
        preserved,
        result["exit_code"] == 0
            && result["output_limit_exceeded"] != true
            && !cancel.is_cancelled(),
    )
}

pub fn run(args: &[String], root: &Path) -> Result<bool, String> {
    let options = parse_args(args)?;
    if options["help"] == true {
        println!(
            "Usage: cargo xtask [--env-file .env] live-validation [--engine native|node-reference] [--case simple,medium,difficult,coding,continuation,thinking_continuation,large_context,example_haiku,example_sonnet,example_opus] [--client-model MODEL] [--no-thinking] [--simulate-evaluator-outage] [--timeout-ms 120000]\nExplicit real Claude subscription and evaluator calls. Requires --safe-mode and --restricted support. Reports independent transport, evaluator, rubric, policy and task gates; private fixtures are removed. Both engines use the same version 2 Rust/cargo fixture, prompts, isolated Claude command and independent verifier. node-reference explicitly requires Node 22+ and verified frozen baseline source as a temporary oracle. Record matched environment/model conditions when comparing separate runs. large_context deliberately sends over 200K input tokens. Outage alias --simulate-jev-outage is retained. Costs are list-price estimates, not subscription charges."
        );
        return Ok(true);
    }
    let reference = if options["engine"] == "node-reference" {
        Some(reference_gateway::Reference::verified(root)?)
    } else {
        None
    };
    runtime()?.block_on(execute(options, reference))
}
async fn execute(
    options: Value,
    reference: Option<reference_gateway::Reference>,
) -> Result<bool, String> {
    let signals = Signals::new()?;
    let fixtures = cases();
    let mut env = environment();
    env["AUTOROUTER_AUTH_MODE"] = json!("subscription");
    env["AUTOROUTER_TOKEN"] = json!(tool_process::token()?);
    let cwd = std::env::current_dir().map_err(|_| "Cannot resolve working directory")?;
    let config = read_config(&env, false, &cwd)?;
    let mut policy_options = json!({"profile":profile(&config)});
    if options["selectedCases"] == true {
        let mut tiers = Vec::new();
        for name in options["cases"].as_array().unwrap() {
            let name = name.as_str().unwrap();
            if let Some(tier) = fixtures[name]["expectedTier"]
                .as_str()
                .or((name == "large_context").then_some("sonnet"))
            {
                let tier = profile_tier(tier, profile(&config));
                if !tiers.contains(&tier) {
                    tiers.push(tier);
                }
            }
        }
        policy_options["requiredTiers"] = json!(tiers);
    }
    let policy = create_evaluation_policy(Some(&policy_options))?;
    require_keys(&config)?;
    let (help, help_text) = tool_process::capture(
        "claude",
        &["--help"],
        &cwd,
        Duration::from_secs(10),
        &signals.token,
    )
    .await;
    if help["exit_code"] != 0
        || !help_text.contains("--safe-mode")
        || !help_text.contains("--restricted")
    {
        return Err("A Claude CLI with --safe-mode and --restricted support is required".into());
    }
    let max_turns = help_text.contains("--max-turns");
    let (_, version) = tool_process::capture(
        "claude",
        &["--version"],
        &cwd,
        Duration::from_secs(10),
        &signals.token,
    )
    .await;
    let provenance: Value = serde_json::from_str(CASES_JSON).unwrap();
    let mut report = json!({"type":"live_validation","cli_version":version.trim().chars().take(100).collect::<String>(),"auth_mode":"subscription","fixture_version":2,"fixture_sha256":fingerprint(&fixtures),"historical_fixture_version":provenance["historical_fixture_version"],"historical_fixture_sha256":provenance["historical_fixture_sha256"],"fixture_adaptation":provenance["adaptation"],"native_repair_source_sha256":digest(IMPLEMENTATION.as_bytes()),"native_repair_test_sha256":digest(TEST_SOURCE.as_bytes()),"native_repair_manifest_sha256":digest(MANIFEST.as_bytes()),"evaluator":if config.evaluator==Evaluator::Ollama{"ollama"}else{"jev"},"classifier_timeout_ms":if config.evaluator==Evaluator::Ollama{config.ollama_timeout_ms}else{config.jev_timeout_ms},"client_profile":profile(&config),"client_model":options.get("model").cloned().unwrap_or_else(||if profile(&config)=="compatible"{json!(config.models.haiku)}else{env.get("ANTHROPIC_MODEL").cloned().unwrap_or(json!("default"))}),"thinking_disabled":options["noThinking"]==true||profile(&config)=="compatible"||env["MAX_THINKING_TOKENS"]=="0","simulated_classifier_outage":options["simulateJevOutage"]==true,"quality_policy":policy,"coverage_scope":if options["selectedCases"]==true{"selected_cases"}else{"full_profile"},"cost_basis":"CLI list-price estimate, not subscription charges","max_turns_supported":max_turns,"cases":[]});
    report["engine"] = options["engine"].clone();
    if let Some(reference) = &reference {
        report["reference"] = reference.provenance.clone();
    }
    let transport =
        Arc::new(NativeHttpClient::new().map_err(|_| "Cannot construct HTTP transport")?);
    for name in options["cases"].as_array().unwrap() {
        if signals.token.is_cancelled() {
            return Err("Live validation cancelled".into());
        }
        let name = name.as_str().unwrap();
        let scenario = &fixtures[name];
        let dir = Scratch::new(&format!("live-{name}"))?;
        let system = if name == "large_context" {
            Some(dir.file("synthetic-system-context.txt", large_context().as_bytes())?)
        } else {
            None
        };
        if scenario["tools"] == true {
            dir.file(live_claude::SOURCE, IMPLEMENTATION.as_bytes())?;
            dir.file(live_claude::TEST, TEST_SOURCE.as_bytes())?;
            dir.file("Cargo.toml", MANIFEST.as_bytes())?;
        }
        let routes = Arc::new(Mutex::new(Vec::new()));
        let evidence = Arc::new(Mutex::new(Evidence {
            state: StatusState::new(&config.models.opus),
            observations: Vec::new(),
            upstream_models: Vec::new(),
            statuses: Vec::new(),
            usage: Vec::new(),
            errors: Vec::new(),
        }));
        let mut native = None;
        let mut oracle = None;
        let address = if let Some(reference) = &reference {
            let (routing, status, log) = (routes.clone(), evidence.clone(), evidence.clone());
            let gateway = reference_gateway::Gateway::start(reference,
                json!({"env":env,"scenario":scenario,"outage":options["simulateJevOutage"]==true,"reminder":reminder()}),
                Duration::from_millis(options["timeoutMs"].as_u64().unwrap()),
                reference_gateway::Events {
                    route: Arc::new(move |event| routing.lock().unwrap().push(event)),
                    status: Arc::new(move |event| status.lock().unwrap().status(&event)),
                    log: Arc::new(move |event| log.lock().unwrap().log(&event)),
                }, &signals.token).await?;
            report["reference_runtime"] = gateway.runtime.clone();
            let address = gateway.address;
            oracle = Some(gateway);
            address
        } else {
            let router = Arc::new(LiveRouter {
                inner: Router::new(
                    Arc::new(ClassifierTransport {
                        native: transport.clone(),
                        outage: options["simulateJevOutage"] == true,
                    }),
                    config.clone(),
                ),
                scenario: scenario.clone(),
                routes: routes.clone(),
            });
            let (status, log) = (evidence.clone(), evidence.clone());
            let sinks = EventSinks {
                status: Some(Arc::new(move |event| {
                    status
                        .lock()
                        .unwrap()
                        .status(&event.to_serde_observation_lossy())
                })),
                log: Some(Arc::new(move |event| {
                    log.lock().unwrap().log(&event.to_serde_observation_lossy())
                })),
                ..Default::default()
            };
            let gateway =
                Gateway::with_router(config.clone(), transport.clone(), router.clone(), sinks)?
                    .listen(0)
                    .await?;
            let address = gateway.address;
            native = Some((gateway, router));
            address
        };
        let mut env = build_claude_env(
            &config,
            &format!("http://{address}"),
            &tool_process::environment(),
        );
        if options["noThinking"] == true {
            env.insert("MAX_THINKING_TOKENS".into(), "0".into());
        }
        let (run, answers) = live_claude::run(
            &dir.0,
            env,
            scenario,
            &options,
            system.as_deref(),
            max_turns,
            &signals.token,
        )
        .await;
        if let Some((gateway, router)) = native {
            gateway.close().await;
            router.shutdown();
        }
        if let Some(gateway) = oracle {
            gateway.close().await?;
        }
        let native_tests = if scenario["tools"] == true {
            Some(independent_tests(&dir, &signals.token).await)
        } else {
            None
        };
        let evidence = evidence.lock().unwrap();
        let routes = routes.lock().unwrap();
        let mut checks = checks(
            name,
            scenario,
            &options,
            &config,
            (&run, &answers),
            &routes,
            &evidence,
        );
        if let Some((preserved, passed)) = native_tests {
            checks["original_tests_preserved"] = json!(preserved);
            checks["independent_tests_pass"] = json!(passed);
            checks["read_edit_bash_exercised"] =
                json!(["Read", "Edit", "Bash"].iter().all(|tool| {
                    run["successful_tools"]
                        .as_array()
                        .unwrap()
                        .contains(&json!(tool))
                }));
            checks["tool_continuation_reached_router"] = json!(
                routes
                    .iter()
                    .any(|route| route["tool_result_count"].as_f64().is_some_and(|n| n > 0.0))
            );
        }
        let mut accept_options = json!({"checks":checks,"routes":*routes,"evaluator":report["evaluator"],"expectOutage":options["simulateJevOutage"]==true});
        if let Some(tier) = scenario
            .get("expectedClassifiedTier")
            .or_else(|| scenario.get("expectedTier"))
        {
            accept_options["expectedClassifiedTier"] = tier.clone();
        }
        let acceptance = evaluate_live_case(&accept_options)?;
        let snapshot = evidence.state.snapshot(std::process::id(), now_ms());
        let savings: Vec<_> = snapshot["savings"]
            .as_object()
            .into_iter()
            .flat_map(|v| v.values().cloned())
            .collect();
        let lines: Vec<_> = snapshot["savings"]
            .as_object()
            .into_iter()
            .flat_map(|v| v.keys())
            .map(|id| {
                render_status_line(
                    &json!({"session_id":id}),
                    &snapshot,
                    &json!({"color":false,"columns":160}),
                )
            })
            .collect();
        let mut item = json!({"case":name,"checks":checks,"routes":*routes,"request_observations":evidence.observations,"upstream_models":evidence.upstream_models,"upstream_statuses":evidence.statuses,"proxy_errors":evidence.errors,"classifier_succeeded":routes.iter().any(|r|r["source"]==report["evaluator"]),"model_changed":routes.iter().any(|r|r["requested_model"]!=r["model"]),"provider_usage":evidence.usage,"api_equivalent_savings":savings,"status_lines":lines});
        item.as_object_mut()
            .unwrap()
            .extend(acceptance.as_object().unwrap().clone());
        item.as_object_mut()
            .unwrap()
            .extend(run.as_object().unwrap().clone());
        let mut progress = json!({"type":"case_result"});
        progress
            .as_object_mut()
            .unwrap()
            .extend(item.as_object().unwrap().clone());
        println!("{progress}");
        report["cases"].as_array_mut().unwrap().push(item);
    }
    let acceptance = evaluate_live_report(
        &report["cases"],
        &json!({"policy":policy,"expectOutage":options["simulateJevOutage"]==true}),
    )?;
    report["passed"] = acceptance["passed"].clone();
    report["gates"] = acceptance["gates"].clone();
    report["classifier_succeeded"] = json!(
        report["cases"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["classifier_succeeded"] == true)
    );
    report["model_changed"] = json!(
        report["cases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["model_changed"] == true)
    );
    println!("{report}");
    Ok(report["passed"] == true)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_cases_and_expensive_case_opt_in_are_preserved() {
        let options = parse_args(&[]).unwrap();
        assert_eq!(options["engine"], "native");
        assert_eq!(
            parse_args(&["--engine".into(), "node-reference".into()]).unwrap()["engine"],
            "node-reference"
        );
        assert!(parse_args(&["--engine".into(), "unknown".into()]).is_err());
        assert!(
            !options["cases"]
                .as_array()
                .unwrap()
                .contains(&json!("large_context"))
        );
        assert!(
            options["cases"]
                .as_array()
                .unwrap()
                .contains(&json!("thinking_continuation"))
        );
        assert_eq!(
            parse_args(&["--simulate-jev-outage".into()]).unwrap()["simulateJevOutage"],
            true
        );
        assert!(parse_args(&["--timeout-ms".into(), "999".into()]).is_err());
        assert!(parse_args(&["--case".into(), "nope".into()]).is_err());
    }
    #[test]
    fn native_fixture_records_version_and_distinct_provenance() {
        let provenance: Value = serde_json::from_str(CASES_JSON).unwrap();
        assert_eq!(provenance["fixture_version"], 2);
        assert_ne!(
            fingerprint(&cases()),
            provenance["historical_fixture_sha256"]
        );
        assert!(
            cases()["coding"]["prompts"][0]
                .as_str()
                .unwrap()
                .contains(live_claude::TEST_COMMAND)
        );
        assert!(TEST_SOURCE.contains("assert_eq!"));
    }
    #[tokio::test]
    async fn independent_verification_restores_modified_tests_before_executing() {
        let dir = Scratch::new("repair-test").unwrap();
        dir.file(live_claude::SOURCE, IMPLEMENTATION.as_bytes())
            .unwrap();
        dir.file(live_claude::TEST, b"modified tests").unwrap();
        dir.file("Cargo.toml", MANIFEST.as_bytes()).unwrap();
        let (preserved, passed) = independent_tests(&dir, &CancellationToken::new()).await;
        assert!(!preserved);
        assert!(!passed, "seeded implementation must fail independent tests");
        assert_eq!(
            std::fs::read_to_string(dir.0.join(live_claude::TEST)).unwrap(),
            TEST_SOURCE
        );
    }
}
