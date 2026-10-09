//! Mock-only admission, quality, cleanup and privacy checks for opt-in tools.
use autorouter_core::config::{DEFAULT_OLLAMA_MODEL, read_config};
use autorouter_core::evaluation_report::create_evaluation_policy;
use autorouter_runtime::http_client::{HttpError, HttpTransport};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio_util::sync::CancellationToken;
#[derive(Default)]
struct Options {
    resident: bool,
    missing: bool,
    cloud: bool,
    wrong: bool,
    fail: bool,
}
struct Mock {
    options: Options,
    resident: AtomicBool,
    calls: Mutex<Vec<(String, Value)>>,
}
impl Mock {
    fn new(options: Options) -> Arc<Self> {
        Arc::new(Self {
            resident: AtomicBool::new(false),
            options,
            calls: Mutex::new(Vec::new()),
        })
    }
    fn paths(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(s, _)| s.clone())
            .collect()
    }
}
impl HttpTransport for Mock {
    type ResponseBody = Full<Bytes>;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        assert!(request.headers().get("authorization").is_none());
        assert!(request.headers().get("x-api-key").is_none());
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let path = parts.uri.path();
        self.calls.lock().unwrap().push((path.into(), body.clone()));
        let value = match path {
            "/api/ps" => {
                json!({"models":if self.options.resident{vec![json!({"name":"unrelated:latest"})]}else if self.resident.load(Ordering::SeqCst){vec![json!({"name":DEFAULT_OLLAMA_MODEL})]}else{Vec::<Value>::new()}})
            }
            "/api/tags" => {
                json!({"models":if self.options.missing{Vec::new()}else{vec![json!({"name":DEFAULT_OLLAMA_MODEL})]}})
            }
            "/api/version" => json!({"version":"0.35.0"}),
            "/api/show" => {
                if self.options.cloud {
                    json!({"remote_host":"private-cloud-host"})
                } else {
                    json!({"details":{"parameter_size":"9B"}})
                }
            }
            "/api/generate" => {
                assert_eq!(body["keep_alive"], 0);
                assert_eq!(body["model"], DEFAULT_OLLAMA_MODEL);
                self.resident.store(false, Ordering::SeqCst);
                json!({})
            }
            "/v1/systemone" => {
                self.resident.store(true, Ordering::SeqCst);
                assert_eq!(body["model"], DEFAULT_OLLAMA_MODEL);
                assert!(
                    !body["state"]
                        .to_string()
                        .contains("SYNTHETIC_REMINDER_ONLY")
                );
                if self.options.fail {
                    return Ok(Response::builder()
                        .status(503)
                        .body(Full::new(Bytes::from_static(b"private-provider-failure")))
                        .unwrap());
                }
                let task = body["state"]["current_task"].as_str().unwrap_or("");
                let tier = if self.options.wrong {
                    "sonnet"
                } else if task.contains("opus") {
                    "opus"
                } else if task.contains("sonnet") {
                    "sonnet"
                } else {
                    "haiku"
                };
                json!({"model":DEFAULT_OLLAMA_MODEL,"answers":{"tier":{"type":"choice","choice":tier,"confidence":1,"probabilities":{"haiku":u8::from(tier=="haiku"),"sonnet":u8::from(tier=="sonnet"),"opus":u8::from(tier=="opus")}}},"usage":{"input_tokens":100,"output_tokens":1}})
            }
            _ => panic!("Unexpected tool HTTP path: {path}"),
        };
        Ok(Response::new(Full::new(Bytes::from(value.to_string()))))
    }
}
fn fixtures() -> Value {
    json!([{"id":"haiku-case","name":"haiku-case","expected":"haiku","split":"heldout","prompt":"Synthetic haiku task"},{"id":"sonnet-case","name":"sonnet-case","expected":"sonnet","split":"heldout","prompt":"Synthetic sonnet task"},{"id":"opus-case","name":"opus-case","expected":"opus","split":"heldout","prompt":"Synthetic opus task"}])
}
#[tokio::test]
async fn routing_integration_warms_once_and_never_counts_or_unloads() {
    let mock = Mock::new(Options::default());
    let config = read_config(
        &json!({"AUTOROUTER_AUTH_MODE":"subscription"}),
        false,
        Path::new("/tmp"),
    )
    .unwrap();
    let report = crate::ollama_routing::run_tests(
        mock.clone(),
        &config,
        fixtures().to_string().as_bytes(),
        &CancellationToken::new(),
        &mut |_| {},
    )
    .await
    .unwrap();
    assert_eq!(report["passed"], true);
    assert_eq!(
        mock.paths()
            .iter()
            .filter(|p| *p == "/v1/systemone")
            .count(),
        4
    );
    assert!(!mock.paths().contains(&"/api/generate".into()));
    assert!(!report.to_string().contains("Synthetic haiku task"));
    assert_eq!(report["downloads"], 0);
}
#[tokio::test]
async fn routing_integration_refuses_unsafe_preflight_without_task_calls() {
    for options in [
        Options {
            resident: true,
            ..Default::default()
        },
        Options {
            missing: true,
            ..Default::default()
        },
        Options {
            cloud: true,
            ..Default::default()
        },
    ] {
        let mock = Mock::new(options);
        let config = read_config(&json!({}), false, Path::new("/tmp")).unwrap();
        assert!(
            crate::ollama_routing::run_tests(
                mock.clone(),
                &config,
                fixtures().to_string().as_bytes(),
                &CancellationToken::new(),
                &mut |_| {}
            )
            .await
            .is_err()
        );
        assert!(!mock.paths().contains(&"/v1/systemone".into()));
        assert!(!mock.paths().contains(&"/api/generate".into()));
        assert!(!mock.paths().contains(&"/api/pull".into()));
    }
}
#[tokio::test]
async fn evaluator_transport_success_cannot_hide_rubric_failure() {
    for wrong in [false, true] {
        let mock = Mock::new(Options {
            wrong,
            ..Default::default()
        });
        let config = read_config(&json!({}), false, Path::new("/tmp")).unwrap();
        let policy = create_evaluation_policy(None).unwrap();
        let report = crate::evaluation::run_evaluation(
            mock.clone(),
            &config,
            &fixtures(),
            policy,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(report["passed"], !wrong);
        assert_eq!(report["fallback_count"], 0);
        assert_eq!(
            mock.paths()
                .iter()
                .filter(|p| *p == "/v1/systemone")
                .count(),
            3
        );
        assert!(
            report["rows"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["confirmed_model"].is_null())
        );
    }
}
#[tokio::test]
async fn local_benchmark_excludes_cold_and_unloads_only_owned_candidate_on_failure() {
    let options =
        crate::ollama_evaluation::parse_args(&["--rounds".into(), "1".into()], Path::new("/tmp"))
            .unwrap();
    for fail in [false, true] {
        let mock = Mock::new(Options {
            fail,
            ..Default::default()
        });
        let report = crate::ollama_evaluation::run_benchmark(
            mock.clone(),
            &options,
            fixtures().to_string().as_bytes(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(report["passed"], !fail);
        assert_eq!(mock.paths().last().unwrap(), "/api/generate");
        assert_eq!(
            mock.paths()
                .iter()
                .filter(|p| *p == "/api/generate")
                .count(),
            1
        );
        assert_eq!(
            report["models"][0]["rows"].as_array().unwrap().len(),
            if fail { 0 } else { 3 }
        );
        assert!(!report.to_string().contains("private-provider-failure"));
        assert!(!mock.resident.load(Ordering::SeqCst));
    }
    let mock = Mock::new(Options {
        resident: true,
        ..Default::default()
    });
    assert!(
        crate::ollama_evaluation::run_benchmark(
            mock.clone(),
            &options,
            fixtures().to_string().as_bytes(),
            &CancellationToken::new()
        )
        .await
        .is_err()
    );
    assert_eq!(mock.paths(), ["/api/ps"]);
}
