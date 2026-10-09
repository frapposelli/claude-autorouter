//! Local-only inspection, explicitly requested downloads and synthetic warmup.
//! Each operation owns its deadline; disabling routing deadlines has no effect.
use crate::bounded_json::{DecodedResponseStream, ReadError, read_response_document};
use crate::evaluator::{EvaluationError, OLLAMA_VERSION_MESSAGE, evaluate_answer};
use crate::http_client::HttpTransport;
use autorouter_core::config::{
    Evaluator, RouterConfig, js_trim, validate_ollama_endpoint, validate_ollama_model,
};
use autorouter_core::js_json::{JsDocument, JsNode, NodeId};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response};
use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::future::Future;
use std::sync::LazyLock;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const JSON_LIMIT: usize = 1024 * 1024;
const PULL_LIMIT: usize = 16 * 1024 * 1024;
const LINE_LIMIT: usize = 64 * 1024;
#[derive(Debug, PartialEq, Eq)]
pub struct SetupError {
    pub code: &'static str,
    pub message: String,
}
impl std::fmt::Display for SetupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for SetupError {}
fn failure(code: &'static str, message: impl Into<String>) -> SetupError {
    SetupError {
        code,
        message: message.into(),
    }
}
fn unavailable() -> SetupError {
    failure(
        "OLLAMA_UNAVAILABLE",
        "Cannot reach local Ollama. Install Ollama from https://ollama.com/download and start it, then retry.",
    )
}
fn cancelled() -> SetupError {
    failure("OLLAMA_CANCELLED", "Ollama setup cancelled.")
}
fn timeout() -> SetupError {
    failure(
        "OLLAMA_TIMEOUT",
        "Ollama operation timed out. Check Ollama and retry.",
    )
}
fn cloud() -> SetupError {
    failure(
        "OLLAMA_CLOUD",
        "The selected Ollama model uses a remote service. Choose a local model.",
    )
}

async fn operation<T, F: Future<Output = Result<T, SetupError>>>(
    cancellation: &CancellationToken,
    timeout_ms: u64,
    action: impl FnOnce(CancellationToken) -> F,
) -> Result<T, SetupError> {
    let token = cancellation.child_token();
    let _guard = token.clone().drop_guard();
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(cancelled()),
        _ = tokio::time::sleep(Duration::from_millis(timeout_ms)) => Err(timeout()),
        result = action(token) => if cancellation.is_cancelled() {Err(cancelled())} else {result},
    }
}
fn field(document: &JsDocument, node: NodeId, key: &str) -> Option<NodeId> {
    document.get(node, key)
}
fn text(document: &JsDocument, node: Option<NodeId>) -> Option<String> {
    node.and_then(|node| document.string(node))
        .and_then(|s| s.to_scalar())
}
fn truthy(document: &JsDocument, node: Option<NodeId>) -> bool {
    match node.and_then(|node| document.node(node)) {
        None | Some(JsNode::Null | JsNode::Bool(false)) => false,
        Some(JsNode::Number(value)) => *value != 0.0 && !value.is_nan(),
        Some(JsNode::String(value)) => !value.units().is_empty(),
        _ => true,
    }
}
fn remote(document: &JsDocument, node: NodeId) -> bool {
    truthy(document, field(document, node, "remote_model"))
        || truthy(document, field(document, node, "remote_host"))
}

async fn fetch_response<T: HttpTransport>(
    transport: &T,
    url: &str,
    body: Option<Value>,
    cancellation: &CancellationToken,
) -> Result<Response<T::ResponseBody>, SetupError> {
    let request = if let Some(body) = body {
        Request::post(url)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body.to_string())))
    } else {
        Request::get(url).body(Full::new(Bytes::new()))
    }
    .map_err(|_| unavailable())?;
    let response = tokio::select! {biased; _=cancellation.cancelled()=>return Err(cancelled()), result=transport.request(request)=>result.map_err(|_|unavailable())?};
    if !response.status().is_success() {
        return Err(failure(
            "OLLAMA_HTTP",
            "Local Ollama rejected the request. Check the selected model and Ollama version.",
        ));
    }
    Ok(response)
}
fn read_error(error: ReadError) -> SetupError {
    match error {
        ReadError::Cancelled => cancelled(),
        ReadError::Oversized => {
            failure("OLLAMA_RESPONSE", "Ollama returned an oversized response.")
        }
        ReadError::InvalidJson => failure("OLLAMA_RESPONSE", "Ollama returned invalid JSON."),
        ReadError::InvalidResponse => {
            failure("OLLAMA_RESPONSE", "Ollama returned an invalid response.")
        }
        ReadError::Transport => unavailable(),
    }
}
async fn fetch_json<T: HttpTransport>(
    transport: &T,
    url: &str,
    body: Option<Value>,
    cancellation: &CancellationToken,
) -> Result<JsDocument, SetupError> {
    let (parts, body) = fetch_response(transport, url, body, cancellation)
        .await?
        .into_parts();
    read_response_document(body, &parts.headers, JSON_LIMIT, cancellation)
        .await
        .map_err(read_error)
}
fn model_identity(model: &str) -> String {
    let model = model.strip_prefix("registry.ollama.ai/").unwrap_or(model);
    let model = model.strip_prefix("library/").unwrap_or(model);
    if model.rsplit('/').next().unwrap_or_default().contains(':') {
        model.into()
    } else {
        format!("{model}:latest")
    }
}
fn supports_decisions(version: Option<String>) -> bool {
    static VERSION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^([0-9]+)\.([0-9]+)\.([0-9]+)(?:-[A-Za-z0-9.-]+)?(?:\+[A-Za-z0-9.-]+)?$")
            .unwrap()
    });
    let Some(version) = version else { return false };
    let Some(parts) = VERSION.captures(&version) else {
        return false;
    };
    let numbers: Option<Vec<_>> = (1..=3)
        .map(|i| {
            parts[i]
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite() && *n <= 9_007_199_254_740_991.0)
        })
        .collect();
    numbers.is_some_and(|n| n[0] > 0.0 || n[1] >= 35.0)
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Inspection {
    pub model: String,
    pub installed: bool,
}
pub async fn inspect_ollama<T: HttpTransport>(
    transport: &T,
    config: &RouterConfig,
    cancellation: &CancellationToken,
    timeout_ms: u64,
) -> Result<Inspection, SetupError> {
    let endpoint = validate_ollama_endpoint(&config.ollama_endpoint)
        .map_err(|m| failure("INVALID_CONFIGURATION", m))?;
    let model = validate_ollama_model(&config.ollama_model)
        .map_err(|m| failure("INVALID_CONFIGURATION", m))?;
    operation(cancellation,timeout_ms,|token|async move{
        let version=fetch_json(transport,&format!("{endpoint}/api/version"),None,&token).await?;
        if !supports_decisions(text(&version,field(&version,version.root(),"version"))) {return Err(failure("OLLAMA_VERSION",OLLAMA_VERSION_MESSAGE));}
        let tags=fetch_json(transport,&format!("{endpoint}/api/tags"),None,&token).await?;
        let models=field(&tags,tags.root(),"models").and_then(|node|tags.node(node));
        let Some(JsNode::Array(models))=models else{return Err(failure("OLLAMA_RESPONSE","Ollama returned an invalid model list."));};
        for item in models {
            let name=field(&tags,*item,"name").filter(|node| !matches!(tags.node(*node),Some(JsNode::Null))).or_else(||field(&tags,*item,"model"));
            if !truthy(&tags,Some(*item)) || !matches!(name.and_then(|node|tags.node(node)),Some(JsNode::String(_))) {return Err(failure("OLLAMA_RESPONSE","Ollama returned an invalid model list."));}
        }
        let identity=model_identity(&model);
        let entry=models.iter().find(|item|["name","model"].iter().any(|key|text(&tags,field(&tags,**item,key)).is_some_and(|name|model_identity(&name)==identity)));
        if let Some(entry)=entry {
            if remote(&tags,*entry){return Err(cloud());}
            let details=fetch_json(transport,&format!("{endpoint}/api/show"),Some(json!({"model":model})),&token).await?;
            if !matches!(details.node(details.root()),Some(JsNode::Object(_))) || truthy(&details,field(&details,details.root(),"error")){return Err(failure("OLLAMA_RESPONSE","Ollama returned invalid model details."));}
            if remote(&details,details.root()){return Err(cloud());}
            let size=field(&details,details.root(),"details").and_then(|n|field(&details,n,"parameter_size"));
            // Any non-whitespace UTF-16 code unit, including a lone surrogate,
            // satisfies the source's string.trim() check without coercion.
            let identified=size.and_then(|n|details.string(n)).is_some_and(|s|!js_trim(&s.to_well_formed()).is_empty());
            if !identified{return Err(failure("OLLAMA_RESPONSE","Ollama did not identify a local model. Check the selected model and Ollama version."));}
        }
        Ok(Inspection{model,installed:entry.is_some()})
    }).await
}

#[derive(Default)]
struct PullProgress {
    buffer: Vec<u8>,
    length: usize,
    success: bool,
    reported: HashSet<String>,
}
impl PullProgress {
    fn progress(&mut self, message: String, write: &mut impl FnMut(String)) {
        if self.reported.insert(message.clone()) {
            write(message);
        }
    }
    fn line(&mut self, line: &[u8], write: &mut impl FnMut(String)) -> Result<(), SetupError> {
        if js_trim(&String::from_utf8_lossy(line)).is_empty() {
            return Ok(());
        }
        if line.len() > LINE_LIMIT {
            return Err(failure(
                "OLLAMA_RESPONSE",
                "Ollama returned an oversized download update.",
            ));
        }
        let update = JsDocument::parse(line).map_err(|_| {
            failure(
                "OLLAMA_RESPONSE",
                "Ollama returned an invalid download update.",
            )
        })?;
        if !matches!(update.node(update.root()), Some(JsNode::Object(_)))
            || truthy(&update, field(&update, update.root(), "error"))
        {
            return Err(failure(
                "OLLAMA_PULL",
                "Ollama model download failed. Check Ollama and the selected model, then retry.",
            ));
        }
        let status = text(&update, field(&update, update.root(), "status"));
        let message = match status.as_deref() {
            Some("success") => {
                self.success = true;
                Some("Ollama model download complete.".into())
            }
            Some("pulling manifest") => Some("Ollama: downloading model manifest.".into()),
            Some("verifying sha256 digest") => Some("Ollama: verifying model data.".into()),
            Some("writing manifest") => Some("Ollama: saving model manifest.".into()),
            Some(status) if status.starts_with("pulling ") => {
                let number = |key| {
                    field(&update, update.root(), key)
                        .and_then(|n| update.node(n))
                        .and_then(|node| match node {
                            JsNode::Number(n)
                                if n.is_finite()
                                    && n.fract() == 0.0
                                    && n.abs() <= 9_007_199_254_740_991.0 =>
                            {
                                Some(*n)
                            }
                            _ => None,
                        })
                };
                match (number("completed"), number("total")) {
                    (Some(completed), Some(total))
                        if total > 0.0 && completed >= 0.0 && completed <= total =>
                    {
                        Some(format!(
                            "Ollama: downloading model data ({}%).",
                            (completed / total * 10.0).floor() * 10.0
                        ))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(message) = message {
            self.progress(message, write);
        }
        Ok(())
    }
    fn chunk(&mut self, chunk: &[u8], write: &mut impl FnMut(String)) -> Result<(), SetupError> {
        self.length = self.length.saturating_add(chunk.len());
        if self.length > PULL_LIMIT {
            return Err(failure(
                "OLLAMA_RESPONSE",
                "Ollama returned too many download updates.",
            ));
        }
        self.buffer.extend_from_slice(chunk);
        let mut consumed = 0;
        while let Some(newline) = self.buffer[consumed..].iter().position(|b| *b == b'\n') {
            let end = consumed + newline;
            let line = self.buffer[consumed..end].to_vec();
            self.line(&line, write)?;
            consumed = end + 1;
        }
        self.buffer.drain(..consumed);
        if self.buffer.len() > LINE_LIMIT {
            return Err(failure(
                "OLLAMA_RESPONSE",
                "Ollama returned an oversized download update.",
            ));
        }
        Ok(())
    }
    fn finish(&mut self, write: &mut impl FnMut(String)) -> Result<(), SetupError> {
        let tail = std::mem::take(&mut self.buffer);
        if !tail.is_empty() {
            self.line(&tail, write)?;
        }
        if !self.success {
            return Err(failure(
                "OLLAMA_PULL",
                "Ollama download ended before completion. Retry setup with --pull.",
            ));
        }
        Ok(())
    }
}
async fn pull_ollama<T: HttpTransport>(
    transport: &T,
    config: &RouterConfig,
    cancellation: &CancellationToken,
    timeout_ms: u64,
    write: &mut impl FnMut(String),
) -> Result<(), SetupError> {
    let endpoint = validate_ollama_endpoint(&config.ollama_endpoint)
        .map_err(|m| failure("INVALID_CONFIGURATION", m))?;
    operation(cancellation, timeout_ms, |token| async move {
        let (parts, body) = fetch_response(
            transport,
            &format!("{endpoint}/api/pull"),
            Some(json!({"model":config.ollama_model,"stream":true})),
            &token,
        )
        .await?
        .into_parts();
        let mut stream = DecodedResponseStream::new(body, &parts.headers, &token)
            .await
            .map_err(read_error)?;
        let mut buffer = [0; 8192];
        let mut progress = PullProgress::default();
        loop {
            let count = stream.read(&mut buffer).await.map_err(read_error)?;
            if count == 0 {
                break;
            }
            progress.chunk(&buffer[..count], write)?;
        }
        progress.finish(write)
    })
    .await
}
pub async fn warm_ollama<T: HttpTransport>(
    transport: &T,
    config: &RouterConfig,
    cancellation: &CancellationToken,
    timeout_ms: u64,
) -> Result<(), SetupError> {
    operation(cancellation,timeout_ms,|token|async move{
        let state=json!({"system":"","original_task":"Return the literal word ready.","current_task":"Return the literal word ready.","recent_messages":[],"message_count":1,"tool_count":0,"context_is_excerpt":true});
        let mut config=config.clone();config.evaluator=Evaluator::Ollama;config.ollama_timeout_ms=timeout_ms;
        if config.ollama_keep_alive.is_null(){config.ollama_keep_alive=json!("5m");}
        evaluate_answer(transport,&config,&state,&token).await.map(|_|()).map_err(|error|match error{
            EvaluationError::Cancelled=>cancelled(),EvaluationError::Timeout=>timeout(),
            EvaluationError::Http{missing_systemone:true,..}=>failure("OLLAMA_VERSION",OLLAMA_VERSION_MESSAGE),
            _=>failure("OLLAMA_WARMUP","Ollama could not prepare the local evaluator. Check the selected model and available memory, then retry."),
        })
    }).await
}
pub struct SetupOptions {
    pub pull: bool,
    pub warm: bool,
    pub inspect_timeout_ms: u64,
    pub pull_timeout_ms: u64,
    pub warm_timeout_ms: u64,
}
impl Default for SetupOptions {
    fn default() -> Self {
        Self {
            pull: false,
            warm: true,
            inspect_timeout_ms: 5000,
            pull_timeout_ms: 15 * 60 * 1000,
            warm_timeout_ms: 60000,
        }
    }
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct SetupResult {
    pub model: String,
    pub pulled: bool,
    pub warmed: bool,
}
pub async fn setup_ollama<T: HttpTransport>(
    transport: &T,
    config: &RouterConfig,
    cancellation: &CancellationToken,
    options: &SetupOptions,
    write: &mut impl FnMut(String),
) -> Result<SetupResult, SetupError> {
    let inspection =
        inspect_ollama(transport, config, cancellation, options.inspect_timeout_ms).await?;
    let mut pulled = false;
    if !inspection.installed {
        if !options.pull {
            return Err(failure(
                "OLLAMA_MODEL_MISSING",
                format!(
                    "The selected Ollama model is not installed. Run ollama pull {}, or rerun setup --evaluator ollama --pull (add --force if already configured).",
                    inspection.model
                ),
            ));
        }
        write(format!(
            "Downloading {} with local Ollama. Model files are fetched from the model registry.",
            inspection.model
        ));
        pull_ollama(
            transport,
            config,
            cancellation,
            options.pull_timeout_ms,
            write,
        )
        .await?;
        if !inspect_ollama(transport, config, cancellation, options.inspect_timeout_ms)
            .await?
            .installed
        {
            return Err(failure(
                "OLLAMA_MODEL_MISSING",
                "Ollama finished downloading but the selected model is not available. Check Ollama and retry.",
            ));
        }
        pulled = true;
    }
    if options.warm {
        write(format!("Preloading {} in local Ollama.", inspection.model));
        warm_ollama(transport, config, cancellation, options.warm_timeout_ms).await?;
    }
    Ok(SetupResult {
        model: inspection.model,
        pulled,
        warmed: options.warm,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_client::HttpError;
    use autorouter_core::config::read_config;
    use http_body_util::BodyExt;
    use hyper::body::{Body, Frame};
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use std::task::{Context, Poll};

    struct TestBody {
        chunks: VecDeque<Bytes>,
        stalled: bool,
        dropped: Arc<AtomicBool>,
    }
    impl Drop for TestBody {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }
    impl Body for TestBody {
        type Data = Bytes;
        type Error = std::io::Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            if let Some(chunk) = self.chunks.pop_front() {
                Poll::Ready(Some(Ok(Frame::data(chunk))))
            } else if self.stalled {
                Poll::Pending
            } else {
                Poll::Ready(None)
            }
        }
    }
    fn body(chunks: Vec<Vec<u8>>) -> TestBody {
        TestBody {
            chunks: chunks.into_iter().map(Bytes::from).collect(),
            stalled: false,
            dropped: Arc::new(AtomicBool::new(false)),
        }
    }
    fn response(value: Value) -> Response<TestBody> {
        Response::new(body(vec![value.to_string().into_bytes()]))
    }
    fn version() -> Response<TestBody> {
        response(json!({"version":"0.35.0"}))
    }
    fn tags(installed: bool) -> Response<TestBody> {
        response(
            json!({"models":if installed{vec![json!({"name":"nimble:9b-q4_K_M"})]}else{Vec::new()}}),
        )
    }
    fn details() -> Response<TestBody> {
        response(json!({"details":{"parameter_size":"9B"}}))
    }
    fn answer() -> Response<TestBody> {
        response(
            json!({"model":"nimble:9b-q4_K_M","answers":{"tier":{"type":"choice","choice":"haiku","probabilities":{"haiku":1,"sonnet":0,"opus":0},"confidence":1}},"usage":{"input_tokens":900,"output_tokens":1}}),
        )
    }
    type RecordedCall = (String, hyper::Method, hyper::HeaderMap, Vec<u8>);
    struct Mock {
        responses: Mutex<VecDeque<Response<TestBody>>>,
        calls: Mutex<Vec<RecordedCall>>,
    }
    impl Mock {
        fn new(responses: Vec<Response<TestBody>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                calls: Mutex::new(Vec::new()),
            }
        }
    }
    impl HttpTransport for Mock {
        type ResponseBody = TestBody;
        async fn request(
            &self,
            request: Request<Full<Bytes>>,
        ) -> Result<Response<TestBody>, HttpError> {
            let (parts, body) = request.into_parts();
            let bytes = body.collect().await.unwrap().to_bytes().to_vec();
            self.calls.lock().unwrap().push((
                parts.uri.path().into(),
                parts.method,
                parts.headers,
                bytes,
            ));
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(HttpError::Network)
        }
    }
    fn config() -> RouterConfig {
        read_config(
            &json!({"AUTOROUTER_AUTH_MODE":"subscription"}),
            false,
            std::path::Path::new("/synthetic"),
        )
        .unwrap()
    }
    fn paths(mock: &Mock) -> Vec<String> {
        mock.calls
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.0.clone())
            .collect()
    }

    #[tokio::test]
    async fn version_gate_and_local_validation_precede_downloads_or_task_data() {
        for value in [
            json!("0.33.3"),
            json!("0.34.9"),
            json!("PRIVATE_VERSION"),
            Value::Null,
            json!("9007199254740992.0.0"),
        ] {
            let mock = Mock::new(vec![response(json!({"version":value}))]);
            let error = setup_ollama(
                &mock,
                &config(),
                &CancellationToken::new(),
                &SetupOptions {
                    pull: true,
                    ..Default::default()
                },
                &mut |_| {},
            )
            .await
            .unwrap_err();
            assert_eq!(error.code, "OLLAMA_VERSION");
            assert!(!error.message.contains("PRIVATE"));
            assert_eq!(paths(&mock), ["/api/version"]);
        }
        for value in ["0.35.0", "0.35.0-rc.1", "0.36.0", "1.0.0"] {
            let mock = Mock::new(vec![response(json!({"version":value})), tags(false)]);
            assert!(
                !inspect_ollama(&mock, &config(), &CancellationToken::new(), 5000)
                    .await
                    .unwrap()
                    .installed
            );
            assert_eq!(paths(&mock), ["/api/version", "/api/tags"]);
        }
        let mock = Mock::new(Vec::new());
        let mut config = config();
        config.ollama_endpoint = "https://synthetic-cloud.invalid".into();
        assert!(
            inspect_ollama(&mock, &config, &CancellationToken::new(), 5000)
                .await
                .is_err()
        );
        assert!(paths(&mock).is_empty());
    }

    #[tokio::test]
    async fn canonical_model_aliases_match_but_cloud_backing_and_ambiguous_metadata_fail() {
        for (requested, installed) in [
            ("nimble", "nimble:latest"),
            ("library/nimble:9b-q4_K_M", "nimble:9b-q4_K_M"),
            ("registry.ollama.ai/team/local:v1", "team/local:v1"),
        ] {
            let mut config = config();
            config.ollama_model = requested.into();
            let mock = Mock::new(vec![
                version(),
                response(json!({"models":[{"name":installed}]})),
                details(),
            ]);
            assert!(
                inspect_ollama(&mock, &config, &CancellationToken::new(), 5000)
                    .await
                    .unwrap()
                    .installed
            );
            assert_eq!(paths(&mock), ["/api/version", "/api/tags", "/api/show"]);
            assert_eq!(
                serde_json::from_slice::<Value>(&mock.calls.lock().unwrap()[2].3).unwrap(),
                json!({"model":requested})
            );
        }
        for payload in [
            r#"{"remote_host":"PRIVATE_REMOTE","details":{"parameter_size":"1B"}}"#,
            r#"{"remote_model":1e999,"details":{"parameter_size":"1B"}}"#,
            r#"{"error":1e999}"#,
            r#"{"details":{"parameter_size":"  "}}"#,
            "{}",
        ] {
            let mock = Mock::new(vec![
                version(),
                tags(true),
                Response::new(body(vec![payload.as_bytes().to_vec()])),
            ]);
            let error = inspect_ollama(&mock, &config(), &CancellationToken::new(), 5000)
                .await
                .unwrap_err();
            assert!(!error.message.contains("PRIVATE"));
            assert_eq!(paths(&mock), ["/api/version", "/api/tags", "/api/show"]);
        }
    }

    #[tokio::test]
    async fn missing_model_is_read_only_until_an_explicit_pull_and_warmup_uses_only_fixed_state() {
        let mock = Mock::new(vec![version(), tags(false)]);
        let error = setup_ollama(
            &mock,
            &config(),
            &CancellationToken::new(),
            &SetupOptions::default(),
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "OLLAMA_MODEL_MISSING");
        assert_eq!(paths(&mock), ["/api/version", "/api/tags"]);
        let mock = Mock::new(vec![version(), tags(true), details(), details(), answer()]);
        let mut config = config();
        config.ollama_timeout_ms = 1;
        config.jev_key = Some("PRIVATE_UNUSED_JEV".into());
        config.anthropic_key = Some("PRIVATE_UNUSED_ANTHROPIC".into());
        let result = setup_ollama(
            &mock,
            &config,
            &CancellationToken::new(),
            &SetupOptions {
                pull: true,
                ..Default::default()
            },
            &mut |_| {},
        )
        .await
        .unwrap();
        assert_eq!(
            result,
            SetupResult {
                model: config.ollama_model.clone(),
                pulled: false,
                warmed: true
            }
        );
        assert_eq!(
            paths(&mock),
            [
                "/api/version",
                "/api/tags",
                "/api/show",
                "/api/show",
                "/v1/systemone"
            ]
        );
        for (_, _, headers, body) in mock.calls.lock().unwrap().iter() {
            assert!(!headers.contains_key("authorization"));
            assert!(!String::from_utf8_lossy(body).contains("PRIVATE"));
        }
        let calls = mock.calls.lock().unwrap();
        let payload: Value = serde_json::from_slice(&calls[4].3).unwrap();
        assert_eq!(
            payload["state"],
            json!({"system":"","original_task":"Return the literal word ready.","current_task":"Return the literal word ready.","recent_messages":[],"message_count":1,"tool_count":0,"context_is_excerpt":true})
        );
        assert_eq!(payload["questions"], crate::evaluator::ollama_questions());
    }

    #[tokio::test]
    async fn split_download_stream_deduplicates_safe_progress_and_verifies_installation() {
        let updates = [
            "{\"status\":\"pulling mani",
            "fest\"}\n",
            "{\"status\":\"pulling PRIVATE_PROVIDER\",\"completed\":50,\"total\":100}\n",
            "{\"status\":\"pulling PRIVATE_PROVIDER\",\"completed\":50,\"total\":100}\n",
            "{\"status\":\"verifying sha256 digest\"}\n{\"status\":\"success\"}",
        ];
        let mock = Mock::new(vec![
            version(),
            tags(false),
            Response::new(body(
                updates.iter().map(|s| s.as_bytes().to_vec()).collect(),
            )),
            version(),
            tags(true),
            details(),
            details(),
            answer(),
        ]);
        let mut progress = Vec::new();
        let result = setup_ollama(
            &mock,
            &config(),
            &CancellationToken::new(),
            &SetupOptions {
                pull: true,
                ..Default::default()
            },
            &mut |line| progress.push(line),
        )
        .await
        .unwrap();
        assert!(result.pulled && result.warmed);
        assert_eq!(
            progress.iter().filter(|line| line.contains("50%")).count(),
            1
        );
        assert!(!progress.join("\n").contains("PRIVATE"));
        assert_eq!(
            paths(&mock),
            [
                "/api/version",
                "/api/tags",
                "/api/pull",
                "/api/version",
                "/api/tags",
                "/api/show",
                "/api/show",
                "/v1/systemone"
            ]
        );
    }

    #[tokio::test]
    async fn incomplete_failed_and_oversized_downloads_never_warm() {
        for raw in [
            "{\"status\":\"pulling manifest\"}\n".to_owned(),
            "{\"error\":\"PRIVATE_DOWNLOAD\"}\n".into(),
            format!("{{\"status\":\"{}\"}}\n", "x".repeat(LINE_LIMIT + 1)),
            "PRIVATE_INVALID_JSON\n".into(),
        ] {
            let mock = Mock::new(vec![
                version(),
                tags(false),
                Response::new(body(vec![raw.into_bytes()])),
            ]);
            let error = setup_ollama(
                &mock,
                &config(),
                &CancellationToken::new(),
                &SetupOptions {
                    pull: true,
                    ..Default::default()
                },
                &mut |_| {},
            )
            .await
            .unwrap_err();
            assert!(!error.message.contains("PRIVATE"));
            assert_eq!(paths(&mock), ["/api/version", "/api/tags", "/api/pull"]);
        }
        let mut progress = PullProgress {
            length: PULL_LIMIT,
            ..Default::default()
        };
        assert_eq!(
            progress.chunk(b"x", &mut |_| {}).unwrap_err().message,
            "Ollama returned too many download updates."
        );
    }

    #[tokio::test(start_paused = true)]
    async fn warmup_has_its_own_deadline_and_cancellation_releases_stalled_bodies() {
        for caller_cancel in [false, true] {
            let dropped = Arc::new(AtomicBool::new(false));
            let stalled = Response::new(TestBody {
                chunks: VecDeque::new(),
                stalled: true,
                dropped: dropped.clone(),
            });
            let mock = Mock::new(vec![version(), tags(true), details(), details(), stalled]);
            let mut config = config();
            config.ollama_timeout_ms = 0;
            let cancel = CancellationToken::new();
            let requested = cancel.clone();
            let stop = async move {
                if caller_cancel {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    requested.cancel();
                }
            };
            let options = SetupOptions {
                warm_timeout_ms: 20,
                ..Default::default()
            };
            let mut sink = |_| {};
            let (result, ()) = tokio::join!(
                setup_ollama(&mock, &config, &cancel, &options, &mut sink),
                stop
            );
            assert_eq!(
                result.unwrap_err().code,
                if caller_cancel {
                    "OLLAMA_CANCELLED"
                } else {
                    "OLLAMA_TIMEOUT"
                }
            );
            assert!(dropped.load(Ordering::SeqCst));
            assert_eq!(config.ollama_timeout_ms, 0);
        }
    }

    #[tokio::test]
    async fn model_that_becomes_remote_before_warmup_is_rejected_before_synthetic_inference() {
        let mock = Mock::new(vec![
            version(),
            tags(true),
            details(),
            response(json!({"remote_host":"PRIVATE_REMOTE","details":{"parameter_size":"9B"}})),
        ]);
        let error = setup_ollama(
            &mock,
            &config(),
            &CancellationToken::new(),
            &SetupOptions::default(),
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "OLLAMA_WARMUP");
        assert!(!error.message.contains("PRIVATE"));
        assert_eq!(
            paths(&mock),
            ["/api/version", "/api/tags", "/api/show", "/api/show"]
        );
    }
}
