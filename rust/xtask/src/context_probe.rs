//! Explicit Claude startup diagnostic. Only metadata leaves the process.
use crate::tool_process::{self, InputAction, RunOptions, Signals};
use autorouter_core::auth::{LOCAL_AUTH_HEADER, build_claude_env};
use autorouter_core::config::{Evaluator, RouterConfig, read_config, require_keys};
use autorouter_core::js_json::{JsDocument, JsNode, NodeId};
use autorouter_core::prompt_state::{build_ollama_state_document, build_state_document};
use autorouter_core::router::{RouteDecision, RouteOptions, context_size_bytes};
use autorouter_core::status_state::StatusState;
use autorouter_core::statusline::render_status_line;
use autorouter_runtime::evaluator::EvaluationError;
use autorouter_runtime::http_client::NativeHttpClient;
use autorouter_runtime::router::Router;
use autorouter_runtime::server::{Gateway, GatewayRouter};
use autorouter_runtime::server_events::EventSinks;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{HeaderMap, Request, Response};
use serde_json::{Value, json};
use std::convert::Infallible;
use std::io::IsTerminal;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

pub fn example(tier: &str) -> Option<&'static str> {
    match tier {
        "haiku" => Some(
            "What does the JavaScript expression `[].length` evaluate to? Reply with only the integer.",
        ),
        "sonnet" => Some(
            "Add pagination to this REST endpoint using `page` and `pageSize`. Validate the parameters, preserve existing filtering, and add tests for empty results and out-of-range pages.",
        ),
        "opus" => Some(
            "Review this distributed locking design: worker A’s lease expires while paused. Worker B acquires a newer fencing token and writes successfully. A resumes and writes using its old token. The database checks only whether a token was ever issued. Explain the failure sequence and design the minimum atomic database check that prevents stale writes, including duplicate retries.",
        ),
        _ => None,
    }
}
pub fn parse_args(args: &[String], cwd: &Path) -> Result<Value, String> {
    let mut options = json!({"cwd":cwd,"toolSearch":"unset"});
    let mut i = 0;
    while i < args.len() {
        let flag = &args[i];
        if ["--help", "--live", "--classify", "--interactive"].contains(&flag.as_str()) {
            options[&flag[2..]] = json!(true);
            i += 1;
            continue;
        }
        let value = args
            .get(i + 1)
            .filter(|v| !v.is_empty())
            .ok_or("Use context-probe --help for supported options")?;
        match flag.as_str() {
            "--cwd" => options["cwd"] = json!(value),
            "--example" if example(value).is_some() => options["example"] = json!(value),
            "--tool-search" if ["unset", "true", "false"].contains(&value.as_str()) => {
                options["toolSearch"] = json!(value)
            }
            _ => return Err("Use context-probe --help for supported options".into()),
        }
        i += 2;
    }
    Ok(options)
}
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
pub fn raw(doc: &JsDocument, node: Option<NodeId>) -> String {
    node.map(|n| doc.stringify_node(n))
        .unwrap_or_else(|| "null".into())
}
pub fn field_raw(doc: &JsDocument, key: &str) -> String {
    raw(doc, doc.get(doc.root(), key))
}
pub fn array(doc: &JsDocument, node: Option<NodeId>) -> &[NodeId] {
    match node.and_then(|n| doc.node(n)) {
        Some(JsNode::Array(a)) => a,
        _ => &[],
    }
}
pub fn string(doc: &JsDocument, node: Option<NodeId>) -> String {
    node.and_then(|n| doc.string(n))
        .map(|s| s.to_well_formed())
        .unwrap_or_default()
}
fn field_string(doc: &JsDocument, node: NodeId, key: &str) -> String {
    string(doc, doc.get(node, key))
}
fn blocks(doc: &JsDocument, message: NodeId) -> Vec<String> {
    let content = doc.get(message, "content");
    if let Some(s) = content.and_then(|n| doc.string(n)) {
        return vec![s.to_well_formed()];
    }
    array(doc, content)
        .iter()
        .filter(|&&n| field_string(doc, n, "type") == "text")
        .filter_map(|&n| {
            doc.get(n, "text")
                .and_then(|n| doc.string(n))
                .map(|s| s.to_well_formed())
        })
        .collect()
}
fn safe_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 100
        && model
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}
fn safe_type(kind: &str) -> bool {
    kind.len() <= 80
        && kind.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && kind
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}
pub fn summarize(
    doc: &JsDocument,
    class: &str,
    length: usize,
    config: &RouterConfig,
    prompt: &str,
) -> Value {
    let tools = array(doc, doc.get(doc.root(), "tools"));
    let messages = array(doc, doc.get(doc.root(), "messages"));
    let mut groups = json!({});
    let mut typed = Vec::new();
    let mut deferred = 0;
    let mut search = false;
    for &tool in tools {
        let name = field_string(doc, tool, "name");
        let group = name
            .strip_prefix("mcp__")
            .and_then(|s| s.split_once("__").map(|(a, _)| a))
            .filter(|s| !s.is_empty())
            .unwrap_or("built-in");
        let group: String = group
            .encode_utf16()
            .map(|u| {
                if u <= 127 && (u as u8).is_ascii_alphanumeric()
                    || b"_.-".iter().any(|b| u == *b as u16)
                {
                    char::from_u32(u as u32).unwrap()
                } else {
                    '_'
                }
            })
            .take(80)
            .collect();
        if groups.get(&group).is_none() {
            groups[&group] =
                json!({"tools":0,"deferred":0,"schema_bytes":0,"active_schema_bytes":0});
        }
        let entry = &mut groups[&group];
        let size = doc.stringify_node(tool).len();
        entry["tools"] = json!(entry["tools"].as_u64().unwrap() + 1);
        entry["schema_bytes"] = json!(entry["schema_bytes"].as_u64().unwrap() + size as u64);
        if matches!(
            doc.get(tool, "defer_loading").and_then(|n| doc.node(n)),
            Some(JsNode::Bool(true))
        ) {
            deferred += 1;
            entry["deferred"] = json!(entry["deferred"].as_u64().unwrap() + 1);
        } else {
            entry["active_schema_bytes"] =
                json!(entry["active_schema_bytes"].as_u64().unwrap() + size as u64);
        }
        search |= name == "ToolSearch";
        if let Some(kind) = doc.get(tool, "type") {
            let kind = string(doc, Some(kind));
            let kind = if safe_type(&kind) {
                kind
            } else {
                "unknown".into()
            };
            if !typed.contains(&kind) {
                typed.push(kind);
            }
        }
    }
    let state = if config.evaluator == Evaluator::Ollama {
        build_ollama_state_document(doc, config.ollama_state_chars)
    } else {
        build_state_document(doc, config.state_chars)
    };
    let serialized = state.stringify();
    let escaped = serde_json::to_string(prompt).unwrap();
    let escaped = &escaped[1..escaped.len() - 1];
    let prompt_message = messages.iter().position(|&m| {
        field_string(doc, m, "role") == "user" && blocks(doc, m).join("\n").contains(prompt)
    });
    let prompt_blocks = prompt_message
        .map(|i| blocks(doc, messages[i]))
        .unwrap_or_default();
    let prompt_block = prompt_blocks.iter().position(|s| s.contains(prompt));
    let joined = prompt_blocks.join("\n");
    let offset = joined
        .find(prompt)
        .map(|i| joined[..i].encode_utf16().count());
    let mut roles = json!({"user":0,"assistant":0,"system":0,"other":0});
    for &m in messages {
        let role = field_string(doc, m, "role");
        let role = if ["user", "assistant", "system", "other"].contains(&role.as_str()) {
            role.as_str()
        } else {
            "other"
        };
        roles[role] = json!(roles[role].as_u64().unwrap() + 1);
    }
    let model = field_string(doc, doc.root(), "model");
    let thinking = doc
        .get(doc.root(), "thinking")
        .map(|n| field_string(doc, n, "type"))
        .unwrap_or_default();
    let specific: Vec<_> = [
        "context_management",
        "speed",
        "container",
        "mcp_servers",
        "output_config",
    ]
    .into_iter()
    .filter(|key| doc.get(doc.root(), key).is_some())
    .collect();
    let truncated = typed.len() > 20;
    typed.truncate(20);
    json!({"request_class":if ["main","compaction","auxiliary"].contains(&class){class}else{"unspecified"},"model":if safe_model(&model){model.as_str()}else{"custom"},"body_bytes":length,"context_guard_bytes":context_size_bytes(doc,&model),"system_bytes":field_raw(doc,"system").len(),"messages_bytes":field_raw(doc,"messages").len(),"tools_bytes":field_raw(doc,"tools").len(),"tools":tools.len(),"deferred_tools":deferred,"tool_search_present":search,"message_roles":roles,"thinking_type":if ["enabled","adaptive","disabled"].contains(&thinking.as_str()){thinking.as_str()}else{"unspecified"},"model_specific_fields":specific,"typed_tool_types":typed,"typed_tool_types_truncated":truncated,"evaluator_state_chars":serialized.encode_utf16().count(),"evaluator_contains_entered_prompt":serialized.contains(escaped),"request_contains_entered_prompt":field_raw(doc,"messages").contains(escaped),"evaluator_current_task_chars":state.current_task.units().len(),"entered_prompt_message_index":prompt_message.map(|n|n as i64).unwrap_or(-1),"entered_prompt_text_block_index":prompt_block.map(|n|n as i64).unwrap_or(-1),"entered_prompt_text_offset":offset.map(|n|n as i64).unwrap_or(-1),"first_user_text_block_chars":messages.iter().find(|&&m|field_string(doc,m,"role")=="user").and_then(|&m|blocks(doc,m).first().cloned()).map(|s|s.encode_utf16().count()).unwrap_or(0),"groups":groups})
}
fn decision_metadata(entry: &mut Value, decision: &Value) {
    for (out, input) in [
        ("selected_model", "model"),
        ("tier", "tier"),
        ("classified_tier", "classified_tier"),
        ("confidence", "confidence"),
        ("reason", "reason"),
        ("source", "source"),
        ("latency_ms", "latency_ms"),
    ] {
        if let Some(value) = decision.get(input) {
            entry[out] = value.clone();
        }
    }
}
struct ProbeRouter {
    inner: Option<Router<NativeHttpClient>>,
    config: RouterConfig,
    prompt: String,
    live: bool,
    requests: Arc<Mutex<Vec<Value>>>,
}
impl ProbeRouter {
    async fn observe(
        &self,
        doc: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancel: &CancellationToken,
        search: &str,
        length: usize,
    ) -> Result<Option<RouteDecision>, EvaluationError> {
        let mut entry = summarize(
            &doc,
            &options.request_class,
            length,
            &self.config,
            &self.prompt,
        );
        let decision = if let Some(router) = &self.inner {
            Some(
                router
                    .route_exact(doc, options, headers, cancel, search)
                    .await?,
            )
        } else {
            None
        };
        if let Some(decision) = &decision {
            decision_metadata(&mut entry, &decision.decision);
        }
        let mut requests = self.requests.lock().unwrap();
        if requests.len() < 10000 {
            requests.push(entry);
        }
        Ok(decision)
    }
}
impl GatewayRouter for ProbeRouter {
    fn transform_document(&self, doc: Arc<JsDocument>) -> Arc<JsDocument> {
        if !self.live {
            return doc;
        }
        let mut doc = (*doc).clone();
        doc.set_root_field_json("tool_choice", br#"{"type":"none"}"#)
            .expect("fixed probe transformation");
        Arc::new(doc)
    }
    async fn route(
        &self,
        doc: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancel: &CancellationToken,
        search: &str,
    ) -> Result<Value, EvaluationError> {
        let length = doc.stringify().len();
        self.observe(doc, options, headers, cancel, search, length)
            .await
            .map(|v| v.map(|r| r.decision).unwrap_or(Value::Null))
    }
    async fn route_exact(
        &self,
        doc: Arc<JsDocument>,
        options: RouteOptions,
        headers: &HeaderMap,
        cancel: &CancellationToken,
        search: &str,
    ) -> Result<RouteDecision, EvaluationError> {
        let length = doc.stringify().len();
        self.observe(doc, options, headers, cancel, search, length)
            .await?
            .ok_or(EvaluationError::InvalidResponse)
    }
    fn complete_exact(
        &self,
        id: &str,
        evidence: &autorouter_runtime::response_observer::CompletionEvidence,
    ) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|r| r.complete_exact(id, evidence))
    }
    fn complete(&self, id: &str, evidence: &Value) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|r| r.complete(id, evidence))
    }
    fn shutdown(&self) {
        if let Some(router) = &self.inner {
            router.shutdown();
        }
    }
}
fn response(status: u16, content_type: &str, bytes: impl Into<Bytes>) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(Full::new(bytes.into()))
        .unwrap()
}
fn stub_message(doc: &JsDocument, decision: Option<&RouteDecision>) -> (String, String) {
    let model = decision
        .map(|d| d.model.stringify())
        .unwrap_or_else(|| raw(doc, doc.get(doc.root(), "model")));
    let mut message=JsDocument::parse(json!({"id":"msg_local_context_probe","type":"message","role":"assistant","model":null,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}).to_string().as_bytes()).unwrap();
    message
        .set_root_field_json("model", model.as_bytes())
        .unwrap();
    if !matches!(
        doc.get(doc.root(), "stream").and_then(|n| doc.node(n)),
        Some(JsNode::Bool(true))
    ) {
        message
            .set_root_field_json("content", br#"[{"type":"text","text":"OK"}]"#)
            .unwrap();
        message
            .set_root_field_json("stop_reason", br#""end_turn""#)
            .unwrap();
        return ("application/json".into(), message.stringify());
    }
    let mut text = format!(
        "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{}}}\n\n",
        message.stringify()
    );
    for event in [
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"OK"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":1}}),
        json!({"type":"message_stop"}),
    ] {
        text.push_str(&format!(
            "event: {}\ndata: {}\n\n",
            event["type"].as_str().unwrap(),
            event
        ));
    }
    ("text/event-stream".into(), text)
}
async fn stub_request(
    req: Request<Incoming>,
    router: Arc<ProbeRouter>,
    cancel: CancellationToken,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let credential = req
        .headers()
        .get(LOCAL_AUTH_HEADER)
        .map(|h| h.as_bytes())
        .unwrap_or_default();
    let expected = router
        .config
        .local_token
        .as_deref()
        .unwrap_or("")
        .as_bytes();
    if credential.len() != expected.len() || !bool::from(credential.ct_eq(expected)) {
        return Ok(response(401, "application/json", ""));
    }
    if req.method() != hyper::Method::POST || req.uri().path() != "/v1/messages" {
        return Ok(response(404, "application/json", ""));
    }
    let (parts, mut body) = req.into_parts();
    let mut bytes = Vec::new();
    while let Some(frame) = tokio::select! {biased;_=cancel.cancelled()=>return Ok(response(400,"application/json","")),frame=body.frame()=>frame}
    {
        let Ok(frame) = frame else {
            return Ok(response(400, "application/json", ""));
        };
        if let Ok(data) = frame.into_data() {
            if bytes.len().saturating_add(data.len()) > router.config.max_body_bytes {
                return Ok(response(413, "application/json", ""));
            }
            bytes.extend_from_slice(&data);
        }
    }
    let Ok(doc) = JsDocument::parse(&bytes) else {
        return Ok(response(400, "application/json", ""));
    };
    let header = |key| {
        parts
            .headers
            .get(key)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned()
    };
    let class = header("x-claude-code-request-class");
    let options = RouteOptions {
        scope: json!([
            parts
                .headers
                .get("x-claude-code-session-id")
                .and_then(|v| v.to_str().ok()),
            parts
                .headers
                .get("x-claude-code-agent-id")
                .and_then(|v| v.to_str().ok())
        ])
        .to_string(),
        request_class: class.clone(),
        prompt_id: header("x-claude-code-prompt-id"),
        ..Default::default()
    };
    let doc = Arc::new(doc);
    let Ok(decision) = router
        .observe(
            doc.clone(),
            options,
            &parts.headers,
            &cancel,
            parts.uri.query().unwrap_or(""),
            bytes.len(),
        )
        .await
    else {
        return Ok(response(400, "application/json", ""));
    };
    let (kind, message) = stub_message(&doc, decision.as_ref());
    Ok(response(200, &kind, message))
}
struct Stub {
    address: std::net::SocketAddr,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl Stub {
    async fn listen(router: Arc<ProbeRouter>, done: CancellationToken) -> Result<Self, String> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| "Cannot bind local context probe")?;
        let address = listener
            .local_addr()
            .map_err(|_| "Cannot inspect local context probe")?;
        let cancel = CancellationToken::new();
        let signal = cancel.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = signal.cancelled() => break,
                    result = listener.accept() => {
                        let Ok((stream,_)) = result else {break;};
                        if connections.len() >= 128 {continue;}
                        let (router,done,cancel)=(router.clone(),done.clone(),signal.clone());
                        connections.spawn(async move {
                            use autorouter_runtime::transport_completion::{CompletionRegistry,Delivery,serve_http1};
                            let registry=CompletionRegistry::new(128);
                            let receipts=registry.clone();
                            let service=service_fn(move |req:Request<Incoming>| {
                                let (router,done,cancel,receipts)=(router.clone(),done.clone(),cancel.clone(),receipts.clone());
                                async move {
                                    let main=req.headers().get("x-claude-code-request-class").is_none_or(|h|h.as_bytes().is_empty()||h=="main");
                                    let response=stub_request(req,router,cancel).await.unwrap();
                                    let complete=main&&response.status().is_success();
                                    receipts.track(response,move |delivery| {
                                        if complete&&delivery==Delivery::Flushed {done.cancel();}
                                    }).map_err(|_|std::io::Error::other("Probe connection capacity"))
                                }
                            });
                            let _=serve_http1(stream,registry,service).await;
                        });
                    },
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        Ok(Self {
            address,
            cancel,
            task,
        })
    }
    async fn close(self) {
        self.cancel.cancel();
        let _ = self.task.await;
    }
}

pub fn run(args: &[String], _root: &Path) -> Result<bool, String> {
    let options = parse_args(
        args,
        &std::env::current_dir().map_err(|_| "Cannot resolve working directory")?,
    )?;
    if options["help"] == true {
        println!(
            "Usage: cargo xtask context-probe [--cwd PATH] [--tool-search unset|true|false] [--example haiku|sonnet|opus] [--classify] [--live] [--interactive]\nExplicit startup diagnostic; Claude can execute configured MCP servers and startup customizations even in stub mode. Default answers locally without inference or emitted tool calls. --classify contacts the evaluator; --live also calls Anthropic with tool choice none. Reports metadata only. Interactive mode requires a macOS terminal."
        );
        return Ok(true);
    }
    let interactive = options["interactive"] == true;
    if interactive && !cfg!(target_os = "macos") {
        return Err("--interactive currently requires macOS /usr/bin/script".into());
    }
    if interactive && !std::io::stdin().is_terminal() {
        return Err("--interactive requires a terminal on stdin".into());
    }
    crate::evaluation::runtime()?.block_on(execute(options))
}
async fn execute(options: Value) -> Result<bool, String> {
    let interactive = options["interactive"] == true;
    let signals = Signals::new();
    let mut env = crate::evaluation::environment();
    env["AUTOROUTER_AUTH_MODE"] = json!("subscription");
    env["AUTOROUTER_CLIENT_PROFILE"] = json!("compatible");
    env["AUTOROUTER_TOKEN"] = json!(tool_process::token()?);
    let config = read_config(&env, false, Path::new(options["cwd"].as_str().unwrap()))?;
    let live = options["live"] == true;
    let classify = live || options["classify"] == true;
    if classify {
        require_keys(&config)?;
    }
    let transport =
        Arc::new(NativeHttpClient::new().map_err(|_| "Cannot construct HTTP transport")?);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let prompt = example(options["example"].as_str().unwrap_or(""))
        .unwrap_or("Reply only OK.")
        .to_owned();
    let router = Arc::new(ProbeRouter {
        inner: classify.then(|| Router::new(transport.clone(), config.clone())),
        config: config.clone(),
        prompt: prompt.clone(),
        live,
        requests: requests.clone(),
    });
    let done = CancellationToken::new();
    let state = Arc::new(Mutex::new(StatusState::new(&config.models.opus)));
    let usages = Arc::new(Mutex::new(Vec::new()));
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let mut gateway = None;
    let mut stub = None;
    let address = if live {
        let (state, usages, statuses, done) = (
            state.clone(),
            usages.clone(),
            statuses.clone(),
            done.clone(),
        );
        let sinks = EventSinks {
            status: Some(Arc::new(move |event| {
                let event = event.to_serde_observation_lossy();
                state.lock().unwrap().update(&event, now_ms());
                match event["event"].as_str() {
                    Some("upstream_response") => {
                        statuses.lock().unwrap().push(event["status"].clone())
                    }
                    Some("upstream_usage") => usages.lock().unwrap().push(event["usage"].clone()),
                    Some("request_complete")
                        if event["request_class"]
                            .as_str()
                            .is_none_or(|s| s.is_empty() || s == "main") =>
                    {
                        done.cancel()
                    }
                    _ => {}
                }
            })),
            ..Default::default()
        };
        let handle = Gateway::with_router(config.clone(), transport, router.clone(), sinks)?
            .listen(0)
            .await?;
        let address = handle.address;
        gateway = Some(handle);
        address
    } else {
        let handle = Stub::listen(router.clone(), done.clone()).await?;
        let address = handle.address;
        stub = Some(handle);
        address
    };
    let mut env = build_claude_env(
        &config,
        &format!("http://{address}"),
        &tool_process::environment(),
    );
    if options["toolSearch"] == "unset" {
        env.remove(std::ffi::OsStr::new("ENABLE_TOOL_SEARCH"));
    } else {
        env.insert(
            "ENABLE_TOOL_SEARCH".into(),
            options["toolSearch"].as_str().unwrap().into(),
        );
    }
    env.insert("MCP_CONNECTION_NONBLOCKING".into(), "0".into());
    let mut args = Vec::new();
    if !interactive || live {
        args.extend(["--permission-mode".into(), "dontAsk".into()]);
    }
    if !interactive {
        args.extend(
            [
                "--print",
                "--no-session-persistence",
                "--output-format",
                "stream-json",
                "--verbose",
            ]
            .map(str::to_owned),
        );
    }
    args.push(prompt);
    let mut command = if interactive {
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLUMNS".into(), "160".into());
        env.insert("LINES".into(), "40".into());
        let mut command = Command::new("/usr/bin/script");
        command.args(["-q", "/dev/null", "claude"]);
        command
    } else {
        Command::new("claude")
    };
    command
        .args(&args)
        .current_dir(options["cwd"].as_str().unwrap())
        .env_clear()
        .envs(env);
    let mut ui = json!({"trust_prompt":false,"api_error":false,"unknown_option":false});
    let result = tool_process::run_child(
        &mut command,
        RunOptions {
            timeout: Duration::from_secs(60),
            grace: Duration::from_secs(3),
            max_stdout: None,
            interactive,
            response: &done,
            cancel: &signals.token,
            initial: InputAction {
                close: true,
                ..Default::default()
            },
        },
        |bytes| {
            if interactive {
                let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
                ui["trust_prompt"] = json!(
                    ui["trust_prompt"] == true
                        || [
                            "trust this folder",
                            "trust this directory",
                            "trust the files"
                        ]
                        .iter()
                        .any(|s| text.contains(s))
                );
                ui["api_error"] = json!(
                    ui["api_error"] == true
                        || text.contains("api error")
                        || text.contains("invalid api key")
                );
                ui["unknown_option"] =
                    json!(ui["unknown_option"] == true || text.contains("unknown option"));
            }
            InputAction::default()
        },
    )
    .await;
    if let Some(handle) = gateway {
        handle.close().await;
    }
    if let Some(handle) = stub {
        handle.close().await;
    }
    router.shutdown();
    let mut report = json!({"probe":if live{"live_inference_tools_prohibited"}else if classify{"local_stub_live_evaluator"}else{"local_stub_no_inference"},"evaluator":if config.evaluator==Evaluator::Ollama{"ollama"}else{"jev"},"mode":if interactive{"interactive"}else{"print"},"example":options.get("example").cloned().unwrap_or(json!("ok")),"tool_search":options["toolSearch"],"code":result["exit_code"],"signal":result["exit_signal"],"timed_out":result["timed_out"],"stdout_bytes":result["stdout_bytes"],"stderr_bytes":result["stderr_bytes"],"requests":*requests.lock().unwrap()});
    if result["spawn_error"] == true {
        report["spawn_error"] = json!(true);
    }
    if interactive {
        report["response_received"] = result["response_received"].clone();
        report["controlled_stop"] = result["controlled_stop"].clone();
        report["ui_signals"] = ui;
    }
    if live {
        let snapshot = state.lock().unwrap().snapshot(std::process::id(), now_ms());
        let lines: Vec<_> = snapshot["sessions"]
            .as_object()
            .unwrap()
            .keys()
            .map(|id| {
                render_status_line(
                    &json!({"session_id":id}),
                    &snapshot,
                    &json!({"color":false,"columns":240}),
                )
            })
            .collect();
        report["upstream_statuses"] = json!(*statuses.lock().unwrap());
        report["provider_usage"] = json!(*usages.lock().unwrap());
        report["status_lines"] = json!(lines);
    }
    let passed = (report["code"] == 0 || report["controlled_stop"] == true)
        && !requests.lock().unwrap().is_empty()
        && (!live
            || (!usages.lock().unwrap().is_empty()
                && statuses.lock().unwrap().iter().all(|s| *s == 200)));
    println!("{report}");
    Ok(passed)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_preserves_exact_sizes_but_omits_private_payloads() {
        let config = read_config(&json!({}), false, Path::new("/tmp")).unwrap();
        let body = json!({"model":config.models.haiku,"messages":[{"role":"user","content":"private-canary"}],"tools":[{"name":"mcp__safe-server__tool","defer_loading":true,"description":"secret schema","input_schema":{"type":"object"}},{"name":"ToolSearch","type":"tool_search_tool_regex_20251119"}]});
        let doc = JsDocument::parse(body.to_string().as_bytes()).unwrap();
        let metadata = summarize(&doc, "main", 123, &config, "private-canary");
        assert_eq!(metadata["body_bytes"], 123);
        assert_eq!(metadata["groups"]["safe-server"]["tools"], 1);
        assert_eq!(metadata["tool_search_present"], true);
        assert_eq!(metadata["evaluator_contains_entered_prompt"], true);
        assert!(!metadata.to_string().contains("private-canary"));
        assert!(!metadata.to_string().contains("secret schema"));
    }
    #[test]
    fn stub_never_emits_tool_actions_and_keeps_stream_protocol() {
        for stream in [false, true] {
            let doc = JsDocument::parse(
                json!({"model":"test","stream":stream})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
            let (kind, body) = stub_message(&doc, None);
            assert!(!body.contains("tool_use"));
            assert!(body.contains("OK"));
            assert_eq!(
                kind,
                if stream {
                    "text/event-stream"
                } else {
                    "application/json"
                }
            );
            if stream {
                assert!(
                    body.ends_with("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n")
                );
            }
        }
    }
    #[tokio::test]
    async fn local_stub_authentication_bounds_and_exact_model_identity() {
        use autorouter_runtime::http_client::HttpTransport;
        let mut config=read_config(&json!({"AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_TOKEN":"synthetic-probe-token"}),false,Path::new("/tmp")).unwrap();
        config.max_body_bytes = 1024;
        let router = Arc::new(ProbeRouter {
            inner: None,
            config,
            prompt: "Reply only OK.".into(),
            live: false,
            requests: Arc::new(Mutex::new(Vec::new())),
        });
        let done = CancellationToken::new();
        let server = Stub::listen(router, done.clone()).await.unwrap();
        let client = NativeHttpClient::new().unwrap();
        let base = format!("http://{}/v1/messages", server.address);
        for (token,body,status) in [("wrong","{}".into(),401),("synthetic-probe-token"," ".repeat(1025),413),("synthetic-probe-token",r#"{"model":"unknown\ud800","messages":[{"role":"user","content":"Reply only OK."}],"stream":false}"#.into(),200)] {
            let response=tokio::time::timeout(Duration::from_secs(3),client.request(Request::post(&base).header(LOCAL_AUTH_HEADER,token).body(Full::new(Bytes::from(body))).unwrap())).await.expect("bounded probe response headers").unwrap();
            assert_eq!(response.status().as_u16(),status);
            let bytes=tokio::time::timeout(Duration::from_secs(3),response.into_body().collect()).await.expect("bounded probe response body").unwrap().to_bytes();
            if status==200 {assert!(String::from_utf8_lossy(&bytes).contains(r#""model":"unknown\ud800""#));}
        }
        assert!(done.is_cancelled());
        server.close().await;
    }
}
