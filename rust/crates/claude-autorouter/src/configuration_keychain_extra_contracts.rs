//! Frozen Keychain definitions 5 and 9 through production command functions.
//! The stdin schedule runs in an isolated child with an in-memory Keychain.
use super::*;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

const CHILD_FLAG: &str = "AUTOROUTER_SYNTHETIC_KEYCHAIN_STDIN_CHILD";
const CHILD_NAME: &str = "configuration::configuration_keychain_contracts::extra_contracts::stdin_secret_edits_preserve_unrelated_settings_and_remove_only_selected_item";
const OUTPUT_LIMIT: u64 = 65_536;

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn read_output(reader: impl Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    reader
        .take(OUTPUT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}

#[tokio::test]
async fn stdin_secret_edits_preserve_unrelated_settings_and_remove_only_selected_item() {
    if std::env::var_os(CHILD_FLAG).is_none() {
        // Never replace the parent test process's stdin or environment. Both
        // output readers are bounded; the child is reaped even on unwinding.
        // Parent ownership also removes the child's nested config directory
        // if a deadline forces termination before its Rust destructors run.
        let directory = Fixture::new();
        let mut child = OwnedChild(
            Command::new(std::env::current_exe().unwrap())
                .env_clear()
                .env(CHILD_FLAG, "1")
                .env("TMPDIR", &directory.directory)
                .env("TMP", &directory.directory)
                .env("TEMP", &directory.directory)
                .args(["--exact", CHILD_NAME, "--nocapture"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let stdout = child.0.stdout.take().unwrap();
        let stderr = child.0.stderr.take().unwrap();
        let output = std::thread::spawn(move || read_output(stdout));
        let errors = std::thread::spawn(move || read_output(stderr));
        child
            .0
            .stdin
            .take()
            .unwrap()
            .write_all(b"private-new\n")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break Some(status);
            }
            if Instant::now() >= deadline {
                child.0.kill().unwrap();
                child.0.wait().unwrap();
                break None;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let output = output.join().unwrap();
        let errors = errors.join().unwrap();
        assert!(output.len() <= OUTPUT_LIMIT as usize && errors.len() <= OUTPUT_LIMIT as usize);
        assert!(
            status.is_some_and(|status| status.success()),
            "Synthetic stdin child failed or timed out: stdout={}, stderr={}",
            String::from_utf8_lossy(&output),
            String::from_utf8_lossy(&errors)
        );
        assert!(String::from_utf8_lossy(&output).contains("synthetic-keychain-stdin-complete"));
        assert!(!String::from_utf8_lossy(&output).contains("private-"));
        assert!(!String::from_utf8_lossy(&errors).contains("private-"));
        return;
    }
    let fixture = Fixture::new();
    let mut keychain = MemoryKeychain::default();
    fixture
        .seed(
            json!({"AUTOROUTER_SECRET_STORE":"keychain", "TYPESAFE_API_KEY":"private-old"}),
            &mut keychain,
        )
        .await;
    let result = fixture
        .run(
            &["set", "TYPESAFE_API_KEY", "--stdin"],
            &fixture.env,
            &mut keychain,
        )
        .await
        .unwrap();
    assert!(result.success);
    assert_eq!(keychain.values(), ["private-new"]);
    assert!(
        !String::from_utf8(fixture.bytes())
            .unwrap()
            .contains("private-")
    );
    assert_eq!(
        result.lines,
        ["Saved TYPESAFE_API_KEY in the macOS Keychain. Other saved settings are unchanged."]
    );
    let result = fixture
        .run(
            &["set", "AUTOROUTER_PORT", "8124"],
            &fixture.env,
            &mut keychain,
        )
        .await
        .unwrap();
    assert!(result.success);
    assert_eq!(keychain.values(), ["private-new"]);
    assert_eq!(
        fixture.saved(),
        json!({"AUTOROUTER_SECRET_STORE":"keychain", "AUTOROUTER_PORT":"8124"})
    );
    let result = fixture
        .run(&["unset", "TYPESAFE_API_KEY"], &fixture.env, &mut keychain)
        .await
        .unwrap();
    assert!(result.success);
    assert!(keychain.items.is_empty());
    assert_eq!(keychain.removals, 1);
    assert_eq!(
        fixture.saved(),
        json!({"AUTOROUTER_SECRET_STORE":"keychain", "AUTOROUTER_PORT":"8124"})
    );
    fixture.assert_private();
    println!("synthetic-keychain-stdin-complete");
}

#[tokio::test]
async fn invalid_store_is_rejected_and_session_history_never_reads_keychain() {
    let fixture = Fixture::new();
    let mut keychain = MemoryKeychain::default();
    let expected = "AUTOROUTER_SECRET_STORE must be file or keychain.";
    assert_eq!(
        save_user_config(
            &json!({"AUTOROUTER_SECRET_STORE":"vault"}),
            &fixture.context(&fixture.env),
            &SaveOptions::default(),
            &mut keychain
        )
        .await
        .unwrap_err(),
        expected
    );
    assert_eq!(
        fixture
            .run(
                &["set", "AUTOROUTER_SECRET_STORE", "vault"],
                &fixture.env,
                &mut keychain
            )
            .await
            .err()
            .unwrap(),
        expected
    );
    assert!(!fixture.path.exists());
    fixture
        .seed(json!({"AUTOROUTER_SECRET_STORE":"keychain"}), &mut keychain)
        .await;
    keychain.locked = true;
    let loaded = load_user_config(
        &fixture.context(&fixture.env),
        &LoadOptions {
            read_secrets: false,
            ..Default::default()
        },
        &mut keychain,
    )
    .await
    .unwrap();
    assert!(!loaded.values.contains_key("ANTHROPIC_API_KEY"));
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        crate::sessions::command(
            &["list".into()],
            &fixture.context(&fixture.env),
            &mut keychain,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(result.success);
    assert_eq!(
        result.lines,
        ["Session logging is disabled. Set AUTOROUTER_SESSION_LOG_DIR to record future sessions."]
    );
    assert_eq!(
        (keychain.reads, keychain.writes, keychain.removals),
        (0, 0, 0)
    );
    fixture.assert_private();
}
