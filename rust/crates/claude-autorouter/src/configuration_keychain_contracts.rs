//! Frozen keychain.test.mjs definitions 4, 6, 7 and 10 through the real
//! configuration command. Only an injected in-memory Keychain is constructed.
use super::{CommandOutput, command};
use autorouter_core::auth::Environment;
use autorouter_runtime::keychain::Keychain;
use autorouter_runtime::user_config::{
    ConfigContext, LoadOptions, SECRET_CONFIG_KEYS, SaveOptions, keychain_removals,
    load_user_config, save_user_config,
};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder};
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::PathBuf;
use std::time::Duration;

const LOCKED: &str = "The macOS Keychain is locked.";
const WRITE_FAILED: &str = "Could not save an AutoRouter secret to the macOS Keychain.";
const UNREADABLE: &str = "Could not read every saved secret from the macOS Keychain. Unlock the login keychain and retry.";

#[path = "configuration_keychain_extra_contracts.rs"]
mod extra_contracts;

#[derive(Default)]
struct MemoryKeychain {
    items: BTreeMap<String, String>,
    locked: bool,
    fail_write: bool,
    reads: usize,
    writes: usize,
    removals: usize,
}
impl MemoryKeychain {
    fn bound(&self, account: &str) {
        assert!(self.reads + self.writes + self.removals <= 64);
        assert!(account.len() <= 512);
        assert!(self.items.len() <= SECRET_CONFIG_KEYS.len());
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
        self.reads += 1;
        self.bound(account);
        if self.locked {
            Err(LOCKED.into())
        } else {
            Ok(self.items.get(account).cloned())
        }
    }
    async fn write(&mut self, account: &str, value: &str, label: &str) -> Result<(), String> {
        self.writes += 1;
        self.bound(account);
        assert!(value.len() <= 512 && label.len() <= 1024);
        if self.fail_write {
            return Err(WRITE_FAILED.into());
        }
        if self.locked {
            return Err(LOCKED.into());
        }
        self.items.insert(account.into(), value.into());
        Ok(())
    }
    async fn remove(&mut self, account: &str) -> Result<(), String> {
        self.removals += 1;
        self.bound(account);
        self.items.remove(account);
        Ok(())
    }
}

struct Fixture {
    directory: PathBuf,
    path: PathBuf,
    env: Environment,
}
impl Fixture {
    fn new() -> Self {
        // The command intentionally has no injected policy override. Fail this
        // fixture before reading any system policy content if one is installed;
        // never delete, replace or bypass an administrator's policy for a test.
        assert_eq!(
            fs::symlink_metadata(autorouter_runtime::policy::default_policy_path())
                .err()
                .map(|error| error.kind()),
            Some(ErrorKind::NotFound),
            "Synthetic command contracts require an absent system policy"
        );
        let mut random = [0u8; 12];
        getrandom::fill(&mut random).unwrap();
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let directory = std::env::temp_dir().join(format!("autorouter-command-keychain-{suffix}"));
        DirBuilder::new().mode(0o700).create(&directory).unwrap();
        let path = directory.join("config.json");
        let env = Environment::from([
            ("AUTOROUTER_EVALUATOR".into(), "jev".into()),
            ("AUTOROUTER_CONFIG".into(), path.clone().into_os_string()),
        ]);
        Self {
            directory,
            path,
            env,
        }
    }
    fn context<'a>(&'a self, env: &'a Environment) -> ConfigContext<'a> {
        ConfigContext {
            env,
            cwd: &self.directory,
            home: &self.directory,
        }
    }
    async fn seed(&self, value: Value, keychain: &mut MemoryKeychain) {
        assert_eq!(
            save_user_config(
                &value,
                &self.context(&self.env),
                &SaveOptions::default(),
                keychain
            )
            .await
            .unwrap(),
            self.path
        );
        self.assert_private();
    }
    fn bytes(&self) -> Vec<u8> {
        let metadata = fs::metadata(&self.path).unwrap();
        assert!(metadata.len() <= 4096);
        fs::read(&self.path).unwrap()
    }
    fn saved(&self) -> Value {
        serde_json::from_slice(&self.bytes()).unwrap()
    }
    fn assert_private(&self) {
        for (path, mode) in [(&self.directory, 0o700), (&self.path, 0o600)] {
            let metadata = fs::symlink_metadata(path).unwrap();
            assert!(!metadata.file_type().is_symlink());
            assert_eq!(metadata.mode() & 0o777, mode);
            assert_eq!(metadata.uid(), nix::unistd::geteuid().as_raw());
        }
        assert_eq!(fs::read_dir(&self.directory).unwrap().count(), 1);
    }
    async fn run(
        &self,
        args: &[&str],
        env: &Environment,
        keychain: &mut MemoryKeychain,
    ) -> Result<CommandOutput, String> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        tokio::time::timeout(
            Duration::from_secs(2),
            command(&args, &self.context(env), keychain),
        )
        .await
        .expect("Synthetic configuration command deadline")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[tokio::test]
async fn store_set_and_unset_migrate_both_directions_with_exact_private_messages() {
    let fixture = Fixture::new();
    let mut keychain = MemoryKeychain::default();
    fixture.seed(json!({"AUTOROUTER_AUTH_MODE":"api-key", "ANTHROPIC_API_KEY":"private-anthropic", "AUTOROUTER_PORT":"8123"}), &mut keychain).await;
    let mut lines = Vec::new();
    for (args, store, message) in [
        (
            vec!["set", "AUTOROUTER_SECRET_STORE", "keychain"],
            Some("keychain"),
            "Saved AUTOROUTER_SECRET_STORE. Moved 1 saved secret to the macOS Keychain.",
        ),
        (
            vec!["set", "AUTOROUTER_SECRET_STORE", "file"],
            Some("file"),
            "Saved AUTOROUTER_SECRET_STORE. Moved 1 saved secret to the configuration file.",
        ),
        (
            vec!["set", "AUTOROUTER_SECRET_STORE", "keychain"],
            Some("keychain"),
            "Saved AUTOROUTER_SECRET_STORE. Moved 1 saved secret to the macOS Keychain.",
        ),
        (
            vec!["unset", "AUTOROUTER_SECRET_STORE"],
            None,
            "Saved AUTOROUTER_SECRET_STORE. Moved 1 saved secret to the configuration file.",
        ),
    ] {
        let result = fixture
            .run(&args, &fixture.env, &mut keychain)
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.lines, [message]);
        lines.extend(result.lines);
        let mut expected = json!({"AUTOROUTER_AUTH_MODE":"api-key", "AUTOROUTER_PORT":"8123"});
        if let Some(store) = store {
            expected["AUTOROUTER_SECRET_STORE"] = json!(store);
        }
        if store == Some("keychain") {
            assert_eq!(keychain.values(), ["private-anthropic"]);
            assert!(
                !String::from_utf8(fixture.bytes())
                    .unwrap()
                    .contains("private-")
            );
        } else {
            expected["ANTHROPIC_API_KEY"] = json!("private-anthropic");
            assert!(keychain.items.is_empty());
        }
        assert_eq!(fixture.saved(), expected);
        fixture.assert_private();
    }
    assert!(!lines.join("\n").contains("private-"));
}

#[tokio::test]
async fn locked_store_environment_overrides_allow_loading_but_refuse_destructive_command_migration()
{
    let fixture = Fixture::new();
    let mut keychain = MemoryKeychain::default();
    fixture
        .seed(
            json!({"AUTOROUTER_SECRET_STORE":"keychain", "ANTHROPIC_API_KEY":"private-anthropic"}),
            &mut keychain,
        )
        .await;
    let before = fixture.bytes();
    keychain.locked = true;
    assert_eq!(
        load_user_config(
            &fixture.context(&fixture.env),
            &LoadOptions::default(),
            &mut keychain
        )
        .await
        .err()
        .expect("Locked read must fail"),
        LOCKED
    );
    let mut overridden = fixture.env.clone();
    for (key, value) in [
        ("ANTHROPIC_API_KEY", "env-a"),
        ("TYPESAFE_API_KEY", "env-b"),
        ("AUTOROUTER_TOKEN", "env-token-0123456789"),
    ] {
        overridden.insert(key.into(), value.into());
    }
    let loaded = load_user_config(
        &fixture.context(&overridden),
        &LoadOptions::default(),
        &mut keychain,
    )
    .await
    .unwrap();
    assert_eq!(
        loaded.env.get(OsStr::new("ANTHROPIC_API_KEY")),
        Some(&OsString::from("env-a"))
    );
    assert_eq!(loaded.unavailable_secrets, SECRET_CONFIG_KEYS);
    assert_eq!(
        keychain_removals(&loaded, "file", false, &Map::new()).unwrap_err(),
        UNREADABLE
    );
    let mutations = (keychain.writes, keychain.removals);
    let error = fixture
        .run(
            &["set", "AUTOROUTER_SECRET_STORE", "file"],
            &overridden,
            &mut keychain,
        )
        .await
        .err()
        .expect("Unreadable secrets must block migration");
    assert_eq!(error, UNREADABLE);
    assert!(!error.contains("private-") && !error.contains("env-token"));
    assert_eq!((keychain.writes, keychain.removals), mutations);
    assert_eq!(fixture.bytes(), before);
    keychain.locked = false;
    assert_eq!(keychain.values(), ["private-anthropic"]);
    assert_eq!(
        fixture.saved(),
        json!({"AUTOROUTER_SECRET_STORE":"keychain"})
    );
    fixture.assert_private();
}

#[tokio::test]
async fn failed_keychain_write_preserves_original_plaintext_bytes_and_private_file() {
    let fixture = Fixture::new();
    let mut keychain = MemoryKeychain::default();
    fixture
        .seed(
            json!({"ANTHROPIC_API_KEY":"private-anthropic"}),
            &mut keychain,
        )
        .await;
    let before = fixture.bytes();
    keychain.fail_write = true;
    let error = fixture
        .run(
            &["set", "AUTOROUTER_SECRET_STORE", "keychain"],
            &fixture.env,
            &mut keychain,
        )
        .await
        .err()
        .expect("Failed write must reject migration");
    assert_eq!(error, WRITE_FAILED);
    assert_eq!(fixture.bytes(), before);
    assert!(keychain.items.is_empty());
    assert_eq!(keychain.writes, 1);
    assert_eq!(keychain.removals, 0);
    fixture.assert_private();
}

#[tokio::test]
async fn show_reports_saved_keychain_provenance_and_secret_presence_without_values() {
    let fixture = Fixture::new();
    let mut keychain = MemoryKeychain::default();
    fixture
        .seed(
            json!({"AUTOROUTER_SECRET_STORE":"keychain", "TYPESAFE_API_KEY":"private-jev"}),
            &mut keychain,
        )
        .await;
    let before = fixture.bytes();
    let output = fixture
        .run(&["show", "--json"], &fixture.env, &mut keychain)
        .await
        .unwrap();
    assert!(output.success);
    assert_eq!(output.lines.len(), 1);
    let report: Value = serde_json::from_str(&output.lines[0]).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["config_path"], json!(fixture.path));
    assert_eq!(report["config_exists"], true);
    assert_eq!(report["valid"], true);
    assert_eq!(report["checked"], "active_evaluator");
    assert_eq!(
        report["settings"]["TYPESAFE_API_KEY"],
        json!({"source":"keychain", "active":true, "secret":true, "present":true})
    );
    assert_eq!(
        report["settings"]["AUTOROUTER_SECRET_STORE"],
        json!({"source":"file", "active":true, "value":"keychain"})
    );
    for key in ["ANTHROPIC_API_KEY", "AUTOROUTER_TOKEN"] {
        assert_eq!(
            report["settings"][key],
            json!({"source":"default", "active":true, "secret":true, "present":false})
        );
    }
    assert!(!output.lines.join("\n").contains("private-"));
    let text = fixture
        .run(&["show"], &fixture.env, &mut keychain)
        .await
        .unwrap();
    assert!(text.success);
    assert!(
        text.lines
            .contains(&"TYPESAFE_API_KEY=[set; hidden] [keychain]".into())
    );
    assert!(
        text.lines
            .contains(&"AUTOROUTER_SECRET_STORE=keychain [file]".into())
    );
    assert!(!text.lines.join("\n").contains("private-"));
    assert_eq!(fixture.bytes(), before);
    assert_eq!(keychain.values(), ["private-jev"]);
    fixture.assert_private();
}
