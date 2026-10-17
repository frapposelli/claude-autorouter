//! Explicitly invoked ordinary CLI integration against an owned synthetic server.
//! No real model, provider, account, credential, download or timing assertion.
use super::*;
use crate::ollama_evaluation::{body_for, stress_case};
use crate::process::owned::Output;
use autorouter_core::prompt_state::build_ollama_state_document;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::{JoinHandle, JoinSet};

struct Model {
    expected: BTreeMap<String, String>,
    resident: AtomicBool,
    drift: AtomicBool,
    hold: AtomicBool,
    inference: AtomicUsize,
    unload: AtomicUsize,
    invalid: AtomicUsize,
    first_inference: Notify,
    cancel: CancellationToken,
}
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(k, v)| (k.clone(), canonical(v)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
        Value::Number(number) => json!(number.as_f64().unwrap()),
        _ => value.clone(),
    }
}
fn state_key(state: &Value) -> String {
    digest(canonical(state).to_string().as_bytes())
}
impl Model {
    fn new() -> Arc<Self> {
        let selected =
            schema::fixture(include_bytes!("../../../test/fixtures/ollama-routing.json")).unwrap();
        let items = selected.into_iter().chain((0..8).map(stress_case));
        let mut expected = BTreeMap::new();
        for item in items {
            let state = build_ollama_state_document(&body_for(&item), 3000);
            let value: Value = serde_json::from_slice(state.stringify().as_bytes()).unwrap();
            assert!(
                expected
                    .insert(state_key(&value), item["expected"].as_str().unwrap().into())
                    .is_none()
            );
        }
        Arc::new(Self {
            expected,
            resident: AtomicBool::new(false),
            drift: AtomicBool::new(false),
            hold: AtomicBool::new(false),
            inference: AtomicUsize::new(0),
            unload: AtomicUsize::new(0),
            invalid: AtomicUsize::new(0),
            first_inference: Notify::new(),
            cancel: CancellationToken::new(),
        })
    }
    async fn request(
        self: Arc<Self>,
        request: Request<Incoming>,
    ) -> Result<Response<Full<Bytes>>, std::convert::Infallible> {
        let (parts, body) = request.into_parts();
        if parts.headers.contains_key("authorization") || parts.headers.contains_key("x-api-key") {
            self.invalid.fetch_add(1, Ordering::SeqCst);
        }
        let body = body.collect().await.unwrap().to_bytes();
        assert!(body.len() <= 65536);
        let data: Value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap()
        };
        let value = match parts.uri.path() {
            "/api/version" => json!({"version":"0.40.0"}),
            "/api/tags" => {
                json!({"models":[{"name":"synthetic:1","digest":if self.drift.load(Ordering::SeqCst){"b".repeat(64)}else{"a".repeat(64)},"size":100,"capabilities":["decision"],"details":{"parameter_size":"synthetic","quantization_level":"synthetic"}}]})
            }
            "/api/show" => {
                assert_eq!(data, json!({"model":"synthetic:1"}));
                json!({"capabilities":["decision"],"details":{"parameter_size":"synthetic","quantization_level":"synthetic"}})
            }
            "/api/ps" => {
                json!({"models":if self.resident.load(Ordering::SeqCst){vec![json!({"name":"synthetic:1","digest":"a".repeat(64),"size":100,"size_vram":100,"context_length":2048,"details":{"parameter_size":"synthetic","quantization_level":"synthetic"}})]}else{vec![]}})
            }
            "/api/generate" => {
                assert_eq!(
                    data,
                    json!({"model":"synthetic:1","stream":false,"keep_alive":0})
                );
                self.unload.fetch_add(1, Ordering::SeqCst);
                self.resident.store(false, Ordering::SeqCst);
                json!({"done":true})
            }
            "/v1/systemone" => {
                assert_eq!(data["model"], "synthetic:1");
                assert_eq!(data["questions"], ollama_questions());
                assert_eq!(data["keep_alive"], "5m");
                self.resident.store(true, Ordering::SeqCst);
                self.inference.fetch_add(1, Ordering::SeqCst);
                self.first_inference.notify_one();
                if self.hold.load(Ordering::SeqCst) {
                    self.cancel.cancelled().await;
                    return Ok(Response::builder()
                        .status(503)
                        .body(Full::new(Bytes::new()))
                        .unwrap());
                }
                let key = state_key(&data["state"]);
                let Some(tier) = self.expected.get(&key) else {
                    self.invalid.fetch_add(1, Ordering::SeqCst);
                    return Ok(Response::builder()
                        .status(503)
                        .body(Full::new(Bytes::new()))
                        .unwrap());
                };
                json!({"model":"synthetic:1","answers":{"tier":{"type":"choice","choice":tier,"confidence":1,"probabilities":{"haiku":u8::from(tier=="haiku"),"sonnet":u8::from(tier=="sonnet"),"opus":u8::from(tier=="opus")}}},"usage":{"input_tokens":700,"output_tokens":0}})
            }
            _ => {
                self.invalid.fetch_add(1, Ordering::SeqCst);
                return Ok(Response::builder()
                    .status(404)
                    .body(Full::new(Bytes::new()))
                    .unwrap());
            }
        };
        Ok(Response::builder()
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(value.to_string())))
            .unwrap())
    }
}
struct Host {
    endpoint: String,
    model: Arc<Model>,
    worker: Option<JoinHandle<()>>,
}
impl Host {
    async fn new() -> Self {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let model = Model::new();
        let served = model.clone();
        let worker = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {biased;_=served.cancel.cancelled()=>break,result=listener.accept()=>{let(stream,_)=result.unwrap();let model=served.clone();connections.spawn(async move{let cancel=model.cancel.clone();let service=service_fn(move|request|model.clone().request(request));tokio::select!{_=cancel.cancelled()=>{},_=hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream),service)=>{}}});},_=connections.join_next(),if !connections.is_empty()=>{}}
            }
            served.cancel.cancel();
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        Self {
            endpoint,
            model,
            worker: Some(worker),
        }
    }
    async fn close(mut self) {
        self.model.cancel.cancel();
        self.worker.take().unwrap().await.unwrap();
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.model.cancel.cancel();
        if let Some(worker) = &self.worker {
            worker.abort();
        }
    }
}
fn paths() -> (PathBuf, PathBuf, PathBuf) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let helper = PathBuf::from(
        std::env::var_os("AUTOROUTER_TEST_MATCHED_XTASK")
            .expect("Supply separately built matching xtask explicitly"),
    );
    let node = PathBuf::from(
        std::env::var_os("AUTOROUTER_TEST_PINNED_NODE")
            .expect("Supply pinned Node22.14 executable explicitly"),
    );
    assert!(helper.is_absolute() && node.is_absolute());
    (root, helper, node)
}
fn command(helper: &Path, root: &Path, args: &[String], temporary: &Path) -> Command {
    let mut command = Command::new(helper);
    command
        .arg("compare-local-evaluators")
        .args(args)
        .current_dir(root);
    child_env(&mut command, temporary);
    command
}
fn prepare_args(host: &Host, node: &Path, output: &Path) -> Vec<String> {
    vec![
        "prepare".into(),
        "--model".into(),
        "synthetic:1".into(),
        "--node".into(),
        node.to_str().unwrap().into(),
        "--warm-timeout-ms".into(),
        "0".into(),
        "--endpoint".into(),
        host.endpoint.clone(),
        "--conditions".into(),
        "normal-use-exclusive-ollama-builds-paused".into(),
        "--output".into(),
        output.to_str().unwrap().into(),
    ]
}
fn run_args(declaration: &Path, output: &Path) -> Vec<String> {
    vec![
        "run".into(),
        "--declaration".into(),
        declaration.to_str().unwrap().into(),
        "--conditions".into(),
        "normal-use-exclusive-ollama-builds-paused".into(),
        "--output".into(),
        output.to_str().unwrap().into(),
    ]
}
async fn call(command: Command) -> Output {
    let mut owner = Owner::start(command, &CancellationToken::new());
    owner.join().await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Explicit matching xtask/pinned Node binaries;990 synthetic loopback calls, never real models"]
async fn ordinary_cli_prepare_run_full_fixed_matrix_create_only_and_identity_guards() {
    let (root, helper, node) = paths();
    let scratch = crate::tool_process::Scratch::new("paired-cli-integration").unwrap();
    let host = Host::new().await;
    let declaration = scratch.0.join("declaration");
    let run = scratch.0.join("run");
    let output = call(command(
        &helper,
        &root,
        &prepare_args(&host, &node, &declaration),
        &scratch.0,
    ))
    .await;
    assert_eq!(
        output.status["exit_code"], 0,
        "prepare failed; inspect retained bounded private output"
    );
    assert_eq!(host.model.inference.load(Ordering::SeqCst), 0);
    assert_eq!(host.model.unload.load(Ordering::SeqCst), 0);
    let original = regular(&declaration.join("declaration.json"), MAX_REPORT).unwrap();
    let output = call(command(
        &helper,
        &root,
        &prepare_args(&host, &node, &declaration),
        &scratch.0,
    ))
    .await;
    assert_eq!(output.status["exit_code"], 1);
    assert_eq!(
        regular(&declaration.join("declaration.json"), MAX_REPORT).unwrap(),
        original
    );
    assert_eq!(host.model.inference.load(Ordering::SeqCst), 0);
    let output = call(command(
        &helper,
        &root,
        &run_args(&declaration, &run),
        &scratch.0,
    ))
    .await;
    assert_eq!(
        output.status["exit_code"], 0,
        "complete run failed; inspect retained bounded private output"
    );
    let comparison: Value =
        serde_json::from_slice(&regular(&run.join("comparison.json"), MAX_REPORT).unwrap())
            .unwrap();
    assert_eq!(comparison["complete"], true);
    assert_eq!(comparison["language_port_equal"], true);
    assert_eq!(comparison["strict_model_quality_passed"], true);
    assert_eq!(comparison["pairs"].as_array().unwrap().len(), 5);
    assert_eq!(host.model.inference.load(Ordering::SeqCst), 990);
    assert_eq!(host.model.unload.load(Ordering::SeqCst), 10);
    assert_eq!(host.model.invalid.load(Ordering::SeqCst), 0);
    assert!(!host.model.resident.load(Ordering::SeqCst));
    let retained = regular(&run.join("comparison.json"), MAX_REPORT).unwrap();
    let output = call(command(
        &helper,
        &root,
        &run_args(&declaration, &run),
        &scratch.0,
    ))
    .await;
    assert_eq!(output.status["exit_code"], 1);
    assert_eq!(
        regular(&run.join("comparison.json"), MAX_REPORT).unwrap(),
        retained
    );
    assert_eq!(host.model.inference.load(Ordering::SeqCst), 990);
    host.model.drift.store(true, Ordering::SeqCst);
    let refused = scratch.0.join("drift-run");
    let output = call(command(
        &helper,
        &root,
        &run_args(&declaration, &refused),
        &scratch.0,
    ))
    .await;
    assert_eq!(output.status["exit_code"], 1);
    assert!(!refused.exists());
    assert_eq!(host.model.inference.load(Ordering::SeqCst), 990);
    assert_eq!(host.model.unload.load(Ordering::SeqCst), 10);
    host.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Explicit matching xtask/pinned Node binaries;owned mock cancellation, never real models"]
async fn ordinary_cli_cancelled_trial_joins_engine_and_unloads_only_admitted_candidate() {
    let (root, helper, node) = paths();
    let scratch = crate::tool_process::Scratch::new("paired-cli-cancel").unwrap();
    let host = Host::new().await;
    let declaration = scratch.0.join("declaration");
    let run = scratch.0.join("cancelled-run");
    let output = call(command(
        &helper,
        &root,
        &prepare_args(&host, &node, &declaration),
        &scratch.0,
    ))
    .await;
    assert_eq!(output.status["exit_code"], 0);
    host.model.hold.store(true, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let mut owner = Owner::start(
        command(&helper, &root, &run_args(&declaration, &run), &scratch.0),
        &cancel,
    );
    tokio::time::timeout(
        Duration::from_secs(30),
        host.model.first_inference.notified(),
    )
    .await
    .expect("Cancel only after genuine synthetic inference admission");
    cancel.cancel();
    let output = owner.join().await.unwrap();
    assert_eq!(output.status["cancelled"], true);
    let comparison: Value =
        serde_json::from_slice(&regular(&run.join("comparison.json"), MAX_REPORT).unwrap())
            .unwrap();
    assert_eq!(comparison["complete"], false);
    assert_eq!(comparison["passed"], false);
    assert_eq!(comparison["failures"][0]["cancelled"], true);
    assert_eq!(host.model.inference.load(Ordering::SeqCst), 1);
    assert_eq!(host.model.unload.load(Ordering::SeqCst), 1);
    assert!(!host.model.resident.load(Ordering::SeqCst));
    assert_eq!(host.model.invalid.load(Ordering::SeqCst), 0);
    host.close().await;
}
