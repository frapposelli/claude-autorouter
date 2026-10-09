use super::setup;
use autorouter_core::auth::Environment;
use autorouter_core::config::read_config;
use autorouter_runtime::keychain::Keychain;
use autorouter_runtime::user_config::ConfigContext;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio_util::sync::CancellationToken;

struct NoCalls {
    url: String,
    stop: Arc<AtomicBool>,
    unexpected: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl NoCalls {
    fn new() -> Self {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let unexpected = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let seen = unexpected.clone();
        let worker = std::thread::spawn(move || {
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        seen.store(true, Ordering::SeqCst);
                        let _=stream.write_all(b"HTTP/1.1 500 Synthetic unexpected call\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if flag.load(Ordering::SeqCst) {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(_) => panic!("Synthetic admission service failed"),
                }
            }
        });
        Self {
            url,
            stop,
            unexpected,
            worker: Some(worker),
        }
    }
}
impl Drop for NoCalls {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
        if !std::thread::panicking() {
            assert!(
                !self.unexpected.load(Ordering::SeqCst),
                "Setup unexpectedly contacted a provider"
            );
        }
    }
}

struct NoKeychain;
impl Keychain for NoKeychain {
    fn available(&self) -> bool {
        false
    }
    async fn read(&mut self, _: &str) -> Result<Option<String>, String> {
        panic!("Unexpected Keychain read")
    }
    async fn write(&mut self, _: &str, _: &str, _: &str) -> Result<(), String> {
        panic!("Unexpected Keychain write")
    }
    async fn remove(&mut self, _: &str) -> Result<(), String> {
        panic!("Unexpected Keychain removal")
    }
}
struct Fixture {
    path: PathBuf,
    env: Environment,
    http: NoCalls,
}
impl Fixture {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "autorouter-onboarding-admission-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let http = NoCalls::new();
        let mut env = Environment::new();
        for (key, value) in [
            (
                "AUTOROUTER_CONFIG",
                path.join("config.json").into_os_string(),
            ),
            ("AUTOROUTER_EVALUATOR", "jev".into()),
            ("AUTOROUTER_SECRET_STORE", "file".into()),
            (
                "AUTOROUTER_JEV_URL",
                format!("{}/v1/systemone", http.url).into(),
            ),
            ("AUTOROUTER_OLLAMA_URL", http.url.clone().into()),
        ] {
            env.insert(key.into(), value);
        }
        Self { path, env, http }
    }
    async fn run(
        &self,
        args: &[&str],
        cancellation: &CancellationToken,
    ) -> (Result<(), String>, Vec<String>) {
        let context = ConfigContext {
            env: &self.env,
            cwd: &self.path,
            home: &self.path,
        };
        let mut lines = Vec::new();
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        let result = setup(
            &args,
            &context,
            &mut NoKeychain,
            cancellation,
            &mut |line| lines.push(line),
        )
        .await;
        (result, lines)
    }
    fn absent(&self) {
        assert!(!self.path.join("config.json").exists());
    }
    fn saved(&self) -> Value {
        serde_json::from_slice(&fs::read(self.path.join("config.json")).unwrap()).unwrap()
    }
    fn base(&self, profile: &str, key: &str) -> Value {
        json!({"AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_CLIENT_PROFILE":profile,"AUTOROUTER_EVALUATOR":"jev","AUTOROUTER_JEV_URL":format!("{}/v1/systemone",self.http.url),"AUTOROUTER_SECRET_STORE":"file","AUTOROUTER_OLLAMA_URL":self.http.url,"TYPESAFE_API_KEY":key})
    }
}

#[tokio::test]
async fn relevant_keys_only_default_subscription_and_forced_api_mode_match_saved_maps() {
    let mut fixture = Fixture::new();
    for (key, value) in [
        ("TYPESAFE_API_KEY", "private-jev-sentinel"),
        ("ANTHROPIC_API_KEY", "unused-api-secret"),
        ("UNRELATED", "other-secret"),
    ] {
        fixture.env.insert(key.into(), value.into());
    }
    let (result, lines) = fixture.run(&[], &CancellationToken::new()).await;
    result.unwrap();
    assert_eq!(
        fixture.saved(),
        fixture.base("compatible", "private-jev-sentinel")
    );
    for secret in ["private-jev-sentinel", "unused-api-secret", "other-secret"] {
        assert!(!lines.join("\n").contains(secret));
    }
    let (result, _) = fixture.run(&[], &CancellationToken::new()).await;
    assert!(result.unwrap_err().contains("already exists"));
    let (result, _) = fixture
        .run(
            &["--force", "--auth-mode", "api-key"],
            &CancellationToken::new(),
        )
        .await;
    result.unwrap();
    assert_eq!(fixture.saved()["AUTOROUTER_AUTH_MODE"], "api-key");
    assert_eq!(fixture.saved()["ANTHROPIC_API_KEY"], "unused-api-secret");
}

#[tokio::test]
async fn all_profile_cli_precedence_cases_preserve_parent_permission_environment() {
    for (inherited, args, expected) in [
        ("native", vec!["--client-profile", "auto"], "auto"),
        ("auto", vec![], "auto"),
        ("auto", vec!["--client-profile", "native"], "native"),
        ("auto", vec!["--client-profile", "compatible"], "compatible"),
    ] {
        let mut fixture = Fixture::new();
        for (key, value) in [
            ("AUTOROUTER_CLIENT_PROFILE", inherited),
            ("TYPESAFE_API_KEY", "synthetic-jev-key"),
            ("ANTHROPIC_MODEL", "claude-opus-5-5"),
            ("MAX_THINKING_TOKENS", "10000"),
            ("CLAUDE_CODE_AUTO_MODE_SERVER", "0"),
        ] {
            fixture.env.insert(key.into(), value.into());
        }
        let before = fixture.env.clone();
        let (result, lines) = fixture.run(&args, &CancellationToken::new()).await;
        result.unwrap();
        assert_eq!(fixture.saved(), fixture.base(expected, "synthetic-jev-key"));
        assert_eq!(fixture.env, before);
        let config = read_config(&fixture.saved(), false, &fixture.path).unwrap();
        assert_eq!(
            serde_json::to_value(config.client_profile).unwrap(),
            expected
        );
        if expected == "auto" {
            let text = lines.join("\n");
            assert!(text.contains("Sonnet/Opus task routing"));
            assert!(
                text.contains("Claude controls permission-mode availability and safety checks")
            );
        }
    }
}

#[tokio::test]
async fn metadata_mode_updates_only_that_setting_without_enabling_a_log_directory() {
    let mut fixture = Fixture::new();
    fixture
        .env
        .insert("TYPESAFE_API_KEY".into(), "synthetic-jev-key".into());
    fixture
        .run(&["--client-profile", "auto"], &CancellationToken::new())
        .await
        .0
        .unwrap();
    let mut expected = fixture.saved();
    expected["AUTOROUTER_SESSION_LOG_MODE"] = json!("metadata");
    fixture
        .run(
            &["--force", "--session-log-mode", "metadata"],
            &CancellationToken::new(),
        )
        .await
        .0
        .unwrap();
    assert_eq!(fixture.saved(), expected);
    assert!(
        read_config(&fixture.saved(), false, &fixture.path)
            .unwrap()
            .session_log_dir
            .is_none()
    );
    for args in [
        vec!["--force", "--session-log-mode"],
        vec!["--force", "--session-log-mode", "invalid"],
    ] {
        let (result, _) = fixture.run(&args, &CancellationToken::new()).await;
        assert!(result.unwrap_err().contains("session-log-mode"));
        assert_eq!(fixture.saved(), expected);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[tokio::test]
async fn every_invalid_profile_is_rejected_before_secret_input_or_persistence() {
    let mut fixture = Fixture::new();
    for value in [
        None,
        Some(""),
        Some(" "),
        Some("automatic"),
        Some("Auto"),
        Some("--force"),
    ] {
        let mut args = vec!["--client-profile"];
        args.extend(value);
        let (result, lines) = fixture.run(&args, &CancellationToken::new()).await;
        assert!(
            result
                .unwrap_err()
                .contains("--client-profile must be compatible, native or auto")
        );
        assert!(lines.is_empty());
        fixture.absent();
    }
    fixture
        .env
        .insert("AUTOROUTER_CLIENT_PROFILE".into(), "unknown".into());
    let (result, lines) = fixture.run(&[], &CancellationToken::new()).await;
    assert!(result.unwrap_err().contains("client-profile"));
    assert!(lines.is_empty());
    fixture.absent();
}

#[tokio::test]
async fn invalid_log_directory_arguments_and_nul_environment_fail_before_secret_input() {
    let mut fixture = Fixture::new();
    for args in [
        vec!["--session-log-dir"],
        vec!["--session-log-dir", "--force"],
        vec!["--session-log-dir", "private\npath"],
    ] {
        let (result, lines) = fixture.run(&args, &CancellationToken::new()).await;
        assert!(result.unwrap_err().contains("session-log-dir"));
        assert!(lines.is_empty());
        fixture.absent();
    }
    fixture
        .env
        .insert("AUTOROUTER_SESSION_LOG_DIR".into(), "private\0path".into());
    let (result, lines) = fixture.run(&[], &CancellationToken::new()).await;
    assert!(result.unwrap_err().contains("AUTOROUTER_SESSION_LOG_DIR"));
    assert!(lines.is_empty());
    fixture.absent();
}

#[tokio::test]
async fn all_invalid_local_deadline_values_and_jev_option_fail_before_input() {
    let fixture = Fixture::new();
    for value in [
        None,
        Some(""),
        Some(" "),
        Some("--pull"),
        Some("-1"),
        Some("30001"),
        Some("1.5"),
        Some("1e-999"),
        Some("-1e-999"),
        Some("NaN"),
        Some("Infinity"),
    ] {
        let mut args = vec!["--evaluator", "ollama", "--ollama-timeout-ms"];
        args.extend(value);
        let (result, lines) = fixture.run(&args, &CancellationToken::new()).await;
        assert!(
            result
                .unwrap_err()
                .contains("--ollama-timeout-ms requires an integer")
        );
        assert!(lines.is_empty());
        fixture.absent();
    }
    let (result, lines) = fixture
        .run(&["--ollama-timeout-ms", "0"], &CancellationToken::new())
        .await;
    assert!(result.unwrap_err().contains("require --evaluator ollama"));
    assert!(lines.is_empty());
    fixture.absent();
}

#[tokio::test]
async fn pre_cancelled_setup_performs_no_keychain_input_output_or_file_access() {
    let fixture = Fixture::new();
    // A malformed config would fail immediately if cancellation were checked
    // after loading. Its bytes must remain untouched and its error undisclosed.
    let config = fixture.path.join("config.json");
    fs::write(&config, b"PRIVATE malformed configuration").unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let (result, lines) = fixture.run(&[], &cancellation).await;
    assert_eq!(result.unwrap_err(), "Setup cancelled");
    assert!(lines.is_empty());
    assert_eq!(
        fs::read(config).unwrap(),
        b"PRIVATE malformed configuration"
    );
    fs::remove_file(fixture.path.join("config.json")).unwrap();
    let (result, lines) = fixture.run(&[], &cancellation).await;
    assert_eq!(result.unwrap_err(), "Setup cancelled");
    assert!(lines.is_empty());
    fixture.absent();
}
