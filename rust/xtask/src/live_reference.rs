//! Opt-in, temporary frozen Node oracle. The Rust harness owns the same v2
//! fixtures, Claude invocation and independent verifier for either gateway.
use crate::tool_process::{self, InputAction, RunOptions, Scratch};
use autorouter_core::js_json::JsDocument;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
const ADAPTER: &str = include_str!("../../parity/live-gateway.mjs");
type Event = Arc<dyn Fn(Value) + Send + Sync>;

pub struct Reference {
    directory: PathBuf,
    pub provenance: Value,
}
impl Reference {
    pub fn verified(root: &Path) -> Result<Self, String> {
        let directory = root.join("artifacts/rust-rewrite/reference");
        let files = crate::reference::freeze(root, &directory)?;
        let manifest: Value = serde_json::from_slice(&crate::process::read_bounded(
            &root.join("rust/parity/baseline.json"),
            1024 * 1024,
        )?)
        .map_err(|_| "Cannot read frozen reference identity")?;
        Ok(Self {
            directory,
            provenance: json!({
                "baseline_commit":manifest["baseline_commit"], "verified_source_files":files,
                "adapter_sha256":crate::evaluation::digest(ADAPTER.as_bytes()),
                "temporary_reference_only":true,
            }),
        })
    }
}
pub struct Events {
    pub route: Event,
    pub status: Event,
    pub log: Event,
}
pub struct Gateway {
    pub address: SocketAddr,
    pub runtime: Value,
    cancel: CancellationToken,
    task: Option<tokio::task::JoinHandle<Value>>,
    _scratch: Scratch,
}
impl Drop for Gateway {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
impl Gateway {
    pub async fn start(
        reference: &Reference,
        initialization: Value,
        timeout: Duration,
        events: Events,
        cancellation: &CancellationToken,
    ) -> Result<Self, String> {
        let scratch = Scratch::new("live-node-reference")?;
        let adapter = scratch.file("gateway.mjs", ADAPTER.as_bytes())?;
        let mut initial = initialization.to_string().into_bytes();
        if initial.len() > 1024 * 1024 {
            return Err("Reference initialization exceeds its byte limit".into());
        }
        initial.push(b'\n');
        let mut command = Command::new("node");
        command
            .arg("--no-warnings")
            .arg(adapter)
            .arg(&reference.directory)
            .env_clear()
            .envs(tool_process::environment())
            .env_remove("NODE_OPTIONS")
            .env_remove("NODE_PATH");
        let cancel = cancellation.child_token();
        let stopping = cancel.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut ready = Some(sender);
            let mut buffer = Vec::new();
            let mut invalid = false;
            let mut closed = false;
            let unused = CancellationToken::new();
            let result = tool_process::run_child(
                &mut command,
                RunOptions {
                    timeout: timeout + Duration::from_secs(15),
                    grace: Duration::from_secs(2),
                    max_stdout: Some(8 * 1024 * 1024),
                    interactive: false,
                    response: &unused,
                    cancel: &stopping,
                    initial: InputAction {
                        bytes: initial,
                        close: false,
                    },
                },
                |bytes| {
                    buffer.extend_from_slice(bytes);
                    while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
                        let line: Vec<_> = buffer.drain(..=end).collect();
                        let Some(value) = JsDocument::parse(&line)
                            .ok()
                            .map(|d| d.to_serde_observation_lossy())
                        else {
                            invalid = true;
                            stopping.cancel();
                            break;
                        };
                        match value["type"].as_str() {
                            Some("ready") if ready.is_some() => {
                                if let Some(sender) = ready.take() {
                                    let _ = sender.send(value["value"].clone());
                                }
                            }
                            Some("route") if ready.is_none() => {
                                (events.route)(value["value"].clone())
                            }
                            Some("status") if ready.is_none() => {
                                (events.status)(value["value"].clone())
                            }
                            Some("log") if ready.is_none() => (events.log)(value["value"].clone()),
                            Some("closed") => closed = value["value"] == true,
                            _ => {
                                invalid = true;
                                stopping.cancel();
                            }
                        }
                    }
                    InputAction::default()
                },
            )
            .await;
            json!({"process":result,"protocol_valid":!invalid && buffer.is_empty(),"closed":closed})
        });
        let startup = tokio::select! {
            value = tokio::time::timeout(Duration::from_secs(10), receiver) => value.ok().and_then(Result::ok),
            _ = cancellation.cancelled() => None,
        };
        let address = startup
            .as_ref()
            .and_then(|v| v["address"].as_str())
            .and_then(|v| v.parse::<SocketAddr>().ok())
            .filter(|v| v.ip().is_loopback() && v.port() != 0);
        let Some(address) = address else {
            cancel.cancel();
            let _ = task.await;
            return Err(
                "Frozen Node gateway could not become ready; no reference output was logged".into(),
            );
        };
        Ok(Self {
            address,
            runtime: startup.unwrap(),
            cancel,
            task: Some(task),
            _scratch: scratch,
        })
    }
    pub async fn close(mut self) -> Result<Value, String> {
        self.cancel.cancel();
        let result = self
            .task
            .take()
            .expect("owned reference process")
            .await
            .map_err(|_| "Frozen Node gateway cleanup failed")?;
        if result["process"]["exit_code"] != 0
            || result["process"]["timed_out"] == true
            || result["process"]["output_limit_exceeded"] == true
            || result["protocol_valid"] != true
            || result["closed"] != true
        {
            return Err("Frozen Node gateway did not complete its bounded protocol cleanly".into());
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use autorouter_runtime::http_client::{HttpTransport, NativeHttpClient};
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full, Limited};
    use hyper::service::service_fn;
    use hyper::{Request, Response};
    use hyper_util::rt::TokioIo;
    use std::sync::Mutex;

    // Explicit oracle check: no model/Claude executable, downloads, registry,
    // saved configuration or non-loopback services are used. The native-only
    // default Rust suite does not require Node to be installed.
    #[tokio::test]
    #[ignore = "temporary frozen Node oracle; synthetic loopback only"]
    async fn frozen_gateway_routes_identical_v2_fixture_and_reports_only_metadata() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let reference = Reference::verified(&root).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let accepted = tokio::select! {
                    value = listener.accept() => value,
                    _ = connections.join_next(), if !connections.is_empty() => continue,
                };
                let Ok((stream, _)) = accepted else { break };
                let calls = recorded.clone();
                connections.spawn(async move {
                    let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                        let calls = calls.clone();
                        async move {
                            let path = request.uri().path().to_owned();
                            let body = Limited::new(request.into_body(), 1024 * 1024).collect().await.unwrap().to_bytes();
                            let body: Value = serde_json::from_slice(&body).unwrap();
                            calls.lock().unwrap().push((path.clone(), body.clone()));
                            let response = match path.as_str() {
                                "/v1/systemone" => json!({"answers":{"tier":{"choice":"sonnet","confidence":1}}}),
                                "/v1/messages/count_tokens" => json!({"input_tokens":1000}),
                                "/v1/messages" => json!({"id":"synthetic-message","type":"message","role":"assistant","model":body["model"],"content":[{"type":"text","text":"synthetic-PRIVATE-answer"}],"stop_reason":"end_turn","usage":{"input_tokens":100,"output_tokens":3}}),
                                _ => panic!("Unexpected synthetic endpoint"),
                            };
                            Ok::<_, std::convert::Infallible>(Response::builder().header("content-type", "application/json").body(Full::new(Bytes::from(response.to_string()))).unwrap())
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
                });
            }
        });
        let routes = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let (routing, status, log) = (routes.clone(), events.clone(), events.clone());
        let scenario = super::super::cases()["coding"].clone();
        assert!(
            scenario["prompts"][0]
                .as_str()
                .unwrap()
                .contains("cargo test --offline --quiet")
        );
        let gateway = Gateway::start(&reference, json!({
            "env":{"AUTOROUTER_AUTH_MODE":"api-key","AUTOROUTER_EVALUATOR":"jev",
                "AUTOROUTER_JEV_URL":format!("http://{address}/v1/systemone"),"AUTOROUTER_UPSTREAM_URL":format!("http://{address}"),
                "AUTOROUTER_TOKEN":"synthetic-local-credential-12345678","TYPESAFE_API_KEY":"synthetic-evaluator-key","ANTHROPIC_API_KEY":"synthetic-provider-key"},
            "scenario":scenario,"outage":false,"reminder":"unused",
        }), Duration::from_secs(5), Events {
            route: Arc::new(move |value| routing.lock().unwrap().push(value)),
            status: Arc::new(move |value| status.lock().unwrap().push(value)),
            log: Arc::new(move |value| log.lock().unwrap().push(value)),
        }, &CancellationToken::new()).await.unwrap();
        let client = NativeHttpClient::new().unwrap();
        let body = json!({"model":"claude-haiku-4-5-20251001","max_tokens":32,"messages":[{"role":"user","content":scenario["prompts"][0]}]});
        let response = client
            .request(
                Request::post(format!("http://{}/v1/messages", gateway.address))
                    .header("x-api-key", "synthetic-local-credential-12345678")
                    .header("content-type", "application/json")
                    .body(Full::new(Bytes::from(body.to_string())))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let response = Limited::new(response.into_body(), 65536)
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let response: Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(response["model"], "claude-sonnet-5");
        gateway.close().await.unwrap();
        task.abort();
        let _ = task.await;
        let calls = calls.lock().unwrap();
        assert!(calls.iter().any(|(path, body)| path == "/v1/systemone"
            && body["state"]["current_task"] == scenario["prompts"][0]));
        let routes = routes.lock().unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0]["model"], "claude-sonnet-5");
        assert_eq!(routes[0]["expected_classified_tier"], "sonnet");
        assert!(!json!(*routes).to_string().contains("cargo test"));
        assert!(
            !json!(*events.lock().unwrap())
                .to_string()
                .contains("synthetic-PRIVATE-answer")
        );
    }
}
