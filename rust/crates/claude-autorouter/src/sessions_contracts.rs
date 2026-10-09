//! Frozen session-history.test.mjs #11 through the actual command boundary.
use super::command;
use autorouter_core::auth::Environment;
use autorouter_runtime::keychain::Keychain;
use autorouter_runtime::user_config::{ConfigContext, SaveOptions, save_user_config};
use serde_json::{Value, json};
use std::fs::{self, DirBuilder};
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::PathBuf;
use std::time::Duration;

struct NoSecrets;
impl Keychain for NoSecrets {
    fn available(&self) -> bool {
        panic!("History must not inspect Keychain availability")
    }
    async fn read(&mut self, _: &str) -> Result<Option<String>, String> {
        panic!("History must not read secrets")
    }
    async fn write(&mut self, _: &str, _: &str, _: &str) -> Result<(), String> {
        panic!("History must not write secrets")
    }
    async fn remove(&mut self, _: &str) -> Result<(), String> {
        panic!("History must not remove secrets")
    }
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        assert_eq!(
            fs::symlink_metadata(autorouter_runtime::policy::default_policy_path())
                .unwrap_err()
                .kind(),
            ErrorKind::NotFound,
            "Synthetic history command requires absent system policy"
        );
        let mut random = [0u8; 12];
        getrandom::fill(&mut random).unwrap();
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = std::env::temp_dir().join(format!("autorouter-session-inspection-{suffix}"));
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        DirBuilder::new()
            .mode(0o700)
            .create(path.join("logs"))
            .unwrap();
        Self(path)
    }
    fn entries(&self) -> Vec<std::ffi::OsString> {
        let mut result = fs::read_dir(&self.0)
            .unwrap()
            .map(|row| row.unwrap().file_name())
            .collect::<Vec<_>>();
        result.sort();
        result
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn disabled_and_missing_history_inspection_creates_no_files_or_credentials() {
    let fixture = Fixture::new();
    let path = fixture.0.join("config.json");
    let env = Environment::from([("AUTOROUTER_CONFIG".into(), path.clone().into_os_string())]);
    let context = ConfigContext {
        env: &env,
        cwd: &fixture.0,
        home: &fixture.0,
    };
    let mut keychain = NoSecrets;
    let list = tokio::time::timeout(
        Duration::from_secs(2),
        command(&["list".into(), "--json".into()], &context, &mut keychain),
    )
    .await
    .expect("History list deadline")
    .unwrap();
    assert!(list.success);
    assert_eq!(list.lines.len(), 1);
    assert!(list.lines[0].len() < 4096);
    let report: Value = serde_json::from_str(&list.lines[0]).unwrap();
    assert_eq!(report["logging_enabled"], false);
    assert_eq!(report["sessions"], json!([]));
    let show = tokio::time::timeout(
        Duration::from_secs(2),
        command(
            &[
                "show".into(),
                "autorouter-session-20261005T120000Z-run-a".into(),
            ],
            &context,
            &mut keychain,
        ),
    )
    .await
    .expect("History show deadline")
    .unwrap();
    assert!(!show.success);
    assert_eq!(
        show.lines,
        ["Session logging is disabled. Set AUTOROUTER_SESSION_LOG_DIR to record future sessions."]
    );
    assert_eq!(fs::read_dir(fixture.0.join("logs")).unwrap().count(), 0);
    assert_eq!(fixture.entries(), ["logs"]);

    let missing = fixture.0.join("not-created");
    save_user_config(
        &json!({"AUTOROUTER_SESSION_LOG_DIR":missing}),
        &context,
        &SaveOptions::default(),
        &mut keychain,
    )
    .await
    .unwrap();
    let before = fs::read(&path).unwrap();
    let list = tokio::time::timeout(
        Duration::from_secs(2),
        command(&["list".into(), "--json".into()], &context, &mut keychain),
    )
    .await
    .expect("Missing history list deadline")
    .unwrap();
    assert!(list.success);
    assert_eq!(list.lines.len(), 1);
    assert!(list.lines[0].len() < 8192);
    let report: Value = serde_json::from_str(&list.lines[0]).unwrap();
    assert_eq!(report["coverage"]["directory_missing"], true);
    assert_eq!(report["sessions"], json!([]));
    assert_eq!(report["logging_enabled"], true);
    let absent = fs::metadata(&missing).unwrap_err();
    assert_eq!(absent.kind(), ErrorKind::NotFound);
    assert_eq!(absent.raw_os_error(), Some(nix::libc::ENOENT));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fixture.entries(), ["config.json", "logs"]);
    assert_eq!(fs::read_dir(fixture.0.join("logs")).unwrap().count(), 0);
    for (p, mode) in [(&fixture.0, 0o700), (&path, 0o600)] {
        let metadata = fs::symlink_metadata(p).unwrap();
        assert!(!metadata.file_type().is_symlink());
        assert_eq!(metadata.mode() & 0o777, mode);
        assert_eq!(metadata.uid(), nix::unistd::geteuid().as_raw());
    }
}
