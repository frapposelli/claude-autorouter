//! Exact finite setup inputs from frozen keychain.test.mjs #8, #11 and #12.
//! Platform and prompt are private adapters; no system Keychain is constructed.
use super::{SecretPrompt, setup, setup_with_prompt};
use autorouter_core::auth::Environment;
use autorouter_runtime::keychain::Keychain;
use autorouter_runtime::user_config::{ConfigContext, SaveOptions, save_user_config};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, DirBuilder};
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
struct Trace(Arc<Mutex<Vec<String>>>);
impl Trace {
    fn push(&self, event: String) {
        let mut events = self.0.lock().unwrap();
        assert!(events.len() < 128 && event.len() < 2048);
        events.push(event);
    }
    fn events(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
    fn assert_no_prompt(&self) {
        assert!(
            self.events()
                .iter()
                .all(|event| !event.starts_with("prompt:"))
        );
    }
}

#[derive(Default)]
struct MemoryKeychain {
    items: BTreeMap<String, String>,
    locked: bool,
    trace: Trace,
}
impl MemoryKeychain {
    fn record(&self, operation: &str, account: &str) {
        assert!(account.len() <= 512 && self.items.len() <= 3);
        self.trace.push(format!("{operation}:{account}"));
    }
    fn values(&self) -> Vec<&str> {
        self.items.values().map(String::as_str).collect()
    }
}
impl Keychain for MemoryKeychain {
    fn available(&self) -> bool {
        true
    }
    async fn read(&mut self, account: &str) -> Result<Option<String>, String> {
        self.record("read", account);
        if self.locked {
            Err("The macOS Keychain is locked.".into())
        } else {
            Ok(self.items.get(account).cloned())
        }
    }
    async fn write(&mut self, account: &str, value: &str, label: &str) -> Result<(), String> {
        self.record("write", account);
        assert!(value.len() <= 512 && label.len() <= 1024);
        if self.locked {
            return Err("The macOS Keychain is locked.".into());
        }
        self.items.insert(account.into(), value.into());
        Ok(())
    }
    async fn remove(&mut self, account: &str) -> Result<(), String> {
        self.record("remove", account);
        self.items.remove(account);
        Ok(())
    }
}
struct ForbiddenPrompt(Trace);
impl SecretPrompt for ForbiddenPrompt {
    async fn read(&mut self, key: &str) -> Result<String, String> {
        self.0.push(format!("prompt:{key}"));
        Err("Synthetic unexpected secret prompt".into())
    }
}

struct Fixture {
    directory: PathBuf,
    path: PathBuf,
    env: Environment,
}
impl Fixture {
    fn new(secret: Option<&str>) -> Self {
        assert_eq!(
            fs::symlink_metadata(autorouter_runtime::policy::default_policy_path())
                .err()
                .map(|error| error.kind()),
            Some(ErrorKind::NotFound),
            "Synthetic setup contracts require an absent system policy"
        );
        let mut random = [0u8; 12];
        getrandom::fill(&mut random).unwrap();
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let directory = std::env::temp_dir().join(format!("autorouter-setup-keychain-{suffix}"));
        DirBuilder::new().mode(0o700).create(&directory).unwrap();
        let path = directory.join("config.json");
        let mut env = Environment::from([
            ("AUTOROUTER_EVALUATOR".into(), "jev".into()),
            ("AUTOROUTER_CONFIG".into(), path.clone().into_os_string()),
            // Doctor must take its real unavailable-command path, never the
            // user's installed Claude or startup configuration.
            ("PATH".into(), directory.clone().into_os_string()),
            ("HOME".into(), directory.clone().into_os_string()),
        ]);
        if let Some(secret) = secret {
            env.insert("TYPESAFE_API_KEY".into(), secret.into());
        }
        Self {
            directory,
            path,
            env,
        }
    }
    fn context(&self) -> ConfigContext<'_> {
        ConfigContext {
            env: &self.env,
            cwd: &self.directory,
            home: &self.directory,
        }
    }
    fn saved(&self) -> Value {
        self.assert_private();
        serde_json::from_slice(&fs::read(&self.path).unwrap()).unwrap()
    }
    fn assert_private(&self) {
        for (path, mode) in [(&self.directory, 0o700), (&self.path, 0o600)] {
            let metadata = fs::symlink_metadata(path).unwrap();
            assert!(!metadata.file_type().is_symlink());
            assert_eq!(metadata.mode() & 0o777, mode);
            assert_eq!(metadata.uid(), nix::unistd::geteuid().as_raw());
        }
        assert!(fs::metadata(&self.path).unwrap().len() <= 4096);
        assert_eq!(fs::read_dir(&self.directory).unwrap().count(), 1);
    }
    fn assert_absent(&self) {
        assert_eq!(
            fs::symlink_metadata(&self.path).unwrap_err().kind(),
            ErrorKind::NotFound
        );
        assert_eq!(fs::read_dir(&self.directory).unwrap().count(), 0);
    }
    async fn run(
        &self,
        args: &[&str],
        macos: bool,
        keychain: &mut MemoryKeychain,
    ) -> (Result<(), String>, Vec<String>) {
        let mut lines = Vec::new();
        let trace = keychain.trace.clone();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            setup_with_prompt(
                &args.iter().map(OsString::from).collect::<Vec<_>>(),
                &self.context(),
                keychain,
                &CancellationToken::new(),
                &mut |line| {
                    trace.push(format!("output:{line}"));
                    lines.push(line);
                },
                macos,
                &mut ForbiddenPrompt(trace.clone()),
            ),
        )
        .await
        .expect("Synthetic setup deadline");
        trace.assert_no_prompt();
        assert!(!lines.join("\n").contains("private-"));
        if let Err(error) = &result {
            assert!(!error.contains("private-"));
        }
        (result, lines)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[tokio::test]
async fn explicit_store_checks_precede_prompt_and_forced_file_update_removes_items() {
    let fixture = Fixture::new(Some("private-jev"));
    let mut keychain = MemoryKeychain::default();
    let (result, lines) = fixture
        .run(&["--secret-store", "keychain"], true, &mut keychain)
        .await;
    result.unwrap();
    assert_eq!(
        fixture.saved(),
        json!({"AUTOROUTER_AUTH_MODE":"subscription", "AUTOROUTER_CLIENT_PROFILE":"compatible", "AUTOROUTER_EVALUATOR":"jev", "AUTOROUTER_SECRET_STORE":"keychain"})
    );
    assert!(
        !fs::read_to_string(&fixture.path)
            .unwrap()
            .contains("private-")
    );
    assert_eq!(keychain.values(), ["private-jev"]);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("Keys are stored in the macOS Keychain"))
    );

    let linux = Fixture::new(None);
    let before = keychain.trace.events();
    let error = linux
        .run(&["--secret-store", "keychain"], false, &mut keychain)
        .await
        .0
        .unwrap_err();
    assert!(error.contains("only on macOS"));
    linux.assert_absent();
    assert!(
        keychain.trace.events()[before.len()..]
            .iter()
            .all(|event| event.starts_with("output:"))
    );
    let error = linux
        .run(&["--secret-store", "vault"], false, &mut keychain)
        .await
        .0
        .unwrap_err();
    assert!(error.contains("file or keychain"));
    linux.assert_absent();

    fixture
        .run(&["--force", "--secret-store", "file"], true, &mut keychain)
        .await
        .0
        .unwrap();
    assert_eq!(fixture.saved()["TYPESAFE_API_KEY"], "private-jev");
    assert_eq!(fixture.saved()["AUTOROUTER_SECRET_STORE"], "file");
    assert!(keychain.items.is_empty());
    assert!(
        keychain
            .trace
            .events()
            .iter()
            .any(|event| event.starts_with("remove:"))
    );
}

#[tokio::test]
async fn new_platform_defaults_preserve_legacy_plaintext_and_doctor_warns_privately() {
    let fixture = Fixture::new(Some("private-jev"));
    let mut keychain = MemoryKeychain::default();
    fixture.run(&[], true, &mut keychain).await.0.unwrap();
    assert_eq!(fixture.saved()["AUTOROUTER_SECRET_STORE"], "keychain");
    assert!(
        !fs::read_to_string(&fixture.path)
            .unwrap()
            .contains("private-")
    );
    assert_eq!(keychain.values(), ["private-jev"]);

    let legacy = Fixture::new(Some("private-legacy"));
    save_user_config(
        &json!({"AUTOROUTER_EVALUATOR":"jev", "TYPESAFE_API_KEY":"private-legacy"}),
        &legacy.context(),
        &SaveOptions::default(),
        &mut keychain,
    )
    .await
    .unwrap();
    let (result, lines) = legacy
        .run(
            &["--force", "--auth-mode", "subscription"],
            true,
            &mut keychain,
        )
        .await;
    result.unwrap();
    assert_eq!(legacy.saved()["TYPESAFE_API_KEY"], "private-legacy");
    assert!(legacy.saved().get("AUTOROUTER_SECRET_STORE").is_none());
    assert_eq!(keychain.values(), ["private-jev"]);
    assert!(
        lines.iter().any(|line| line == "Keys are plaintext in this file. Move them into the macOS Keychain: claude-autorouter config set AUTOROUTER_SECRET_STORE keychain")
    );
    let before = fs::read(&legacy.path).unwrap();
    let mut checks = Vec::new();
    let healthy = tokio::time::timeout(
        Duration::from_secs(2),
        crate::doctor::doctor_for_platform(
            &legacy.context(),
            &mut keychain,
            &CancellationToken::new(),
            &mut |line| {
                assert!(checks.len() < 32 && line.len() < 2048);
                checks.push(line);
            },
            true,
        ),
    )
    .await
    .expect("Synthetic doctor deadline")
    .unwrap();
    assert!(!healthy);
    assert!(
        checks
            .iter()
            .any(|line| line.starts_with("WARN  Saved keys are plaintext"))
    );
    assert!(
        checks.iter().any(|line| line
            == "FAIL  Claude Code unavailable. Install claude and ensure it is on PATH.")
    );
    assert!(!checks.join("\n").contains("private-"));
    assert_eq!(fs::read(&legacy.path).unwrap(), before);

    let linux = Fixture::new(Some("private-jev"));
    linux.run(&[], false, &mut keychain).await.0.unwrap();
    assert!(linux.saved().get("AUTOROUTER_SECRET_STORE").is_none());
    assert_eq!(linux.saved()["TYPESAFE_API_KEY"], "private-jev");
}

#[tokio::test]
async fn locked_keychain_falls_back_only_for_implicit_default_before_final_messages() {
    let fixture = Fixture::new(Some("private-jev"));
    let mut keychain = MemoryKeychain {
        locked: true,
        ..Default::default()
    };
    let (result, lines) = fixture.run(&[], true, &mut keychain).await;
    result.unwrap();
    assert_eq!(fixture.saved()["TYPESAFE_API_KEY"], "private-jev");
    assert!(fixture.saved().get("AUTOROUTER_SECRET_STORE").is_none());
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Keychain is unavailable"))
    );
    let events = keychain.trace.events();
    let write = events
        .iter()
        .position(|event| event.starts_with("write:"))
        .unwrap();
    let warning = events
        .iter()
        .position(|event| event.contains("Keychain is unavailable"))
        .unwrap();
    let saved = events
        .iter()
        .position(|event| event.starts_with("output:Saved "))
        .unwrap();
    assert!(write < warning && warning < saved);
    assert!(keychain.items.is_empty());
    let explicit = Fixture::new(Some("private-jev"));
    let (result, lines) = explicit
        .run(&["--secret-store", "keychain"], true, &mut keychain)
        .await;
    let error = result.unwrap_err();
    assert!(error.contains("macOS Keychain") || error.contains("locked"));
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("Keychain is unavailable") || line.starts_with("Saved "))
    );
    explicit.assert_absent();
}

#[tokio::test]
async fn public_setup_and_doctor_wrappers_use_actual_host_platform() {
    let fixture = Fixture::new(Some("private-jev"));
    let mut keychain = MemoryKeychain::default();
    let mut lines = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(2),
        setup(
            &[],
            &fixture.context(),
            &mut keychain,
            &CancellationToken::new(),
            &mut |line| {
                assert!(lines.len() < 16 && line.len() < 2048);
                lines.push(line);
            },
        ),
    )
    .await
    .expect("Public setup deadline")
    .unwrap();
    let saved = fixture.saved();
    assert_eq!(
        saved.get("AUTOROUTER_SECRET_STORE"),
        cfg!(target_os = "macos")
            .then(|| json!("keychain"))
            .as_ref()
    );
    if cfg!(target_os = "macos") {
        assert!(saved.get("TYPESAFE_API_KEY").is_none());
        assert_eq!(keychain.values(), ["private-jev"]);
    } else {
        assert_eq!(saved["TYPESAFE_API_KEY"], "private-jev");
        assert!(keychain.items.is_empty());
    }
    assert!(!lines.join("\n").contains("private-"));
    let legacy = Fixture::new(Some("private-legacy"));
    save_user_config(
        &json!({"AUTOROUTER_EVALUATOR":"jev", "TYPESAFE_API_KEY":"private-legacy"}),
        &legacy.context(),
        &SaveOptions::default(),
        &mut keychain,
    )
    .await
    .unwrap();
    lines.clear();
    assert!(
        !tokio::time::timeout(
            Duration::from_secs(2),
            crate::doctor::doctor(
                &legacy.context(),
                &mut keychain,
                &CancellationToken::new(),
                &mut |line| {
                    assert!(lines.len() < 32 && line.len() < 2048);
                    lines.push(line);
                },
            )
        )
        .await
        .expect("Public doctor deadline")
        .unwrap()
    );
    assert_eq!(
        lines
            .iter()
            .any(|line| line.starts_with("WARN  Saved keys are plaintext")),
        cfg!(target_os = "macos")
    );
    assert!(!lines.join("\n").contains("private-"));
}
