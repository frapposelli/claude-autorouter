//! Configuration parsing with the existing environment/file contract.
//!
//! This module has no environment or filesystem access. The caller supplies the
//! effective environment and working directory after storage/policy resolution.
use crate::js_json::{JsDocument, JsString};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;
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
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouterConfig {
    pub evaluator: Evaluator,
    pub auth_mode: AuthMode,
    pub client_profile: ClientProfile,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_log_dir: Option<String>,
    pub session_log_mode: SessionLogMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_hook_block_cap: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jev_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_token: Option<String>,
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
    #[serde(skip)]
    pub exact_models: BTreeMap<String, JsString>,
    #[serde(skip)]
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
    pub upstream_timeout_ms: u64,
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

fn endpoint(raw: Option<&Value>, default: &str, name: &str) -> Result<String, String> {
    let raw = raw
        .and_then(Value::as_str)
        .unwrap_or(if raw.is_some() { "" } else { default });
    let url = Url::parse(raw).map_err(|_| format!("{name} must be a valid URL"))?;
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

pub fn validate_ollama_endpoint(raw: &str) -> Result<String, String> {
    let error =
        "Ollama must use a loopback base URL without a path, credentials, a query, or a fragment.";
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
        nullish(env, "AUTOROUTER_UPSTREAM_URL"),
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
        nullish(jev_env, "AUTOROUTER_JEV_URL"),
        "https://api.typesafe.ai/v1/systemone",
        "AUTOROUTER_JEV_URL",
    )?;
    let jev_model = model_name(
        value(jev_env, "AUTOROUTER_JEV_MODEL"),
        "jev-latest",
        "AUTOROUTER_JEV_MODEL",
    )?;
    let ollama_endpoint = validate_ollama_endpoint(enum_text(
        ollama_env,
        "AUTOROUTER_OLLAMA_URL",
        "http://127.0.0.1:11434",
    ))?;
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
    Ok(RouterConfig {
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
        upstream_timeout_ms: 10 * 60 * 1000,
    })
}

/// Saved JSON permits UTF-16 strings that cannot be represented in an OS
/// environment. Validation of controls, enums and catalog IDs is unaffected by
/// replacement projection; unconstrained model identities retain their source.
pub fn read_config_document(
    env: &JsDocument,
    validate_all: bool,
    cwd: &Path,
) -> Result<RouterConfig, String> {
    let mut config = read_config(&env.to_serde_observation_lossy(), validate_all, cwd)?;
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
    let mut keys = Vec::new();
    if config.evaluator == Evaluator::Jev {
        keys.push(("TYPESAFE_API_KEY", &config.jev_key));
    }
    if config.auth_mode == AuthMode::ApiKey {
        keys.push(("ANTHROPIC_API_KEY", &config.anthropic_key));
    }
    for (key, value) in keys {
        if value.as_deref().is_none_or(|v| js_trim(v).is_empty()) {
            return Err(format!("Set {key} before starting the router"));
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
}
