//! Credential expectations come from actual pinned Node HTTP and Fetch peers.
use super::forwarding_tests::credential_config;
use super::*;
use crate::http_client::NativeHttpClient;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn credential_fields(result: Result<HeaderMap, HttpError>) -> Option<Vec<Vec<u8>>> {
    match result {
        Ok(headers) => {
            for name in [LOCAL_AUTH_HEADER, "authorization", "cookie"] {
                assert!(!headers.contains_key(name));
            }
            assert_eq!(headers["accept-encoding"], "identity");
            assert_eq!(headers["x-synthetic-pass-through"], "retained");
            Some(
                headers
                    .get_all("x-api-key")
                    .iter()
                    .map(|v| v.as_bytes().to_vec())
                    .collect(),
            )
        }
        Err(error) => {
            assert_eq!(error, HttpError::InvalidRequest);
            None
        }
    }
}
fn credential_incoming() -> HeaderMap {
    let mut headers = HeaderMap::new();
    for name in [LOCAL_AUTH_HEADER, "x-api-key", "authorization", "cookie"] {
        headers.insert(name, "synthetic-local-secret".parse().unwrap());
    }
    headers.insert("x-synthetic-pass-through", "retained".parse().unwrap());
    headers
}

#[test]
fn observed_header_values() {
    assert_eq!(OBSERVED_CREDENTIAL_CASES.len(), 39);
    let incoming = credential_incoming();
    for (id, raw, http, count) in OBSERVED_CREDENTIAL_CASES {
        let mut config = credential_config(*raw);
        let before = serde_json::to_string(&config).unwrap();
        assert_eq!(
            credential_fields(upstream_headers(&incoming, &config)),
            http.map(|v| v.iter().map(|v| v.to_vec()).collect()),
            "HTTP {id}"
        );
        assert_eq!(
            credential_fields(count_headers(&incoming, &config)),
            count.map(|v| vec![v.to_vec()]),
            "Fetch {id}"
        );
        assert_eq!(serde_json::to_string(&config).unwrap(), before);
        config.auth_mode = AuthMode::Subscription;
        for headers in [
            upstream_headers(&incoming, &config),
            count_headers(&incoming, &config),
        ] {
            let headers = headers.unwrap();
            assert_eq!(headers["authorization"], "synthetic-local-secret");
            assert!(!headers.contains_key("x-api-key"));
        }
        config.auth_mode = AuthMode::ApiKey;
        config.replace_credential(Credential::Anthropic, Some("synthetic-replacement".into()));
        assert_eq!(
            upstream_headers(&incoming, &config).unwrap()["x-api-key"],
            "synthetic-replacement"
        );
        config.replace_credential(Credential::Anthropic, None);
        assert_eq!(
            upstream_headers(&incoming, &config),
            Err(HttpError::InvalidRequest)
        );
        assert_eq!(
            count_headers(&incoming, &config).unwrap()["x-api-key"],
            "undefined"
        );
    }
}

// Join both directly-owned futures: timeout or failure cannot detach a peer task.
async fn credential_wire(client: &NativeHttpClient, headers: HeaderMap) -> Vec<Vec<u8>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut request = Request::builder()
        .uri(format!("http://{address}/v1/models"))
        .body(Full::new(Bytes::new()))
        .unwrap();
    *request.headers_mut() = headers;
    let peer = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            head.push(byte[0]);
            assert!(head.len() <= 8192);
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .await
            .unwrap();
        socket.shutdown().await.unwrap();
        head.split(|byte| *byte == b'\n')
            .filter_map(|line| {
                let line = line.strip_suffix(b"\r")?;
                let colon = line.iter().position(|byte| *byte == b':')?;
                if !line[..colon].eq_ignore_ascii_case(b"x-api-key") {
                    return None;
                }
                Some(
                    line[colon + 1..]
                        .strip_prefix(b" ")
                        .unwrap_or(&line[colon + 1..])
                        .to_vec(),
                )
            })
            .collect::<Vec<_>>()
    };
    let (response, fields) = tokio::join!(client.request_raw(request), peer);
    let response = response.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        http_body_util::Limited::new(response.into_body(), 16)
            .collect()
            .await
            .unwrap()
            .to_bytes(),
        "{}"
    );
    fields
}

#[tokio::test]
async fn observed_node_http_wire_fields() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let client = NativeHttpClient::new().unwrap();
        let incoming = credential_incoming();
        let mut counts = [0, 0];
        for (id, raw, http, count) in OBSERVED_CREDENTIAL_CASES {
            let config = credential_config(*raw);
            if let Some(expected) = http {
                let received =
                    credential_wire(&client, upstream_headers(&incoming, &config).unwrap()).await;
                assert_eq!(
                    received,
                    expected.iter().map(|v| v.to_vec()).collect::<Vec<_>>(),
                    "HTTP wire {id}"
                );
                counts[0] += 1;
            }
            if let Some(expected) = count {
                let received =
                    credential_wire(&client, count_headers(&incoming, &config).unwrap()).await;
                assert_eq!(
                    received,
                    vec![expected.to_vec()],
                    "Fetch-converted wire {id}"
                );
                counts[1] += 1;
            }
        }
        assert_eq!(counts, [25, 26]);
    })
    .await
    .unwrap();
}

// Actual corrected Source observations only; no production helper generated these expectations.
// (id, literal raw credential JSON/absence, NodeHTTP repeated values/error, actual Fetch wire value/unavailable).
// HTTP None=error; Some(&[])=omitted; Some(&[b" "])=a literal field value, including empty b"".
type ObservedCredentialCase = (
    &'static str,
    Option<&'static str>,
    Option<&'static [&'static [u8]]>,
    Option<&'static [u8]>,
);
const OBSERVED_CREDENTIAL_CASES: &[ObservedCredentialCase] = &[
    ("api-absent", None, None, Some(b"undefined")),
    ("api-null", Some(r#"null"#), Some(&[b"null"]), Some(b"null")),
    (
        "api-false",
        Some(r#"false"#),
        Some(&[b"false"]),
        Some(b"false"),
    ),
    ("api-true", Some(r#"true"#), Some(&[b"true"]), Some(b"true")),
    ("api-zero", Some(r#"0"#), Some(&[b"0"]), Some(b"0")),
    (
        "api-negative-zero",
        Some(r#"-0"#),
        Some(&[b"0"]),
        Some(b"0"),
    ),
    ("api-number", Some(r#"42"#), Some(&[b"42"]), Some(b"42")),
    (
        "api-tiny-number",
        Some(r#"1e-7"#),
        Some(&[b"1e-7"]),
        Some(b"1e-7"),
    ),
    (
        "api-large-number",
        Some(r#"1e21"#),
        Some(&[b"1e+21"]),
        Some(b"1e+21"),
    ),
    (
        "api-overflow",
        Some(r#"1e400"#),
        Some(&[b"Infinity"]),
        Some(b"Infinity"),
    ),
    (
        "api-negative-overflow",
        Some(r#"-1e400"#),
        Some(&[b"-Infinity"]),
        Some(b"-Infinity"),
    ),
    ("api-empty", Some(r#""""#), Some(&[b""]), Some(b"")),
    (
        "api-spaces",
        Some(r#"" synthetic ""#),
        Some(&[b" synthetic "]),
        Some(b"synthetic"),
    ),
    (
        "api-tab",
        Some(r#""\tsynthetic\t""#),
        Some(&[b"\tsynthetic\t"]),
        Some(b"synthetic"),
    ),
    (
        "api-ascii",
        Some(r#""synthetic-api-key""#),
        Some(&[b"synthetic-api-key"]),
        Some(b"synthetic-api-key"),
    ),
    (
        "api-latin-e9",
        Some(r#""synthetic-\u00e9""#),
        Some(&[b"synthetic-\xe9"]),
        Some(b"synthetic-\xe9"),
    ),
    (
        "api-latin-ff",
        Some(r#""synthetic-\u00ff""#),
        Some(&[b"synthetic-\xff"]),
        Some(b"synthetic-\xff"),
    ),
    (
        "api-c85",
        Some(r#""synthetic-\u0085""#),
        Some(&[b"synthetic-\x85"]),
        Some(b"synthetic-\x85"),
    ),
    (
        "api-c9f",
        Some(r#""synthetic-\u009f""#),
        Some(&[b"synthetic-\x9f"]),
        Some(b"synthetic-\x9f"),
    ),
    ("api-non-latin", Some(r#""synthetic-\u0100""#), None, None),
    ("api-lone-high", Some(r#""synthetic-\ud800""#), None, None),
    ("api-lone-low", Some(r#""synthetic-\udfff""#), None, None),
    ("api-nul", Some(r#""synthetic-\u0000""#), None, None),
    ("api-c01", Some(r#""synthetic-\u0001""#), None, None),
    ("api-c1f", Some(r#""synthetic-\u001f""#), None, None),
    ("api-del", Some(r#""synthetic-\u007f""#), None, None),
    ("api-crlf", Some(r#""synthetic-\r\n-injected""#), None, None),
    ("api-empty-array", Some(r#"[]"#), Some(&[]), Some(b"")),
    (
        "api-singleton-array",
        Some(r#"["synthetic-a"]"#),
        Some(&[b"synthetic-a"]),
        Some(b"synthetic-a"),
    ),
    (
        "api-two-array",
        Some(r#"["synthetic-a","synthetic-b"]"#),
        Some(&[b"synthetic-a", b"synthetic-b"]),
        Some(b"synthetic-a,synthetic-b"),
    ),
    (
        "api-nested-array",
        Some(r#"[["synthetic-a","synthetic-b"],["synthetic-c"]]"#),
        Some(&[b"synthetic-a,synthetic-b", b"synthetic-c"]),
        Some(b"synthetic-a,synthetic-b,synthetic-c"),
    ),
    (
        "api-null-array",
        Some(r#"["synthetic-a",null]"#),
        Some(&[b"synthetic-a", b"null"]),
        Some(b"synthetic-a,"),
    ),
    (
        "api-number-array",
        Some(r#"[0,1e400,false]"#),
        Some(&[b"0", b"Infinity", b"false"]),
        Some(b"0,Infinity,false"),
    ),
    (
        "api-object",
        Some(r#"{"synthetic":"object"}"#),
        Some(&[b"[object Object]"]),
        Some(b"[object Object]"),
    ),
    ("api-shadow-null", Some(r#"{"toString":null}"#), None, None),
    (
        "api-shadow-string",
        Some(r#"{"toString":"synthetic"}"#),
        None,
        None,
    ),
    (
        "api-shadow-both",
        Some(r#"{"toString":{},"valueOf":0}"#),
        None,
        None,
    ),
    (
        "api-array-shadow-first",
        Some(r#"[{"toString":null},"synthetic-\n"]"#),
        None,
        None,
    ),
    (
        "api-array-shadow-last",
        Some(r#"["synthetic-\n",{"toString":null}]"#),
        None,
        None,
    ),
];

// Gateway phases and actual product-router counting use owned synthetic services.
use autorouter_core::config::{Credential, Evaluator};
use std::sync::atomic::{AtomicUsize, Ordering};

const CREDENTIAL_BOUND: Duration = Duration::from_secs(5);
const CREDENTIAL_MODEL: &str = "claude-haiku-4-5-20251001";

#[derive(Default)]
struct CredentialWire {
    calls: Mutex<Vec<(&'static str, HeaderMap)>>,
    invalid: AtomicUsize,
}
impl HttpTransport for CredentialWire {
    type ResponseBody = Full<Bytes>;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        let lane = match parts.uri.path() {
            "/v1/systemone" => "jev",
            "/v1/messages/count_tokens" => "count",
            "/v1/messages" => "forward",
            _ => "unexpected",
        };
        self.calls.lock().unwrap().push((lane, parts.headers));
        if lane == "unexpected"
            || parts.method != Method::POST
            || bytes.len() > 65536
            || serde_json::from_slice::<Value>(&bytes).is_err()
        {
            self.invalid.fetch_add(1, Ordering::SeqCst);
            return Err(HttpError::InvalidRequest);
        }
        let response = match lane {
            "jev" => json!({"answers":{"tier":{"choice":"haiku","confidence":0.99}}}),
            "count" => json!({"input_tokens":123}),
            _ => json!({"type":"message","role":"assistant","model":CREDENTIAL_MODEL,
                "content":[{"type":"text","text":"Synthetic credential response"}],
                "stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}),
        }
        .to_string();
        Ok(Response::builder()
            .header("content-type", "application/json")
            .header("content-length", response.len())
            .body(Full::new(Bytes::from(response)))
            .unwrap())
    }
}
impl CredentialWire {
    fn lane_count(&self, lane: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(kind, _)| *kind == lane)
            .count()
    }
}
#[derive(Default)]
struct CredentialStub {
    routes: AtomicUsize,
    completed: Mutex<Vec<Value>>,
    closed: AtomicUsize,
}
impl GatewayRouter for CredentialStub {
    async fn route(
        &self,
        document: Arc<JsDocument>,
        _: RouteOptions,
        _: &HeaderMap,
        _: &CancellationToken,
        _: &str,
    ) -> Result<Value, EvaluationError> {
        self.routes.fetch_add(1, Ordering::SeqCst);
        let model = document
            .get(document.root(), "model")
            .and_then(|node| document.string(node))
            .unwrap()
            .to_scalar()
            .unwrap();
        Ok(json!({"model":model,"source":"test","reason":"fixed_requested_model"}))
    }
    fn complete(&self, _: &str, evidence: &Value) -> bool {
        self.completed.lock().unwrap().push(evidence.clone());
        !evidence.is_null()
    }
    fn shutdown(&self) {}
    async fn close(&self) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}

struct CredentialFixture {
    client: Option<NativeHttpClient>,
    handle: GatewayHandle,
    statuses: Arc<Mutex<Vec<Value>>>,
    token: String,
}
impl CredentialFixture {
    async fn new<R: GatewayRouter>(
        config: RouterConfig,
        router: Arc<R>,
        wire: Arc<CredentialWire>,
    ) -> Self {
        let statuses = Arc::new(Mutex::new(Vec::new()));
        let seen = statuses.clone();
        let token = config.local_token.clone().unwrap();
        // Construct the client before the owned listener so setup failure owns no listener.
        let client = NativeHttpClient::new().unwrap();
        let gateway = Gateway::with_router(
            config,
            wire.clone(),
            router.clone(),
            EventSinks {
                status: Some(Arc::new(move |event| {
                    seen.lock()
                        .unwrap()
                        .push(event.to_serde_observation_lossy())
                })),
                ..Default::default()
            },
        )
        .unwrap();
        let handle = gateway.listen(0).await.unwrap();
        Self {
            client: Some(client),
            handle,
            statuses,
            token,
        }
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        body: &str,
        authorized: bool,
        extra: Option<(&str, &str)>,
    ) -> Result<(u16, Value), &'static str> {
        let mut request = Request::builder()
            .method(method)
            .uri(format!("http://{}{path}", self.handle.address))
            .header("connection", "close")
            .header(
                "x-api-key",
                if authorized {
                    self.token.as_str()
                } else {
                    "synthetic-wrong-local-key"
                },
            )
            .header("content-length", body.len())
            .body(Full::new(Bytes::copy_from_slice(body.as_bytes())))
            .unwrap();
        if let Some((key, value)) = extra {
            request.headers_mut().insert(
                hyper::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        tokio::time::timeout(CREDENTIAL_BOUND, async {
            let response = self
                .client
                .as_ref()
                .unwrap()
                .request(request)
                .await
                .map_err(|_| "downstream request failed")?;
            let status = response.status().as_u16();
            let mut body = response.into_body();
            let mut bytes = Vec::new();
            while let Some(frame) = body.frame().await {
                if let Ok(data) = frame.map_err(|_| "downstream response failed")?.into_data() {
                    if data.len() > 8192usize.saturating_sub(bytes.len()) {
                        return Err("response bound exceeded");
                    }
                    bytes.extend_from_slice(&data);
                }
            }
            Ok((
                status,
                serde_json::from_slice(&bytes).map_err(|_| "response is not JSON")?,
            ))
        })
        .await
        .map_err(|_| "downstream request timeout")?
    }
    async fn close(mut self) -> Result<(), &'static str> {
        // Child module may take the real listener JoinHandle: timeout never detaches it.
        drop(self.client.take());
        self.handle.cancellation.cancel();
        let mut task = self.handle.task.take().unwrap();
        match tokio::time::timeout(CREDENTIAL_BOUND, &mut task).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err("listener owner failed"),
            Err(_) => {
                task.abort();
                let _ = task.await;
                Err("listener owner did not drain")
            }
        }
    }
}
fn credential_safe_error(response: &(u16, Value)) {
    assert_eq!(
        response,
        &(
            502,
            json!({"type":"error","error":{
        "type":"api_error","message":"Router could not complete the upstream request"}})
        )
    );
}
fn credential_message(attachment: bool) -> String {
    let content = if attachment {
        json!([{"type":"text","text":"Synthetic count availability control"},
            {"type":"image","source":{"type":"base64","media_type":"image/png","data":"AA=="}}])
    } else {
        json!("Synthetic inference phase control")
    };
    json!({"model":CREDENTIAL_MODEL,"messages":[{"role":"user","content":content}],"max_tokens":64})
        .to_string()
}

#[tokio::test]
async fn credential_errors_preserve_six_earlier_gateway_rejections() {
    for (id, method, path, body, authorized, extra, status) in [
        ("unauthorized", "GET", "/v1/models", "", false, None, 401),
        ("unsupported", "GET", "/unsupported", "", true, None, 404),
        (
            "encoding",
            "GET",
            "/v1/models",
            "",
            true,
            Some(("content-encoding", "gzip")),
            415,
        ),
        // Same source inequality, smaller valid-framed native body:2>1 instead of40MB>32MiB.
        ("length", "GET", "/v1/models", "{}", true, None, 413),
        (
            "malformed",
            "POST",
            "/v1/messages/count_tokens",
            "{",
            true,
            None,
            400,
        ),
        (
            "shape",
            "POST",
            "/v1/messages/count_tokens",
            "{}",
            true,
            None,
            400,
        ),
    ] {
        // A typed valid scalar beyond Latin1 makes the old eager phase fail502.
        let mut config = forwarding_tests::credential_config(Some(r#""synthetic-\u0100""#));
        if id == "length" {
            config.max_body_bytes = 1;
        }
        let router = Arc::new(CredentialStub::default());
        let wire = Arc::new(CredentialWire::default());
        let fixture = CredentialFixture::new(config, router.clone(), wire.clone()).await;
        let result = fixture.request(method, path, body, authorized, extra).await;
        let observed = (
            router.routes.load(Ordering::SeqCst),
            wire.calls.lock().unwrap().len(),
        );
        let closed = fixture.close().await;
        assert_eq!(closed, Ok(()), "{id}");
        assert_eq!(result.unwrap().0, status, "{id}");
        assert_eq!(observed, (0, 0), "{id}");
        assert_eq!(wire.invalid.load(Ordering::SeqCst), 0);
        assert!(router.completed.lock().unwrap().is_empty());
        assert_eq!(router.closed.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn valid_inference_routes_before_rejecting_forwarding_credentials() {
    for (id, raw, status) in [
        ("ascii", r#""synthetic-inference-key""#, 200),
        ("invalid-latin1", r#""synthetic-\u0100""#, 502),
        ("noncoercible", r#"{"toString":null}"#, 502),
    ] {
        let config = forwarding_tests::credential_config(Some(raw));
        let router = Arc::new(CredentialStub::default());
        let wire = Arc::new(CredentialWire::default());
        let fixture = CredentialFixture::new(config, router.clone(), wire.clone()).await;
        let result = fixture
            .request(
                "POST",
                "/v1/messages",
                &credential_message(false),
                true,
                None,
            )
            .await;
        let routes = router.routes.load(Ordering::SeqCst);
        let statuses = fixture.statuses.lock().unwrap().clone();
        let closed = fixture.close().await;
        assert_eq!(closed, Ok(()), "{id}");
        let response = result.unwrap();
        assert_eq!(response.0, status, "{id}");
        assert_eq!(
            routes, 1,
            "{id}: inference must route before a forwarding error"
        );
        assert_eq!(router.closed.load(Ordering::SeqCst), 1);
        assert_eq!(
            wire.lane_count("forward"),
            usize::from(status == 200),
            "{id}"
        );
        assert_eq!(wire.lane_count("count") + wire.lane_count("jev"), 0);
        assert_eq!(wire.invalid.load(Ordering::SeqCst), 0);
        if status == 502 {
            credential_safe_error(&response);
            assert_eq!(*router.completed.lock().unwrap(), vec![Value::Null]);
            assert!(
                !statuses
                    .iter()
                    .any(|event| event["event"] == "request_complete")
            );
        } else {
            assert_eq!(response.1["type"], "message");
            assert_eq!(response.1["model"], CREDENTIAL_MODEL);
        }
    }
}

#[tokio::test]
async fn production_router_keeps_unavailable_count_callback_and_missing_key_fetch_conversion() {
    for (id, raw, counts, context) in [
        (
            "invalid-byte",
            Some(r#""synthetic-\u0100""#),
            0,
            "count_unavailable",
        ),
        (
            "noncoercible",
            Some(r#"{"toString":null}"#),
            0,
            "count_unavailable",
        ),
        ("missing", None, 1, "within_budget"),
    ] {
        let mut config = forwarding_tests::credential_config(raw);
        config.evaluator = Evaluator::Jev;
        config.replace_credential(Credential::Jev, Some("synthetic-jev-key".into()));
        let wire = Arc::new(CredentialWire::default());
        let router = Arc::new(Router::new(wire.clone(), config.clone()));
        let fixture = CredentialFixture::new(config, router.clone(), wire.clone()).await;
        let result = fixture
            .request(
                "POST",
                "/v1/messages",
                &credential_message(true),
                true,
                None,
            )
            .await;
        // Capture the real product router before owner close; shutdown must not manufacture success.
        let before_close = router.continuity_snapshot();
        let lanes = (
            wire.lane_count("jev"),
            wire.lane_count("count"),
            wire.lane_count("forward"),
        );
        let statuses = fixture.statuses.lock().unwrap().clone();
        let count_headers = wire
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(lane, _)| *lane == "count")
            .map(|(_, headers)| headers.clone())
            .collect::<Vec<_>>();
        let closed = fixture.close().await;
        assert_eq!(closed, Ok(()), "{id}");
        credential_safe_error(&result.unwrap());
        assert_eq!(lanes, (1, counts, 0), "{id}");
        assert_eq!(wire.invalid.load(Ordering::SeqCst), 0);
        assert_eq!(
            (before_close.records, before_close.attempts),
            (0, 0),
            "{id}"
        );
        let route = statuses
            .iter()
            .find(|event| event["event"] == "route")
            .unwrap();
        assert_eq!(
            route["source"], "jev",
            "{id}: classifier must still execute"
        );
        assert_eq!(
            route["context_check"], context,
            "{id}: present count callback is not disabled"
        );
        assert!(
            !statuses
                .iter()
                .any(|event| event["event"] == "request_complete")
        );
        if counts == 1 {
            assert_eq!(count_headers[0]["x-api-key"].as_bytes(), b"undefined");
            assert_eq!(route["counted_input_tokens"].as_f64(), Some(123.0));
        }
        let after_close = router.continuity_snapshot();
        assert_eq!((after_close.records, after_close.attempts), (0, 0));
    }
}

// A failed new task cannot replace a different confirmed tool owner.
#[tokio::test]
async fn credential_forwarding_failure_retains_previously_confirmed_opus_tool_owner() {
    let mut config = credential_config(Some(r#""synthetic-\u0100""#));
    config.evaluator = Evaluator::Jev;
    config.replace_credential(Credential::Jev, Some("synthetic-jev-key".into()));
    let opus = config.models.opus.clone();
    let sonnet = config.models.sonnet.clone();
    let wire = Arc::new(CredentialWire::default());
    let router = Arc::new(Router::new(wire.clone(), config.clone()));
    let fixture = CredentialFixture::new(config, router.clone(), wire.clone()).await;
    // The gateway has no session/agent headers, so this is its exact explicit scope.
    let scope = json!([Value::Null, Value::Null]).to_string();
    let headers = HeaderMap::new();
    let cancellation = CancellationToken::new();
    let options = |request: &str, prompt: &str| RouteOptions {
        scope: scope.clone(),
        prompt_id: prompt.into(),
        request_id: Some(request.into()),
        ..RouteOptions::default()
    };
    let seed = json!({"model":CREDENTIAL_MODEL,"messages":[{
        "role":"user","content":"Seed the original confirmed tool owner"}],"max_tokens":64});
    let mut continuation = seed.clone();
    let messages = continuation["messages"].as_array_mut().unwrap();
    messages.push(json!({"role":"assistant","content":[{
        "type":"tool_use","id":"original-tool","name":"Read","input":{}}]}));
    messages.push(json!({"role":"user","content":[{
        "type":"tool_result","tool_use_id":"original-tool","content":"Synthetic original result"}]}));
    let run = tokio::time::timeout(CREDENTIAL_BOUND, async {
        let selected = router
            .route(
                Arc::new(JsDocument::parse(seed.to_string().as_bytes()).unwrap()),
                options("owner-seed", "original-owner"),
                &headers,
                &cancellation,
                "",
            )
            .await?;
        // Public complete accepts actual provider fallback as the confirmed owner.
        let accepted = router.complete(
            "owner-seed",
            &json!({
            "continuation_model":opus,"tool_uses":[{"id":"original-tool","model":opus}]}),
        );
        let seeded = router.continuity_snapshot();
        let seed_calls = wire.lane_count("jev");
        let failed = fixture
            .request(
                "POST",
                "/v1/messages",
                &credential_message(true),
                true,
                Some(("x-claude-code-prompt-id", "failed-selection")),
            )
            .await;
        let after_failure = router.continuity_snapshot();
        let failure_calls = wire.lane_count("jev");
        let probe = router
            .route(
                Arc::new(JsDocument::parse(continuation.to_string().as_bytes()).unwrap()),
                options("owner-probe", ""),
                &headers,
                &cancellation,
                "",
            )
            .await?;
        let probing = router.continuity_snapshot();
        let probe_calls = wire.lane_count("jev");
        router.complete("owner-probe", &Value::Null);
        let released = router.continuity_snapshot();
        Ok::<_, EvaluationError>((
            selected,
            accepted,
            seeded,
            seed_calls,
            failed,
            after_failure,
            failure_calls,
            probe,
            probing,
            probe_calls,
            released,
        ))
    })
    .await;
    let statuses = fixture.statuses.lock().unwrap().clone();
    // Close before assertions, including the original timeout/error result.
    let closed = fixture.close().await;
    assert_eq!(closed, Ok(()));
    let (
        selected,
        accepted,
        seeded,
        seed_calls,
        failed,
        after_failure,
        failure_calls,
        probe,
        probing,
        probe_calls,
        released,
    ) = run.unwrap().unwrap();
    assert_eq!(selected["model"], CREDENTIAL_MODEL);
    assert!(accepted, "seed must establish a real confirmed owner");
    assert_eq!((seeded.records, seeded.attempts), (1, 0));
    credential_safe_error(&failed.unwrap());
    assert_eq!((after_failure.records, after_failure.attempts), (1, 0));
    let attempted = statuses
        .iter()
        .find(|event| event["event"] == "route")
        .unwrap();
    assert_eq!(attempted["model"], sonnet);
    assert_ne!(attempted["model"], opus);
    assert_eq!(attempted["context_check"], "count_unavailable");
    assert!(
        !statuses
            .iter()
            .any(|event| event["event"] == "request_complete")
    );
    assert_eq!(probe["model"], opus);
    assert_eq!(probe["continuity_state"], "confirmed");
    assert_eq!(probe["reason"], "tool_turn_pinned");
    // Ordinary continuations still classify; this fresh full-body key is not cached.
    assert_eq!((seed_calls, failure_calls, probe_calls), (1, 2, 3));
    assert_eq!((probing.records, probing.attempts), (1, 1));
    assert_eq!((released.records, released.attempts), (1, 0));
    assert_eq!(
        (wire.lane_count("count"), wire.lane_count("forward")),
        (0, 0)
    );
    assert_eq!(wire.invalid.load(Ordering::SeqCst), 0);
    let final_state = router.continuity_snapshot();
    assert_eq!((final_state.records, final_state.attempts), (1, 0));
}

#[test]
fn local_gateway_token_replacement_clears_captured_source_before_admission() {
    use autorouter_core::config::read_config_document;
    const MINIMUM: &str = "abcdefghijklmnop";
    const REJECT: &str = "AUTOROUTER_TOKEN must contain at least 16 characters";
    assert_eq!(MINIMUM.encode_utf16().count(), 16);
    for mode in ["api-key", "subscription"] {
        let source = JsDocument::parse(format!(
            r#"{{"AUTOROUTER_AUTH_MODE":"{mode}","AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_TOKEN":true}}"#
        ).as_bytes()).unwrap();
        let original_source = source.stringify();
        let mut config =
            read_config_document(&source, false, std::path::Path::new("/synthetic")).unwrap();
        let never_wire = Arc::new(CredentialWire::default());
        let never_router = Arc::new(CredentialStub::default());
        let construct = |config: &RouterConfig| {
            let before = serde_json::to_string(config).unwrap();
            let result = Gateway::with_router(
                config.clone(),
                never_wire.clone(),
                never_router.clone(),
                EventSinks::default(),
            );
            assert_eq!(serde_json::to_string(config).unwrap(), before);
            assert_eq!(source.stringify(), original_source);
            assert!(never_wire.calls.lock().unwrap().is_empty());
            assert_eq!(never_wire.invalid.load(Ordering::SeqCst), 0);
            assert_eq!(never_router.routes.load(Ordering::SeqCst), 0);
            assert!(never_router.completed.lock().unwrap().is_empty());
            assert_eq!(never_router.closed.load(Ordering::SeqCst), 0);
            result
        };
        assert_eq!(
            config.credential_json(Credential::LocalToken).as_deref(),
            Some("true")
        );
        assert!(config.local_token.is_none());
        assert_eq!(construct(&config).err().as_deref(), Some(REJECT));

        config.replace_credential(Credential::LocalToken, Some(MINIMUM.into()));
        assert_eq!(config.local_token.as_deref(), Some(MINIMUM));
        assert_eq!(
            config.credential_json(Credential::LocalToken).as_deref(),
            Some(r#""abcdefghijklmnop""#)
        );
        // No runtime, NativeHttpClient, listener, request or TLS context exists.
        // Drop the accepted owned Gateway immediately without ever listening.
        assert!(construct(&config).is_ok());

        config.replace_credential(Credential::LocalToken, None);
        assert!(config.local_token.is_none());
        assert!(config.credential_json(Credential::LocalToken).is_none());
        let cleared =
            JsDocument::parse(serde_json::to_string(&config).unwrap().as_bytes()).unwrap();
        assert!(cleared.get(cleared.root(), "localToken").is_none());
        assert_eq!(construct(&config).err().as_deref(), Some(REJECT));
        assert_eq!(source.stringify(), original_source);
    }
}
