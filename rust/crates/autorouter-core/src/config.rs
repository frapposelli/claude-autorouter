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
    /// Exact accepted keep_alive/path source. Typed fields remain display and
    /// compatibility surfaces; replacement methods erase old retained values.
    pub source_values: SourceConfigValues,
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

// Exact noncredential configuration state has no diagnostic/Debug exposure.
#[derive(Clone, Default)]
pub struct SourceConfigValues {
    keep_alive: Option<CapturedConfigValue>,
    session_log_dir: Option<CapturedConfigText>,
}
#[derive(Clone)]
struct CapturedConfigValue {
    document: Arc<JsDocument>,
    // Bounded metadata only, used solely to detect typed-field replacement.
    projection: Value,
}
impl CapturedConfigValue {
    fn new(document: JsDocument, original: Option<&Value>) -> Self {
        // Preserve the ordinary Value API's number representations when its
        // existing tree can be cloned within the metadata-depth bound.
        let projection = match original.filter(|value| bounded_metadata_value(value)) {
            Some(value) => value.clone(),
            None => document.to_serde_observation_lossy(),
        };
        Self {
            document: Arc::new(document),
            projection,
        }
    }
}
// This chooses a display-copy strategy only; it never rejects an input or
// limits exact arena ownership.64 is the existing metadata projection bound.
fn bounded_metadata_value(value: &Value) -> bool {
    let mut pending = vec![(value, 0usize)];
    while let Some((value, depth)) = pending.pop() {
        if depth > 64 {
            return false;
        }
        match value {
            Value::Array(values) => pending.extend(values.iter().map(|value| (value, depth + 1))),
            Value::Object(values) => {
                pending.extend(values.values().map(|value| (value, depth + 1)))
            }
            _ => {}
        }
    }
    true
}
#[derive(Clone)]
struct CapturedConfigText {
    value: JsString,
    projection: String,
}
// serde Value cannot represent lone UTF-16, and may reject a deep RawValue.
// JSON-text serialization stays exact and never builds a recursive deep Value.
enum ConfigOutput<'a> {
    Json(&'a Value),
    Source(&'a JsDocument),
    Text(&'a str),
    Utf16(&'a JsString),
}
impl Serialize for ConfigOutput<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let json = match self {
            Self::Text(value) => return serializer.serialize_str(value),
            Self::Json(value) => JsDocument::from_serde_value(value).stringify(),
            Self::Source(document) => document.stringify(),
            Self::Utf16(value) => value.stringify(),
        };
        let raw = serde_json::value::RawValue::from_string(json)
            .map_err(|_| S::Error::custom("Configuration JSON serialization failed"))?;
        raw.serialize(serializer)
    }
}
struct ConfigModels<'a>(&'a RouterConfig);
impl Serialize for ConfigModels<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        for (tier, model) in [
            ("haiku", &self.0.models.haiku),
            ("sonnet", &self.0.models.sonnet),
            ("opus", &self.0.models.opus),
        ] {
            // Public exact overrides retain the same unconditional authority as
            // routing. Explicit replacement clears them; typed edits do not.
            let value = self
                .0
                .exact_models
                .get(tier)
                .map(ConfigOutput::Utf16)
                .unwrap_or(ConfigOutput::Text(model));
            map.serialize_entry(tier, &value)?;
        }
        map.end()
    }
}
// Taking ownership of an embedding's Value must not recursively drop a deep
// input or a directly edited old typed field. The arena itself drops flat.
fn drop_value_iteratively(value: Value) {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => pending.extend(values.into_values()),
            _ => {}
        }
    }
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
    /// Copy evaluator settings without replacing routing, cache or task owners.
    /// Like direct field edits, this does not rerun configuration validation.
    pub fn copy_evaluator_settings_from(&mut self, source: &RouterConfig) {
        let keep_alive = match source
            .source_values
            .keep_alive
            .as_ref()
            .filter(|capture| capture.projection == source.ollama_keep_alive)
        {
            Some(capture) => capture.clone(),
            None => CapturedConfigValue::new(
                JsDocument::from_serde_value(&source.ollama_keep_alive),
                Some(&source.ollama_keep_alive),
            ),
        };
        self.evaluator = source.evaluator;
        self.jev_endpoint.clone_from(&source.jev_endpoint);
        self.jev_model.clone_from(&source.jev_model);
        self.exact_jev_model.clone_from(&source.exact_jev_model);
        self.jev_key.clone_from(&source.jev_key);
        let jev = Credential::Jev.index();
        self.source_credentials.slots[jev].clone_from(&source.source_credentials.slots[jev]);
        self.jev_timeout_ms = source.jev_timeout_ms;
        self.min_confidence = source.min_confidence;
        self.state_chars = source.state_chars;
        self.ollama_endpoint.clone_from(&source.ollama_endpoint);
        self.ollama_model.clone_from(&source.ollama_model);
        self.ollama_timeout_ms = source.ollama_timeout_ms;
        self.ollama_state_chars = source.ollama_state_chars;
        self.install_ollama_keep_alive(keep_alive);
    }

    /// Effective exact source when the captured bounded display is unchanged.
    /// None means the current public Value is authoritative, not absent.
    pub fn ollama_keep_alive_document(&self) -> Option<&JsDocument> {
        self.source_values
            .keep_alive
            .as_ref()
            .filter(|capture| capture.projection == self.ollama_keep_alive)
            .map(|capture| capture.document.as_ref())
    }
    /// Canonical JSON for request/cache use; never serialize the display
    /// projection when an effective exact source exists.
    pub fn ollama_keep_alive_json(&self) -> String {
        match self.ollama_keep_alive_document() {
            Some(document) => document.stringify(),
            None => JsDocument::from_serde_value(&self.ollama_keep_alive).stringify(),
        }
    }
    pub fn ollama_keep_alive_is_string(&self) -> bool {
        match self.ollama_keep_alive_document() {
            Some(document) => matches!(document.node(document.root()), Some(JsNode::String(_))),
            None => self.ollama_keep_alive.is_string(),
        }
    }
    pub fn ollama_keep_alive_is_nullish(&self) -> bool {
        match self.ollama_keep_alive_document() {
            Some(document) => matches!(document.node(document.root()), Some(JsNode::Null)),
            None => self.ollama_keep_alive.is_null(),
        }
    }
    fn install_ollama_keep_alive(&mut self, capture: CapturedConfigValue) {
        let old = std::mem::replace(&mut self.ollama_keep_alive, capture.projection.clone());
        self.source_values.keep_alive = Some(capture);
        drop_value_iteratively(old);
    }
    /// Replace the effective value, erasing all old source state. Even a deep
    /// Value is copied into a flat arena before its owned input is dismantled.
    /// Like a direct config-field edit, this does not rerun read_config validation.
    pub fn replace_ollama_keep_alive(&mut self, value: Value) {
        let document = JsDocument::from_serde_value(&value);
        self.install_ollama_keep_alive(CapturedConfigValue::new(document, Some(&value)));
        drop_value_iteratively(value);
    }
    /// Invalid node IDs leave the old value untouched. Own only the selected
    /// subtree, never the complete caller environment.
    pub fn replace_ollama_keep_alive_document(
        &mut self,
        document: &JsDocument,
        node: NodeId,
    ) -> Result<(), String> {
        let owned = document
            .compact_subtree(node)
            .ok_or_else(|| "Invalid keep_alive source".to_owned())?;
        self.install_ollama_keep_alive(CapturedConfigValue::new(owned, None));
        Ok(())
    }
    /// Clear a replacement/source and restore read_config's default.
    pub fn clear_ollama_keep_alive(&mut self) {
        let old = std::mem::replace(&mut self.ollama_keep_alive, Value::String("5m".into()));
        self.source_values.keep_alive = None;
        drop_value_iteratively(old);
    }
    /// Effective original resolved path. A direct changed/cleared typed field
    /// masks capture; explicit replacement erases it permanently.
    pub fn session_log_dir_exact(&self) -> Option<&JsString> {
        self.source_values
            .session_log_dir
            .as_ref()
            .filter(|capture| self.session_log_dir.as_deref() == Some(capture.projection.as_str()))
            .map(|capture| &capture.value)
    }
    pub fn replace_session_log_dir(&mut self, value: Option<String>) {
        self.source_values.session_log_dir = None;
        self.session_log_dir = value;
    }
    /// Existing exact-model sidecars remain explicit unconditional overrides.
    /// Use this method to replace a typed model and erase its old override.
    pub fn replace_model(&mut self, tier: &str, model: String) -> Result<(), String> {
        let target = match tier {
            "haiku" => &mut self.models.haiku,
            "sonnet" => &mut self.models.sonnet,
            "opus" => &mut self.models.opus,
            _ => return Err("Invalid model tier".to_owned()),
        };
        *target = model;
        self.exact_models.remove(tier);
        Ok(())
    }
    pub fn replace_jev_model(&mut self, model: String) {
        self.jev_model = model;
        self.exact_jev_model = None;
    }
    pub fn clear_model_override(&mut self, tier: &str) -> bool {
        self.exact_models.remove(tier).is_some()
    }
    pub fn clear_jev_model_override(&mut self) {
        self.exact_jev_model = None;
    }

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
    /// Sensitive Node HTTP field values. A top-level array is a repeated field,
    /// including zero fields for an empty array. None retains missing/undefined;
    /// each present element uses primitive conversion before byte validation.
    pub fn credential_node_http_values(
        &self,
        key: Credential,
    ) -> Result<Option<Vec<Cow<'_, str>>>, &'static str> {
        match self.credential_value(key) {
            None => Ok(None),
            Some(CredentialValue::Text(text)) => Ok(Some(vec![Cow::Borrowed(text)])),
            Some(CredentialValue::Source(source)) => {
                let convert = |node| {
                    primitive_string(ConfigValue::Source(source, node))
                        .map_err(|()| "Cannot convert credential to text")
                };
                match source.node(source.root()) {
                    Some(JsNode::Array(values)) => values
                        .iter()
                        .map(|node| convert(*node))
                        .collect::<Result<Vec<_>, _>>()
                        .map(Some),
                    _ => Ok(Some(vec![convert(source.root())?])),
                }
            }
        }
    }
    /// Sensitive primitive text; validate permitted bytes at the OS/HTTP boundary.
    pub fn credential_text(&self, key: Credential) -> Result<Option<Cow<'_, str>>, &'static str> {
        self.credential_value(key)
            .map(|value| match value {
                CredentialValue::Text(text) => Ok(Cow::Borrowed(text)),
                CredentialValue::Source(source) => {
                    primitive_string(ConfigValue::Source(source, source.root()))
                }
            })
            .transpose()
            .map_err(|()| "Cannot convert credential to text")
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
        if let Some(path) = &self.session_log_dir {
            let value = self
                .session_log_dir_exact()
                .map(ConfigOutput::Utf16)
                .unwrap_or(ConfigOutput::Text(path));
            map.serialize_entry("sessionLogDir", &value)?;
        }
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
        let jev_model = self
            .exact_jev_model
            .as_ref()
            .map(ConfigOutput::Utf16)
            .unwrap_or(ConfigOutput::Text(&self.jev_model));
        map.serialize_entry("jevModel", &jev_model)?;
        field!(ollama_endpoint, "ollamaEndpoint");
        field!(ollama_model, "ollamaModel");
        field!(ollama_timeout_ms, "ollamaTimeoutMs");
        field!(ollama_state_chars, "ollamaStateChars");
        let keep_alive = self
            .ollama_keep_alive_document()
            .map(ConfigOutput::Source)
            .unwrap_or(ConfigOutput::Json(&self.ollama_keep_alive));
        map.serialize_entry("ollamaKeepAlive", &keep_alive)?;
        map.serialize_entry("models", &ConfigModels(self))?;
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

// Original input kinds stay borrowed until their declared validation phase.
#[derive(Clone, Copy)]
enum ConfigInput<'a> {
    Json(&'a Value),
    Source(&'a JsDocument),
}
impl<'a> ConfigInput<'a> {
    fn field(self, key: &str) -> Option<ConfigValue<'a>> {
        match self {
            Self::Json(value) => value.get(key).map(ConfigValue::Json),
            Self::Source(document) => document
                .get(document.root(), key)
                .map(|node| ConfigValue::Source(document, node)),
        }
    }
}
#[derive(Clone, Copy)]
enum ConfigText<'a> {
    Scalar(&'a str),
    Source(&'a JsString),
}
impl<'a> ConfigText<'a> {
    fn is(self, expected: &str) -> bool {
        match self {
            Self::Scalar(value) => value == expected,
            Self::Source(value) => value.units().iter().copied().eq(expected.encode_utf16()),
        }
    }
    fn any_unit(self, test: impl FnMut(u16) -> bool) -> bool {
        match self {
            Self::Scalar(value) => value.encode_utf16().any(test),
            Self::Source(value) => value.units().iter().copied().any(test),
        }
    }
    fn blank(self) -> bool {
        !self.any_unit(|unit| {
            !matches!(unit,
            0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a |
            0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
        })
    }
    fn scalar(self) -> Option<Cow<'a, str>> {
        match self {
            Self::Scalar(value) => Some(Cow::Borrowed(value)),
            Self::Source(value) => value.to_scalar().map(Cow::Owned),
        }
    }
    // This is the existing typed display/OS surface, not exact source JSON.
    fn display(self) -> Cow<'a, str> {
        match self {
            Self::Scalar(value) => Cow::Borrowed(value),
            Self::Source(value) => Cow::Owned(value.to_well_formed()),
        }
    }
}
fn value<'a>(env: Option<ConfigInput<'a>>, key: &str) -> Option<ConfigValue<'a>> {
    env.and_then(|env| env.field(key))
}
fn nullish<'a>(env: Option<ConfigInput<'a>>, key: &str) -> Option<ConfigValue<'a>> {
    value(env, key).filter(|value| !value.is_null())
}
fn text(env: Option<ConfigInput<'_>>, key: &str) -> Option<String> {
    value(env, key)?
        .text()
        .map(|value| value.display().into_owned())
}
fn enum_text<'a>(
    env: Option<ConfigInput<'a>>,
    key: &str,
    fallback: &'a str,
) -> Result<ConfigText<'a>, ()> {
    match nullish(env, key) {
        None => Ok(ConfigText::Scalar(fallback)),
        Some(value) => value.text().ok_or(()),
    }
}
fn decimal(raw: ConfigValue<'_>, integer: bool) -> Option<f64> {
    static INTEGER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9]+$").unwrap());
    static DECIMAL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(?:[0-9]+(?:\.[0-9]+)?|\.[0-9]+)$").unwrap());
    if let Some(text) = raw.text() {
        let text = text.scalar()?;
        let text = js_trim(&text);
        if (if integer { &INTEGER } else { &DECIMAL }).is_match(text) {
            text.parse().ok()
        } else {
            None
        }
    } else {
        raw.number()
    }
}
fn number(
    env: Option<ConfigInput<'_>>,
    key: &str,
    fallback: f64,
    min: f64,
    max: f64,
    integer: bool,
) -> Result<f64, String> {
    let raw = value(env, key);
    // Number(raw) can fail before the source's later string/number type gate.
    if let Some(raw) = raw
        && raw.number().is_none()
        && raw.text().is_none()
    {
        primitive_string(raw).map_err(|_| "Cannot convert object to primitive value".to_owned())?;
    }
    let number = raw.map_or(Some(fallback), |raw| decimal(raw, integer));
    number
        .filter(|number| {
            number.is_finite()
                && *number >= min
                && *number <= max
                && (!integer || (number.fract() == 0.0 && *number <= MAX_SAFE_INTEGER as f64))
        })
        .ok_or_else(|| {
            format!(
                "{key} must be {} between {min} and {max}",
                if integer { "an integer" } else { "a number" }
            )
        })
}
fn stop_hook_block_cap(raw: ConfigValue<'_>, name: &str) -> Result<u64, String> {
    decimal(raw, true).filter(|number| number.is_finite() && *number >= 0.0
        && number.fract() == 0.0 && *number <= MAX_SAFE_INTEGER as f64)
        .map(|number| number as u64).ok_or_else(|| format!("{name} requires a nonnegative safe integer (0 disables the Stop-hook continuation cap)"))
}
pub fn parse_stop_hook_block_cap(raw: &Value, name: &str) -> Result<u64, String> {
    stop_hook_block_cap(ConfigValue::Json(raw), name)
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
    session_log_dir(raw.map(ConfigValue::Json), cwd)
}
fn session_log_dir(raw: Option<ConfigValue<'_>>, cwd: &Path) -> Result<Option<String>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let error = "AUTOROUTER_SESSION_LOG_DIR must be a directory path, or an empty string to disable session logging";
    let text = raw.text().ok_or_else(|| error.to_owned())?;
    if text.is("") {
        return Ok(None);
    }
    if text.blank() || text.any_unit(|unit| unit <= 0x1f || unit == 0x7f) {
        return Err(error.to_owned());
    }
    Ok(Some(
        resolve_path(cwd, Path::new(text.display().as_ref()))
            .to_string_lossy()
            .into_owned(),
    ))
}
fn model_name(raw: Option<ConfigValue<'_>>, default: &str, name: &str) -> Result<String, String> {
    let text = match raw {
        Some(raw) => raw.text(),
        None => Some(ConfigText::Scalar(default)),
    };
    text.filter(|text| {
        !text.blank() && !text.any_unit(|unit| unit <= 0x1f || (0x7f..=0x9f).contains(&unit))
    })
    .map(|text| text.display().into_owned())
    .ok_or_else(|| format!("{name} must be a nonempty model name without control characters"))
}
// Borrowed primitive conversion is shared by URLs, credentials and keep_alive.
#[derive(Clone, Copy)]
enum ConfigValue<'a> {
    Json(&'a Value),
    Source(&'a JsDocument, NodeId),
}
impl<'a> ConfigValue<'a> {
    fn is_null(self) -> bool {
        matches!(self, Self::Json(Value::Null))
            || matches!(self, Self::Source(document, node) if matches!(document.node(node), Some(JsNode::Null)))
    }
    fn number(self) -> Option<f64> {
        match self {
            Self::Json(Value::Number(number)) => number.as_f64(),
            Self::Source(document, node) => match document.node(node) {
                Some(JsNode::Number(number)) => Some(*number),
                _ => None,
            },
            _ => None,
        }
    }
    fn text(self) -> Option<ConfigText<'a>> {
        match self {
            Self::Json(Value::String(text)) => Some(ConfigText::Scalar(text)),
            Self::Source(document, node) => document.string(node).map(ConfigText::Source),
            _ => None,
        }
    }
    fn capture(self) -> Option<CapturedConfigValue> {
        match self {
            Self::Json(value) => Some(CapturedConfigValue::new(
                JsDocument::from_serde_value(value),
                Some(value),
            )),
            Self::Source(document, node) => document
                .compact_subtree(node)
                .map(|document| CapturedConfigValue::new(document, None)),
        }
    }
    fn is_text(self, expected: &str) -> bool {
        self.text().is_some_and(|text| text.is(expected))
    }
}
fn url_input<'a>(env: Option<ConfigInput<'a>>, key: &str) -> Option<ConfigValue<'a>> {
    nullish(env, key)
}
fn primitive_string(input: ConfigValue<'_>) -> Result<Cow<'_, str>, ()> {
    if let ConfigValue::Json(Value::String(text)) = input {
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
            ConfigValue::Json(Value::Number(value)) => Some(value.as_f64().ok_or(())?),
            ConfigValue::Source(source, node) => match source.node(node).ok_or(())? {
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
            ConfigValue::Json(value) => match value {
                Value::Null => {
                    if !array_item {
                        output.push_str("null");
                    }
                }
                Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
                Value::String(value) => output.push_str(value),
                Value::Array(values) => {
                    for (index, value) in values.iter().enumerate().rev() {
                        pending.push((Some(ConfigValue::Json(value)), true));
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
            ConfigValue::Source(source, node) => match source.node(node).ok_or(())? {
                JsNode::Null => {
                    if !array_item {
                        output.push_str("null");
                    }
                }
                JsNode::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
                JsNode::String(value) => output.push_str(&value.to_well_formed()),
                JsNode::Array(values) => {
                    for (index, node) in values.iter().enumerate().rev() {
                        pending.push((Some(ConfigValue::Source(source, *node)), true));
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
fn endpoint(raw: Option<ConfigValue<'_>>, default: &str, name: &str) -> Result<String, String> {
    let text = raw
        .map(primitive_string)
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
    read_config_inner(ConfigInput::Json(env), validate_all, cwd)
}
fn read_config_inner(
    input: ConfigInput<'_>,
    validate_all: bool,
    cwd: &Path,
) -> Result<RouterConfig, String> {
    let env = Some(input);
    let session_log_mode = match value(env, "AUTOROUTER_SESSION_LOG_MODE") {
        None => SessionLogMode::Metadata,
        Some(v) if v.is_text("metadata") => SessionLogMode::Metadata,
        Some(v) if v.is_text("prompts") => SessionLogMode::Prompts,
        _ => return Err("AUTOROUTER_SESSION_LOG_MODE must be metadata or prompts".to_owned()),
    };
    for key in ["AUTOROUTER_STATUSLINE", "AUTOROUTER_DEBUG"] {
        if value(env, key).is_some_and(|v| !v.is_text("0") && !v.is_text("1")) {
            return Err(format!("{key} must be 0 or 1"));
        }
    }
    let evaluator = match enum_text(env, "AUTOROUTER_EVALUATOR", "ollama") {
        Ok(value) if value.is("ollama") => Evaluator::Ollama,
        Ok(value) if value.is("jev") => Evaluator::Jev,
        _ => return Err("AUTOROUTER_EVALUATOR must be jev or ollama".to_owned()),
    };
    let ollama_env = (evaluator == Evaluator::Ollama || validate_all).then_some(input);
    let jev_env = (evaluator == Evaluator::Jev || validate_all).then_some(input);
    let ollama_model = enum_text(ollama_env, "AUTOROUTER_OLLAMA_MODEL", DEFAULT_OLLAMA_MODEL)
        .ok()
        .and_then(ConfigText::scalar)
        .ok_or_else(|| {
            "Ollama requires a valid local model tag; cloud models are not supported.".to_owned()
        })?;
    let ollama_model = validate_ollama_model(&ollama_model)?;
    if let Some(raw) = value(ollama_env, "AUTOROUTER_OLLAMA_TIMEOUT_MS")
        && raw.number().is_none()
        && decimal(raw, true).is_none()
    {
        return Err("AUTOROUTER_OLLAMA_TIMEOUT_MS must be an integer between 0 and 30000 (0 disables the runtime deadline)".to_owned());
    }
    static KEEP: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(?:0|[1-9][0-9]{0,3}(?:s|m|h))$").unwrap());
    let keep_alive_error =
        || "AUTOROUTER_OLLAMA_KEEP_ALIVE must be 0 or a positive duration such as 5m".to_owned();
    let keep_alive = nullish(ollama_env, "AUTOROUTER_OLLAMA_KEEP_ALIVE");
    if let Some(raw) = keep_alive {
        let converted = primitive_string(raw)
            .map_err(|_| "Cannot convert object to primitive value".to_owned())?;
        if !KEEP.is_match(&converted) {
            return Err(keep_alive_error());
        }
    }
    let auth_mode = match enum_text(env, "AUTOROUTER_AUTH_MODE", "api-key") {
        Ok(value) if value.is("api-key") => AuthMode::ApiKey,
        Ok(value) if value.is("subscription") => AuthMode::Subscription,
        _ => return Err("AUTOROUTER_AUTH_MODE must be api-key or subscription".to_owned()),
    };
    let client_profile = match enum_text(env, "AUTOROUTER_CLIENT_PROFILE", "compatible") {
        Ok(value) if value.is("compatible") => ClientProfile::Compatible,
        Ok(value) if value.is("native") => ClientProfile::Native,
        Ok(value) if value.is("auto") => ClientProfile::Auto,
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
        url_input(env, "AUTOROUTER_UPSTREAM_URL"),
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
    let session_log_dir = session_log_dir(value(env, "AUTOROUTER_SESSION_LOG_DIR"), cwd)?;
    let stop_hook_block_cap = value(env, "CLAUDE_CODE_STOP_HOOK_BLOCK_CAP")
        .map(|raw| stop_hook_block_cap(raw, "CLAUDE_CODE_STOP_HOOK_BLOCK_CAP"))
        .transpose()?;
    let jev_endpoint = endpoint(
        url_input(jev_env, "AUTOROUTER_JEV_URL"),
        "https://api.typesafe.ai/v1/systemone",
        "AUTOROUTER_JEV_URL",
    )?;
    let jev_model = model_name(
        value(jev_env, "AUTOROUTER_JEV_MODEL"),
        "jev-latest",
        "AUTOROUTER_JEV_MODEL",
    )?;
    let ollama_url = url_input(ollama_env, "AUTOROUTER_OLLAMA_URL")
        .map(primitive_string)
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
    let keep_alive_capture = keep_alive
        .map(|value| value.capture().ok_or_else(keep_alive_error))
        .transpose()?;
    let ollama_keep_alive = keep_alive_capture
        .as_ref()
        .map(|capture| capture.projection.clone())
        .unwrap_or_else(|| Value::String("5m".into()));
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
        source_values: SourceConfigValues {
            keep_alive: keep_alive_capture,
            session_log_dir: None,
        },
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
        if let ConfigInput::Source(source) = input {
            if let Some(node) = source.get(source.root(), key.env_name()) {
                config.replace_credential_document(key, source, node)?;
            }
        } else if let ConfigInput::Json(env) = input {
            config.replace_credential_value(key, env.get(key.env_name()));
        }
    }
    Ok(config)
}

/// Validate original JSON kinds and UTF-16. Only accepted selected values are
/// retained in owned flat arenas; bounded typed fields are display surfaces.
/// JSON text preserves UTF-16. serde_json::to_value may fail for lone UTF-16 or
/// deeply nested exact values rather than silently losing source information.
pub fn read_config_document(
    env: &JsDocument,
    validate_all: bool,
    cwd: &Path,
) -> Result<RouterConfig, String> {
    let mut config = read_config_inner(ConfigInput::Source(env), validate_all, cwd)?;
    if let Some(value) = env
        .get(env.root(), "AUTOROUTER_SESSION_LOG_DIR")
        .and_then(|node| env.string(node))
        && let Some(projection) = &config.session_log_dir
    {
        let value = resolve_js_path(cwd, value);
        if value.to_scalar().is_none() {
            config.source_values.session_log_dir = Some(CapturedConfigText {
                value,
                projection: projection.clone(),
            });
        }
    }
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

    // Fixed synthetic-source admission keeps raw fixtures and owned arenas local.
    fn source_document(bytes: impl AsRef<[u8]>) -> JsDocument {
        JsDocument::parse(bytes.as_ref()).unwrap()
    }
    fn read_source(document: &JsDocument, validate_all: bool) -> Result<RouterConfig, String> {
        read_config_document(document, validate_all, Path::new("/synthetic"))
    }
    fn source_config(bytes: impl AsRef<[u8]>) -> (JsDocument, RouterConfig) {
        let document = source_document(bytes);
        let config = read_source(&document, false).unwrap();
        (document, config)
    }
    fn config_text(config: &RouterConfig) -> String {
        serde_json::to_string(config).unwrap()
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
    fn numeric_conversion_errors_keep_type_guards_and_backend_order() {
        let check = |env: Value, all: bool, expected: &str| {
            let before = env.to_string();
            let document = source_document(&before);
            for error in [
                read_config(&env, all, Path::new("/synthetic")).err(),
                read_source(&document, all).err(),
            ] {
                assert_eq!(error.as_deref(), Some(expected), "{before}");
            }
            assert_eq!(env.to_string(), before);
            assert_eq!(document.stringify(), before);
        };
        for (key, error) in [
            (
                "AUTOROUTER_PORT",
                "AUTOROUTER_PORT must be an integer between 0 and 65535",
            ),
            (
                "AUTOROUTER_JEV_TIMEOUT_MS",
                "AUTOROUTER_JEV_TIMEOUT_MS must be an integer between 1 and 10000",
            ),
            (
                "AUTOROUTER_TOKEN_COUNT_TIMEOUT_MS",
                "AUTOROUTER_TOKEN_COUNT_TIMEOUT_MS must be an integer between 1 and 10000",
            ),
            (
                "AUTOROUTER_MIN_CONFIDENCE",
                "AUTOROUTER_MIN_CONFIDENCE must be a number between 0 and 1",
            ),
        ] {
            for property in [
                json!(null),
                json!(false),
                json!(1),
                json!("value"),
                json!([]),
                json!({}),
            ] {
                let object = json!({"toString": property});
                for raw in [object.clone(), json!([object])] {
                    check(
                        json!({"AUTOROUTER_EVALUATOR":"jev", key:raw}),
                        false,
                        "Cannot convert object to primitive value",
                    );
                }
            }
            for raw in [json!({}), json!({"valueOf":null})] {
                check(json!({"AUTOROUTER_EVALUATOR":"jev", key:raw}), false, error);
            }
        }
        for (key, error) in [
            (
                "CLAUDE_CODE_STOP_HOOK_BLOCK_CAP",
                "CLAUDE_CODE_STOP_HOOK_BLOCK_CAP requires a nonnegative safe integer (0 disables the Stop-hook continuation cap)",
            ),
            (
                "AUTOROUTER_OLLAMA_TIMEOUT_MS",
                "AUTOROUTER_OLLAMA_TIMEOUT_MS must be an integer between 0 and 30000 (0 disables the runtime deadline)",
            ),
        ] {
            check(json!({key:{"toString":null}}), false, error);
        }
        for key in ["AUTOROUTER_JEV_TIMEOUT_MS", "AUTOROUTER_MIN_CONFIDENCE"] {
            let env = json!({"AUTOROUTER_EVALUATOR":"ollama", key:{"toString":null}});
            let document = source_document(env.to_string());
            assert!(read_config(&env, false, Path::new("/")).is_ok());
            assert!(read_source(&document, false).is_ok());
            check(env, true, "Cannot convert object to primitive value");
        }
        check(
            json!({"AUTOROUTER_EVALUATOR":"jev", "AUTOROUTER_PORT":{"toString":null},
            "AUTOROUTER_AUTH_MODE":"bad"}),
            false,
            "AUTOROUTER_AUTH_MODE must be api-key or subscription",
        );
        check(
            json!({"AUTOROUTER_EVALUATOR":"jev", "AUTOROUTER_PORT":"-1",
            "AUTOROUTER_MIN_CONFIDENCE":{"toString":null}}),
            false,
            "AUTOROUTER_PORT must be an integer between 0 and 65535",
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
                let document = source_document(bytes.as_bytes());
                let actual = read_source(&document, false).map(|config| match index {
                    0 => config.upstream,
                    1 => config.jev_endpoint,
                    _ => config.ollama_endpoint,
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
            let document = source_document(bytes.as_bytes());
            for validate_all in [false, true] {
                let actual = read_source(&document, validate_all);
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
            let document = source_document(raw.as_bytes());
            assert_eq!(read_source(&document, false).err().as_deref(), Some(error));
            checked += 1;
        }
        assert_eq!(checked, 6);
    }

    const CREDENTIAL_FIELDS: [(&str, &str); 3] = [
        ("ANTHROPIC_API_KEY", "anthropicKey"),
        ("TYPESAFE_API_KEY", "jevKey"),
        ("AUTOROUTER_TOKEN", "localToken"),
    ];

    fn credential_profiles() -> impl Iterator<Item = (&'static str, &'static str, &'static str)> {
        CREDENTIAL_FIELDS.into_iter().flat_map(|(target, _)| {
            ["api-key", "subscription"]
                .into_iter()
                .flat_map(move |mode| {
                    ["jev", "ollama"]
                        .into_iter()
                        .map(move |evaluator| (target, mode, evaluator))
                })
        })
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
        let mut checked = 0;
        for (target, mode, evaluator) in credential_profiles() {
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
                    .unwrap_or_else(|error| panic!("Unexpected admission rejection: {error}"));
                let encoded = config_text(&config);
                let observed: Value = serde_json::from_str(&encoded).unwrap();
                for (key, field) in CREDENTIAL_FIELDS {
                    let expected = if key == "ANTHROPIC_API_KEY" && mode == "subscription" {
                        None
                    } else {
                        env.get(key)
                    };
                    assert_eq!(observed.get(field), expected, "{target}/{mode}/{evaluator}");
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
        let mut checked = 0;
        for (target, mode, evaluator) in credential_profiles() {
            for (raw, serialized, kind) in cases {
                let mut members = vec![
                    format!(r#""AUTOROUTER_AUTH_MODE":"{mode}""#),
                    format!(r#""AUTOROUTER_EVALUATOR":"{evaluator}""#),
                ];
                for (key, _) in CREDENTIAL_FIELDS {
                    if key == target {
                        if let Some(raw) = raw {
                            members.push(format!("\"{key}\":{raw}"));
                        }
                    } else {
                        members.push(format!(r#""{key}":"synthetic-credential-value-123456789""#));
                    }
                }
                let input = format!("{{{}}}", members.join(","));
                let (_document, config) = source_config(input.as_bytes());
                let before = config_text(&config);
                let observed = source_document(before.as_bytes());
                for (key, field) in CREDENTIAL_FIELDS {
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
                assert_eq!(config_text(&config), before);
                checked += 1;
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
            let before = config_text(&settings);
            assert_eq!(require_keys(&settings).err().as_deref(), expected);
            assert_eq!(config_text(&settings), before);
        }
    }

    #[test]
    fn credential_text_serialization_is_exact_and_value_observation_is_fallible() {
        let (source, mut config) = source_config(br#"{"AUTOROUTER_EVALUATOR":"jev","TYPESAFE_API_KEY":"synthetic-\ud800","unrelated":"synthetic-unrelated-input"}"#);
        let text = config_text(&config);
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

    #[test]
    fn raw_configuration_numbers_do_not_become_nullish_defaults() {
        for (key, error) in [
            (
                "AUTOROUTER_EVALUATOR",
                "AUTOROUTER_EVALUATOR must be jev or ollama",
            ),
            (
                "AUTOROUTER_AUTH_MODE",
                "AUTOROUTER_AUTH_MODE must be api-key or subscription",
            ),
            (
                "AUTOROUTER_CLIENT_PROFILE",
                "AUTOROUTER_CLIENT_PROFILE must be compatible, native or auto",
            ),
            (
                "AUTOROUTER_OLLAMA_MODEL",
                "Ollama requires a valid local model tag; cloud models are not supported.",
            ),
            (
                "AUTOROUTER_OLLAMA_KEEP_ALIVE",
                "AUTOROUTER_OLLAMA_KEEP_ALIVE must be 0 or a positive duration such as 5m",
            ),
        ] {
            for raw in ["1e999", "-1e999"] {
                let input = format!(r#"{{"{key}":{raw}}}"#);
                let document = source_document(input.as_bytes());
                assert_eq!(
                    read_source(&document, false).err().as_deref(),
                    Some(error),
                    "{key} {raw}"
                );
                let id = document.get(document.root(), key).unwrap();
                assert!(
                    matches!(document.node(id), Some(JsNode::Number(value)) if value.is_infinite())
                );
            }
            let input = format!(r#"{{"{key}":null}}"#);
            let document = source_document(input.as_bytes());
            assert!(read_source(&document, false).is_ok(), "actual null {key}");
        }
        for key in ["AUTOROUTER_OLLAMA_MODEL", "AUTOROUTER_OLLAMA_KEEP_ALIVE"] {
            let raw = format!(r#"{{"AUTOROUTER_EVALUATOR":"jev","{key}":1e999}}"#);
            let document = source_document(raw.as_bytes());
            assert!(read_source(&document, false).is_ok());
            assert!(read_source(&document, true).is_err());
        }
    }

    #[test]
    fn raw_timeout_preserves_numeric_gate_and_first_error_order() {
        for (raw, error) in [
            (
                r#"{"AUTOROUTER_OLLAMA_TIMEOUT_MS":1e999}"#,
                "AUTOROUTER_OLLAMA_TIMEOUT_MS must be an integer between 0 and 30000",
            ),
            (
                r#"{"AUTOROUTER_OLLAMA_TIMEOUT_MS":-1e999}"#,
                "AUTOROUTER_OLLAMA_TIMEOUT_MS must be an integer between 0 and 30000",
            ),
            (
                r#"{"AUTOROUTER_OLLAMA_TIMEOUT_MS":1e999,"AUTOROUTER_AUTH_MODE":"bad"}"#,
                "AUTOROUTER_AUTH_MODE must be api-key or subscription",
            ),
            (
                r#"{"AUTOROUTER_OLLAMA_TIMEOUT_MS":1e999,"AUTOROUTER_CLIENT_PROFILE":"bad"}"#,
                "AUTOROUTER_CLIENT_PROFILE must be compatible, native or auto",
            ),
            (
                r#"{"AUTOROUTER_OLLAMA_TIMEOUT_MS":1e999,"AUTOROUTER_UPSTREAM_URL":"bad"}"#,
                "AUTOROUTER_UPSTREAM_URL must be a valid URL",
            ),
            (
                r#"{"AUTOROUTER_OLLAMA_TIMEOUT_MS":1e999,"AUTOROUTER_SESSION_LOG_DIR":null}"#,
                "AUTOROUTER_SESSION_LOG_DIR must be a directory path, or an empty string to disable session logging",
            ),
            (
                r#"{"AUTOROUTER_OLLAMA_TIMEOUT_MS":null,"AUTOROUTER_AUTH_MODE":"bad"}"#,
                "AUTOROUTER_OLLAMA_TIMEOUT_MS must be an integer between 0 and 30000 (0 disables the runtime deadline)",
            ),
            (
                r#"{"AUTOROUTER_EVALUATOR":1e999,"AUTOROUTER_OLLAMA_MODEL":"bad model"}"#,
                "AUTOROUTER_EVALUATOR must be jev or ollama",
            ),
            (
                r#"{"AUTOROUTER_OLLAMA_KEEP_ALIVE":1e999,"AUTOROUTER_AUTH_MODE":"bad"}"#,
                "AUTOROUTER_OLLAMA_KEEP_ALIVE must be 0 or a positive duration such as 5m",
            ),
        ] {
            let document = source_document(raw.as_bytes());
            assert_eq!(
                read_source(&document, false).err().as_deref(),
                Some(error),
                "{raw}"
            );
        }
    }

    #[test]
    fn keep_alive_primitive_failure_keeps_original_kind_and_input() {
        let document = source_document(br#"{"AUTOROUTER_OLLAMA_KEEP_ALIVE":{"toString":null}}"#);
        let before = document.stringify();
        assert_eq!(
            read_source(&document, false).err().as_deref(),
            Some("Cannot convert object to primitive value")
        );
        assert_eq!(document.stringify(), before);
        for (raw, expected) in [("0", json!(0)), (r#""5m""#, json!("5m"))] {
            let input = format!(r#"{{"AUTOROUTER_OLLAMA_KEEP_ALIVE":{raw}}}"#);
            let (_document, config) = source_config(input.as_bytes());
            assert_eq!(
                config.ollama_keep_alive_json(),
                serde_json::to_string(&expected).unwrap()
            );
        }
    }

    #[test]
    fn source_accepted_deep_keep_alive_requires_exact_owned_output() {
        // Literal original Node controls observed all three successful; assert
        // complete returned JSON, not just the converted primitive string.
        for depth in [63, 64, 80] {
            let value = format!("{}\"5m\"{}", "[".repeat(depth), "]".repeat(depth));
            let input = format!(r#"{{"AUTOROUTER_OLLAMA_KEEP_ALIVE":{value}}}"#);
            let document = source_document(input.as_bytes());
            let config = read_source(&document, false)
                .unwrap_or_else(|error| panic!("source accepted depth{depth}: {error}"));
            assert_eq!(config.ollama_keep_alive_json(), value);
            let output = config_text(&config);
            let observed = source_document(output.as_bytes());
            let keep_alive = observed.get(observed.root(), "ollamaKeepAlive").unwrap();
            assert_eq!(observed.stringify_node(keep_alive), value);
        }
    }

    #[test]
    fn exact_keep_alive_is_owned_and_replacements_erase_old_source() {
        let (source, mut config) = source_config(br#"{"AUTOROUTER_OLLAMA_KEEP_ALIVE":[[["5m"]]],"unrelated":"do not retain the environment"}"#);
        let original = config.ollama_keep_alive_json();
        drop(source);
        assert_eq!(original, r#"[[["5m"]]]"#);
        let retained = config.ollama_keep_alive_document().unwrap();
        assert!(retained.get(retained.root(), "unrelated").is_none());
        assert!(retained.node(3).is_some());
        assert!(
            retained.node(4).is_none(),
            "only the four reachable nodes are owned"
        );
        assert!(!config.ollama_keep_alive_is_string());
        assert!(!config.ollama_keep_alive_is_nullish());
        let clone = config.clone();
        config.ollama_keep_alive = json!("changed by public field");
        assert_eq!(
            config.ollama_keep_alive_json(),
            r#""changed by public field""#
        );
        assert!(config.ollama_keep_alive_document().is_none());
        config.replace_ollama_keep_alive(json!("30m"));
        assert_eq!(config.ollama_keep_alive_json(), r#""30m""#);
        assert!(config.ollama_keep_alive_is_string());
        assert_eq!(clone.ollama_keep_alive_json(), original);
        config.clear_ollama_keep_alive();
        assert_eq!(config.ollama_keep_alive_json(), r#""5m""#);
        assert!(config.ollama_keep_alive_document().is_none());
        config.replace_ollama_keep_alive(Value::Null);
        assert!(config.ollama_keep_alive_is_nullish());
        let overflow = source_document(b"1e999");
        config
            .replace_ollama_keep_alive_document(&overflow, overflow.root())
            .unwrap();
        assert_eq!(config.ollama_keep_alive_json(), "null");
        assert!(
            !config.ollama_keep_alive_is_nullish(),
            "original Number is not null"
        );
        assert!(!config.ollama_keep_alive_is_string());
        let before = config.ollama_keep_alive_json();
        let other = source_document(br#"{"value":0}"#);
        assert!(
            config
                .replace_ollama_keep_alive_document(&other, usize::MAX)
                .is_err()
        );
        assert_eq!(config.ollama_keep_alive_json(), before);
    }

    #[test]
    fn arena_keep_alive_handles_twenty_thousand_levels_without_deep_value_ownership() {
        // Ownership/replacement API control, not a claim that Node readConfig
        // accepted this unmeasured host-stack-dependent primitive conversion.
        let raw = format!("{}\"5m\"{}", "[".repeat(20_000), "]".repeat(20_000));
        let source = source_document(raw.as_bytes());
        let mut config = config(json!({}));
        config
            .replace_ollama_keep_alive_document(&source, source.root())
            .unwrap();
        drop(source);
        assert_eq!(config.ollama_keep_alive_json(), raw);
        let clone = config.clone();
        let encoded = config_text(&config);
        let document = source_document(encoded.as_bytes());
        let id = document.get(document.root(), "ollamaKeepAlive").unwrap();
        assert_eq!(document.stringify_node(id), raw);
        assert!(serde_json::to_value(&config).is_err());
        // The owned Value API also drains its consumed deep input iteratively.
        let mut value = json!("0");
        for _ in 0..20_000 {
            value = Value::Array(vec![value]);
        }
        config.replace_ollama_keep_alive(value);
        assert_eq!(
            config.ollama_keep_alive_json(),
            format!("{}\"0\"{}", "[".repeat(20_000), "]".repeat(20_000))
        );
        config.clear_ollama_keep_alive();
        assert_eq!(clone.ollama_keep_alive_json(), raw);
        drop(clone);
    }

    #[test]
    fn exact_config_text_keeps_utf16_and_existing_model_override_authority() {
        let (source, mut config) = source_config(br#"{"AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_HAIKU_MODEL":"model\ud800","AUTOROUTER_JEV_MODEL":"jev\udfff","AUTOROUTER_SESSION_LOG_DIR":"logs\ud800"}"#);
        drop(source);
        assert!(serde_json::to_value(&config).is_err());
        let encoded = config_text(&config);
        let output = source_document(encoded.as_bytes());
        let models = output.get(output.root(), "models").unwrap();
        assert_eq!(
            output.stringify_node(output.get(models, "haiku").unwrap()),
            r#""model\ud800""#
        );
        assert_eq!(
            output.stringify_node(output.get(output.root(), "jevModel").unwrap()),
            r#""jev\udfff""#
        );
        assert_eq!(
            output.stringify_node(output.get(output.root(), "sessionLogDir").unwrap()),
            r#""/synthetic/logs\ud800""#
        );
        config.models.haiku = "direct typed edit".into();
        config.jev_model = "different display".into();
        let encoded = config_text(&config);
        assert!(encoded.contains(r#""haiku":"model\ud800""#));
        assert!(encoded.contains(r#""jevModel":"jev\udfff""#));
        // An unequal explicit override remains authoritative as runtime expects.
        config.exact_jev_model = Some(JsString::from_utf16(vec![0xd801]));
        assert!(config_text(&config).contains(r#""jevModel":"\ud801""#));
        config.replace_model("haiku", "new model".into()).unwrap();
        config.replace_jev_model("new jev".into());
        assert!(!config.exact_models.contains_key("haiku"));
        assert!(config.exact_jev_model.is_none());
        let captured_path = config.session_log_dir.clone();
        config.replace_session_log_dir(Some("/new path".into()));
        config.session_log_dir = captured_path;
        assert!(config.session_log_dir_exact().is_none());
        config.replace_session_log_dir(None);
        let encoded = config_text(&config);
        assert!(!encoded.contains("sessionLogDir"));
        assert!(encoded.contains(r#""haiku":"new model""#));
        assert!(encoded.contains(r#""jevModel":"new jev""#));
    }

    #[test]
    fn evaluator_copy_preserves_every_non_evaluator_field_and_source_sidecar() {
        fn non_evaluator_fields(config: &RouterConfig) -> Vec<(String, String)> {
            let document = source_document(config_text(config));
            let Some(JsNode::Object(fields)) = document.node(document.root()) else {
                panic!("configuration is an object");
            };
            let evaluator_fields = [
                "evaluator",
                "jevKey",
                "jevEndpoint",
                "jevModel",
                "jevTimeoutMs",
                "minConfidence",
                "stateChars",
                "ollamaEndpoint",
                "ollamaModel",
                "ollamaTimeoutMs",
                "ollamaStateChars",
                "ollamaKeepAlive",
            ];
            fields
                .entries()
                .iter()
                .filter_map(|(key, node)| {
                    let key = key.to_scalar().unwrap();
                    (!evaluator_fields.contains(&key.as_str()))
                        .then(|| (key, document.stringify_node(*node)))
                })
                .collect()
        }
        let (_, mut target) = source_config(br#"{"AUTOROUTER_EVALUATOR":"jev","ANTHROPIC_API_KEY":{"synthetic":"target-provider"},"AUTOROUTER_TOKEN":"target-local-\ud800","AUTOROUTER_SESSION_LOG_DIR":"target-path\ud801","AUTOROUTER_HAIKU_MODEL":"target-model\ud802"}"#);
        let (_, mut source) = source_config(br#"{"AUTOROUTER_EVALUATOR":"jev","TYPESAFE_API_KEY":"source-jev-\ud803","ANTHROPIC_API_KEY":{"synthetic":"source-provider"},"AUTOROUTER_TOKEN":"source-local-\ud804","AUTOROUTER_SESSION_LOG_DIR":"source-path\ud805","AUTOROUTER_HAIKU_MODEL":"source-model\ud806"}"#);
        target.auth_mode = AuthMode::Subscription;
        target.client_profile = ClientProfile::Native;
        target.session_log_mode = SessionLogMode::Prompts;
        target.stop_hook_block_cap = Some(7);
        target.upstream = "https://target.invalid".into();
        target.models.sonnet = "target-sonnet".into();
        target.models.opus = "target-opus".into();
        target.port = 8001;
        target.token_count_timeout_ms = 15;
        target.max_body_bytes = 1_048_576;
        target.cache_entries = 3;
        target.cache_ttl_ms = 7;
        target.turn_ttl_ms = 11;
        target.turn_entries = Some(5);
        target.upstream_timeout_ms = 77;
        source.client_profile = ClientProfile::Auto;
        source.stop_hook_block_cap = Some(11);
        source.upstream = "https://source.invalid".into();
        source.port = 8002;
        source.token_count_timeout_ms = 20;
        source.max_body_bytes = 2_000_000;
        source.cache_entries = 8;
        source.cache_ttl_ms = 9;
        source.turn_ttl_ms = 17;
        source.turn_entries = Some(6);
        source.upstream_timeout_ms = 88;
        source.evaluator = Evaluator::Ollama;
        source.jev_endpoint = "https://source.invalid/v1/systemone".into();
        source.jev_model = "source-jev-model".into();
        source.exact_jev_model = Some(JsString::from_utf16(vec![0xd807]));
        source.jev_timeout_ms = 900;
        source.min_confidence = 0.25;
        source.state_chars = 1000;
        source.ollama_endpoint = "http://127.0.0.1:11435".into();
        source.ollama_model = "synthetic-local:1b".into();
        source.ollama_timeout_ms = 0;
        source.ollama_state_chars = 777;
        source.replace_ollama_keep_alive(json!("30m"));
        let target_before = target.clone();
        let non_evaluator_before = non_evaluator_fields(&target);
        let source_before = config_text(&source);
        target.copy_evaluator_settings_from(&source);
        assert_eq!(non_evaluator_fields(&target), non_evaluator_before);
        assert_eq!(target.models.haiku, target_before.models.haiku);
        assert_eq!(target.models.sonnet, target_before.models.sonnet);
        assert_eq!(target.models.opus, target_before.models.opus);
        assert!(target.exact_models == target_before.exact_models);
        assert_eq!(target.anthropic_key, target_before.anthropic_key);
        assert_eq!(target.local_token, target_before.local_token);
        assert_eq!(target.session_log_dir, target_before.session_log_dir);
        assert!(target.session_log_dir_exact() == target_before.session_log_dir_exact());
        for key in [Credential::Anthropic, Credential::LocalToken] {
            let before = target_before.source_credentials.slots[key.index()]
                .as_ref()
                .unwrap();
            let after = target.source_credentials.slots[key.index()]
                .as_ref()
                .unwrap();
            assert!(Arc::ptr_eq(&before.document, &after.document));
            assert_eq!(before.projection, after.projection);
        }
        assert!(target.evaluator == source.evaluator);
        assert_eq!(target.jev_endpoint, source.jev_endpoint);
        assert_eq!(target.jev_model, source.jev_model);
        assert!(target.exact_jev_model == source.exact_jev_model);
        assert_eq!(target.jev_key, source.jev_key);
        assert_eq!(
            target.credential_json(Credential::Jev),
            source.credential_json(Credential::Jev)
        );
        assert_eq!(target.jev_timeout_ms, source.jev_timeout_ms);
        assert_eq!(target.min_confidence, source.min_confidence);
        assert_eq!(target.state_chars, source.state_chars);
        assert_eq!(target.ollama_endpoint, source.ollama_endpoint);
        assert_eq!(target.ollama_model, source.ollama_model);
        assert_eq!(target.ollama_timeout_ms, source.ollama_timeout_ms);
        assert_eq!(target.ollama_state_chars, source.ollama_state_chars);
        assert_eq!(
            target.ollama_keep_alive_json(),
            source.ollama_keep_alive_json()
        );
        assert_eq!(config_text(&source), source_before);
    }

    #[test]
    fn evaluator_copy_keeps_missing_null_utf16_and_masked_jev_source_distinct() {
        for (entry, expected) in [
            ("", None),
            (r#", "TYPESAFE_API_KEY":null"#, Some("null")),
            (
                r#", "TYPESAFE_API_KEY":"synthetic-\ud800""#,
                Some(r#""synthetic-\ud800""#),
            ),
            (
                r#", "TYPESAFE_API_KEY":{"synthetic":true}"#,
                Some(r#"{"synthetic":true}"#),
            ),
        ] {
            let raw = format!(r#"{{"AUTOROUTER_EVALUATOR":"jev"{entry}}}"#);
            let (_, source) = source_config(raw);
            let (_, mut target) =
                source_config(br#"{"AUTOROUTER_EVALUATOR":"jev","TYPESAFE_API_KEY":"old-\ud801"}"#);
            let before = config_text(&source);
            target.copy_evaluator_settings_from(&source);
            assert_eq!(target.credential_json(Credential::Jev).as_deref(), expected);
            assert_eq!(target.jev_key, source.jev_key);
            assert_eq!(config_text(&source), before);
        }
        let (_, mut source) = source_config(br#"{"AUTOROUTER_EVALUATOR":"jev","TYPESAFE_API_KEY":"synthetic-\ud800","AUTOROUTER_JEV_MODEL":"model\ud801"}"#);
        let captured_projection = source.jev_key.clone();
        source.jev_key = Some("typed-mask".into());
        source.jev_model = "different typed model".into();
        let mut target = config(json!({}));
        target.copy_evaluator_settings_from(&source);
        assert_eq!(
            target.credential_json(Credential::Jev).as_deref(),
            Some(r#""typed-mask""#)
        );
        assert_eq!(
            target.exact_jev_model.as_ref().unwrap().units(),
            &[0x006d, 0x006f, 0x0064, 0x0065, 0x006c, 0xd801]
        );
        target.jev_key = captured_projection;
        assert_eq!(
            target.credential_json(Credential::Jev).as_deref(),
            Some(r#""synthetic-\ud800""#)
        );
        assert!(config_text(&target).contains(r#""jevModel":"model\ud801""#));
        source.replace_credential(Credential::Jev, None);
        source.replace_jev_model("replacement model".into());
        target.copy_evaluator_settings_from(&source);
        assert!(target.credential_json(Credential::Jev).is_none());
        assert!(target.source_credentials.slots[Credential::Jev.index()].is_none());
        assert!(target.exact_jev_model.is_none());
        assert_eq!(target.jev_model, "replacement model");
    }

    #[test]
    fn evaluator_copy_keeps_effective_exact_keep_alive_kind_and_masks_old_capture() {
        let mut source = config(json!({}));
        let exact = source_document(br#"["synthetic-\ud800",1e999]"#);
        source
            .replace_ollama_keep_alive_document(&exact, exact.root())
            .unwrap();
        let mut target = config(json!({}));
        target.copy_evaluator_settings_from(&source);
        assert_eq!(
            target.ollama_keep_alive_json(),
            r#"["synthetic-\ud800",null]"#
        );
        let original = source.ollama_keep_alive_document().unwrap();
        let copied = target.ollama_keep_alive_document().unwrap();
        let Some(JsNode::Array(values)) = copied.node(copied.root()) else {
            panic!("exact keep_alive is an array");
        };
        assert!(
            matches!(copied.node(values[1]), Some(JsNode::Number(value)) if value.is_infinite())
        );
        assert!(std::ptr::eq(original, copied));
        source.ollama_keep_alive = json!("typed-mask");
        assert!(source.ollama_keep_alive_document().is_none());
        target.copy_evaluator_settings_from(&source);
        assert_eq!(target.ollama_keep_alive_json(), r#""typed-mask""#);
        assert_eq!(source.ollama_keep_alive_json(), r#""typed-mask""#);
        assert!(target.ollama_keep_alive_is_string());
        source.ollama_keep_alive = Value::Null;
        target.copy_evaluator_settings_from(&source);
        assert_eq!(target.ollama_keep_alive_json(), "null");
        assert!(target.ollama_keep_alive_is_nullish());
    }

    #[test]
    fn evaluator_copy_imports_and_replaces_twenty_thousand_level_direct_values_safely() {
        let depth = 20_000;
        let mut source = config(json!({}));
        let mut target = config(json!({}));
        let mut incoming = json!("incoming");
        let mut replaced = json!("replaced");
        for _ in 0..depth {
            incoming = Value::Array(vec![incoming]);
            replaced = Value::Array(vec![replaced]);
        }
        source.ollama_keep_alive = incoming;
        target.ollama_keep_alive = replaced;
        let expected = format!("{}\"incoming\"{}", "[".repeat(depth), "]".repeat(depth));
        target.copy_evaluator_settings_from(&source);
        // The source still owns its direct deep Value; dispose of it safely
        // before assertions so failed assertions cannot recursively drop it.
        source.clear_ollama_keep_alive();
        assert_eq!(target.ollama_keep_alive_json(), expected);
        let owned = target.ollama_keep_alive_document().unwrap();
        assert!(owned.node(depth).is_some());
        assert!(owned.node(depth + 1).is_none());
        target.clear_ollama_keep_alive();
        assert_eq!(target.ollama_keep_alive_json(), r#""5m""#);
    }

    #[test]
    fn ollama_model_tag_length_grammar_and_inactive_mask_are_explicit() {
        const ERROR: &str =
            "Ollama requires a valid local model tag; cloud models are not supported.";
        let cases = [
            ("a".to_owned(), true),
            ("a".repeat(200), true),
            ("a".repeat(201), false),
            ("A09_./:-".to_owned(), true),
            ("team/decision-model:v1".to_owned(), true),
            ("model:cloud-extra".to_owned(), true),
            ("model-cloud/child".to_owned(), true),
            ("model:CLOUD".to_owned(), false),
            ("model-ClOuD".to_owned(), false),
            ("https://localhost/model".to_owned(), false),
            ("".to_owned(), false),
            ("_model".to_owned(), false),
            (".model".to_owned(), false),
            ("/model".to_owned(), false),
            (":model".to_owned(), false),
            ("-model".to_owned(), false),
            (" model".to_owned(), false),
            ("model ".to_owned(), false),
            ("mod el".to_owned(), false),
            ("mod\tel".to_owned(), false),
            ("mod\0el".to_owned(), false),
            ("modèle".to_owned(), false),
        ];
        for (model, accepted) in cases {
            let document = source_document(
                serde_json::to_vec(&json!({
                    "AUTOROUTER_OLLAMA_MODEL":model
                }))
                .unwrap(),
            );
            let before = document.stringify();
            let result = read_source(&document, false);
            if accepted {
                let config = result.unwrap();
                assert_eq!(config.ollama_model, model);
                assert_eq!(config.ollama_timeout_ms, 1500);
                assert_eq!(validate_ollama_model(&model).unwrap(), model);
            } else {
                assert_eq!(result.err().as_deref(), Some(ERROR), "{model:?}");
                assert_eq!(validate_ollama_model(&model).err().as_deref(), Some(ERROR));
            }
            assert_eq!(document.stringify(), before);
        }
        for terminator in ["\n", "\r", "\r\n", "\u{2028}", "\u{2029}"] {
            let model = format!("model{terminator}");
            let document = source_document(
                serde_json::to_vec(&json!({
                    "AUTOROUTER_OLLAMA_MODEL":model
                }))
                .unwrap(),
            );
            let before = document.stringify();
            assert_eq!(read_source(&document, false).err().as_deref(), Some(ERROR));
            assert_eq!(document.stringify(), before);
        }
        for raw in ["null", "0", "false", "[]", "{}"] {
            let document = source_document(format!(r#"{{"AUTOROUTER_OLLAMA_MODEL":{raw}}}"#));
            let before = document.stringify();
            match raw {
                "null" => assert_eq!(
                    read_source(&document, false).unwrap().ollama_model,
                    DEFAULT_OLLAMA_MODEL
                ),
                _ => assert_eq!(read_source(&document, false).err().as_deref(), Some(ERROR)),
            }
            assert_eq!(document.stringify(), before);
        }
        let inactive = source_document(br#"{"AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_OLLAMA_MODEL":"","AUTOROUTER_OLLAMA_KEEP_ALIVE":"bad"}"#);
        let before = inactive.stringify();
        let masked = read_source(&inactive, false).unwrap();
        assert_eq!(masked.ollama_model, DEFAULT_OLLAMA_MODEL);
        assert_eq!(masked.ollama_keep_alive_json(), r#""5m""#);
        assert_eq!(read_source(&inactive, true).err().as_deref(), Some(ERROR));
        assert_eq!(inactive.stringify(), before);
    }

    #[test]
    fn ollama_keep_alive_duration_extrema_retain_kind_and_first_error() {
        const ERROR: &str =
            "AUTOROUTER_OLLAMA_KEEP_ALIVE must be 0 or a positive duration such as 5m";
        for (raw, accepted, expected) in [
            (r#""0""#, true, r#""0""#),
            ("0", true, "0"),
            ("null", true, r#""5m""#),
            (r#""1s""#, true, r#""1s""#),
            (r#""9999s""#, true, r#""9999s""#),
            (r#""1m""#, true, r#""1m""#),
            (r#""9999m""#, true, r#""9999m""#),
            (r#""1h""#, true, r#""1h""#),
            (r#""9999h""#, true, r#""9999h""#),
            (r#""""#, false, ""),
            (r#""00""#, false, ""),
            (r#""01m""#, false, ""),
            (r#""10000m""#, false, ""),
            (r#""-1""#, false, ""),
            (r#""0s""#, false, ""),
            (r#""1M""#, false, ""),
            (r#""1d""#, false, ""),
            (r#""1""#, false, ""),
            (r#""1.5m""#, false, ""),
            (r#"" 1m""#, false, ""),
            (r#""1m ""#, false, ""),
            (r#""١m""#, false, ""),
        ] {
            let document = source_document(format!(r#"{{"AUTOROUTER_OLLAMA_KEEP_ALIVE":{raw}}}"#));
            let before = document.stringify();
            let result = read_source(&document, false);
            if accepted {
                let config = result.unwrap();
                assert_eq!(config.ollama_keep_alive_json(), expected);
                assert_eq!(config.ollama_keep_alive_is_string(), raw != "0");
                let serialized = source_document(config_text(&config));
                let value = serialized
                    .get(serialized.root(), "ollamaKeepAlive")
                    .unwrap();
                assert_eq!(serialized.stringify_node(value), expected);
            } else {
                assert_eq!(result.err().as_deref(), Some(ERROR), "{raw}");
            }
            assert_eq!(document.stringify(), before);
        }
        for terminator in ["\n", "\r", "\r\n", "\u{2028}", "\u{2029}"] {
            for prefix in ["0", "1m"] {
                let document = source_document(
                    serde_json::to_vec(&json!({
                        "AUTOROUTER_OLLAMA_KEEP_ALIVE":format!("{prefix}{terminator}")
                    }))
                    .unwrap(),
                );
                let before = document.stringify();
                assert_eq!(read_source(&document, false).err().as_deref(), Some(ERROR));
                assert_eq!(document.stringify(), before);
            }
        }
        for (raw, error) in [
            (
                r#"{"AUTOROUTER_OLLAMA_MODEL":"","AUTOROUTER_OLLAMA_KEEP_ALIVE":"bad"}"#,
                "Ollama requires a valid local model tag; cloud models are not supported.",
            ),
            (
                r#"{"AUTOROUTER_OLLAMA_TIMEOUT_MS":"bad","AUTOROUTER_OLLAMA_KEEP_ALIVE":"bad"}"#,
                "AUTOROUTER_OLLAMA_TIMEOUT_MS must be an integer between 0 and 30000 (0 disables the runtime deadline)",
            ),
            (
                r#"{"AUTOROUTER_OLLAMA_KEEP_ALIVE":"bad","AUTOROUTER_AUTH_MODE":"bad"}"#,
                ERROR,
            ),
            (
                r#"{"AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_OLLAMA_KEEP_ALIVE":"bad"}"#,
                ERROR,
            ),
        ] {
            let document = source_document(raw);
            let before = document.stringify();
            let validate_all = raw.contains(r#""AUTOROUTER_EVALUATOR":"jev""#);
            if validate_all {
                let masked = read_source(&document, false).unwrap();
                assert_eq!(masked.ollama_keep_alive_json(), r#""5m""#);
            }
            assert_eq!(
                read_source(&document, validate_all).err().as_deref(),
                Some(error)
            );
            assert_eq!(document.stringify(), before);
        }
    }

    #[test]
    fn parsed_keep_alive_depths_retain_exact_config_after_source_drop() {
        use sha2::{Digest, Sha256};
        let default_config = config_text(&config(json!({})));
        for depth in [63, 64, 80, 2048, 20_000] {
            let value = format!("{}\"5m\"{}", "[".repeat(depth), "]".repeat(depth));
            let input = format!(r#"{{"AUTOROUTER_OLLAMA_KEEP_ALIVE":{value}}}"#);
            let source = source_document(input.as_bytes());
            let before = source.stringify();
            assert_eq!(before, input);
            let parsed = read_source(&source, false);
            assert_eq!(source.stringify(), before);
            let config = parsed.unwrap_or_else(|error| {
                eprintln!(
                    "OLLAMA_KEEP_ALIVE_DEPTH {}",
                    json!({
                        "depth":depth,"phase":"read_config","accepted":false,"error":error
                    })
                );
                panic!("native parsed keep_alive depth{depth}: {error}");
            });
            assert_eq!(config.ollama_keep_alive_json(), value);
            drop(source);
            assert_eq!(config.ollama_keep_alive_json(), value);
            assert!(!config.ollama_keep_alive_is_string());
            assert!(!config.ollama_keep_alive_is_nullish());
            let retained = config.ollama_keep_alive_document().unwrap();
            assert!(retained.node(depth).is_some());
            assert!(retained.node(depth + 1).is_none());
            let encoded = config_text(&config);
            let mut expected = source_document(default_config.as_bytes());
            expected
                .set_field_json(expected.root(), "ollamaKeepAlive", value.as_bytes())
                .unwrap();
            assert_eq!(encoded, expected.stringify());
            let complete = source_document(encoded.as_bytes());
            let retained_value = complete.get(complete.root(), "ollamaKeepAlive").unwrap();
            assert_eq!(complete.stringify_node(retained_value), value);
            eprintln!(
                "OLLAMA_KEEP_ALIVE_DEPTH {}",
                json!({
                    "depth":depth,"phase":"complete","accepted":true,
                    "source_unchanged":true,"owned_after_source_drop":true,
                    "value_bytes":value.len(),
                    "value_sha256":format!("{:x}", Sha256::digest(value.as_bytes())),
                    "config_bytes":encoded.len(),
                    "config_sha256":format!("{:x}", Sha256::digest(encoded.as_bytes()))
                })
            );
        }
    }

    #[test]
    fn parsed_url_depths_preserve_config_and_detach_normalized_endpoints() {
        use sha2::{Digest, Sha256};
        let mut checked = 0;
        for (key, field, backend, scalar, expected_url) in [
            (
                "AUTOROUTER_UPSTREAM_URL",
                "upstream",
                "ollama",
                "https://example.test/gateway/",
                "https://example.test/gateway",
            ),
            (
                "AUTOROUTER_JEV_URL",
                "jevEndpoint",
                "jev",
                "https://example.test/v1/systemone/",
                "https://example.test/v1/systemone",
            ),
            (
                "AUTOROUTER_OLLAMA_URL",
                "ollamaEndpoint",
                "ollama",
                "http://localhost:11434/",
                "http://127.0.0.1:11434",
            ),
        ] {
            let baseline = config_text(&config(json!({"AUTOROUTER_EVALUATOR":backend})));
            let mut expected = source_document(baseline.as_bytes());
            expected
                .set_field_json(
                    expected.root(),
                    field,
                    serde_json::to_string(expected_url).unwrap().as_bytes(),
                )
                .unwrap();
            let expected = expected.stringify();
            for depth in [63, 64, 80, 2048, 20_000] {
                let value = format!(
                    "{}{}{}",
                    "[".repeat(depth),
                    serde_json::to_string(scalar).unwrap(),
                    "]".repeat(depth)
                );
                let input = format!(r#"{{"AUTOROUTER_EVALUATOR":"{backend}","{key}":{value}}}"#);
                let source = source_document(input.as_bytes());
                let before = source.stringify();
                assert_eq!(before, input);
                let parsed = read_source(&source, false);
                assert_eq!(source.stringify(), before);
                let config = parsed.unwrap_or_else(|error| {
                    eprintln!("CONFIG_URL_DEPTH {}", json!({"key":key,"depth":depth,"phase":"read_config","accepted":false,"error":error}));
                    panic!("native parsed URL {key} depth{depth}: {error}");
                });
                let endpoint = match key {
                    "AUTOROUTER_UPSTREAM_URL" => &config.upstream,
                    "AUTOROUTER_JEV_URL" => &config.jev_endpoint,
                    _ => &config.ollama_endpoint,
                };
                assert_eq!(endpoint, expected_url);
                drop(source);
                let encoded = config_text(&config);
                assert_eq!(encoded, expected);
                let complete = source_document(encoded.as_bytes());
                let retained = complete.get(complete.root(), field).unwrap();
                assert_eq!(
                    complete.stringify_node(retained),
                    serde_json::to_string(expected_url).unwrap()
                );
                eprintln!(
                    "CONFIG_URL_DEPTH {}",
                    json!({
                        "key":key,"depth":depth,"phase":"complete","accepted":true,
                        "source_unchanged":true,"owned_after_source_drop":true,"endpoint":expected_url,
                        "input_bytes":input.len(),"input_sha256":format!("{:x}",Sha256::digest(input.as_bytes())),
                        "config_bytes":encoded.len(),"config_sha256":format!("{:x}",Sha256::digest(encoded.as_bytes()))
                    })
                );
                checked += 1;
            }
        }
        assert_eq!(checked, 15);
    }
}
