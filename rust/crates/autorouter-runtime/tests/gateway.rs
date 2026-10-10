//! Actual loopback HTTP: the versioned synthetic provider corpus is shared with
//! the frozen JavaScript suite. No provider, model, or user state is contacted.
use autorouter_core::config::read_config;
use autorouter_runtime::http_client::{HttpTransport, NativeHttpClient};
use autorouter_runtime::server::Gateway;
use autorouter_runtime::server_events::EventSinks;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::task::{Context, Poll};

fn wire(response: &Value) -> Vec<u8> {
    let ending = response["line_ending"].as_str().unwrap();
    let mut wire = response["prelude"].as_str().unwrap().to_owned();
    for event in response["events"].as_array().unwrap() {
        wire.push_str(&format!(
            "event: {}{ending}data: {event}{ending}{ending}",
            event["type"].as_str().unwrap()
        ));
    }
    wire.into_bytes()
}
struct Chunks {
    chunks: VecDeque<Bytes>,
    yielded: bool,
}
impl Chunks {
    fn new(bytes: Vec<u8>) -> Self {
        let mut chunks = VecDeque::new();
        let mut start = 0;
        // Fixed pseudo-random fragmentation includes boundaries inside UTF-8,
        // frame names, line endings and JSON escape sequences.
        while start < bytes.len() {
            let end = (start + 1 + (start * 17 % 43)).min(bytes.len());
            chunks.push_back(Bytes::copy_from_slice(&bytes[start..end]));
            start = end;
        }
        Self {
            chunks,
            yielded: false,
        }
    }
}
impl Body for Chunks {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let this = self.get_mut();
        if !this.yielded {
            this.yielded = true;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        this.yielded = false;
        Poll::Ready(this.chunks.pop_front().map(|bytes| Ok(Frame::data(bytes))))
    }
}

#[tokio::test]
async fn protocol_v1_corpus_preserves_bytes_calls_policy_and_confirmed_continuity() {
    let corpus: Value = serde_json::from_str(include_str!(
        "../../../../test/fixtures/claude-protocol-v1.json"
    ))
    .unwrap();
    for scenario in corpus["cases"].as_array().unwrap() {
        let active = Arc::new(Mutex::new(Value::Null));
        let received = Arc::new(Mutex::new(Vec::new()));
        let evaluations = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_address = listener.local_addr().unwrap();
        let active_server = active.clone();
        let received_server = received.clone();
        let evaluations_server = evaluations.clone();
        let mock = tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let active = active_server.clone();
                let received = received_server.clone();
                let evaluations = evaluations_server.clone();
                children.spawn(async move{
                    let service=service_fn(move |request:Request<Incoming>|{
                        let active=active.clone();let received=received.clone();let evaluations=evaluations.clone();
                        async move{
                            let(parts,body)=request.into_parts();let bytes=body.collect().await.unwrap().to_bytes();let step=active.lock().unwrap().clone();
                            let response=if parts.uri.path()=="/v1/systemone" {
                                evaluations.fetch_add(1,Ordering::SeqCst);assert!(step["evaluator_tier"].is_string(),"Unexpected evaluator call");
                                Response::builder().header("content-type","application/json").body(Chunks::new(json!({"answers":{"tier":{"choice":step["evaluator_tier"],"confidence":0.99}}}).to_string().into_bytes())).unwrap()
                            }else{
                                assert_eq!(parts.uri.path(),"/v1/messages","Unexpected token count or endpoint");
                                received.lock().unwrap().push((serde_json::from_slice::<Value>(&bytes).unwrap(),parts.headers,parts.uri.to_string()));
                                Response::builder().header("content-type",step["response"]["content_type"].as_str().unwrap()).header("request-id",format!("synthetic-{}",step["id"].as_str().unwrap())).body(Chunks::new(wire(&step["response"]))).unwrap()
                            };
                            Ok::<_,Infallible>(response)
                        }
                    });let _=hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(socket),service).await;
                });
            }
        });
        let mut config=read_config(&json!({"AUTOROUTER_EVALUATOR":"jev","ANTHROPIC_API_KEY":"synthetic-upstream-key","TYPESAFE_API_KEY":"synthetic-evaluator-key","AUTOROUTER_CLIENT_PROFILE":scenario["profile"],"AUTOROUTER_HAIKU_MODEL":corpus["models"]["haiku"],"AUTOROUTER_SONNET_MODEL":corpus["models"]["sonnet"],"AUTOROUTER_OPUS_MODEL":corpus["models"]["opus"]}),false,std::path::Path::new("/tmp")).unwrap();
        config.local_token = Some("synthetic-protocol-local-token".into());
        config.upstream = format!("http://{upstream_address}");
        config.jev_endpoint = format!("http://{upstream_address}/v1/systemone");
        let records = Arc::new(Mutex::new(Vec::<Value>::new()));
        let sink = records.clone();
        let client = Arc::new(NativeHttpClient::new().unwrap());
        let gateway = Gateway::new(
            config,
            client.clone(),
            EventSinks {
                record: Some(Arc::new(move |record| sink.lock().unwrap().push(record))),
                ..Default::default()
            },
        )
        .unwrap();
        let handle = gateway.listen(0).await.unwrap();
        for (index, step) in scenario["steps"].as_array().unwrap().iter().enumerate() {
            *active.lock().unwrap() = step.clone();
            let before = evaluations.load(Ordering::SeqCst);
            let mut request =
                Request::post(format!("http://{}/v1/messages?beta=true", handle.address))
                    .header("x-api-key", "synthetic-protocol-local-token")
                    .header("content-type", "application/json")
                    .header("anthropic-beta", "synthetic-protocol-contract")
                    .header("anthropic-version", "2023-06-01");
            for (name, value) in step["identity"].as_object().unwrap() {
                request = request.header(
                    format!("x-claude-code-{}", name.replace('_', "-")),
                    value.as_str().unwrap(),
                );
            }
            let response = client
                .request(
                    request
                        .body(Full::new(Bytes::from(step["request"].to_string())))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), 200, "{}", step["id"]);
            assert_eq!(
                response.headers()["request-id"],
                format!("synthetic-{}", step["id"].as_str().unwrap())
            );
            assert_eq!(
                response
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .as_ref(),
                wire(&step["response"]),
                "{} bytes",
                step["id"]
            );
            for _ in 0..100 {
                if records.lock().unwrap().len() >= (index + 1) * 2 {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert_eq!(
                evaluations.load(Ordering::SeqCst) - before,
                usize::from(!step["evaluator_tier"].is_null()),
                "{} evaluator",
                step["id"]
            );
            let received = received.lock().unwrap();
            let (actual, headers, path) = &received[index];
            let mut expected = step["request"].clone();
            expected.as_object_mut().unwrap().extend(
                step["expected_request_overrides"]
                    .as_object()
                    .unwrap()
                    .clone(),
            );
            assert_eq!(*actual, expected, "{} body", step["id"]);
            assert_eq!(path, "/v1/messages?beta=true");
            assert_eq!(headers["x-api-key"], "synthetic-upstream-key");
            assert_eq!(headers["anthropic-beta"], "synthetic-protocol-contract");
            assert_eq!(headers["anthropic-version"], "2023-06-01");
            let records = records.lock().unwrap();
            assert_eq!(
                records.len(),
                (index + 1) * 2,
                "{} records {records:?}",
                step["id"]
            );
            let decision = &records[index * 2];
            let outcome = &records[index * 2 + 1];
            assert_eq!(decision["event"], "decision");
            assert_eq!(outcome["event"], "outcome");
            assert_eq!(outcome["request_id"], decision["request_id"]);
            for (name, value) in step["identity"].as_object().unwrap() {
                assert_eq!(&decision[name], value);
                assert_eq!(&outcome[name], value);
            }
            for field in ["selected_model", "source", "reason", "compatibility_reason"] {
                assert_eq!(
                    decision[field], step["expected"][field],
                    "{} {field}",
                    step["id"]
                );
            }
            assert_eq!(decision["classified_tier"], step["evaluator_tier"]);
            assert_eq!(outcome["status"], "completed");
            for field in [
                "confirmed_model",
                "completion_confirmed",
                "usage_complete",
                "pricing_eligible",
                "unpriced_reason",
                "continuity_state",
            ] {
                assert_eq!(
                    outcome[field], step["expected"][field],
                    "{} {field}",
                    step["id"]
                );
            }
            let transitions = step["expected"]
                .get("model_transitions")
                .cloned()
                .unwrap_or_else(|| json!([step["expected"]["confirmed_model"]]));
            assert_eq!(outcome["model_transitions"], transitions);
            assert!(!decision.as_object().unwrap().contains_key("prompt_excerpt"));
            assert!(!format!("{decision}{outcome}").contains("synthetic-evaluator-key"));
        }
        handle.close().await;
        mock.abort();
        let _ = mock.await;
    }
}
