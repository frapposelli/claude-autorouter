//! Optional exact-token estimates; every failure leaves routing fallback intact.
//! The cache contains only keyed digests, counts and expirations, never prompts
//! or credentials. Count requests share inference's compatibility/adaptation.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use autorouter_core::auto_routing::target_compatibility_document;
use autorouter_core::config::{ClientProfile, RouterConfig};
use autorouter_core::js_json::{JsDocument, JsNode, NodeId};
use autorouter_core::model_request::thinking_adaptation_document;
use bytes::Bytes;
use http_body_util::Full;
use hyper::{HeaderMap, Request};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::bounded_json::DecodedResponseStream;
use crate::http_client::HttpTransport;

const COUNT_FIELDS: &[&str] = &[
    "model",
    "messages",
    "system",
    "tools",
    "tool_choice",
    "thinking",
    "cache_control",
    "context_management",
    "compaction",
    "output_config",
    "output_format",
    "speed",
];
const GENERATION_FIELDS: &[&str] = &[
    "max_tokens",
    "stream",
    "temperature",
    "top_p",
    "top_k",
    "stop_sequences",
    "metadata",
    "service_tier",
    "inference_geo",
];
const CACHE_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "anthropic-beta",
    "anthropic-version",
    "anthropic-workspace-id",
    "anthropic-user-profile-id",
];
const RESPONSE_LIMIT: usize = 65_536;

fn field(document: &JsDocument, node: NodeId, key: &str) -> Option<NodeId> {
    document.get(node, key)
}
fn text(document: &JsDocument, node: Option<NodeId>) -> Option<String> {
    document.string(node?).and_then(|value| value.to_scalar())
}
fn truthy(document: &JsDocument, node: Option<NodeId>) -> bool {
    match node.and_then(|node| document.node(node)) {
        None | Some(JsNode::Null) => false,
        Some(JsNode::Bool(value)) => *value,
        Some(JsNode::Number(value)) => *value != 0.0 && !value.is_nan(),
        Some(JsNode::String(value)) => !value.units().is_empty(),
        Some(JsNode::Array(_) | JsNode::Object(_)) => true,
    }
}

/// Check count-endpoint support without descending opaque tool-input objects or
/// forgetting remote attachments nested inside content/tool-result wrappers.
pub fn can_count(document: &JsDocument) -> bool {
    let root = document.root();
    let Some(JsNode::Object(object)) = document.node(root) else {
        return false;
    };
    let Some(JsNode::Array(messages)) =
        field(document, root, "messages").and_then(|node| document.node(node))
    else {
        return false;
    };
    if object.entries().iter().any(|(key, _)| {
        key.to_scalar().is_none_or(|key| {
            !COUNT_FIELDS.contains(&key.as_str()) && !GENERATION_FIELDS.contains(&key.as_str())
        })
    }) {
        return false;
    }
    if let Some(tools) = field(document, root, "tools") {
        let Some(JsNode::Array(tools)) = document.node(tools) else {
            return false;
        };
        for &tool in tools {
            if !truthy(document, Some(tool)) {
                return false;
            }
            let tool_type = field(document, tool, "type");
            if truthy(document, tool_type) {
                let Some(kind) = text(document, tool_type) else {
                    return false;
                };
                let dated = ["bash_", "text_editor_", "computer_", "memory_"]
                    .into_iter()
                    .any(|prefix| {
                        kind.strip_prefix(prefix).is_some_and(|date| {
                            date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit())
                        })
                    });
                if kind != "custom" && kind != "advisor_20260301" && !dated {
                    return false;
                }
            }
        }
    }
    let mut pending: Vec<_> = field(document, root, "system")
        .into_iter()
        .chain(
            messages
                .iter()
                .filter_map(|&message| field(document, message, "content")),
        )
        .collect();
    while let Some(content) = pending.pop() {
        let Some(JsNode::Array(blocks)) = document.node(content) else {
            continue;
        };
        for &block in blocks {
            if matches!(
                text(document, field(document, block, "type")).as_deref(),
                Some("image" | "document")
            ) && let Some(source) = field(document, block, "source")
            {
                match text(document, field(document, source, "type")).as_deref() {
                    Some("url" | "file") => return false,
                    Some("content") => pending.extend(field(document, source, "content")),
                    _ => {}
                }
            }
            pending.extend(field(document, block, "content"));
        }
    }
    true
}

/// Serialize from the authoritative document. Observation projections must
/// never replace lone surrogates, deep schemas or signed history on the wire.
pub fn count_payload(document: &JsDocument, model: &str, auto_mode: bool) -> Option<String> {
    if model.is_empty() || !can_count(document) {
        return None;
    }
    let source_model =
        field(document, document.root(), "model").and_then(|node| document.string(node));
    if source_model.is_some_and(|model| model.to_scalar().is_none()) {
        return None;
    }
    if target_compatibility_document(document, model, auto_mode)["compatible"] != true {
        return None;
    }
    let adaptation = thinking_adaptation_document(document, model)
        .and_then(|(kind, _)| serde_json::to_string(&json!({"type":kind})).ok());
    let Some(JsNode::Object(object)) = document.node(document.root()) else {
        return None;
    };
    let model_json = serde_json::to_string(model).ok()?;
    let mut output = String::from("{");
    let mut first = true;
    let mut has_model = false;
    let mut has_thinking = false;
    for (key, node) in object.entries() {
        let name = key.to_scalar()?;
        if !COUNT_FIELDS.contains(&name.as_str()) {
            continue;
        }
        if !first {
            output.push(',');
        }
        first = false;
        output.push_str(&key.stringify());
        output.push(':');
        match name.as_str() {
            "model" => {
                has_model = true;
                output.push_str(&model_json);
            }
            "thinking" => {
                has_thinking = true;
                output.push_str(
                    &adaptation
                        .clone()
                        .unwrap_or_else(|| document.stringify_node(*node)),
                );
            }
            _ => output.push_str(&document.stringify_node(*node)),
        }
    }
    if !has_model {
        if !first {
            output.push(',');
        }
        first = false;
        output.push_str("\"model\":");
        output.push_str(&model_json);
    }
    if !has_thinking && let Some(thinking) = adaptation {
        if !first {
            output.push(',');
        }
        output.push_str("\"thinking\":");
        output.push_str(&thinking);
    }
    output.push('}');
    Some(output)
}

#[derive(Clone, Copy)]
pub struct CacheOptions {
    pub entries: usize,
    pub ttl: Duration,
}
impl Default for CacheOptions {
    fn default() -> Self {
        Self {
            entries: 100,
            ttl: Duration::from_secs(300),
        }
    }
}
struct Entry {
    count: u64,
    expires: Instant,
}
#[derive(Default)]
struct Cache {
    values: HashMap<[u8; 32], Entry>,
    order: VecDeque<[u8; 32]>,
}
impl Cache {
    fn get(&mut self, key: &[u8; 32]) -> Option<u64> {
        let entry = self.values.remove(key)?;
        self.order.retain(|candidate| candidate != key);
        if entry.expires <= Instant::now() {
            return None;
        }
        let count = entry.count;
        self.values.insert(*key, entry);
        self.order.push_back(*key);
        Some(count)
    }
    fn insert(&mut self, key: [u8; 32], count: u64, options: CacheOptions) {
        self.order.retain(|candidate| *candidate != key);
        self.values.insert(
            key,
            Entry {
                count,
                expires: Instant::now() + options.ttl,
            },
        );
        self.order.push_back(key);
        while self.values.len() > options.entries {
            if let Some(key) = self.order.pop_front() {
                self.values.remove(&key);
            }
        }
    }
}

pub struct TokenCounter<T> {
    transport: Arc<T>,
    upstream: String,
    auto_mode: bool,
    timeout: Duration,
    options: CacheOptions,
    cache: Mutex<Cache>,
}
impl<T: HttpTransport> TokenCounter<T> {
    pub fn new(transport: Arc<T>, config: &RouterConfig) -> Self {
        Self::with_cache(transport, config, CacheOptions::default())
    }
    pub fn with_cache(transport: Arc<T>, config: &RouterConfig, options: CacheOptions) -> Self {
        Self {
            transport,
            upstream: config.upstream.clone(),
            auto_mode: config.client_profile == ClientProfile::Auto,
            timeout: Duration::from_millis(config.token_count_timeout_ms),
            options,
            cache: Mutex::new(Cache::default()),
        }
    }
    pub async fn count(
        &self,
        document: &JsDocument,
        model: &str,
        headers: &HeaderMap,
        cancellation: &CancellationToken,
        search: &str,
    ) -> Option<u64> {
        if cancellation.is_cancelled() {
            return None;
        }
        let serialized = count_payload(document, model, self.auto_mode)?;
        let mut headers = headers.clone();
        headers.remove("content-length");
        headers.insert("content-type", "application/json".parse().ok()?);
        headers.insert("accept", "application/json".parse().ok()?);
        let upstream = self.upstream.strip_suffix('/').unwrap_or(&self.upstream);
        let mut url = url::Url::parse(&format!("{upstream}/v1/messages/count_tokens")).ok()?;
        url.set_query((!search.is_empty()).then(|| search.strip_prefix('?').unwrap_or(search)));
        let url = url.to_string();
        let scope: Vec<Option<String>> = CACHE_HEADERS
            .iter()
            .map(|name| {
                let values: Vec<_> = headers
                    .get_all(*name)
                    .iter()
                    .map(|value| {
                        value
                            .as_bytes()
                            .iter()
                            .map(|&byte| char::from(byte))
                            .collect::<String>()
                    })
                    .collect();
                (!values.is_empty()).then(|| values.join(", "))
            })
            .collect();
        let key: [u8; 32] =
            Sha256::digest(serde_json::to_vec(&json!([url, scope, serialized])).ok()?).into();
        if let Some(count) = self.cache.lock().unwrap().get(&key) {
            return Some(count);
        }
        let work = async {
            let mut request = Request::post(&url)
                .body(Full::new(Bytes::from(serialized)))
                .ok()?;
            *request.headers_mut() = headers;
            let response = self.transport.request(request).await.ok()?;
            if !response.status().is_success() {
                return None;
            }
            let (parts, body) = response.into_parts();
            // Unlike evaluator readBoundedJson, source readCount does not use
            // Content-Length as an early rejection. Only decoded bytes count.
            let mut stream = DecodedResponseStream::new(body, &parts.headers, cancellation)
                .await
                .ok()?;
            let mut buffer = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                let count = stream.read(&mut chunk).await.ok()?;
                if count == 0 {
                    break;
                }
                if count > RESPONSE_LIMIT - buffer.len() {
                    return None;
                }
                buffer.extend_from_slice(&chunk[..count]);
            }
            let response = JsDocument::parse(&buffer).ok()?;
            let node = response.get(response.root(), "input_tokens")?;
            let JsNode::Number(value) = response.node(node)? else {
                return None;
            };
            if !(0.0..=9_007_199_254_740_991.0).contains(value) || value.fract() != 0.0 {
                return None;
            }
            Some(*value as u64)
        };
        let count = tokio::select! {
            biased;
            _=cancellation.cancelled()=>None,
            _=tokio::time::sleep(self.timeout)=>None,
            count=work=>count,
        }?;
        if cancellation.is_cancelled() {
            return None;
        }
        self.cache.lock().unwrap().insert(key, count, self.options);
        Some(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_client::HttpError;
    use autorouter_core::config::read_config;
    use http_body_util::BodyExt;
    use hyper::Response;
    use hyper::body::{Body, Frame};
    use serde_json::Value;
    use std::{
        io,
        path::Path,
        pin::Pin,
        sync::atomic::{AtomicUsize, Ordering},
        task::{Context, Poll},
    };

    const HAIKU: &str = "claude-haiku-4-5-20251001";
    const SONNET: &str = "claude-sonnet-5";
    struct TestBody {
        chunks: VecDeque<Bytes>,
        stall: bool,
        dropped: Arc<AtomicUsize>,
    }
    impl Body for TestBody {
        type Data = Bytes;
        type Error = io::Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
            if let Some(chunk) = self.chunks.pop_front() {
                Poll::Ready(Some(Ok(Frame::data(chunk))))
            } else if self.stall {
                Poll::Pending
            } else {
                Poll::Ready(None)
            }
        }
    }
    impl Drop for TestBody {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
    struct Step {
        bytes: Vec<u8>,
        status: u16,
        stall: bool,
        wait: bool,
        headers: HeaderMap,
        network_error: bool,
    }
    impl Step {
        fn json(value: Value) -> Self {
            Self {
                bytes: serde_json::to_vec(&value).unwrap(),
                status: 200,
                stall: false,
                wait: false,
                headers: HeaderMap::new(),
                network_error: false,
            }
        }
    }
    struct Mock {
        steps: Mutex<VecDeque<Step>>,
        requests: Mutex<Vec<(String, HeaderMap, Bytes)>>,
        methods: Mutex<Vec<hyper::Method>>,
        dropped: Arc<AtomicUsize>,
    }
    impl Mock {
        fn new(steps: impl IntoIterator<Item = Step>) -> Self {
            Self {
                steps: Mutex::new(steps.into_iter().collect()),
                requests: Mutex::new(Vec::new()),
                methods: Mutex::new(Vec::new()),
                dropped: Arc::new(AtomicUsize::new(0)),
            }
        }
        fn calls(&self) -> usize {
            self.requests.lock().unwrap().len()
        }
    }
    impl HttpTransport for Mock {
        type ResponseBody = TestBody;
        async fn request(
            &self,
            request: Request<Full<Bytes>>,
        ) -> Result<Response<TestBody>, HttpError> {
            let (parts, body) = request.into_parts();
            let bytes = body.collect().await.unwrap().to_bytes();
            self.methods.lock().unwrap().push(parts.method);
            self.requests
                .lock()
                .unwrap()
                .push((parts.uri.to_string(), parts.headers, bytes));
            let step = self
                .steps
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Step::json(json!({"input_tokens":self.calls()})));
            if step.network_error {
                return Err(HttpError::Network);
            }
            let body = TestBody {
                chunks: step.bytes.chunks(31).map(Bytes::copy_from_slice).collect(),
                stall: step.stall,
                dropped: self.dropped.clone(),
            };
            if step.wait {
                std::future::pending::<()>().await;
            }
            let mut response = Response::builder().status(step.status).body(body).unwrap();
            *response.headers_mut() = step.headers;
            Ok(response)
        }
    }
    fn config() -> RouterConfig {
        read_config(
            &json!({"ANTHROPIC_API_KEY":"synthetic"}),
            false,
            Path::new("/synthetic"),
        )
        .unwrap()
    }
    fn document(value: Value) -> JsDocument {
        JsDocument::parse(&serde_json::to_vec(&value).unwrap()).unwrap()
    }
    fn request() -> Value {
        json!({"model":SONNET,"max_tokens":4096,"stream":true,"system":[{"type":"text","text":"Synthetic context","cache_control":{"type":"ephemeral"}}],"messages":[{"role":"user","content":"Synthetic task"}],"tools":[{"name":"Read","description":"complete schema","input_schema":{"type":"object"}}],"thinking":{"type":"disabled"},"metadata":{"user_id":"synthetic"}})
    }
    fn headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            "Bearer synthetic-request-key".parse().unwrap(),
        );
        headers.insert("anthropic-beta", "oauth-2025-04-20".parse().unwrap());
        headers
    }

    #[tokio::test]
    async fn full_context_projection_shares_adaptation_and_keeps_request_credentials() {
        let mock = Arc::new(Mock::new([Step::json(json!({"input_tokens":87654}))]));
        let counter = TokenCounter::new(mock.clone(), &config());
        let mut body = request();
        body["output_config"] = json!({"format":{"type":"json_schema","schema":{"type":"object"}}});
        body["tool_choice"] = json!({"type":"auto"});
        body["context_management"] = json!({"edits":[]});
        body["cache_control"] = json!({"type":"ephemeral","ttl":"1h"});
        let original = document(body.clone());
        let before = original.stringify();
        let mut headers = headers();
        headers.insert("content-length", "9999".parse().unwrap());
        assert_eq!(
            counter
                .count(
                    &original,
                    "claude-opus-5",
                    &headers,
                    &CancellationToken::new(),
                    "?beta=true"
                )
                .await,
            Some(87654)
        );
        assert_eq!(original.stringify(), before);
        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(*mock.methods.lock().unwrap(), vec![hyper::Method::POST]);
        let (url, headers, bytes) = &requests[0];
        assert_eq!(
            url,
            "https://api.anthropic.com/v1/messages/count_tokens?beta=true"
        );
        assert!(!headers.contains_key("content-length"));
        assert_eq!(headers["content-type"], "application/json");
        assert_eq!(headers["accept"], "application/json");
        assert_eq!(headers["authorization"], "Bearer synthetic-request-key");
        assert_eq!(headers["anthropic-beta"], "oauth-2025-04-20");
        let mut expected = body;
        for key in ["max_tokens", "stream", "metadata"] {
            expected.as_object_mut().unwrap().remove(key);
        }
        expected["model"] = json!("claude-opus-5");
        expected["thinking"] = json!({"type":"adaptive"});
        assert_eq!(serde_json::from_slice::<Value>(bytes).unwrap(), expected);
    }

    #[tokio::test]
    async fn cache_separates_auth_features_query_and_adaptations_not_generation_settings() {
        let mock = Arc::new(Mock::new([]));
        let counter = TokenCounter::new(mock.clone(), &config());
        let body = request();
        let headers = headers();
        let cancel = CancellationToken::new();
        assert_eq!(
            counter
                .count(&document(body.clone()), HAIKU, &headers, &cancel, "")
                .await,
            Some(1)
        );
        let mut generation = body.clone();
        generation["max_tokens"] = json!(128);
        generation["stream"] = json!(false);
        assert_eq!(
            counter
                .count(&document(generation), HAIKU, &headers, &cancel, "")
                .await,
            Some(1)
        );
        assert_eq!(
            counter
                .count(&document(body.clone()), SONNET, &headers, &cancel, "")
                .await,
            Some(2)
        );
        for (index, name) in CACHE_HEADERS.iter().enumerate() {
            let mut scoped = headers.clone();
            scoped.insert(
                hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                "synthetic-different".parse().unwrap(),
            );
            assert_eq!(
                counter
                    .count(&document(body.clone()), HAIKU, &scoped, &cancel, "")
                    .await,
                Some(3 + index as u64)
            );
        }
        assert_eq!(
            counter
                .count(
                    &document(body.clone()),
                    HAIKU,
                    &headers,
                    &cancel,
                    "?beta=true"
                )
                .await,
            Some(9)
        );
        assert_eq!(
            counter
                .count(&document(body), HAIKU, &headers, &cancel, "")
                .await,
            Some(1)
        );
        assert_eq!(mock.calls(), 9);
    }

    #[tokio::test(start_paused = true)]
    async fn bounded_lru_refreshes_hits_and_expires_successful_counts() {
        let mock = Arc::new(Mock::new([]));
        let counter = TokenCounter::with_cache(
            mock.clone(),
            &config(),
            CacheOptions {
                entries: 2,
                ttl: Duration::from_millis(10),
            },
        );
        let body = document(request());
        let h = headers();
        let cancel = CancellationToken::new();
        for (model, expected) in [
            (HAIKU, 1),
            (SONNET, 2),
            (HAIKU, 1),
            ("claude-opus-5", 3),
            (SONNET, 4),
        ] {
            assert_eq!(
                counter.count(&body, model, &h, &cancel, "").await,
                Some(expected)
            );
        }
        tokio::time::advance(Duration::from_millis(11)).await;
        assert_eq!(counter.count(&body, SONNET, &h, &cancel, "").await, Some(5));
        assert_eq!(counter.cache.lock().unwrap().values.len(), 2);
    }

    #[tokio::test]
    async fn unsupported_fields_tools_and_deep_remote_attachments_make_no_request() {
        let mock = Arc::new(Mock::new([]));
        let counter = TokenCounter::new(mock.clone(), &config());
        for extra in [
            json!({"tools":[{"type":"web_search_20250305"}]}),
            json!({"tools":[{"type":"tool_search_tool_regex_20251119"}]}),
            json!({"tools":[{"type":"code_execution_20260120","name":"code_execution"}]}),
            json!({"messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"read","content":[{"type":"image","source":{"type":"url","url":"https://example.test/file","file_id":"file_test"}}]}]}]}),
            json!({"messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"read","content":[{"type":"document","source":{"type":"file","url":"https://example.test/file","file_id":"file_test"}}]}]}]}),
            json!({"mcp_servers":[]}),
            json!({"future_input_context":"opaque"}),
            json!({"container":"id"}),
        ] {
            let mut body = request();
            body.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            assert_eq!(
                counter
                    .count(
                        &document(body),
                        HAIKU,
                        &headers(),
                        &CancellationToken::new(),
                        ""
                    )
                    .await,
                None
            );
        }
        let nested = "[{\"type\":\"tool_result\",\"content\":".repeat(150)
            + "[{\"type\":\"image\",\"source\":{\"type\":\"url\"}}]"
            + &"}]".repeat(150);
        let bytes = format!(
            "{{\"model\":\"{SONNET}\",\"messages\":[{{\"role\":\"user\",\"content\":{nested}}}]}}"
        );
        let body = JsDocument::parse(bytes.as_bytes()).unwrap();
        assert!(!can_count(&body));
        assert_eq!(
            counter
                .count(&body, SONNET, &headers(), &CancellationToken::new(), "")
                .await,
            None
        );
        assert_eq!(mock.calls(), 0);
    }

    #[test]
    fn wire_projection_preserves_opaque_surrogates_deep_input_and_signed_history() {
        let bytes = format!(
            r#"{{"model":"{SONNET}","messages":[{{"role":"assistant","content":[{{"type":"thinking","signature":"opaque+/=","thinking":"\ud800"}}]}}],"tools":[{{"name":"Read","input_schema":{{"type":"object","opaque":{}}}}}],"max_tokens":4096}}"#,
            "[".repeat(150) + "1e999" + &"]".repeat(150)
        );
        let body = JsDocument::parse(bytes.as_bytes()).unwrap();
        let output = count_payload(&body, SONNET, false).unwrap();
        assert!(output.contains("\\ud800"));
        assert!(output.contains("opaque+/="));
        assert!(output.contains(&("[".repeat(150) + "null" + &"]".repeat(150))));
        assert!(!output.contains("max_tokens"));
        assert!(output.contains("input_schema"));
    }

    #[tokio::test]
    async fn failures_invalid_counts_and_oversized_responses_never_cache() {
        for first in [
            Step {
                network_error: true,
                ..Step::json(json!({}))
            },
            Step {
                status: 429,
                ..Step::json(json!({"input_tokens":1}))
            },
            Step {
                status: 307,
                ..Step::json(json!({"input_tokens":1}))
            },
            Step::json(json!({"input_tokens":"100"})),
            Step::json(json!({"input_tokens":-1})),
            Step::json(json!({"input_tokens":1.5})),
            Step::json(json!({"input_tokens":9_007_199_254_740_992_u64})),
            Step::json(json!({})),
            Step {
                bytes: b"synthetic invalid JSON".to_vec(),
                ..Step::json(json!({}))
            },
            Step {
                bytes: vec![b'x'; 65_537],
                ..Step::json(json!({}))
            },
        ] {
            let mock = Arc::new(Mock::new([first, Step::json(json!({"input_tokens":0}))]));
            let counter = TokenCounter::new(mock.clone(), &config());
            let body = document(request());
            let headers = headers();
            let cancel = CancellationToken::new();
            assert_eq!(
                counter.count(&body, HAIKU, &headers, &cancel, "").await,
                None
            );
            assert_eq!(
                counter.count(&body, HAIKU, &headers, &cancel, "").await,
                Some(0)
            );
            assert_eq!(
                counter.count(&body, HAIKU, &headers, &cancel, "").await,
                Some(0)
            );
            assert_eq!(mock.calls(), 2);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_covers_fetch_and_body_and_cancellation_precedes_cache_hits() {
        for wait in [true, false] {
            let step = Step {
                wait,
                stall: !wait,
                ..Step::json(json!({"input_tokens":123}))
            };
            let mock = Arc::new(Mock::new([step, Step::json(json!({"input_tokens":123}))]));
            let counter = TokenCounter::new(mock.clone(), &config());
            let body = document(request());
            let headers = headers();
            let cancel = CancellationToken::new();
            assert_eq!(
                counter.count(&body, HAIKU, &headers, &cancel, "").await,
                None
            );
            assert_eq!(mock.dropped.load(Ordering::SeqCst), 1);
            assert_eq!(
                counter.count(&body, HAIKU, &headers, &cancel, "").await,
                Some(123)
            );
            cancel.cancel();
            assert_eq!(
                counter.count(&body, HAIKU, &headers, &cancel, "").await,
                None
            );
            assert_eq!(mock.calls(), 2);
        }
    }

    #[tokio::test]
    async fn caller_cancellation_drops_inflight_fetch_and_does_not_affect_other_counts() {
        let mock = Arc::new(Mock::new([
            Step {
                wait: true,
                ..Step::json(json!({}))
            },
            Step::json(json!({"input_tokens":77})),
        ]));
        let counter = Arc::new(TokenCounter::new(mock.clone(), &config()));
        let cancel = CancellationToken::new();
        let caller = cancel.clone();
        let pending = counter.clone();
        let task = tokio::spawn(async move {
            pending
                .count(&document(request()), HAIKU, &headers(), &caller, "")
                .await
        });
        while mock.calls() == 0 {
            tokio::task::yield_now().await;
        }
        cancel.cancel();
        assert_eq!(task.await.unwrap(), None);
        assert_eq!(mock.dropped.load(Ordering::SeqCst), 1);
        assert_eq!(
            counter
                .count(
                    &document(request()),
                    HAIKU,
                    &headers(),
                    &CancellationToken::new(),
                    ""
                )
                .await,
            Some(77)
        );
    }

    #[tokio::test]
    async fn response_content_length_is_not_authoritative_and_opaque_json_is_allowed() {
        let mut first = Step {
            bytes: br#"{"input_tokens":123,"opaque":["\ud800",1e999]}"#.to_vec(),
            ..Step::json(json!({}))
        };
        first
            .headers
            .insert("content-length", "999999".parse().unwrap());
        let mock = Arc::new(Mock::new([first]));
        let counter = TokenCounter::new(mock, &config());
        assert_eq!(
            counter
                .count(
                    &document(request()),
                    HAIKU,
                    &headers(),
                    &CancellationToken::new(),
                    ""
                )
                .await,
            Some(123)
        );
    }
    #[tokio::test]
    async fn every_baseline_thinking_target_preserves_complete_context_and_source() {
        for (source, target, expected_thinking) in [
            (HAIKU, "claude-sonnet-5-5", "between_tools"),
            (HAIKU, "claude-opus-5-5", "adaptive"),
            (HAIKU, SONNET, "disabled"),
            ("claude-sonnet-5-5", "claude-sonnet-5-5", "disabled"),
        ] {
            let mock = Arc::new(Mock::new([Step::json(json!({"input_tokens":12000}))]));
            let counter = TokenCounter::new(mock.clone(), &config());
            let mut body = request();
            body["model"] = json!(source);
            body["output_config"] = json!({"effort":"medium"});
            body["tool_choice"] = json!({"type":"auto"});
            let original = document(body.clone());
            let before = original.stringify();
            assert_eq!(
                counter
                    .count(&original, target, &headers(), &CancellationToken::new(), "")
                    .await,
                Some(12000)
            );
            assert_eq!(original.stringify(), before);
            for key in ["max_tokens", "stream", "metadata"] {
                body.as_object_mut().unwrap().remove(key);
            }
            body["model"] = json!(target);
            body["thinking"] = json!({"type":expected_thinking});
            let requests = mock.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(
                serde_json::from_slice::<Value>(&requests[0].2).unwrap(),
                body
            );
        }
    }

    #[tokio::test]
    async fn adapted_thinking_and_high_effort_have_exact_cache_equivalence() {
        let mock = Arc::new(Mock::new([]));
        let counter = TokenCounter::new(mock.clone(), &config());
        let target = "claude-sonnet-5-5";
        let mut body = request();
        body["model"] = json!(HAIKU);
        let h = headers();
        let cancel = CancellationToken::new();
        assert_eq!(
            counter
                .count(&document(body.clone()), target, &h, &cancel, "")
                .await,
            Some(1)
        );
        let payload: Value = serde_json::from_slice(&mock.requests.lock().unwrap()[0].2).unwrap();
        assert_eq!(payload["thinking"], json!({"type":"between_tools"}));
        let mut generation = body.clone();
        generation["max_tokens"] = json!(20);
        assert_eq!(
            counter
                .count(&document(generation), target, &h, &cancel, "")
                .await,
            Some(1)
        );
        let mut explicit = body.clone();
        explicit["model"] = json!(target);
        explicit["thinking"] = json!({"type":"between_tools"});
        assert_eq!(
            counter
                .count(&document(explicit), target, &h, &cancel, "")
                .await,
            Some(1)
        );
        let mut high = body.clone();
        high["output_config"] = json!({"effort":"xhigh"});
        assert_eq!(
            counter
                .count(&document(high.clone()), target, &h, &cancel, "")
                .await,
            Some(2)
        );
        let payload: Value = serde_json::from_slice(&mock.requests.lock().unwrap()[1].2).unwrap();
        assert_eq!(payload["thinking"], json!({"type":"adaptive"}));
        assert_eq!(payload["output_config"], high["output_config"]);
        high["thinking"] = json!({"type":"adaptive"});
        assert_eq!(
            counter
                .count(&document(high), target, &h, &cancel, "")
                .await,
            Some(2)
        );
        assert_eq!(
            counter
                .count(&document(body), target, &h, &cancel, "")
                .await,
            Some(1)
        );
        assert_eq!(mock.calls(), 2);
    }

    #[tokio::test]
    async fn tokenizer_context_modifiers_credentials_and_features_each_separate_cache_entries() {
        let mock = Arc::new(Mock::new([]));
        let counter = TokenCounter::new(mock.clone(), &config());
        let body = request();
        let h = headers();
        let cancel = CancellationToken::new();
        assert_eq!(
            counter
                .count(&document(body.clone()), HAIKU, &h, &cancel, "")
                .await,
            Some(1)
        );
        let mut generation = body.clone();
        generation["max_tokens"] = json!(128);
        generation["stream"] = json!(false);
        assert_eq!(
            counter
                .count(&document(generation), HAIKU, &h, &cancel, "")
                .await,
            Some(1)
        );
        assert_eq!(
            counter
                .count(&document(body.clone()), SONNET, &h, &cancel, "")
                .await,
            Some(2)
        );
        let mut thinking = body.clone();
        thinking["thinking"] = json!({"type":"adaptive"});
        assert_eq!(
            counter
                .count(&document(thinking), SONNET, &h, &cancel, "")
                .await,
            Some(3)
        );
        let mut effort = body.clone();
        effort["output_config"] = json!({"effort":"low"});
        assert_eq!(
            counter
                .count(&document(effort), SONNET, &h, &cancel, "")
                .await,
            Some(4)
        );
        for (key, value, expected) in [
            ("authorization", "Bearer refreshed-test-credential", 5),
            ("anthropic-beta", "different-feature", 6),
            ("anthropic-workspace-id", "workspace-2", 7),
        ] {
            let mut scoped = h.clone();
            scoped.insert(
                hyper::header::HeaderName::from_static(key),
                value.parse().unwrap(),
            );
            assert_eq!(
                counter
                    .count(&document(body.clone()), HAIKU, &scoped, &cancel, "")
                    .await,
                Some(expected)
            );
        }
        assert_eq!(
            counter.count(&document(body), HAIKU, &h, &cancel, "").await,
            Some(1)
        );
        assert_eq!(mock.calls(), 7);
    }

    #[tokio::test]
    async fn allowed_base64_client_advisor_and_beta_context_reaches_count_api_unchanged() {
        let mock = Arc::new(Mock::new([Step::json(json!({"input_tokens":1000}))]));
        let counter = TokenCounter::new(mock.clone(), &config());
        let mut body = request();
        body["model"] = json!(HAIKU);
        body["compaction"] = json!({"type":"summarize"});
        body["speed"] = json!("standard");
        body["output_format"] = json!({"type":"json_schema","schema":{"type":"object"}});
        body["tools"] = json!([{"name":"bash","type":"bash_20250124"},{"name":"advisor","type":"advisor_20260301","model":"claude-opus-5-5"}]);
        body["messages"] = json!([{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"test-image"}}]}]);
        assert_eq!(
            counter
                .count(
                    &document(body.clone()),
                    HAIKU,
                    &headers(),
                    &CancellationToken::new(),
                    ""
                )
                .await,
            Some(1000)
        );
        for key in ["max_tokens", "stream", "metadata"] {
            body.as_object_mut().unwrap().remove(key);
        }
        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            serde_json::from_slice::<Value>(&requests[0].2).unwrap(),
            body
        );
    }

    #[tokio::test(start_paused = true)]
    async fn configured_deadline_is_exact_for_fetch_and_body_and_failed_work_is_retriable() {
        for wait in [true, false] {
            let mock = Arc::new(Mock::new([
                Step {
                    wait,
                    stall: !wait,
                    ..Step::json(json!({"input_tokens":123}))
                },
                Step::json(json!({"input_tokens":123})),
            ]));
            let mut configuration = config();
            configuration.token_count_timeout_ms = 15;
            let counter = Arc::new(TokenCounter::new(mock.clone(), &configuration));
            let pending = counter.clone();
            let started = tokio::time::Instant::now();
            let task = tokio::spawn(async move {
                pending
                    .count(
                        &document(request()),
                        HAIKU,
                        &headers(),
                        &CancellationToken::new(),
                        "",
                    )
                    .await
            });
            while mock.calls() == 0 {
                tokio::task::yield_now().await;
            }
            tokio::time::advance(Duration::from_millis(14)).await;
            tokio::task::yield_now().await;
            assert!(
                !task.is_finished(),
                "configured deadline must not fire early"
            );
            assert_eq!(mock.dropped.load(Ordering::SeqCst), 0);
            tokio::time::advance(Duration::from_millis(1)).await;
            assert_eq!(task.await.unwrap(), None);
            assert_eq!(started.elapsed(), Duration::from_millis(15));
            assert_eq!(mock.dropped.load(Ordering::SeqCst), 1);
            assert_eq!(
                counter
                    .count(
                        &document(request()),
                        HAIKU,
                        &headers(),
                        &CancellationToken::new(),
                        ""
                    )
                    .await,
                Some(123)
            );
            assert_eq!(mock.calls(), 2);
        }
    }

    #[tokio::test]
    async fn native_count_transport_rejects_redirect_without_following_location() {
        use crate::http_client::NativeHttpClient;
        use hyper::body::Incoming;
        use hyper::service::service_fn;
        use hyper_util::rt::TokioIo;
        use std::convert::Infallible;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = requests.clone();
        let server = tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let observed = observed.clone();
                children.spawn(async move {
                    let service = service_fn(move |request: Request<Incoming>| {
                        let observed = observed.clone();
                        async move {
                            observed
                                .lock()
                                .unwrap()
                                .push((request.method().clone(), request.uri().path().to_owned()));
                            let follow = request.uri().path() == "/must-not-follow";
                            let _ = request.into_body().collect().await.unwrap();
                            let response = if follow {
                                Response::builder()
                                    .body(Full::new(Bytes::from_static(br#"{"input_tokens":123}"#)))
                                    .unwrap()
                            } else {
                                Response::builder()
                                    .status(307)
                                    .header("location", format!("http://{address}/must-not-follow"))
                                    .body(Full::new(Bytes::from_static(b"synthetic redirect")))
                                    .unwrap()
                            };
                            Ok::<_, Infallible>(response)
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(socket), service)
                        .await;
                });
            }
        });
        let mut configuration = config();
        configuration.upstream = format!("http://{address}");
        configuration.token_count_timeout_ms = 1000;
        let counter = TokenCounter::new(Arc::new(NativeHttpClient::new().unwrap()), &configuration);
        let result = counter
            .count(
                &document(request()),
                HAIKU,
                &headers(),
                &CancellationToken::new(),
                "",
            )
            .await;
        server.abort();
        let _ = server.await;
        assert_eq!(result, None);
        assert_eq!(
            *requests.lock().unwrap(),
            vec![(hyper::Method::POST, "/v1/messages/count_tokens".into())]
        );
    }
}
