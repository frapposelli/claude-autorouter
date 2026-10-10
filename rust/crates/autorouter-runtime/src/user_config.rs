//! Saved configuration with environment precedence, revision checks and
//! atomic private writes. No implicit project configuration or migration.
use crate::keychain::{Keychain, UNAVAILABLE};
use crate::policy::{LoadedPolicy, load_policy};
use autorouter_core::auth::Environment;
use autorouter_core::config::{js_trim, resolve_path};
use autorouter_core::js_json::{JsDocument, JsNode, JsString};
use autorouter_core::policy::apply_policy;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const CONFIG_KEYS: [&str; 27] = [
    "AUTOROUTER_AUTH_MODE",
    "AUTOROUTER_CLIENT_PROFILE",
    "AUTOROUTER_SECRET_STORE",
    "ANTHROPIC_API_KEY",
    "TYPESAFE_API_KEY",
    "AUTOROUTER_TOKEN",
    "AUTOROUTER_UPSTREAM_URL",
    "AUTOROUTER_JEV_URL",
    "AUTOROUTER_JEV_MODEL",
    "AUTOROUTER_HAIKU_MODEL",
    "AUTOROUTER_SONNET_MODEL",
    "AUTOROUTER_OPUS_MODEL",
    "AUTOROUTER_PORT",
    "AUTOROUTER_JEV_TIMEOUT_MS",
    "AUTOROUTER_TOKEN_COUNT_TIMEOUT_MS",
    "AUTOROUTER_MIN_CONFIDENCE",
    "AUTOROUTER_STATUSLINE",
    "AUTOROUTER_DEBUG",
    "AUTOROUTER_SESSION_LOG_DIR",
    "AUTOROUTER_SESSION_LOG_MODE",
    "ENABLE_TOOL_SEARCH",
    "CLAUDE_CODE_STOP_HOOK_BLOCK_CAP",
    "AUTOROUTER_EVALUATOR",
    "AUTOROUTER_OLLAMA_URL",
    "AUTOROUTER_OLLAMA_MODEL",
    "AUTOROUTER_OLLAMA_TIMEOUT_MS",
    "AUTOROUTER_OLLAMA_KEEP_ALIVE",
];
pub const SECRET_CONFIG_KEYS: [&str; 3] =
    ["ANTHROPIC_API_KEY", "TYPESAFE_API_KEY", "AUTOROUTER_TOKEN"];
const EXISTS: &str = "AutoRouter configuration already exists; use overwrite to replace it.";
const CHANGED: &str = "AutoRouter configuration changed while this operation was running. Retry with the current settings.";

// No Debug on environments, snapshots or save options: they contain secrets.
pub struct ConfigContext<'a> {
    pub env: &'a Environment,
    pub cwd: &'a Path,
    pub home: &'a Path,
}
pub struct LoadedConfig {
    pub env: Environment,
    /// Authoritative merged configuration. OS environment forwarding converts
    /// lone surrogates to U+FFFD just as Node's process boundary does.
    pub env_document: JsDocument,
    pub values: Map<String, Value>,
    pub values_document: JsDocument,
    pub path: PathBuf,
    pub exists: bool,
    pub revision: Option<String>,
    pub secret_store: String,
    pub keychain_secrets: Vec<String>,
    pub unavailable_secrets: Vec<String>,
    pub policy: Option<Value>,
    pub policy_path: Option<PathBuf>,
    pub policy_locked: Vec<String>,
}

impl LoadedConfig {
    /// Keep unchanged saved UTF-16 values during an edit. Explicitly written
    /// keys are replaced even when their display projection compares equal.
    pub fn updated_document(&self, next: &Map<String, Value>, replaced: &[&str]) -> JsDocument {
        let mut document = scalar_document(&Value::Object(next.clone()));
        for (key, value) in next {
            if !replaced.contains(&key.as_str())
                && self.values.get(key) == Some(value)
                && let Some(node) = self.values_document.get(self.values_document.root(), key)
                && let Some(exact) = self.values_document.string(node)
            {
                document
                    .set_root_field_json(key, exact.stringify().as_bytes())
                    .expect("validated string");
            }
        }
        document
    }
}

pub fn scalar_document(value: &Value) -> JsDocument {
    JsDocument::parse(value.to_string().as_bytes()).expect("serde JSON is valid JSON")
}

/// JSON.stringify(value, null, 2), used only for bounded-depth configuration
/// and command reports. String escapes are retained exactly by this formatter.
pub fn pretty_document(document: &JsDocument) -> String {
    let text = document.stringify();
    let mut output = String::new();
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if quoted {
            output.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
            continue;
        }
        match ch {
            '"' => {
                quoted = true;
                output.push(ch);
            }
            '{' | '[' => {
                output.push(ch);
                let closing = if ch == '{' { '}' } else { ']' };
                if chars.peek() == Some(&closing) {
                    output.push(chars.next().expect("closing delimiter"));
                } else {
                    depth += 1;
                    output.push('\n');
                    output.push_str(&"  ".repeat(depth));
                }
            }
            '}' | ']' => {
                depth -= 1;
                output.push('\n');
                output.push_str(&"  ".repeat(depth));
                output.push(ch);
            }
            ',' => {
                output.push_str(",\n");
                output.push_str(&"  ".repeat(depth));
            }
            ':' => output.push_str(": "),
            _ => output.push(ch),
        }
    }
    output
}
pub struct LoadOptions {
    pub allow_missing: bool,
    pub read_secrets: bool,
    pub enforce_policy: bool,
}
impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            allow_missing: false,
            read_secrets: true,
            enforce_policy: true,
        }
    }
}
#[derive(Default)]
pub struct SaveOptions {
    pub overwrite: bool,
    /// None means no comparison; Some(None) requires a missing file.
    pub expected_revision: Option<Option<String>>,
    pub remove_secrets: Vec<String>,
}

fn unsafe_path(path: &Path) -> bool {
    path.to_string_lossy().chars().any(|c| matches!(c,
        '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
}
pub fn get_config_path(context: &ConfigContext<'_>) -> Result<PathBuf, String> {
    let path = if let Some(value) = context.env.get(OsStr::new("AUTOROUTER_CONFIG")) {
        if js_trim(&value.to_string_lossy()).is_empty() {
            return Err("AUTOROUTER_CONFIG must be a non-empty path.".into());
        }
        resolve_path(context.cwd, Path::new(value))
    } else {
        let xdg = context
            .env
            .get(OsStr::new("XDG_CONFIG_HOME"))
            .filter(|v| !v.is_empty());
        if xdg.is_some_and(|value| !Path::new(value).is_absolute()) {
            return Err("XDG_CONFIG_HOME must be an absolute path when set.".into());
        }
        let base = xdg
            .map(PathBuf::from)
            .unwrap_or_else(|| context.home.join(".config"));
        resolve_path(context.cwd, &base.join("claude-autorouter/config.json"))
    };
    if unsafe_path(&path) {
        return Err(
            "The AutoRouter configuration path must not contain control characters.".into(),
        );
    }
    Ok(path)
}

pub fn validate_values(values: &Value) -> Result<Map<String, Value>, String> {
    let values = values
        .as_object()
        .ok_or("AutoRouter configuration must be a JSON object of string values.")?;
    for (key, value) in values {
        if !CONFIG_KEYS.contains(&key.as_str()) {
            return Err("AutoRouter configuration contains an unsupported key.".into());
        }
        if !value.is_string() {
            return Err("AutoRouter configuration values must be strings.".into());
        }
    }
    if values
        .get("AUTOROUTER_SECRET_STORE")
        .is_some_and(|value| value != "file" && value != "keychain")
    {
        return Err("AUTOROUTER_SECRET_STORE must be file or keychain.".into());
    }
    Ok(values.clone())
}
pub fn validate_document(document: &JsDocument) -> Result<Map<String, Value>, String> {
    let Some(JsNode::Object(object)) = document.node(document.root()) else {
        return Err("AutoRouter configuration must be a JSON object of string values.".into());
    };
    let mut values = Map::new();
    for (key, node) in object.entries() {
        let key = key
            .to_scalar()
            .filter(|key| CONFIG_KEYS.contains(&key.as_str()))
            .ok_or("AutoRouter configuration contains an unsupported key.")?;
        let value = document
            .string(*node)
            .ok_or("AutoRouter configuration values must be strings.")?;
        values.insert(key, Value::String(value.to_well_formed()));
    }
    validate_values(&Value::Object(values))
}
pub fn keychain_account(path: &Path, key: &str) -> String {
    format!(
        "{key}:{}",
        &revision(path.to_string_lossy().as_bytes())[..16]
    )
}
fn revision(content: &[u8]) -> String {
    // Node readFileSync(...,'utf8') replaces invalid UTF-8 before hashing.
    format!(
        "{:x}",
        Sha256::digest(String::from_utf8_lossy(content).as_bytes())
    )
}
fn filesystem_error(error: io::Error, operation: &str) -> String {
    use nix::errno::Errno;
    let code = error.raw_os_error().map(Errno::from_raw);
    let suffix = match code {
        Some(
            code @ (Errno::EACCES
            | Errno::EPERM
            | Errno::ENOENT
            | Errno::ENOTDIR
            | Errno::EISDIR
            | Errno::ENOSPC
            | Errno::EROFS
            | Errno::EMFILE
            | Errno::ENFILE
            | Errno::ELOOP
            | Errno::EIO
            | Errno::EEXIST),
        ) => format!(" ({code:?})"),
        _ => String::new(),
    };
    format!("Could not {operation} AutoRouter configuration{suffix}.")
}

pub async fn load_user_config(
    context: &ConfigContext<'_>,
    options: &LoadOptions,
    keychain: &mut impl Keychain,
) -> Result<LoadedConfig, String> {
    // Policy trust is checked before reading config or consulting Keychain.
    let policy = load_policy()?;
    load_with_policy(context, options, keychain, policy.as_ref()).await
}

/// Injected policy is an adapter boundary for isolated tests and embeddings.
/// Product commands load policy from the fixed system path before calling this
/// adapter, either via load_user_config or their private dependency wrapper.
pub async fn load_with_policy(
    context: &ConfigContext<'_>,
    options: &LoadOptions,
    keychain: &mut impl Keychain,
    policy: Option<&LoadedPolicy>,
) -> Result<LoadedConfig, String> {
    let path = get_config_path(context)?;
    let content = match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if context.env.contains_key(OsStr::new("AUTOROUTER_CONFIG")) && !options.allow_missing {
                return Err("AUTOROUTER_CONFIG points to a missing configuration file.".into());
            }
            None
        }
        Err(error) => return Err(filesystem_error(error, "read")),
    };
    let mut values_document = match content.as_ref() {
        Some(bytes) => JsDocument::parse(bytes)
            .map_err(|_| "AutoRouter configuration must contain valid JSON.")?,
        None => scalar_document(&Value::Object(Map::new())),
    };
    let mut values = validate_document(&values_document)?;
    let secret_store = values
        .get("AUTOROUTER_SECRET_STORE")
        .and_then(Value::as_str)
        .unwrap_or("file")
        .to_owned();
    let mut keychain_secrets = Vec::new();
    let mut unavailable_secrets = Vec::new();
    if secret_store == "keychain" && options.read_secrets {
        for key in SECRET_CONFIG_KEYS {
            if values.contains_key(key) {
                continue;
            }
            match keychain.read(&keychain_account(&path, key)).await {
                Ok(Some(value)) => {
                    values_document
                        .set_root_field_json(
                            key,
                            JsString::from_scalar(&value).stringify().as_bytes(),
                        )
                        .expect("scalar secret");
                    values.insert(key.into(), Value::String(value));
                    keychain_secrets.push(key.into());
                }
                Ok(None) => {}
                Err(_) if context.env.contains_key(OsStr::new(key)) => {
                    unavailable_secrets.push(key.into())
                }
                Err(error) => return Err(error),
            }
        }
    }
    let mut env: Environment = values
        .iter()
        .map(|(key, value)| (key.into(), value.as_str().unwrap_or_default().into()))
        .collect();
    env.extend(context.env.clone());
    let mut env_document = values_document.clone();
    for (key, value) in context.env {
        env_document
            .set_root_field_json(
                &key.to_string_lossy(),
                JsString::from_scalar(&value.to_string_lossy())
                    .stringify()
                    .as_bytes(),
            )
            .expect("scalar environment");
    }
    let mut policy_locked = Vec::new();
    if let Some(policy) = policy {
        let applied = apply_policy(
            &environment_json(&env),
            &policy.values,
            options.enforce_policy,
        )?;
        for key in applied["locked"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            env.insert(
                key.into(),
                applied["env"][key].as_str().unwrap_or_default().into(),
            );
            env_document
                .set_root_field_json(key, applied["env"][key].to_string().as_bytes())
                .expect("validated policy setting");
            policy_locked.push(key.to_owned());
        }
    }
    Ok(LoadedConfig {
        env,
        env_document,
        values,
        values_document,
        path,
        exists: content.is_some(),
        revision: content.as_deref().map(revision),
        secret_store,
        keychain_secrets,
        unavailable_secrets,
        policy: policy.map(|p| p.values.clone()),
        policy_path: policy.map(|p| p.path.clone()),
        policy_locked,
    })
}

/// Convert only string environment values for pure configuration evaluation;
/// process forwarding retains the original OS strings in LoadedConfig.env.
pub fn environment_json(env: &Environment) -> Value {
    Value::Object(
        env.iter()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    Value::String(value.to_string_lossy().into_owned()),
                )
            })
            .collect(),
    )
}

pub fn keychain_removals(
    loaded: &LoadedConfig,
    next_store: &str,
    replace: bool,
    values: &Map<String, Value>,
) -> Result<Vec<String>, String> {
    if loaded.secret_store != "keychain" {
        return Ok(Vec::new());
    }
    if next_store != "keychain" {
        if !loaded.unavailable_secrets.is_empty() {
            return Err("Could not read every saved secret from the macOS Keychain. Unlock the login keychain and retry.".into());
        }
        return Ok(loaded.keychain_secrets.clone());
    }
    Ok(if replace {
        loaded
            .keychain_secrets
            .iter()
            .filter(|key| !values.contains_key(*key))
            .cloned()
            .collect()
    } else {
        Vec::new()
    })
}

fn existing_file(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err("Refusing to write a symbolic-link AutoRouter configuration.".into())
        }
        Ok(metadata) if !metadata.is_file() => {
            Err("AutoRouter configuration must be a regular file.".into())
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(filesystem_error(error, "save")),
    }
}
fn check_revision(path: &Path, expected: &Option<Option<String>>) -> Result<(), String> {
    if let Some(expected) = expected {
        let actual = if existing_file(path)? {
            Some(revision(
                &fs::read(path).map_err(|e| filesystem_error(e, "save"))?,
            ))
        } else {
            None
        };
        if &actual != expected {
            return Err(CHANGED.into());
        }
    }
    Ok(())
}
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub async fn save_user_config(
    values: &Value,
    context: &ConfigContext<'_>,
    options: &SaveOptions,
    keychain: &mut impl Keychain,
) -> Result<PathBuf, String> {
    save_user_config_document(&scalar_document(values), context, options, keychain).await
}

pub async fn save_user_config_document(
    document: &JsDocument,
    context: &ConfigContext<'_>,
    options: &SaveOptions,
    keychain: &mut impl Keychain,
) -> Result<PathBuf, String> {
    let validated = validate_document(document)?;
    let store = validated
        .get("AUTOROUTER_SECRET_STORE")
        .and_then(Value::as_str)
        .unwrap_or("file");
    let keychain_keys: Vec<_> = SECRET_CONFIG_KEYS
        .into_iter()
        .filter(|key| store == "keychain" && validated.contains_key(*key))
        .collect();
    let mut file_values = validated.clone();
    for key in &keychain_keys {
        file_values.remove(*key);
    }
    let path = get_config_path(context)?;
    if existing_file(&path)? && !options.overwrite {
        return Err(EXISTS.into());
    }
    check_revision(&path, &options.expected_revision)?;
    if store == "keychain" && !keychain.available() {
        return Err(UNAVAILABLE.into());
    }
    for key in &keychain_keys {
        keychain
            .write(
                &keychain_account(&path, key),
                validated[*key].as_str().unwrap_or_default(),
                &format!("AutoRouter {key} ({})", path.to_string_lossy()),
            )
            .await?;
    }
    let parent = path
        .parent()
        .ok_or("Could not save AutoRouter configuration.")?;
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|e| filesystem_error(e, "save"))?;
    let mut random = [0u8; 12];
    getrandom::fill(&mut random).map_err(|_| "Could not save AutoRouter configuration.")?;
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let mut name = std::ffi::OsString::from(".");
    name.push(
        path.file_name()
            .ok_or("Could not save AutoRouter configuration.")?,
    );
    name.push(format!(".{}.{suffix}.tmp", std::process::id()));
    let temporary = Temporary(parent.join(name));
    // create_new is O_EXCL and refuses even dangling symlinks atomically.
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary.0)
        .map_err(|e| filesystem_error(e, "save"))?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|e| filesystem_error(e, "save"))?;
    let mut exact_file = scalar_document(&Value::Object(file_values.clone()));
    for key in file_values.keys() {
        let exact = document
            .get(document.root(), key)
            .and_then(|node| document.string(node))
            .expect("validated saved value");
        exact_file
            .set_root_field_json(key, exact.stringify().as_bytes())
            .expect("validated saved value");
    }
    let mut content = pretty_document(&exact_file).into_bytes();
    content.push(b'\n');
    file.write_all(&content)
        .map_err(|e| filesystem_error(e, "save"))?;
    file.sync_all().map_err(|e| filesystem_error(e, "save"))?;
    drop(file);
    if existing_file(&path)? && !options.overwrite {
        return Err(EXISTS.into());
    }
    check_revision(&path, &options.expected_revision)?;
    if options.overwrite {
        fs::rename(&temporary.0, &path).map_err(|e| filesystem_error(e, "save"))?;
    } else {
        fs::hard_link(&temporary.0, &path).map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                EXISTS.into()
            } else {
                filesystem_error(e, "save")
            }
        })?;
    }
    for key in &options.remove_secrets {
        if SECRET_CONFIG_KEYS.contains(&key.as_str()) && !keychain_keys.contains(&key.as_str()) {
            keychain.remove(&keychain_account(&path, key)).await?;
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::os::unix::fs::{MetadataExt, symlink};

    #[derive(Default)]
    struct MemoryKeychain {
        items: BTreeMap<String, String>,
        locked: bool,
        fail_write: bool,
        calls: usize,
    }
    impl Keychain for MemoryKeychain {
        fn available(&self) -> bool {
            true
        }
        async fn read(&mut self, account: &str) -> Result<Option<String>, String> {
            self.calls += 1;
            if self.locked {
                Err("The macOS Keychain is locked.".into())
            } else {
                Ok(self.items.get(account).cloned())
            }
        }
        async fn write(&mut self, account: &str, value: &str, _: &str) -> Result<(), String> {
            self.calls += 1;
            if self.locked || self.fail_write {
                return Err("Could not save an AutoRouter secret to the macOS Keychain.".into());
            }
            self.items.insert(account.into(), value.into());
            Ok(())
        }
        async fn remove(&mut self, account: &str) -> Result<(), String> {
            self.calls += 1;
            self.items.remove(account);
            Ok(())
        }
    }
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let mut random = [0; 12];
            getrandom::fill(&mut random).unwrap();
            let name: String = random.iter().map(|b| format!("{b:02x}")).collect();
            let path = std::env::temp_dir().join(format!("autorouter-native-config-{name}"));
            DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
        fn context<'a>(&'a self, env: &'a Environment) -> ConfigContext<'a> {
            ConfigContext {
                env,
                cwd: &self.0,
                home: &self.0,
            }
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    async fn load(
        context: &ConfigContext<'_>,
        keychain: &mut MemoryKeychain,
    ) -> Result<LoadedConfig, String> {
        load_with_policy(context, &LoadOptions::default(), keychain, None).await
    }

    #[tokio::test]
    async fn paths_missing_files_and_environment_precedence_match_existing_behavior() {
        let directory = Directory::new();
        let mut env = Environment::from([
            ("TYPESAFE_API_KEY".into(), "environment-key".into()),
            ("PATH".into(), "/synthetic-test-path".into()),
        ]);
        let mut keychain = MemoryKeychain::default();
        let context = directory.context(&env);
        assert_eq!(
            get_config_path(&context).unwrap(),
            directory.0.join(".config/claude-autorouter/config.json")
        );
        let mut missing = load(&context, &mut keychain).await.unwrap();
        assert!(!missing.exists);
        assert_eq!(missing.path, get_config_path(&context).unwrap());
        assert_eq!(missing.env, env);
        missing.env.insert("COPY_ONLY".into(), "1".into());
        assert!(!env.contains_key(OsStr::new("COPY_ONLY")));
        fs::write(directory.0.join(".env"), "TYPESAFE_API_KEY=project-key").unwrap();
        save_user_config(
            &json!({"TYPESAFE_API_KEY":"synthetic-saved","AUTOROUTER_PORT":"8123"}),
            &context,
            &SaveOptions::default(),
            &mut keychain,
        )
        .await
        .unwrap();
        env.insert("TYPESAFE_API_KEY".into(), "synthetic-environment".into());
        let loaded = load(&directory.context(&env), &mut keychain).await.unwrap();
        assert_eq!(
            loaded.env[OsStr::new("TYPESAFE_API_KEY")],
            "synthetic-environment"
        );
        assert_eq!(loaded.values["TYPESAFE_API_KEY"], "synthetic-saved");
        assert_eq!(loaded.env[OsStr::new("AUTOROUTER_PORT")], "8123");
        assert!(!env.contains_key(OsStr::new("AUTOROUTER_PORT")));
        env.insert(
            "AUTOROUTER_CONFIG".into(),
            "./chosen/../missing.json".into(),
        );
        assert_eq!(
            get_config_path(&directory.context(&env)).unwrap(),
            directory.0.join("missing.json")
        );
        assert!(
            load(&directory.context(&env), &mut keychain)
                .await
                .unwrap_err_message()
                .contains("missing configuration")
        );
        let missing = load_with_policy(
            &directory.context(&env),
            &LoadOptions {
                allow_missing: true,
                ..Default::default()
            },
            &mut keychain,
            None,
        )
        .await
        .unwrap();
        assert_eq!(missing.revision, None);
        assert_eq!(missing.env, env);
        env.insert("AUTOROUTER_CONFIG".into(), "".into());
        assert!(
            get_config_path(&directory.context(&env))
                .unwrap_err()
                .contains("non-empty")
        );
        env.remove(OsStr::new("AUTOROUTER_CONFIG"));
        env.insert("XDG_CONFIG_HOME".into(), "./relative".into());
        assert!(
            get_config_path(&directory.context(&env))
                .unwrap_err()
                .contains("absolute path")
        );
        env.insert(
            "XDG_CONFIG_HOME".into(),
            directory.0.join("xdg").into_os_string(),
        );
        assert_eq!(
            get_config_path(&directory.context(&env)).unwrap(),
            directory.0.join("xdg/claude-autorouter/config.json")
        );
        env.insert(
            "AUTOROUTER_CONFIG".into(),
            directory.0.join("custom.json").into_os_string(),
        );
        assert_eq!(
            get_config_path(&directory.context(&env)).unwrap(),
            directory.0.join("custom.json")
        );
    }
    #[tokio::test]
    async fn saved_utf16_survives_edits_and_environment_overrides_without_identity_aliases() {
        let directory = Directory::new();
        let mut env = Environment::from([(
            "AUTOROUTER_CONFIG".into(),
            directory.0.join("config.json").into_os_string(),
        )]);
        let path = directory.0.join("config.json");
        fs::write(&path, br#"{"AUTOROUTER_HAIKU_MODEL":"custom\ud800","AUTOROUTER_JEV_MODEL":"jev\udfff","AUTOROUTER_EVALUATOR":"jev"}"#).unwrap();
        let mut keychain = MemoryKeychain::default();
        let loaded = load(&directory.context(&env), &mut keychain).await.unwrap();
        let config = autorouter_core::config::read_config_document(
            &loaded.env_document,
            false,
            &directory.0,
        )
        .unwrap();
        assert_eq!(
            config.exact_models["haiku"].stringify(),
            r#""custom\ud800""#
        );
        assert_eq!(
            config.exact_jev_model.unwrap().stringify(),
            r#""jev\udfff""#
        );
        let mut next = loaded.values.clone();
        next.insert("AUTOROUTER_PORT".into(), json!("8000"));
        save_user_config_document(
            &loaded.updated_document(&next, &["AUTOROUTER_PORT"]),
            &directory.context(&env),
            &SaveOptions {
                overwrite: true,
                expected_revision: Some(loaded.revision.clone()),
                ..Default::default()
            },
            &mut keychain,
        )
        .await
        .unwrap();
        let saved = String::from_utf8(fs::read(&path).unwrap()).unwrap();
        assert!(saved.contains(r#""custom\ud800""#));
        assert!(saved.contains(r#""jev\udfff""#));
        assert!(saved.contains("\n  \"AUTOROUTER_PORT\": \"8000\"\n"));
        env.insert("AUTOROUTER_HAIKU_MODEL".into(), "custom\u{fffd}".into());
        let overridden = load(&directory.context(&env), &mut keychain).await.unwrap();
        let config = autorouter_core::config::read_config_document(
            &overridden.env_document,
            false,
            &directory.0,
        )
        .unwrap();
        assert!(!config.exact_models.contains_key("haiku"));
        let replacement = loaded.updated_document(&next, &["AUTOROUTER_HAIKU_MODEL"]);
        assert!(replacement.stringify().contains("custom\u{fffd}"));
        assert!(!replacement.stringify().contains(r#"custom\ud800"#));
    }
    // A test assertion helper keeps secret-bearing successful values out of Debug.
    trait ErrorMessage {
        fn unwrap_err_message(self) -> String;
    }
    impl<T> ErrorMessage for Result<T, String> {
        fn unwrap_err_message(self) -> String {
            match self {
                Err(e) => e,
                Ok(_) => panic!("Expected a content-free configuration error"),
            }
        }
    }

    #[tokio::test]
    async fn private_atomic_files_reject_stale_revisions_and_all_symlinks() {
        let directory = Directory::new();
        fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o755)).unwrap();
        let env = Environment::from([(
            "AUTOROUTER_CONFIG".into(),
            directory.0.join("new/config.json").into_os_string(),
        )]);
        let context = directory.context(&env);
        let mut keychain = MemoryKeychain::default();
        let path = save_user_config(
            &json!({"AUTOROUTER_PORT":"8000"}),
            &context,
            &SaveOptions {
                expected_revision: Some(None),
                ..Default::default()
            },
            &mut keychain,
        )
        .await
        .unwrap();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o777,
            0o700
        );
        assert_eq!(fs::metadata(&directory.0).unwrap().mode() & 0o777, 0o755);
        let snapshot = load(&context, &mut keychain).await.unwrap();
        let revision = snapshot.revision.as_ref().unwrap();
        assert_eq!(revision.len(), 64);
        assert!(
            revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        let inode = fs::metadata(&path).unwrap().ino();
        assert_eq!(
            save_user_config(&json!({}), &context, &SaveOptions::default(), &mut keychain)
                .await
                .unwrap_err(),
            EXISTS
        );
        assert_eq!(
            load(&context, &mut keychain).await.unwrap().values,
            snapshot.values
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        save_user_config(
            &json!({"AUTOROUTER_PORT":"7000"}),
            &context,
            &SaveOptions {
                overwrite: true,
                ..Default::default()
            },
            &mut keychain,
        )
        .await
        .unwrap();
        assert_ne!(fs::metadata(&path).unwrap().ino(), inode);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            save_user_config(
                &json!({"AUTOROUTER_PORT":"6000"}),
                &context,
                &SaveOptions {
                    overwrite: true,
                    expected_revision: Some(snapshot.revision),
                    ..Default::default()
                },
                &mut keychain
            )
            .await
            .unwrap_err(),
            CHANGED
        );
        assert_eq!(
            load(&context, &mut keychain).await.unwrap().values["AUTOROUTER_PORT"],
            "7000"
        );
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        let victim = directory.0.join("victim");
        fs::write(&victim, "original contents").unwrap();
        for target in [&victim, &directory.0.join("dangling")] {
            fs::remove_file(&path).unwrap();
            symlink(target, &path).unwrap();
            for overwrite in [false, true] {
                assert!(
                    save_user_config(
                        &json!({}),
                        &context,
                        &SaveOptions {
                            overwrite,
                            ..Default::default()
                        },
                        &mut keychain
                    )
                    .await
                    .unwrap_err()
                    .contains("symbolic-link")
                );
            }
            assert!(
                fs::symlink_metadata(&path)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(fs::read(&victim).unwrap(), b"original contents");
            assert!(!directory.0.join("dangling").exists());
        }
    }

    #[tokio::test]
    async fn both_store_migrations_preserve_secrets_and_failed_writes_leave_original_intact() {
        let directory = Directory::new();
        let env = Environment::new();
        let context = directory.context(&env);
        let mut keychain = MemoryKeychain::default();
        let saved = json!({"ANTHROPIC_API_KEY":"synthetic-private","AUTOROUTER_PORT":"8000"});
        let path = save_user_config(&saved, &context, &SaveOptions::default(), &mut keychain)
            .await
            .unwrap();
        let before = fs::read(&path).unwrap();
        let mut next = saved.clone();
        next["AUTOROUTER_SECRET_STORE"] = json!("keychain");
        keychain.fail_write = true;
        assert!(
            save_user_config(
                &next,
                &context,
                &SaveOptions {
                    overwrite: true,
                    ..Default::default()
                },
                &mut keychain
            )
            .await
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        keychain.fail_write = false;
        save_user_config(
            &next,
            &context,
            &SaveOptions {
                overwrite: true,
                ..Default::default()
            },
            &mut keychain,
        )
        .await
        .unwrap();
        assert!(
            !String::from_utf8(fs::read(&path).unwrap())
                .unwrap()
                .contains("synthetic-private")
        );
        let loaded = load(&context, &mut keychain).await.unwrap();
        assert_eq!(loaded.values["ANTHROPIC_API_KEY"], "synthetic-private");
        assert_eq!(loaded.keychain_secrets, ["ANTHROPIC_API_KEY"]);
        let removals = keychain_removals(&loaded, "file", false, &Map::new()).unwrap();
        save_user_config(
            &saved,
            &context,
            &SaveOptions {
                overwrite: true,
                remove_secrets: removals,
                ..Default::default()
            },
            &mut keychain,
        )
        .await
        .unwrap();
        assert!(keychain.items.is_empty());
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[tokio::test]
    async fn unavailable_keychain_can_be_overridden_but_not_destructively_migrated() {
        let directory = Directory::new();
        let mut env = Environment::new();
        let mut keychain = MemoryKeychain::default();
        save_user_config(
            &json!({"AUTOROUTER_SECRET_STORE":"keychain", "TYPESAFE_API_KEY":"synthetic-private"}),
            &directory.context(&env),
            &SaveOptions::default(),
            &mut keychain,
        )
        .await
        .unwrap();
        keychain.locked = true;
        assert!(load(&directory.context(&env), &mut keychain).await.is_err());
        for key in SECRET_CONFIG_KEYS {
            env.insert(key.into(), "synthetic-environment".into());
        }
        let loaded = load(&directory.context(&env), &mut keychain).await.unwrap();
        assert_eq!(loaded.unavailable_secrets, SECRET_CONFIG_KEYS);
        assert!(
            keychain_removals(&loaded, "file", false, &Map::new())
                .unwrap_err()
                .contains("every saved secret")
        );
        let before = keychain.calls;
        let no_secrets = load_with_policy(
            &directory.context(&env),
            &LoadOptions {
                read_secrets: false,
                ..Default::default()
            },
            &mut keychain,
            None,
        )
        .await
        .unwrap();
        assert_eq!(before, keychain.calls);
        assert!(!no_secrets.values.contains_key("TYPESAFE_API_KEY"));
        assert_eq!(keychain.items.len(), 1);
        assert_ne!(
            keychain_account(&directory.0.join("first"), "TYPESAFE_API_KEY"),
            keychain_account(&directory.0.join("second"), "TYPESAFE_API_KEY")
        );
        assert_eq!(
            keychain_account(Path::new("/tmp/config.json"), "KEY"),
            "KEY:6fbdb03fcb005a7d"
        );
    }

    #[tokio::test]
    async fn invalid_content_and_hostile_paths_fail_without_secret_disclosure_or_side_effects() {
        let directory = Directory::new();
        let mut keychain = MemoryKeychain::default();
        let mut env = Environment::from([(
            "AUTOROUTER_CONFIG".into(),
            directory.0.join("config.json").into_os_string(),
        )]);
        for content in [
            "{\"PRIVATE_KEY_NAME\":\"x\"}",
            "{\"TYPESAFE_API_KEY\":{\"PRIVATE_VALUE\":1}}",
            "{\"TYPESAFE_API_KEY\":\"PRIVATE_VALUE\",}",
            "[]",
            "null",
            "\"a string\"",
            "{\"__proto__\":{\"polluted\":\"value\"}}",
        ] {
            fs::write(directory.0.join("config.json"), content).unwrap();
            let error = load(&directory.context(&env), &mut keychain)
                .await
                .unwrap_err_message();
            assert!(!error.contains("PRIVATE"));
        }
        for name in [
            "x\ndelete-keychain",
            "x\r",
            "x\0",
            "x\u{1b}",
            "x\u{85}",
            "x\u{2028}",
            "x\u{2029}",
            "x\u{202e}",
        ] {
            for key in ["AUTOROUTER_CONFIG", "XDG_CONFIG_HOME"] {
                let hostile =
                    Environment::from([(key.into(), directory.0.join(name).into_os_string())]);
                let context = directory.context(&hostile);
                let error = get_config_path(&context).unwrap_err();
                assert!(error.contains("control characters"));
                assert!(!error.contains(&directory.0.to_string_lossy().to_string()));
                assert!(!error.contains("synthetic"));
                assert!(
                    load_with_policy(
                        &context,
                        &LoadOptions {
                            allow_missing: true,
                            ..Default::default()
                        },
                        &mut keychain,
                        None
                    )
                    .await
                    .unwrap_err_message()
                    .contains("control characters")
                );
                assert!(save_user_config(
                    &json!({"AUTOROUTER_SECRET_STORE":"keychain", "TYPESAFE_API_KEY":"synthetic"}),
                    &context,
                    &SaveOptions::default(),
                    &mut keychain
                )
                .await
                .unwrap_err()
                .contains("control characters")
                );
            }
        }
        assert_eq!(keychain.calls, 0);
        let valid = directory.0.join("Müller 設定/config.json");
        let unicode =
            Environment::from([("AUTOROUTER_CONFIG".into(), valid.clone().into_os_string())]);
        assert_eq!(
            get_config_path(&directory.context(&unicode)).unwrap(),
            valid
        );
        let invalid = Environment::from([(
            "AUTOROUTER_CONFIG".into(),
            directory.0.join("new/config.json").into_os_string(),
        )]);
        for values in [
            json!({"PRIVATE_KEY":"value"}),
            json!({"TYPESAFE_API_KEY":{"private":"PRIVATE_VALUE"}}),
            json!([]),
            Value::Null,
        ] {
            assert!(
                !save_user_config(
                    &values,
                    &directory.context(&invalid),
                    &SaveOptions::default(),
                    &mut keychain
                )
                .await
                .unwrap_err()
                .contains("PRIVATE")
            );
        }
        assert!(!directory.0.join("new").exists());
        let private_path = directory.0.join("PRIVATE_PATH");
        fs::write(&private_path, "file").unwrap();
        env.insert(
            "AUTOROUTER_CONFIG".into(),
            private_path.join("config.json").into_os_string(),
        );
        assert!(
            !load(&directory.context(&env), &mut keychain)
                .await
                .unwrap_err_message()
                .contains("PRIVATE")
        );
        assert!(
            !save_user_config(
                &json!({}),
                &directory.context(&env),
                &SaveOptions::default(),
                &mut keychain
            )
            .await
            .unwrap_err()
            .contains("PRIVATE")
        );
    }

    #[tokio::test]
    async fn saved_settings_and_stop_cap_keep_environment_precedence_without_mutation() {
        let directory = Directory::new();
        let mut env = Environment::from([
            (
                "XDG_CONFIG_HOME".into(),
                directory.0.clone().into_os_string(),
            ),
            ("TYPESAFE_API_KEY".into(), "environment-key".into()),
            ("AUTOROUTER_PORT".into(), "9000".into()),
            ("PATH".into(), "/synthetic-test-path".into()),
        ]);
        let before = env.clone();
        let mut keychain = MemoryKeychain::default();
        let saved = json!({"TYPESAFE_API_KEY":"saved-key","AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_PORT":"8787","ENABLE_TOOL_SEARCH":"auto:5","CLAUDE_CODE_STOP_HOOK_BLOCK_CAP":"2"});
        let path = save_user_config(
            &saved,
            &directory.context(&env),
            &SaveOptions::default(),
            &mut keychain,
        )
        .await
        .unwrap();
        let original = fs::read(&path).unwrap();
        let loaded = load(&directory.context(&env), &mut keychain).await.unwrap();
        let mut expected: Environment = saved
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.into(), v.as_str().unwrap().into()))
            .collect();
        expected.extend(env.clone());
        assert!(loaded.exists);
        assert_eq!(loaded.path, path);
        assert_eq!(loaded.env, expected);
        assert_eq!(env, before);
        assert_eq!(serde_json::from_slice::<Value>(&original).unwrap(), saved);
        let config = autorouter_core::config::read_config_document(
            &loaded.env_document,
            false,
            &directory.0,
        )
        .unwrap();
        assert_eq!(
            loaded.env[OsStr::new("CLAUDE_CODE_STOP_HOOK_BLOCK_CAP")],
            "2"
        );
        assert_eq!(config.stop_hook_block_cap, Some(2));
        env.insert("CLAUDE_CODE_STOP_HOOK_BLOCK_CAP".into(), "0".into());
        let before = env.clone();
        let overridden = load(&directory.context(&env), &mut keychain).await.unwrap();
        assert_eq!(
            autorouter_core::config::read_config_document(
                &overridden.env_document,
                false,
                &directory.0
            )
            .unwrap()
            .stop_hook_block_cap,
            Some(0)
        );
        assert_eq!(env, before);
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(
            save_user_config(
                &json!({"CLAUDE_CODE_STOP_HOOK_BLOCK_CAP":2}),
                &directory.context(&env),
                &SaveOptions {
                    overwrite: true,
                    ..Default::default()
                },
                &mut keychain
            )
            .await
            .unwrap_err()
            .contains("values must be strings")
        );
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[tokio::test]
    async fn global_settings_ignore_both_repository_files_and_create_private_xdg_ancestors() {
        let directory = Directory::new();
        fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o755)).unwrap();
        let xdg = directory.0.join("new-parent");
        let env = Environment::from([("XDG_CONFIG_HOME".into(), xdg.clone().into_os_string())]);
        let mut keychain = MemoryKeychain::default();
        let path = save_user_config(
            &json!({"TYPESAFE_API_KEY":"user-key"}),
            &directory.context(&env),
            &SaveOptions::default(),
            &mut keychain,
        )
        .await
        .unwrap();
        assert_eq!(fs::metadata(&directory.0).unwrap().mode() & 0o777, 0o755);
        for parent in [&xdg, path.parent().unwrap()] {
            assert_eq!(fs::metadata(parent).unwrap().mode() & 0o777, 0o700);
        }
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            fs::read_dir(path.parent().unwrap())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>(),
            ["config.json"]
        );
        for name in ["first-repository", "second-repository"] {
            let cwd = directory.0.join(name);
            fs::create_dir(&cwd).unwrap();
            fs::write(
                cwd.join(".env"),
                "TYPESAFE_API_KEY=repository-key\nAUTOROUTER_AUTH_MODE=subscription\n",
            )
            .unwrap();
            fs::write(
                cwd.join("config.json"),
                r#"{"TYPESAFE_API_KEY":"repository-key"}"#,
            )
            .unwrap();
            let loaded = load(
                &ConfigContext {
                    env: &env,
                    cwd: &cwd,
                    home: &directory.0,
                },
                &mut keychain,
            )
            .await
            .unwrap();
            assert_eq!(loaded.env[OsStr::new("TYPESAFE_API_KEY")], "user-key");
            assert!(!loaded.env.contains_key(OsStr::new("AUTOROUTER_AUTH_MODE")));
            assert_eq!(loaded.path, path);
        }
    }

    #[tokio::test]
    async fn snapshots_distinguish_saved_values_from_overrides_and_reject_stale_replacements() {
        let directory = Directory::new();
        let path = directory.0.join("config.json");
        let env = Environment::from([
            ("AUTOROUTER_CONFIG".into(), path.clone().into_os_string()),
            ("AUTOROUTER_PORT".into(), "9000".into()),
        ]);
        let context = directory.context(&env);
        let mut keychain = MemoryKeychain::default();
        let missing = load_with_policy(
            &context,
            &LoadOptions {
                allow_missing: true,
                ..Default::default()
            },
            &mut keychain,
            None,
        )
        .await
        .unwrap();
        assert_eq!(missing.revision, None);
        assert!(missing.values.is_empty());
        save_user_config(
            &json!({"AUTOROUTER_PORT":"8000"}),
            &context,
            &SaveOptions {
                expected_revision: Some(missing.revision),
                ..Default::default()
            },
            &mut keychain,
        )
        .await
        .unwrap();
        let snapshot = load(&context, &mut keychain).await.unwrap();
        assert_eq!(
            Value::Object(snapshot.values),
            json!({"AUTOROUTER_PORT":"8000"})
        );
        assert_eq!(snapshot.env[OsStr::new("AUTOROUTER_PORT")], "9000");
        let revision = snapshot.revision.as_ref().unwrap();
        assert_eq!(revision.len(), 64);
        assert!(
            revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        save_user_config(
            &json!({"AUTOROUTER_PORT":"7000"}),
            &context,
            &SaveOptions {
                overwrite: true,
                ..Default::default()
            },
            &mut keychain,
        )
        .await
        .unwrap();
        assert_eq!(
            save_user_config(
                &json!({"AUTOROUTER_PORT":"6000"}),
                &context,
                &SaveOptions {
                    overwrite: true,
                    expected_revision: Some(snapshot.revision),
                    ..Default::default()
                },
                &mut keychain
            )
            .await
            .unwrap_err(),
            CHANGED
        );
        assert_eq!(
            load(&context, &mut keychain).await.unwrap().values["AUTOROUTER_PORT"],
            "7000"
        );
        assert_eq!(
            fs::read_dir(&directory.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>(),
            ["config.json"]
        );
    }

    #[tokio::test]
    async fn keychain_secrets_are_absent_from_disk_scoped_to_the_path_and_overridden_by_environment()
     {
        let directory = Directory::new();
        let mut env = Environment::from([(
            "AUTOROUTER_CONFIG".into(),
            directory.0.join("config.json").into_os_string(),
        )]);
        let mut keychain = MemoryKeychain::default();
        let path = save_user_config(&json!({"AUTOROUTER_SECRET_STORE":"keychain","AUTOROUTER_AUTH_MODE":"api-key","ANTHROPIC_API_KEY":"private-provider","TYPESAFE_API_KEY":"private-jev"}), &directory.context(&env), &SaveOptions::default(), &mut keychain).await.unwrap();
        let contents = fs::read(&path).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&contents).unwrap(),
            json!({"AUTOROUTER_SECRET_STORE":"keychain","AUTOROUTER_AUTH_MODE":"api-key"})
        );
        assert!(!String::from_utf8(contents).unwrap().contains("private-"));
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(keychain.items.len(), 2);
        let loaded = load(&directory.context(&env), &mut keychain).await.unwrap();
        assert_eq!(
            loaded.env[OsStr::new("ANTHROPIC_API_KEY")],
            "private-provider"
        );
        assert_eq!(loaded.values["TYPESAFE_API_KEY"], "private-jev");
        assert_eq!(
            loaded.keychain_secrets,
            ["ANTHROPIC_API_KEY", "TYPESAFE_API_KEY"]
        );
        assert!(!loaded.env.contains_key(OsStr::new("AUTOROUTER_TOKEN")));
        let other_env = Environment::from([(
            "AUTOROUTER_CONFIG".into(),
            directory.0.join("other.json").into_os_string(),
        )]);
        save_user_config(
            &json!({"AUTOROUTER_SECRET_STORE":"keychain"}),
            &directory.context(&other_env),
            &SaveOptions::default(),
            &mut keychain,
        )
        .await
        .unwrap();
        let other = load(&directory.context(&other_env), &mut keychain)
            .await
            .unwrap();
        assert!(other.keychain_secrets.is_empty());
        assert!(!other.env.contains_key(OsStr::new("ANTHROPIC_API_KEY")));
        env.insert("ANTHROPIC_API_KEY".into(), "environment-provider".into());
        assert_eq!(
            load(&directory.context(&env), &mut keychain)
                .await
                .unwrap()
                .env[OsStr::new("ANTHROPIC_API_KEY")],
            "environment-provider"
        );
        assert_eq!(keychain.items.len(), 2);
    }

    #[tokio::test]
    async fn saved_and_environment_policy_locks_survive_allowlist_repair_mode() {
        let directory = Directory::new();
        let mut env = Environment::new();
        let mut keychain = MemoryKeychain::default();
        save_user_config(&json!({"AUTOROUTER_SESSION_LOG_MODE":"prompts","AUTOROUTER_SESSION_LOG_DIR":directory.0.join("logs")}), &directory.context(&env), &SaveOptions::default(), &mut keychain).await.unwrap();
        env.insert("AUTOROUTER_SESSION_LOG_MODE".into(), "prompts".into());
        env.insert(
            "AUTOROUTER_UPSTREAM_URL".into(),
            "https://proxy.example".into(),
        );
        let before = env.clone();
        let policy = LoadedPolicy {
            path: directory.0.join("synthetic-policy.json"),
            values: json!({"allowed_evaluators":["ollama"],"session_log_mode":"metadata","upstream_url":"https://api.anthropic.com"}),
        };
        let loaded = load_with_policy(
            &directory.context(&env),
            &LoadOptions::default(),
            &mut keychain,
            Some(&policy),
        )
        .await
        .unwrap();
        let config = autorouter_core::config::read_config_document(
            &loaded.env_document,
            false,
            &directory.0,
        )
        .unwrap();
        assert!(config.session_log_mode == autorouter_core::config::SessionLogMode::Metadata);
        assert_eq!(config.upstream, "https://api.anthropic.com");
        assert_eq!(
            loaded.policy_locked,
            ["AUTOROUTER_SESSION_LOG_MODE", "AUTOROUTER_UPSTREAM_URL"]
        );
        assert_eq!(loaded.policy_path.as_ref(), Some(&policy.path));
        assert_eq!(env, before);
        env.insert("AUTOROUTER_EVALUATOR".into(), "jev".into());
        assert!(
            load_with_policy(
                &directory.context(&env),
                &LoadOptions::default(),
                &mut keychain,
                Some(&policy)
            )
            .await
            .unwrap_err_message()
            .contains("AUTOROUTER_EVALUATOR is not permitted")
        );
        let repaired = load_with_policy(
            &directory.context(&env),
            &LoadOptions {
                enforce_policy: false,
                ..Default::default()
            },
            &mut keychain,
            Some(&policy),
        )
        .await
        .unwrap();
        assert_eq!(repaired.env[OsStr::new("AUTOROUTER_EVALUATOR")], "jev");
        assert_eq!(
            repaired.policy_locked,
            ["AUTOROUTER_SESSION_LOG_MODE", "AUTOROUTER_UPSTREAM_URL"]
        );
    }

    #[tokio::test]
    async fn organization_locks_override_values_and_environment_including_repair_mode() {
        let directory = Directory::new();
        let env = Environment::from([("AUTOROUTER_SESSION_LOG_MODE".into(), "prompts".into())]);
        let mut keychain = MemoryKeychain::default();
        let policy = LoadedPolicy {
            path: PathBuf::from("/synthetic-policy"),
            values: json!({"session_log_mode":"metadata","allowed_auth_modes":["subscription"]}),
        };
        assert!(
            load_with_policy(
                &directory.context(&env),
                &LoadOptions::default(),
                &mut keychain,
                Some(&policy)
            )
            .await
            .is_err()
        );
        let loaded = load_with_policy(
            &directory.context(&env),
            &LoadOptions {
                enforce_policy: false,
                ..Default::default()
            },
            &mut keychain,
            Some(&policy),
        )
        .await
        .unwrap();
        assert_eq!(
            loaded.env[OsStr::new("AUTOROUTER_SESSION_LOG_MODE")],
            "metadata"
        );
        assert_eq!(loaded.policy_locked, ["AUTOROUTER_SESSION_LOG_MODE"]);
        assert_eq!(env[OsStr::new("AUTOROUTER_SESSION_LOG_MODE")], "prompts");
    }
}
