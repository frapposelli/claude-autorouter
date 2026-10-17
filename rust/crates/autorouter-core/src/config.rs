//! Configuration parsing with the existing environment/file contract.
//!
//! This module has no environment or filesystem access. The caller supplies the
//! effective environment and working directory after storage/policy resolution.
use crate::js_json::{JsDocument, JsNode, JsString, NodeId};
use regex::Regex;
use serde::ser::{Error as _, SerializeMap};
use serde::{Serialize, Serializer};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock};
use url::Url;

pub const DEFAULT_OLLAMA_MODEL: &str = "nimble:9b-q4_K_M";
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Evaluator {
    Jev,
    Ollama,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMode {
    ApiKey,
    Subscription,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientProfile {
    Compatible,
    Native,
    Auto,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionLogMode {
    Metadata,
    Prompts,
}

#[derive(Clone, Serialize)]
pub struct Models {
    pub haiku: String,
    pub sonnet: String,
    pub opus: String,
}

// Deliberately no Debug implementation: this contract contains credentials.
#[derive(Clone)]
pub struct RouterConfig {
    pub evaluator: Evaluator,
    pub auth_mode: AuthMode,
    pub client_profile: ClientProfile,
    pub session_log_dir: Option<String>,
    pub session_log_mode: SessionLogMode,
    pub stop_hook_block_cap: Option<u64>,
    pub anthropic_key: Option<String>,
    pub jev_key: Option<String>,
    pub local_token: Option<String>,
    /// Captured values for the three credential fields only. Use replacement
    /// methods to clear both this source state and the typed display fields.
    pub source_credentials: SourceCredentials,
    pub upstream: String,
    pub jev_endpoint: String,
    pub jev_model: String,
    pub ollama_endpoint: String,
    pub ollama_model: String,
    pub ollama_timeout_ms: u64,
    pub ollama_state_chars: usize,
    pub ollama_keep_alive: Value,
    pub models: Models,
    /// Original UTF-16 identities from saved JSON. Scalar model fields remain
    /// suitable for display and catalog lookup, never identity comparison.
    pub exact_models: BTreeMap<String, JsString>,
    pub exact_jev_model: Option<JsString>,
    pub port: u16,
    pub jev_timeout_ms: u64,
    pub token_count_timeout_ms: u64,
    pub min_confidence: f64,
    pub state_chars: usize,
    pub max_body_bytes: usize,
    pub cache_entries: usize,
    pub cache_ttl_ms: u64,
    pub turn_ttl_ms: u64,
    /// Optional embedding limit, matching RouterConfig.turnEntries. CLI
    /// configuration leaves this absent and retains the 1,000-entry default.
    pub turn_entries: Option<usize>,
    pub upstream_timeout_ms: u64,
}

/// Names of the credential fields retained by configuration parsing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Credential {
    Anthropic,
    Jev,
    LocalToken,
}
impl Credential {
    fn index(self) -> usize {
        match self {
            Self::Anthropic => 0,
            Self::Jev => 1,
            Self::LocalToken => 2,
        }
    }
    fn env_name(self) -> &'static str {
        match self {
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::Jev => "TYPESAFE_API_KEY",
            Self::LocalToken => "AUTOROUTER_TOKEN",
        }
    }
    fn json_name(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropicKey",
            Self::Jev => "jevKey",
            Self::LocalToken => "localToken",
        }
    }
}

// No Debug/Serialize implementation: only RouterConfig may emit effective,
// whitelisted values, and ordinary diagnostics must not expose source secrets.
#[derive(Clone, Default)]
pub struct SourceCredentials {
    slots: [Option<CapturedCredential>; 3],
}
#[derive(Clone)]
struct CapturedCredential {
    document: Arc<JsDocument>,
    projection: Option<String>,
}

enum CredentialValue<'a> {
    Text(&'a str),
    Source(&'a JsDocument),
}
impl Serialize for CredentialValue<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Text(value) => serializer.serialize_str(value),
            Self::Source(document) => {
                // This is an output view, never the source used by require_keys.
                // JSON text retains UTF-16 escapes; Value observation may fail.
                let raw = serde_json::value::RawValue::from_string(document.stringify())
                    .map_err(|_| S::Error::custom("Credential JSON serialization failed"))?;
                raw.serialize(serializer)
            }
        }
    }
}
impl RouterConfig {
    fn typed_credential(&self, key: Credential) -> &Option<String> {
        match key {
            Credential::Anthropic => &self.anthropic_key,
            Credential::Jev => &self.jev_key,
            Credential::LocalToken => &self.local_token,
        }
    }
    fn credential_value(&self, key: Credential) -> Option<CredentialValue<'_>> {
        let typed = self.typed_credential(key).as_deref();
        if let Some(captured) = &self.source_credentials.slots[key.index()]
            && typed == captured.projection.as_deref()
        {
            return Some(CredentialValue::Source(&captured.document));
        }
        typed.map(CredentialValue::Text)
    }
    /// Replace or remove a credential and erase its previously captured source.
    /// Direct public field writes cannot communicate Some-to-None write history.
    pub fn replace_credential(&mut self, key: Credential, value: Option<String>) {
        self.source_credentials.slots[key.index()] = None;
        match key {
            Credential::Anthropic => self.anthropic_key = value,
            Credential::Jev => self.jev_key = value,
            Credential::LocalToken => self.local_token = value,
        }
    }
    /// Preserve an explicitly supplied JSON null or non-string, without keeping
    /// unrelated settings. None removes the credential; Some(null) retains null.
    pub fn replace_credential_value(&mut self, key: Credential, value: Option<&Value>) {
        self.replace_credential(key, value.and_then(Value::as_str).map(str::to_owned));
        if let Some(value) = value.filter(|value| !value.is_string()) {
            self.source_credentials.slots[key.index()] = Some(CapturedCredential {
                document: Arc::new(JsDocument::from_serde_value(value)),
                projection: None,
            });
        }
    }
    /// Capture only the selected source subtree. Invalid node IDs leave the
    /// existing credential intact; numbers and UTF-16 are never reparsed from
    /// their lossy JSON serialization.
    pub fn replace_credential_document(
        &mut self,
        key: Credential,
        source: &JsDocument,
        node: NodeId,
    ) -> Result<(), String> {
        if let Some(text) = source.string(node).and_then(JsString::to_scalar) {
            self.replace_credential(key, Some(text));
            return Ok(());
        }
        let document = source
            .compact_subtree(node)
            .ok_or_else(|| "Invalid credential source".to_owned())?;
        let projection = source.string(node).map(JsString::to_well_formed);
        self.replace_credential(key, projection.clone());
        self.source_credentials.slots[key.index()] = Some(CapturedCredential {
            document: Arc::new(document),
            projection,
        });
        Ok(())
    }
    /// Explicit sensitive JSON output used by configuration and private cache
    /// identity. Absence stays distinct from serialized null. Never log it.
    pub fn credential_json(&self, key: Credential) -> Option<String> {
        self.credential_value(key).map(|value| match value {
            CredentialValue::Text(text) => JsString::from_scalar(text).stringify(),
            CredentialValue::Source(document) => document.stringify(),
        })
    }
}
impl Serialize for RouterConfig {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        macro_rules! field {
            ($field:ident, $name:literal) => {
                map.serialize_entry($name, &self.$field)?
            };
        }
        macro_rules! optional {
            ($field:ident, $name:literal) => {
                if let Some(value) = &self.$field {
                    map.serialize_entry($name, value)?;
                }
            };
        }
        field!(evaluator, "evaluator");
        field!(auth_mode, "authMode");
        field!(client_profile, "clientProfile");
        optional!(session_log_dir, "sessionLogDir");
        field!(session_log_mode, "sessionLogMode");
        optional!(stop_hook_block_cap, "stopHookBlockCap");
        for key in [
            Credential::Anthropic,
            Credential::Jev,
            Credential::LocalToken,
        ] {
            if let Some(value) = self.credential_value(key) {
                map.serialize_entry(key.json_name(), &value)?;
            }
        }
        field!(upstream, "upstream");
        field!(jev_endpoint, "jevEndpoint");
        field!(jev_model, "jevModel");
        field!(ollama_endpoint, "ollamaEndpoint");
        field!(ollama_model, "ollamaModel");
        field!(ollama_timeout_ms, "ollamaTimeoutMs");
        field!(ollama_state_chars, "ollamaStateChars");
        field!(ollama_keep_alive, "ollamaKeepAlive");
        field!(models, "models");
        field!(port, "port");
        field!(jev_timeout_ms, "jevTimeoutMs");
        field!(token_count_timeout_ms, "tokenCountTimeoutMs");
        field!(min_confidence, "minConfidence");
        field!(state_chars, "stateChars");
        field!(max_body_bytes, "maxBodyBytes");
        field!(cache_entries, "cacheEntries");
        field!(cache_ttl_ms, "cacheTtlMs");
        field!(turn_ttl_ms, "turnTtlMs");
        optional!(turn_entries, "turnEntries");
        field!(upstream_timeout_ms, "upstreamTimeoutMs");
        map.end()
    }
}

// ECMAScript trim excludes U+0085 but includes the BOM; Rust str::trim differs.
pub fn js_trim(value: &str) -> &str {
    value.trim_matches(|c| {
        matches!(c,
        '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
        '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' |
        '\u{205f}' | '\u{3000}' | '\u{feff}')
    })
}

fn value<'a>(env: &'a Value, key: &str) -> Option<&'a Value> {
    env.get(key)
}
fn nullish<'a>(env: &'a Value, key: &str) -> Option<&'a Value> {
    value(env, key).filter(|v| !v.is_null())
}
fn text(env: &Value, key: &str) -> Option<String> {
    value(env, key).and_then(Value::as_str).map(str::to_owned)
}
// RegExp.test in the source coerces keep_alive before validating but returns
// the original value. Numeric zero and singleton string arrays therefore remain
// distinguishable from the string "0" at the evaluator boundary.
fn regex_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) if value.as_f64() == Some(0.0) => "0".into(),
        Value::Number(value) => value.to_string(),
        Value::Array(values) => values
            .iter()
            .map(|v| {
                if v.is_null() {
                    String::new()
                } else {
                    regex_string(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn enum_text<'a>(env: &'a Value, key: &str, fallback: &'a str) -> &'a str {
    match nullish(env, key) {
        None => fallback,
        Some(v) => v.as_str().unwrap_or(""),
    }
}

fn decimal(raw: &Value, integer: bool) -> Option<f64> {
    static INTEGER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9]+$").unwrap());
    static DECIMAL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(?:[0-9]+(?:\.[0-9]+)?|\.[0-9]+)$").unwrap());
    match raw {
        Value::String(s) => {
            let s = js_trim(s);
            if (if integer { &INTEGER } else { &DECIMAL }).is_match(s) {
                s.parse().ok()
            } else {
                None
            }
        }
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

fn number(
    env: &Value,
    key: &str,
    fallback: f64,
    min: f64,
    max: f64,
    integer: bool,
) -> Result<f64, String> {
    let n = value(env, key).map_or(Some(fallback), |raw| decimal(raw, integer));
    n.filter(|n| {
        n.is_finite()
            && *n >= min
            && *n <= max
            && (!integer || (n.fract() == 0.0 && *n <= MAX_SAFE_INTEGER as f64))
    })
    .ok_or_else(|| {
        format!(
            "{key} must be {} between {min} and {max}",
            if integer { "an integer" } else { "a number" }
        )
    })
}

pub fn parse_stop_hook_block_cap(raw: &Value, name: &str) -> Result<u64, String> {
    decimal(raw, true).filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0 && *n <= MAX_SAFE_INTEGER as f64)
        .map(|n| n as u64).ok_or_else(|| format!("{name} requires a nonnegative safe integer (0 disables the Stop-hook continuation cap)"))
}

pub fn resolve_path(cwd: &Path, path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    };
    let mut result = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            component => result.push(component.as_os_str()),
        }
    }
    result
}

pub fn parse_session_log_dir(raw: Option<&Value>, cwd: &Path) -> Result<Option<String>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.as_str() == Some("") {
        return Ok(None);
    }
    let error = "AUTOROUTER_SESSION_LOG_DIR must be a directory path, or an empty string to disable session logging";
    let text = raw
        .as_str()
        .filter(|s| !js_trim(s).is_empty() && !s.chars().any(|c| c <= '\u{1f}' || c == '\u{7f}'))
        .ok_or_else(|| error.to_owned())?;
    Ok(Some(
        resolve_path(cwd, Path::new(text))
            .to_string_lossy()
            .into_owned(),
    ))
}

fn model_name(raw: Option<&Value>, default: &str, name: &str) -> Result<String, String> {
    let model = match raw {
        Some(value) => value.as_str(),
        None => Some(default),
    };
    model
        .filter(|s| {
            !js_trim(s).is_empty()
                && !s
                    .chars()
                    .any(|c| c <= '\u{1f}' || ('\u{7f}'..='\u{9f}').contains(&c))
        })
        .map(str::to_owned)
        .ok_or_else(|| format!("{name} must be a nonempty model name without control characters"))
}

// URL's JSON primitive conversion is distinct from enum/model validation.
// Source nodes retain nonfinite numbers and UTF-16 until this URL boundary.
#[derive(Clone, Copy)]
enum UrlInput<'a> {
    Json(&'a Value),
    Source(&'a JsDocument, NodeId),
}
fn url_input<'a>(
    env: &'a Value,
    source: Option<&'a JsDocument>,
    key: &str,
    active: bool,
) -> Option<UrlInput<'a>> {
    if !active {
        return None;
    }
    if let Some(source) = source {
        let node = source.get(source.root(), key)?;
        return (!matches!(source.node(node), Some(JsNode::Null)))
            .then_some(UrlInput::Source(source, node));
    }
    nullish(env, key).map(UrlInput::Json)
}
fn url_string(input: UrlInput<'_>) -> Result<Cow<'_, str>, ()> {
    if let UrlInput::Json(Value::String(text)) = input {
        return Ok(Cow::Borrowed(text));
    }
    let mut output = String::new();
    let mut pending = vec![(Some(input), false)];
    let mut number = ryu_js::Buffer::new();
    while let Some((input, array_item)) = pending.pop() {
        let Some(input) = input else {
            output.push(',');
            continue;
        };
        let numeric = match input {
            UrlInput::Json(Value::Number(value)) => Some(value.as_f64().ok_or(())?),
            UrlInput::Source(source, node) => match source.node(node).ok_or(())? {
                JsNode::Number(value) => Some(*value),
                _ => None,
            },
            _ => None,
        };
        if let Some(value) = numeric {
            output.push_str(if value.is_nan() {
                "NaN"
            } else if value == f64::INFINITY {
                "Infinity"
            } else if value == f64::NEG_INFINITY {
                "-Infinity"
            } else if value == 0.0 {
                "0"
            } else {
                number.format_finite(value)
            });
            continue;
        }
        match input {
            UrlInput::Json(value) => match value {
                Value::Null => {
                    if !array_item {
                        output.push_str("null");
                    }
                }
                Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
                Value::String(value) => output.push_str(value),
                Value::Array(values) => {
                    for (index, value) in values.iter().enumerate().rev() {
                        pending.push((Some(UrlInput::Json(value)), true));
                        if index != 0 {
                            pending.push((None, false));
                        }
                    }
                }
                Value::Object(value) => {
                    if value.contains_key("toString") {
                        return Err(());
                    }
                    output.push_str("[object Object]");
                }
                Value::Number(_) => unreachable!("number handled above"),
            },
            UrlInput::Source(source, node) => match source.node(node).ok_or(())? {
                JsNode::Null => {
                    if !array_item {
                        output.push_str("null");
                    }
                }
                JsNode::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
                JsNode::String(value) => output.push_str(&value.to_well_formed()),
                JsNode::Array(values) => {
                    for (index, node) in values.iter().enumerate().rev() {
                        pending.push((Some(UrlInput::Source(source, *node)), true));
                        if index != 0 {
                            pending.push((None, false));
                        }
                    }
                }
                JsNode::Object(value) => {
                    if value.get("toString").is_some() {
                        return Err(());
                    }
                    output.push_str("[object Object]");
                }
                JsNode::Number(_) => unreachable!("number handled above"),
            },
        }
    }
    Ok(Cow::Owned(output))
}
fn endpoint(raw: Option<UrlInput<'_>>, default: &str, name: &str) -> Result<String, String> {
    let text = raw
        .map(url_string)
        .transpose()
        .map_err(|_| format!("{name} must be a valid URL"))?;
    let url = Url::parse(text.as_deref().unwrap_or(default))
        .map_err(|_| format!("{name} must be a valid URL"))?;
    let local = matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"));
    // WHATWG URL.search/hash are empty for a bare trailing '?' or '#'.
    if (url.scheme() != "https" && !(local && url.scheme() == "http"))
        || !url.username().is_empty()
        || url.password().is_some_and(|s| !s.is_empty())
        || url.query().is_some_and(|s| !s.is_empty())
        || url.fragment().is_some_and(|s| !s.is_empty())
    {
        return Err(format!(
            "{name} must use HTTPS (HTTP is allowed on loopback) with no credentials, query, or fragment"
        ));
    }
    Ok(url
        .as_str()
        .strip_suffix('/')
        .unwrap_or(url.as_str())
        .to_owned())
}

const OLLAMA_ENDPOINT_ERROR: &str =
    "Ollama must use a loopback base URL without a path, credentials, a query, or a fragment.";
pub fn validate_ollama_endpoint(raw: &str) -> Result<String, String> {
    let error = OLLAMA_ENDPOINT_ERROR;
    let mut url = Url::parse(raw).map_err(|_| error.to_owned())?;
    if !matches!(url.scheme(), "http" | "https")
        || !matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"))
        || !url.username().is_empty()
        || url.password().is_some_and(|s| !s.is_empty())
        || url.query().is_some_and(|s| !s.is_empty())
        || url.fragment().is_some_and(|s| !s.is_empty())
        || url.path() != "/"
    {
        return Err(error.to_owned());
    }
    if url.host_str() == Some("localhost") {
        url.set_host(Some("127.0.0.1"))
            .map_err(|_| error.to_owned())?;
    }
    Ok(url.origin().ascii_serialization())
}

pub fn validate_ollama_model(model: &str) -> Result<String, String> {
    static MODEL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_./:-]{0,199}$").unwrap());
    let lower = model.to_ascii_lowercase();
    if !MODEL.is_match(model)
        || model.contains("://")
        || lower.ends_with("-cloud")
        || lower.ends_with(":cloud")
    {
        return Err(
            "Ollama requires a valid local model tag; cloud models are not supported.".to_owned(),
        );
    }
    Ok(model.to_owned())
}

pub fn default_ollama_timeout_ms(model: &str) -> u64 {
    static TAG: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:latest|(?:4b|9b)(?:-(?:q[2-8]_(?:0|1|k(?:_[sml])?)|iq[1-4]_(?:xxs|xs|s|m|nl)|f16|bf16|f32))?)$").unwrap()
    });
    let canonical = model.strip_prefix("registry.ollama.ai/").unwrap_or(model);
    let canonical = canonical.strip_prefix("library/").unwrap_or(canonical);
    let (family, tag) = canonical.split_once(':').unwrap_or((canonical, "latest"));
    if !TAG.is_match(tag) {
        return 1500;
    }
    let lower = tag.to_ascii_lowercase();
    match family {
        "tev1" if lower == "latest" || lower.starts_with("4b") => 15000,
        "nimble" if lower == "latest" || lower.starts_with("9b") => 30000,
        _ => 1500,
    }
}

pub fn read_config(env: &Value, validate_all: bool, cwd: &Path) -> Result<RouterConfig, String> {
    read_config_inner(env, None, validate_all, cwd)
}
fn read_config_inner(
    env: &Value,
    source: Option<&JsDocument>,
    validate_all: bool,
    cwd: &Path,
) -> Result<RouterConfig, String> {
    let session_log_mode = match value(env, "AUTOROUTER_SESSION_LOG_MODE") {
        None => SessionLogMode::Metadata,
        Some(v) if v == "metadata" => SessionLogMode::Metadata,
        Some(v) if v == "prompts" => SessionLogMode::Prompts,
        _ => return Err("AUTOROUTER_SESSION_LOG_MODE must be metadata or prompts".to_owned()),
    };
    for key in ["AUTOROUTER_STATUSLINE", "AUTOROUTER_DEBUG"] {
        if value(env, key).is_some_and(|v| v != "0" && v != "1") {
            return Err(format!("{key} must be 0 or 1"));
        }
    }
    let evaluator = match enum_text(env, "AUTOROUTER_EVALUATOR", "ollama") {
        "ollama" => Evaluator::Ollama,
        "jev" => Evaluator::Jev,
        _ => return Err("AUTOROUTER_EVALUATOR must be jev or ollama".to_owned()),
    };
    let empty = Value::Null;
    let ollama_env = if evaluator == Evaluator::Ollama || validate_all {
        env
    } else {
        &empty
    };
    let jev_env = if evaluator == Evaluator::Jev || validate_all {
        env
    } else {
        &empty
    };
    let ollama_model = validate_ollama_model(enum_text(
        ollama_env,
        "AUTOROUTER_OLLAMA_MODEL",
        DEFAULT_OLLAMA_MODEL,
    ))?;
    if let Some(raw) = value(ollama_env, "AUTOROUTER_OLLAMA_TIMEOUT_MS")
        && !raw.is_number()
        && decimal(raw, true).is_none()
    {
        return Err("AUTOROUTER_OLLAMA_TIMEOUT_MS must be an integer between 0 and 30000 (0 disables the runtime deadline)".to_owned());
    }
    let ollama_keep_alive = nullish(ollama_env, "AUTOROUTER_OLLAMA_KEEP_ALIVE")
        .cloned()
        .unwrap_or_else(|| Value::String("5m".into()));
    static KEEP: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(?:0|[1-9][0-9]{0,3}(?:s|m|h))$").unwrap());
    if !KEEP.is_match(&regex_string(&ollama_keep_alive)) {
        return Err(
            "AUTOROUTER_OLLAMA_KEEP_ALIVE must be 0 or a positive duration such as 5m".to_owned(),
        );
    }
    let auth_mode = match enum_text(env, "AUTOROUTER_AUTH_MODE", "api-key") {
        "api-key" => AuthMode::ApiKey,
        "subscription" => AuthMode::Subscription,
        _ => return Err("AUTOROUTER_AUTH_MODE must be api-key or subscription".to_owned()),
    };
    let client_profile = match enum_text(env, "AUTOROUTER_CLIENT_PROFILE", "compatible") {
        "compatible" => ClientProfile::Compatible,
        "native" => ClientProfile::Native,
        "auto" => ClientProfile::Auto,
        _ => return Err("AUTOROUTER_CLIENT_PROFILE must be compatible, native or auto".to_owned()),
    };
    let models = Models {
        haiku: model_name(
            value(env, "AUTOROUTER_HAIKU_MODEL"),
            "claude-haiku-4-5-20251001",
            "AUTOROUTER_HAIKU_MODEL",
        )?,
        sonnet: model_name(
            value(env, "AUTOROUTER_SONNET_MODEL"),
            if client_profile == ClientProfile::Auto {
                "claude-sonnet-5-5"
            } else {
                "claude-sonnet-5"
            },
            "AUTOROUTER_SONNET_MODEL",
        )?,
        opus: model_name(
            value(env, "AUTOROUTER_OPUS_MODEL"),
            "claude-opus-5-5",
            "AUTOROUTER_OPUS_MODEL",
        )?,
    };
    if client_profile == ClientProfile::Auto {
        for (name, model) in [("SONNET", &models.sonnet), ("OPUS", &models.opus)] {
            if !crate::model_catalog::supports_auto_mode(model) {
                return Err(format!(
                    "AUTOROUTER_{name}_MODEL must be a known Auto-mode-capable Sonnet or Opus model for the auto profile"
                ));
            }
        }
    }
    let upstream = endpoint(
        url_input(env, source, "AUTOROUTER_UPSTREAM_URL", true),
        "https://api.anthropic.com",
        "AUTOROUTER_UPSTREAM_URL",
    )?;
    if auth_mode == AuthMode::Subscription && upstream != "https://api.anthropic.com" {
        return Err(
            "Subscription mode requires https://api.anthropic.com as AUTOROUTER_UPSTREAM_URL"
                .to_owned(),
        );
    }
    // Keep evaluation order aligned with readConfig: the first invalid setting
    // determines the public diagnostic, including check-all for inactive backends.
    let session_log_dir = parse_session_log_dir(value(env, "AUTOROUTER_SESSION_LOG_DIR"), cwd)?;
    let stop_hook_block_cap = value(env, "CLAUDE_CODE_STOP_HOOK_BLOCK_CAP")
        .map(|raw| parse_stop_hook_block_cap(raw, "CLAUDE_CODE_STOP_HOOK_BLOCK_CAP"))
        .transpose()?;
    let jev_endpoint = endpoint(
        url_input(
            jev_env,
            source,
            "AUTOROUTER_JEV_URL",
            evaluator == Evaluator::Jev || validate_all,
        ),
        "https://api.typesafe.ai/v1/systemone",
        "AUTOROUTER_JEV_URL",
    )?;
    let jev_model = model_name(
        value(jev_env, "AUTOROUTER_JEV_MODEL"),
        "jev-latest",
        "AUTOROUTER_JEV_MODEL",
    )?;
    let ollama_url = url_input(
        ollama_env,
        source,
        "AUTOROUTER_OLLAMA_URL",
        evaluator == Evaluator::Ollama || validate_all,
    )
    .map(url_string)
    .transpose()
    .map_err(|_| OLLAMA_ENDPOINT_ERROR.to_owned())?;
    let ollama_endpoint =
        validate_ollama_endpoint(ollama_url.as_deref().unwrap_or("http://127.0.0.1:11434"))?;
    let ollama_timeout_ms = number(
        ollama_env,
        "AUTOROUTER_OLLAMA_TIMEOUT_MS",
        default_ollama_timeout_ms(&ollama_model) as f64,
        0.,
        30000.,
        true,
    )? as u64;
    let port = number(env, "AUTOROUTER_PORT", 8787., 0., 65535., true)? as u16;
    let jev_timeout_ms = number(
        jev_env,
        "AUTOROUTER_JEV_TIMEOUT_MS",
        1500.,
        1.,
        10000.,
        true,
    )? as u64;
    let token_count_timeout_ms = number(
        env,
        "AUTOROUTER_TOKEN_COUNT_TIMEOUT_MS",
        1500.,
        1.,
        10000.,
        true,
    )? as u64;
    let min_confidence = number(jev_env, "AUTOROUTER_MIN_CONFIDENCE", 0.75, 0., 1., false)?;
    let mut config = RouterConfig {
        evaluator,
        auth_mode,
        client_profile,
        session_log_dir,
        session_log_mode,
        stop_hook_block_cap,
        anthropic_key: if auth_mode == AuthMode::ApiKey {
            text(env, "ANTHROPIC_API_KEY")
        } else {
            None
        },
        jev_key: text(env, "TYPESAFE_API_KEY"),
        local_token: text(env, "AUTOROUTER_TOKEN"),
        source_credentials: SourceCredentials::default(),
        upstream,
        jev_endpoint,
        jev_model,
        ollama_endpoint,
        ollama_model,
        ollama_timeout_ms,
        ollama_state_chars: 3000,
        ollama_keep_alive,
        models,
        exact_models: BTreeMap::new(),
        exact_jev_model: None,
        port,
        jev_timeout_ms,
        token_count_timeout_ms,
        min_confidence,
        state_chars: 12000,
        max_body_bytes: 32 * 1024 * 1024,
        cache_entries: 1000,
        cache_ttl_ms: 5 * 60 * 1000,
        turn_ttl_ms: 30 * 60 * 1000,
        turn_entries: None,
        upstream_timeout_ms: 10 * 60 * 1000,
    };
    for key in [
        Credential::Anthropic,
        Credential::Jev,
        Credential::LocalToken,
    ] {
        // Match the source's initial capture, not the caller's later auth mode.
        if key == Credential::Anthropic && auth_mode == AuthMode::Subscription {
            continue;
        }
        if let Some(source) = source {
            if let Some(node) = source.get(source.root(), key.env_name()) {
                config.replace_credential_document(key, source, node)?;
            }
        } else {
            config.replace_credential_value(key, env.get(key.env_name()));
        }
    }
    Ok(config)
}

/// Saved JSON permits UTF-16 strings that cannot be represented in an OS
/// environment. Validation of controls, enums and catalog IDs is unaffected by
/// replacement projection; unconstrained model identities retain their source.
pub fn read_config_document(
    env: &JsDocument,
    validate_all: bool,
    cwd: &Path,
) -> Result<RouterConfig, String> {
    let mut config = read_config_inner(
        &env.to_serde_observation_lossy(),
        Some(env),
        validate_all,
        cwd,
    )?;
    for (tier, key) in [
        ("haiku", "AUTOROUTER_HAIKU_MODEL"),
        ("sonnet", "AUTOROUTER_SONNET_MODEL"),
        ("opus", "AUTOROUTER_OPUS_MODEL"),
    ] {
        if let Some(value) = env.get(env.root(), key).and_then(|node| env.string(node))
            && value.to_scalar().is_none()
        {
            config.exact_models.insert(tier.into(), value.clone());
        }
    }
    if (config.evaluator == Evaluator::Jev || validate_all)
        && let Some(value) = env
            .get(env.root(), "AUTOROUTER_JEV_MODEL")
            .and_then(|node| env.string(node))
        && value.to_scalar().is_none()
    {
        config.exact_jev_model = Some(value.clone());
    }
    Ok(config)
}

/// POSIX path.resolve on UTF-16 code units, before conversion at the filesystem
/// boundary. CLI JSON reports and saved setup settings retain these units.
pub fn resolve_js_path(cwd: &Path, path: &JsString) -> JsString {
    let mut units = if path.units().first() == Some(&u16::from(b'/')) {
        Vec::new()
    } else {
        cwd.to_string_lossy()
            .encode_utf16()
            .chain([u16::from(b'/')])
            .collect()
    };
    units.extend_from_slice(path.units());
    let mut parts: Vec<&[u16]> = Vec::new();
    for part in units.split(|unit| *unit == u16::from(b'/')) {
        if part.is_empty() || part == [u16::from(b'.')] {
            continue;
        }
        if part == [u16::from(b'.'), u16::from(b'.')] {
            parts.pop();
        } else {
            parts.push(part);
        }
    }
    let mut resolved = vec![u16::from(b'/')];
    for (index, part) in parts.into_iter().enumerate() {
        if index > 0 {
            resolved.push(u16::from(b'/'));
        }
        resolved.extend_from_slice(part);
    }
    JsString::from_utf16(resolved)
}

pub fn require_keys(config: &RouterConfig) -> Result<(), String> {
    for (key, active) in [
        (Credential::Jev, config.evaluator == Evaluator::Jev),
        (Credential::Anthropic, config.auth_mode == AuthMode::ApiKey),
    ] {
        if !active {
            continue;
        }
        let missing = match config.credential_value(key) {
            None => true,
            Some(CredentialValue::Text(value)) => js_trim(value).is_empty(),
            Some(CredentialValue::Source(document)) => match document.node(document.root()) {
                Some(JsNode::Null) => true,
                Some(JsNode::String(value)) => value.units().iter().all(|unit| {
                    matches!(*unit,
                    0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a |
                    0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
                }),
                _ => return Err("value?.trim is not a function".to_owned()),
            },
        };
        if missing {
            return Err(format!("Set {} before starting the router", key.env_name()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(env: Value) -> RouterConfig {
        read_config(&env, false, Path::new("/test/project")).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn defaults_and_selected_backend_validation() {
        let c = config(json!({}));
        assert!(c.evaluator == Evaluator::Ollama && c.auth_mode == AuthMode::ApiKey);
        assert!(c.session_log_dir.is_none());
        assert_eq!(c.ollama_timeout_ms, 30000);
        assert_eq!(c.models.sonnet, "claude-sonnet-5");
        let env = json!({"AUTOROUTER_EVALUATOR":"jev", "AUTOROUTER_OLLAMA_TIMEOUT_MS":"", "AUTOROUTER_OLLAMA_URL":"private-invalid"});
        assert!(read_config(&env, false, Path::new("/")).is_ok());
        assert_eq!(
            read_config(&env, true, Path::new("/")).err().unwrap(),
            "AUTOROUTER_OLLAMA_TIMEOUT_MS must be an integer between 0 and 30000 (0 disables the runtime deadline)"
        );
    }

    #[test]
    fn deadlines_recognize_only_official_tags() {
        for prefix in [
            "",
            "library/",
            "registry.ollama.ai/",
            "registry.ollama.ai/library/",
        ] {
            for (tag, expected) in [
                ("tev1", 15000),
                ("tev1:latest", 15000),
                ("tev1:4b-q4_K_M", 15000),
                ("tev1:0.8b", 1500),
                ("nimble:9b-q8_0", 30000),
                ("nimble:9b-f16", 30000),
            ] {
                assert_eq!(
                    default_ollama_timeout_ms(&format!("{prefix}{tag}")),
                    expected
                );
            }
        }
        for tag in [
            "team/nimble",
            "team/tev1:4b",
            "tev1:9b",
            "tev1:40b",
            "nimble:4b",
            "nimble:9b-custom",
            "nimble:9b-q4_K_M-extra",
        ] {
            assert_eq!(default_ollama_timeout_ms(tag), 1500);
        }
    }

    #[test]
    fn numeric_semantics_preserve_zero_and_reject_coercion() {
        for raw in [
            json!(null),
            json!(false),
            json!(true),
            json!([]),
            json!({}),
            json!(""),
            json!(" "),
            json!("1e2"),
            json!("0x10"),
            json!("+2"),
            json!("1.5"),
            json!("9007199254740992"),
        ] {
            assert!(parse_stop_hook_block_cap(&raw, "cap").is_err());
        }
        assert_eq!(
            parse_stop_hook_block_cap(&json!("\u{feff}002\u{a0}"), "cap").unwrap(),
            2
        );
        assert!(parse_stop_hook_block_cap(&json!("\u{85}2"), "cap").is_err());
        assert_eq!(
            config(json!({"AUTOROUTER_OLLAMA_TIMEOUT_MS":"0"})).ollama_timeout_ms,
            0
        );
        assert_eq!(
            config(json!({"AUTOROUTER_EVALUATOR":"jev", "AUTOROUTER_MIN_CONFIDENCE":".5"}))
                .min_confidence,
            0.5
        );
    }

    #[test]
    fn endpoints_are_local_and_diagnostics_do_not_echo_values() {
        assert_eq!(
            validate_ollama_endpoint("http://localhost:11434").unwrap(),
            "http://127.0.0.1:11434"
        );
        assert_eq!(
            validate_ollama_endpoint("http://[::1]:11434").unwrap(),
            "http://[::1]:11434"
        );
        for value in [
            "https://private:secret@example.test",
            "http://127.0.0.1/path",
            "http://127.0.0.1/?secret",
            "http://127.0.0.1/#secret",
        ] {
            let error = validate_ollama_endpoint(value).unwrap_err();
            assert!(!error.contains("secret") && !error.contains("private"));
        }
        assert!(validate_ollama_model("nimble:cloud").is_err());
        assert!(validate_ollama_model("nimble-CLOUD").is_err());
    }

    #[test]
    fn paths_modes_profiles_and_required_credentials() {
        assert_eq!(
            config(json!({"AUTOROUTER_SESSION_LOG_DIR":"../logs"}))
                .session_log_dir
                .as_deref(),
            Some("/test/logs")
        );
        assert!(
            read_config(
                &json!({"AUTOROUTER_SESSION_LOG_MODE":null}),
                false,
                Path::new("/")
            )
            .is_err()
        );
        assert_eq!(
            config(json!({"AUTOROUTER_CLIENT_PROFILE":"auto"}))
                .models
                .sonnet,
            "claude-sonnet-5-5"
        );
        assert!(
            read_config(
                &json!({"AUTOROUTER_CLIENT_PROFILE":"auto", "AUTOROUTER_OPUS_MODEL":"team/opus"}),
                false,
                Path::new("/")
            )
            .is_err()
        );
        assert!(require_keys(&config(json!({"AUTOROUTER_AUTH_MODE":"subscription"}))).is_ok());
        assert_eq!(
            require_keys(&config(json!({}))).unwrap_err(),
            "Set ANTHROPIC_API_KEY before starting the router"
        );
    }

    #[test]
    fn raw_url_documents_coerce_before_lossy_projection() {
        let keys = [
            "AUTOROUTER_UPSTREAM_URL",
            "AUTOROUTER_JEV_URL",
            "AUTOROUTER_OLLAMA_URL",
        ];
        let defaults = [
            "https://api.anthropic.com",
            "https://api.typesafe.ai/v1/systemone",
            "http://127.0.0.1:11434",
        ];
        // None expected => generic URL-conversion error. Empty expected => default.
        // All literal goldens were captured from the frozen Node oracle.
        let cases = [
            (None, Some("")),
            (Some("null"), Some("")),
            (Some("1e400"), None),
            (Some("-1e400"), None),
            (
                Some(r#"["https://example.test/path",1e400,-1e400]"#),
                Some("https://example.test/path,Infinity,-Infinity"),
            ),
            (
                Some(r#"["https://example.test/path",[1e400,null,-1e400]]"#),
                Some("https://example.test/path,Infinity,,-Infinity"),
            ),
            (
                Some(r#""https://example.test/path\ud800""#),
                Some("https://example.test/path%EF%BF%BD"),
            ),
            (
                Some(r#"["https://example.test/path\udfff"]"#),
                Some("https://example.test/path%EF%BF%BD"),
            ),
            (
                Some(r#"["https://example.test/path\ud83d\ude00"]"#),
                Some("https://example.test/path%F0%9F%98%80"),
            ),
            (
                Some(r#"["https://example.test/path",[{"toString":null}]]"#),
                None,
            ),
            (
                Some(r#"["https://example.test/path",[{"valueOf":null}]]"#),
                Some("https://example.test/path,[object%20Object]"),
            ),
            (
                Some(r#"["https://example.test/path",[{"toString":null,"valueOf":null}]]"#),
                None,
            ),
        ];
        let mut checked = 0;
        for (index, key) in keys.into_iter().enumerate() {
            let backend = if index == 1 { "jev" } else { "ollama" };
            let error = if index == 2 {
                "Ollama must use a loopback base URL without a path, credentials, a query, or a fragment.".to_owned()
            } else {
                format!("{key} must be a valid URL")
            };
            for (raw, expected) in cases.into_iter().chain(
                (index == 2).then_some((Some(r#"["http://127.0.0.1:11434/path\ud800"]"#), None)),
            ) {
                let member = raw
                    .map(|value| format!(",\"{key}\":{value}"))
                    .unwrap_or_default();
                let bytes = format!("{{\"AUTOROUTER_EVALUATOR\":\"{backend}\"{member}}}");
                let document = JsDocument::parse(bytes.as_bytes()).unwrap();
                let actual =
                    read_config_document(&document, false, Path::new("/synthetic")).map(|config| {
                        match index {
                            0 => config.upstream,
                            1 => config.jev_endpoint,
                            _ => config.ollama_endpoint,
                        }
                    });
                let expected = expected
                    .filter(|text| index != 2 || text.is_empty())
                    .map(|text| {
                        (if text.is_empty() {
                            defaults[index]
                        } else {
                            text
                        })
                        .to_owned()
                    })
                    .ok_or_else(|| error.clone());
                assert_eq!(actual, expected, "key {key}, raw {raw:?}");
                checked += 1;
            }
        }
        assert_eq!(checked, 37);
    }

    #[test]
    fn document_url_conversion_keeps_backend_masks_and_validation_order() {
        let mut checked = 0;
        for (backend, key, error) in [
            (
                "jev",
                "AUTOROUTER_OLLAMA_URL",
                "Ollama must use a loopback base URL without a path, credentials, a query, or a fragment.",
            ),
            (
                "ollama",
                "AUTOROUTER_JEV_URL",
                "AUTOROUTER_JEV_URL must be a valid URL",
            ),
        ] {
            let bytes = format!(
                r#"{{"AUTOROUTER_EVALUATOR":"{backend}","{key}":["https://example.test/path",{{"toString":null}}]}}"#
            );
            let document = JsDocument::parse(bytes.as_bytes()).unwrap();
            for validate_all in [false, true] {
                let actual = read_config_document(&document, validate_all, Path::new("/synthetic"));
                if validate_all {
                    assert_eq!(actual.err().as_deref(), Some(error), "{backend}");
                } else {
                    let config = actual.unwrap_or_else(|error| panic!("{backend}: {error}"));
                    assert_eq!(config.jev_endpoint, "https://api.typesafe.ai/v1/systemone");
                    assert_eq!(config.ollama_endpoint, "http://127.0.0.1:11434");
                }
                checked += 1;
            }
        }
        for (raw, error) in [
            (
                r#"{"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_OLLAMA_MODEL":"synthetic bad model","AUTOROUTER_UPSTREAM_URL":["https://example.test/path",{"toString":null}]}"#,
                "Ollama requires a valid local model tag; cloud models are not supported.",
            ),
            (
                r#"{"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_UPSTREAM_URL":["https://example.test/path",{"toString":null}],"AUTOROUTER_PORT":"bad"}"#,
                "AUTOROUTER_UPSTREAM_URL must be a valid URL",
            ),
        ] {
            let document = JsDocument::parse(raw.as_bytes()).unwrap();
            assert_eq!(
                read_config_document(&document, false, Path::new("/synthetic"))
                    .err()
                    .as_deref(),
                Some(error)
            );
            checked += 1;
        }
        assert_eq!(checked, 6);
    }

    #[test]
    fn credentials_keep_raw_presence_requirement_activation_and_safe_failures() {
        let values = [
            None,
            Some(json!(null)),
            Some(json!("synthetic-credential-value-123456789")),
            Some(json!("")),
            Some(json!(" \t\n")),
            Some(json!(false)),
            Some(json!(0)),
            Some(json!(42)),
            Some(json!(true)),
            Some(json!(["synthetic-credential-value-123456789"])),
            Some(json!([])),
            Some(json!([["synthetic-credential-value-123456789"]])),
            Some(json!({"synthetic":"credential-input"})),
            Some(json!({"trim":null,"synthetic":"credential-input"})),
        ];
        let fields = [
            ("ANTHROPIC_API_KEY", "anthropicKey"),
            ("TYPESAFE_API_KEY", "jevKey"),
            ("AUTOROUTER_TOKEN", "localToken"),
        ];
        let mut checked = 0;
        for (target, _) in fields {
            for mode in ["api-key", "subscription"] {
                for evaluator in ["jev", "ollama"] {
                    for raw in &values {
                        let mut env = json!({"ANTHROPIC_API_KEY":"synthetic-anthropic-valid",
                        "TYPESAFE_API_KEY":"synthetic-jev-valid", "AUTOROUTER_TOKEN":"synthetic-local-token-123456789",
                        "AUTOROUTER_AUTH_MODE":mode,"AUTOROUTER_EVALUATOR":evaluator});
                        if let Some(value) = raw {
                            env[target] = value.clone();
                        } else {
                            env.as_object_mut().unwrap().remove(target);
                        }
                        let config = read_config(&env, false, Path::new("/synthetic"))
                            .unwrap_or_else(|error| {
                                panic!("Unexpected admission rejection: {error}")
                            });
                        let encoded = serde_json::to_string(&config).unwrap();
                        let observed: Value = serde_json::from_str(&encoded).unwrap();
                        for (key, field) in fields {
                            let expected = if key == "ANTHROPIC_API_KEY" && mode == "subscription" {
                                None
                            } else {
                                env.get(key)
                            };
                            assert_eq!(
                                observed.get(field),
                                expected,
                                "{target}/{mode}/{evaluator}"
                            );
                            assert_eq!(
                                encoded.matches(&format!("\"{field}\":")).count(),
                                usize::from(expected.is_some())
                            );
                        }
                        let required = (target == "ANTHROPIC_API_KEY" && mode == "api-key")
                            || (target == "TYPESAFE_API_KEY" && evaluator == "jev");
                        let expected = if !required {
                            Ok(())
                        } else {
                            match raw {
                                None | Some(Value::Null) => {
                                    Err(format!("Set {target} before starting the router"))
                                }
                                Some(Value::String(value)) if value.trim().is_empty() => {
                                    Err(format!("Set {target} before starting the router"))
                                }
                                Some(Value::String(_)) => Ok(()),
                                _ => Err("value?.trim is not a function".to_owned()),
                            }
                        };
                        let actual = require_keys(&config);
                        if let Err(error) = &actual {
                            assert!(!error.contains("synthetic-"));
                        }
                        assert_eq!(actual, expected, "{target}/{mode}/{evaluator}");
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, 168); // Two observed contracts per original paired input.
    }

    #[test]
    fn document_credentials_keep_source_types_and_exact_json_before_requirement_checks() {
        // Original outcomes: source-supplement's nine forms over twelve profiles.
        let cases = [
            (None, None, 0),
            (Some("null"), Some("null"), 0),
            (Some("1e400"), Some("null"), 1),
            (Some("-1e400"), Some("null"), 1),
            (
                Some(r#""synthetic-credential-\ud800""#),
                Some(r#""synthetic-credential-\ud800""#),
                2,
            ),
            (
                Some(r#""synthetic-credential-\ud83d\ude00""#),
                Some("\"synthetic-credential-😀\""),
                2,
            ),
            (Some("[[1e400,null,-1e400]]"), Some("[[null,null,null]]"), 1),
            (
                Some(r#"[["synthetic-credential-value-123456789"]]"#),
                Some(r#"[["synthetic-credential-value-123456789"]]"#),
                1,
            ),
            (Some(r#"{"trim":null}"#), Some(r#"{"trim":null}"#), 1),
        ];
        let fields = [
            ("ANTHROPIC_API_KEY", "anthropicKey"),
            ("TYPESAFE_API_KEY", "jevKey"),
            ("AUTOROUTER_TOKEN", "localToken"),
        ];
        let mut checked = 0;
        for (target, _) in fields {
            for mode in ["api-key", "subscription"] {
                for evaluator in ["jev", "ollama"] {
                    for (raw, serialized, kind) in cases {
                        let mut members = vec![
                            format!(r#""AUTOROUTER_AUTH_MODE":"{mode}""#),
                            format!(r#""AUTOROUTER_EVALUATOR":"{evaluator}""#),
                        ];
                        for (key, _) in fields {
                            if key == target {
                                if let Some(raw) = raw {
                                    members.push(format!("\"{key}\":{raw}"));
                                }
                            } else {
                                members.push(format!(
                                    r#""{key}":"synthetic-credential-value-123456789""#
                                ));
                            }
                        }
                        let input = format!("{{{}}}", members.join(","));
                        let document = JsDocument::parse(input.as_bytes()).unwrap();
                        let config =
                            read_config_document(&document, false, Path::new("/synthetic"))
                                .unwrap();
                        let before = serde_json::to_string(&config).unwrap();
                        let observed = JsDocument::parse(before.as_bytes()).unwrap();
                        for (key, field) in fields {
                            let expected = if key == "ANTHROPIC_API_KEY" && mode == "subscription" {
                                None
                            } else if key == target {
                                serialized.map(str::to_owned)
                            } else {
                                Some(r#""synthetic-credential-value-123456789""#.to_owned())
                            };
                            let actual = observed
                                .get(observed.root(), field)
                                .map(|node| observed.stringify_node(node));
                            assert_eq!(actual, expected, "{target}/{mode}/{evaluator}/{raw:?}");
                            assert_eq!(
                                before.matches(&format!("\"{field}\":")).count(),
                                usize::from(expected.is_some())
                            );
                        }
                        let required = (target == "ANTHROPIC_API_KEY" && mode == "api-key")
                            || (target == "TYPESAFE_API_KEY" && evaluator == "jev");
                        let expected = if !required || kind == 2 {
                            Ok(())
                        } else if kind == 1 {
                            Err("value?.trim is not a function".to_owned())
                        } else {
                            Err(format!("Set {target} before starting the router"))
                        };
                        assert_eq!(
                            require_keys(&config),
                            expected,
                            "{target}/{mode}/{evaluator}/{raw:?}"
                        );
                        assert_eq!(serde_json::to_string(&config).unwrap(), before);
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, 108);
    }

    #[test]
    fn credential_replacement_and_mode_changes_do_not_resurrect_captured_values() {
        for key in [
            Credential::Anthropic,
            Credential::Jev,
            Credential::LocalToken,
        ] {
            let mut config = config(
                json!({"AUTOROUTER_EVALUATOR":"jev","ANTHROPIC_API_KEY":"synthetic-valid","TYPESAFE_API_KEY":"synthetic-valid"}),
            );
            config.replace_credential_value(key, Some(&json!(false)));
            assert_eq!(config.credential_json(key).as_deref(), Some("false"));
            config.replace_credential(key, Some("synthetic-new".into()));
            assert_eq!(
                config.credential_json(key).as_deref(),
                Some(r#""synthetic-new""#)
            );
            config.replace_credential(key, None);
            assert!(config.credential_json(key).is_none());
            config.replace_credential_value(key, Some(&Value::Null));
            assert_eq!(config.credential_json(key).as_deref(), Some("null"));
            config.replace_credential(key, Some(String::new()));
            assert_eq!(config.credential_json(key).as_deref(), Some(r#""""#));
            config.replace_credential_value(key, None);
            assert!(config.credential_json(key).is_none());
            assert!(config.source_credentials.slots.iter().all(Option::is_none));
        }
        let mut api = config(
            json!({"AUTOROUTER_EVALUATOR":"ollama","ANTHROPIC_API_KEY":false,"TYPESAFE_API_KEY":false}),
        );
        api.auth_mode = AuthMode::Subscription;
        assert_eq!(
            api.credential_json(Credential::Anthropic).as_deref(),
            Some("false")
        );
        assert!(require_keys(&api).is_ok());
        api.auth_mode = AuthMode::ApiKey;
        assert_eq!(
            require_keys(&api).unwrap_err(),
            "value?.trim is not a function"
        );
        api.auth_mode = AuthMode::Subscription;
        api.evaluator = Evaluator::Jev;
        assert_eq!(
            require_keys(&api).unwrap_err(),
            "value?.trim is not a function"
        );
        let mut subscription =
            config(json!({"AUTOROUTER_AUTH_MODE":"subscription","ANTHROPIC_API_KEY":false}));
        subscription.auth_mode = AuthMode::ApiKey;
        assert!(
            subscription
                .credential_json(Credential::Anthropic)
                .is_none()
        );
        assert_eq!(
            require_keys(&subscription).unwrap_err(),
            "Set ANTHROPIC_API_KEY before starting the router"
        );
    }

    #[test]
    fn credential_requirements_preserve_first_active_key_failure() {
        // Ten simultaneous-failure controls from the frozen oracle.
        let cases = [
            (
                r#"{"AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_AUTH_MODE":"api-key"}"#,
                Some("Set TYPESAFE_API_KEY before starting the router"),
            ),
            (
                r#"{"TYPESAFE_API_KEY":null,"ANTHROPIC_API_KEY":false,"AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_AUTH_MODE":"api-key"}"#,
                Some("Set TYPESAFE_API_KEY before starting the router"),
            ),
            (
                r#"{"TYPESAFE_API_KEY":false,"ANTHROPIC_API_KEY":"synthetic-anthropic-valid","AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_AUTH_MODE":"api-key"}"#,
                Some("value?.trim is not a function"),
            ),
            (
                r#"{"TYPESAFE_API_KEY":"synthetic-jev-valid","ANTHROPIC_API_KEY":false,"AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_AUTH_MODE":"api-key"}"#,
                Some("value?.trim is not a function"),
            ),
            (
                r#"{"TYPESAFE_API_KEY":"","ANTHROPIC_API_KEY":false,"AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_AUTH_MODE":"api-key"}"#,
                Some("Set TYPESAFE_API_KEY before starting the router"),
            ),
            (
                r#"{"TYPESAFE_API_KEY":false,"ANTHROPIC_API_KEY":"","AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_AUTH_MODE":"api-key"}"#,
                Some("value?.trim is not a function"),
            ),
            (
                r#"{"TYPESAFE_API_KEY":"synthetic-jev-valid","ANTHROPIC_API_KEY":"synthetic-anthropic-valid","AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_AUTH_MODE":"api-key"}"#,
                None,
            ),
            (
                r#"{"TYPESAFE_API_KEY":false,"ANTHROPIC_API_KEY":"synthetic-anthropic-valid","AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_AUTH_MODE":"api-key"}"#,
                None,
            ),
            (
                r#"{"TYPESAFE_API_KEY":false,"ANTHROPIC_API_KEY":false,"AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_AUTH_MODE":"subscription"}"#,
                Some("value?.trim is not a function"),
            ),
            (
                r#"{"TYPESAFE_API_KEY":false,"ANTHROPIC_API_KEY":false,"AUTOROUTER_EVALUATOR":"ollama","AUTOROUTER_AUTH_MODE":"subscription"}"#,
                None,
            ),
        ];
        for (raw, expected) in cases {
            let env: Value = serde_json::from_str(raw).unwrap();
            let settings = config(env);
            let before = serde_json::to_string(&settings).unwrap();
            assert_eq!(require_keys(&settings).err().as_deref(), expected);
            assert_eq!(serde_json::to_string(&settings).unwrap(), before);
        }
    }

    #[test]
    fn credential_text_serialization_is_exact_and_value_observation_is_fallible() {
        let source = JsDocument::parse(br#"{"AUTOROUTER_EVALUATOR":"jev","TYPESAFE_API_KEY":"synthetic-\ud800","unrelated":"synthetic-unrelated-input"}"#).unwrap();
        let mut config = read_config_document(&source, false, Path::new("/synthetic")).unwrap();
        let text = serde_json::to_string(&config).unwrap();
        assert!(text.contains(r#""jevKey":"synthetic-\ud800""#));
        assert!(!text.contains("synthetic-unrelated-input"));
        assert!(!text.contains('\u{fffd}'));
        assert!(serde_json::to_value(&config).is_err());
        let before = config.credential_json(Credential::Jev);
        assert!(
            config
                .replace_credential_document(Credential::Jev, &source, usize::MAX)
                .is_err()
        );
        assert_eq!(config.credential_json(Credential::Jev), before);
        config.jev_key = Some("synthetic-typed-override".into());
        assert_eq!(
            config.credential_json(Credential::Jev).as_deref(),
            Some(r#""synthetic-typed-override""#)
        );
        config.replace_credential(Credential::Jev, None);
        assert!(config.credential_json(Credential::Jev).is_none());
        config.jev_key = Some(String::new());
        assert_eq!(
            config.credential_json(Credential::Jev).as_deref(),
            Some(r#""""#)
        );
    }
}
