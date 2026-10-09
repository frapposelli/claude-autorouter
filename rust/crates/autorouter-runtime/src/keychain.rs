//! Existing macOS Keychain identity and bounded `security` adapter. Secrets
//! travel only over stdin/stdout; tool output never enters error messages.
use std::future::Future;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

const SERVICE: &str = "claude-autorouter";
const TOOL_ERROR: &str = "Could not run the macOS Keychain tool.";
pub const UNAVAILABLE: &str = "The macOS Keychain secret store is available only on macOS.";

pub trait Keychain {
    fn available(&self) -> bool;
    fn read(
        &mut self,
        account: &str,
    ) -> impl Future<Output = Result<Option<String>, String>> + Send;
    fn write(
        &mut self,
        account: &str,
        value: &str,
        label: &str,
    ) -> impl Future<Output = Result<(), String>> + Send;
    fn remove(&mut self, account: &str) -> impl Future<Output = Result<(), String>> + Send;
}

/// Intentionally does not implement Debug: stdout can contain a credential.
pub struct SecurityOutput {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
}
pub trait SecurityRunner: Send {
    fn run(
        &mut self,
        args: &[&str],
        input: Option<&str>,
    ) -> impl Future<Output = Result<SecurityOutput, String>> + Send;
}

pub struct NativeSecurityRunner;
async fn limited_read(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| TOOL_ERROR)?;
    if bytes.len() > 64 * 1024 {
        return Err(TOOL_ERROR.into());
    }
    Ok(bytes)
}
impl SecurityRunner for NativeSecurityRunner {
    async fn run(&mut self, args: &[&str], input: Option<&str>) -> Result<SecurityOutput, String> {
        let mut child = tokio::process::Command::new("/usr/bin/security")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| TOOL_ERROR)?;
        let mut stdin = child.stdin.take().ok_or(TOOL_ERROR)?;
        let stdout = child.stdout.take().ok_or(TOOL_ERROR)?;
        let stderr = child.stderr.take().ok_or(TOOL_ERROR)?;
        let result = tokio::time::timeout(Duration::from_secs(15), async {
            let write = async {
                if let Some(input) = input {
                    stdin
                        .write_all(input.as_bytes())
                        .await
                        .map_err(|_| TOOL_ERROR)?;
                }
                stdin.shutdown().await.map_err(|_| TOOL_ERROR)?;
                // Closing the pipe is essential for the interactive parser.
                drop(stdin);
                Ok::<_, String>(())
            };
            let ((), stdout, _, status) =
                tokio::try_join!(write, limited_read(stdout), limited_read(stderr), async {
                    child.wait().await.map_err(|_| TOOL_ERROR.to_string())
                })?;
            Ok::<SecurityOutput, String>(SecurityOutput {
                status: status.code(),
                stdout,
            })
        })
        .await;
        match result {
            Ok(Ok(output)) => Ok(output),
            _ => {
                // Kill and reap on timeout, pipe failure or output overflow.
                let _ = child.kill().await;
                let _ = child.wait().await;
                Err(TOOL_ERROR.into())
            }
        }
    }
}

pub struct MacKeychain<R = NativeSecurityRunner> {
    runner: R,
    available: bool,
}
impl Default for MacKeychain {
    fn default() -> Self {
        Self::with_runner(NativeSecurityRunner, cfg!(target_os = "macos"))
    }
}
impl<R: SecurityRunner> MacKeychain<R> {
    pub fn with_runner(runner: R, available: bool) -> Self {
        Self { runner, available }
    }
    async fn call(&mut self, args: &[&str], input: Option<&str>) -> Result<SecurityOutput, String> {
        if !self.available {
            return Err(UNAVAILABLE.into());
        }
        self.runner
            .run(args, input)
            .await
            .map_err(|_| TOOL_ERROR.into())
    }
}

fn quote(value: &str) -> Result<String, String> {
    if value
        .chars()
        .any(|c| matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{2028}' | '\u{2029}'))
    {
        return Err("Keychain item names must not contain control characters.".into());
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}
impl<R: SecurityRunner> Keychain for MacKeychain<R> {
    fn available(&self) -> bool {
        self.available
    }
    async fn read(&mut self, account: &str) -> Result<Option<String>, String> {
        let output = self
            .call(
                &["find-generic-password", "-s", SERVICE, "-a", account, "-w"],
                None,
            )
            .await?;
        match output.status {
            Some(44) => Ok(None),
            Some(0) => {
                let mut value = String::from_utf8_lossy(&output.stdout).into_owned();
                if value.ends_with('\n') { value.pop(); }
                Ok(Some(value))
            },
            _ => Err("Could not read an AutoRouter secret from the macOS Keychain. Unlock the login keychain, or set the key in the environment.".into()),
        }
    }
    async fn write(&mut self, account: &str, value: &str, label: &str) -> Result<(), String> {
        if value.is_empty() || !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) {
            return Err("Keychain secrets must be printable single-line ASCII.".into());
        }
        let input = format!(
            "add-generic-password -U -s {} -a {} -l {} -w {}\n",
            quote(SERVICE)?,
            quote(account)?,
            quote(label)?,
            quote(value)?
        );
        self.call(&["-i"], Some(&input)).await?;
        // Interactive mode can exit successfully after a failed command.
        if self.read(account).await?.as_deref() != Some(value) {
            return Err("Could not save an AutoRouter secret to the macOS Keychain.".into());
        }
        Ok(())
    }
    async fn remove(&mut self, account: &str) -> Result<(), String> {
        match self
            .call(
                &["delete-generic-password", "-s", SERVICE, "-a", account],
                None,
            )
            .await?
            .status
        {
            Some(0 | 44) => Ok(()),
            _ => Err("Could not remove an AutoRouter secret from the macOS Keychain.".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct Runner {
        outputs: VecDeque<(i32, String)>,
        calls: Vec<(Vec<String>, Option<String>)>,
    }
    impl SecurityRunner for Runner {
        async fn run(
            &mut self,
            args: &[&str],
            input: Option<&str>,
        ) -> Result<SecurityOutput, String> {
            self.calls.push((
                args.iter().map(|v| v.to_string()).collect(),
                input.map(str::to_owned),
            ));
            let (status, output) = self
                .outputs
                .pop_front()
                .unwrap_or((51, "PRIVATE TOOL OUTPUT".into()));
            Ok(SecurityOutput {
                status: Some(status),
                stdout: output.into_bytes(),
            })
        }
    }
    fn fixture(outputs: &[(i32, &str)]) -> MacKeychain<Runner> {
        MacKeychain::with_runner(
            Runner {
                outputs: outputs.iter().map(|(s, v)| (*s, v.to_string())).collect(),
                calls: Vec::new(),
            },
            true,
        )
    }
    #[tokio::test]
    async fn secrets_use_stdin_and_readback_checks_interactive_failures() {
        let secret = "synthetic\" back\\slash $HOME 'single'";
        let line = format!("{secret}\n");
        let mut keychain = fixture(&[(0, ""), (0, &line), (0, &line), (44, ""), (0, ""), (44, "")]);
        keychain
            .write("KEY:abc", secret, "AutoRouter Müller \"label\"")
            .await
            .unwrap();
        assert_eq!(
            keychain.read("KEY:abc").await.unwrap().as_deref(),
            Some(secret)
        );
        assert_eq!(keychain.read("missing").await.unwrap(), None);
        keychain.remove("KEY:abc").await.unwrap();
        keychain.remove("missing").await.unwrap();
        assert_eq!(keychain.runner.calls[0].0, ["-i"]);
        assert_eq!(
            keychain.runner.calls[1].0,
            [
                "find-generic-password",
                "-s",
                "claude-autorouter",
                "-a",
                "KEY:abc",
                "-w"
            ]
        );
        assert_eq!(
            keychain.runner.calls[4].0,
            [
                "delete-generic-password",
                "-s",
                "claude-autorouter",
                "-a",
                "KEY:abc"
            ]
        );
        assert!(
            keychain
                .runner
                .calls
                .iter()
                .all(|(args, _)| !args.iter().any(|a| a.contains("synthetic")))
        );
        assert!(
            keychain.runner.calls[0]
                .1
                .as_ref()
                .unwrap()
                .ends_with("-w \"synthetic\\\" back\\\\slash $HOME 'single'\"\n")
        );
        let mut silent = fixture(&[(0, ""), (44, "")]);
        assert!(
            silent
                .write("KEY", "synthetic", "label")
                .await
                .unwrap_err()
                .contains("Could not save")
        );
    }
    #[tokio::test]
    async fn invalid_names_and_secrets_never_invoke_the_tool_and_errors_are_private() {
        let mut keychain = fixture(&[]);
        for value in ["", "two\nlines", "non-ascii-é"] {
            assert!(
                keychain
                    .write("KEY", value, "label")
                    .await
                    .unwrap_err()
                    .contains("printable single-line ASCII")
            );
        }
        for name in ["x\ndelete-keychain", "x\r", "x\u{2028}", "x\0", "x\u{1b}"] {
            assert!(
                keychain
                    .write(name, "synthetic", "label")
                    .await
                    .unwrap_err()
                    .contains("control characters")
            );
            assert!(
                keychain
                    .write("KEY", "synthetic", name)
                    .await
                    .unwrap_err()
                    .contains("control characters")
            );
        }
        assert!(keychain.runner.calls.is_empty());
        for error in [
            keychain.read("KEY").await.unwrap_err(),
            keychain.remove("KEY").await.unwrap_err(),
            keychain
                .write("KEY", "synthetic", "label")
                .await
                .unwrap_err(),
        ] {
            assert!(error.contains("macOS Keychain"));
            assert!(!error.contains("PRIVATE"));
        }
        keychain.available = false;
        assert!(!keychain.available());
        assert_eq!(keychain.read("KEY").await.unwrap_err(), UNAVAILABLE);
        let mut single = fixture(&[(0, ""), (0, "synthetic-secret\n")]);
        single
            .write(
                "ACCOUNT:0123456789abcdef",
                "synthetic-secret",
                "AutoRouter KEY (/Users/Müller \"a\" \\ b/config.json)",
            )
            .await
            .unwrap();
        let input = single.runner.calls[0].1.as_ref().unwrap();
        assert_eq!(input.lines().filter(|line| !line.is_empty()).count(), 1);
        assert!(input.contains("Müller \\\"a\\\" \\\\ b/config.json"));
    }
}
