//! Opt-in tool subprocesses: bounded pipes, private fixtures and group cleanup.
use autorouter_core::auth::Environment;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

pub struct Signals {
    pub token: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl Signals {
    pub fn new() -> Result<Self, String> {
        // Register before returning or exposing any tool startup evidence. On a
        // current-thread runtime the spawned receiver cannot run until a yield.
        #[cfg(unix)]
        let receive = {
            use tokio::signal::unix::{SignalKind, signal};
            let mut interrupt = signal(SignalKind::interrupt())
                .map_err(|_| "Cannot register tool interrupt handler")?;
            let mut terminate = signal(SignalKind::terminate())
                .map_err(|_| "Cannot register tool termination handler")?;
            async move {
                tokio::select! { _ = interrupt.recv() => {}, _ = terminate.recv() => {} }
            }
        };
        #[cfg(windows)]
        let receive = {
            let mut interrupt = tokio::signal::windows::ctrl_c()
                .map_err(|_| "Cannot register tool interrupt handler")?;
            async move {
                interrupt.recv().await;
            }
        };
        let token = CancellationToken::new();
        let copy = token.clone();
        let task = tokio::spawn(async move {
            receive.await;
            copy.cancel();
        });
        Ok(Self { token, task })
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub fn token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "Cannot obtain local authentication entropy")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub fn environment() -> Environment {
    let mut env: Environment = std::env::vars_os().collect();
    for (key, value) in crate::env_file::effective().as_object().unwrap() {
        if !env.contains_key(std::ffi::OsStr::new(key))
            && let Some(value) = value.as_str()
        {
            env.insert(key.into(), value.into());
        }
    }
    env
}
pub struct Scratch(pub PathBuf);
impl Scratch {
    pub fn new(prefix: &str) -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!("autorouter-{prefix}-{}", &token()?[..24]));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .map_err(|_| "Cannot create private tool directory")?;
        Ok(Self(
            path.canonicalize()
                .map_err(|_| "Cannot resolve private tool directory")?,
        ))
    }
    pub fn file(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        let path = self.0.join(name);
        write_private(&path, bytes)?;
        Ok(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts
        .open(path)
        .map_err(|_| "Cannot create private tool file")?;
    file.write_all(bytes)
        .map_err(|_| "Cannot write private tool file".into())
}
fn signal_group(pid: u32, kill: bool) {
    #[cfg(unix)]
    {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pid as i32),
            if kill {
                nix::sys::signal::Signal::SIGKILL
            } else {
                nix::sys::signal::Signal::SIGTERM
            },
        );
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, kill);
    }
}
struct Group(u32);
impl Drop for Group {
    fn drop(&mut self) {
        signal_group(self.0, true);
    }
}
#[derive(Default)]
pub struct InputAction {
    pub bytes: Vec<u8>,
    pub close: bool,
}
pub struct RunOptions<'a> {
    pub timeout: Duration,
    pub grace: Duration,
    /// Legacy field name: bounds observed stdout and stderr together. The
    /// stdout callback also stops once this aggregate byte ceiling is exceeded.
    /// None preserves explicitly streaming callers without an output ceiling.
    pub max_stdout: Option<usize>,
    pub interactive: bool,
    pub response: &'a CancellationToken,
    pub cancel: &'a CancellationToken,
    pub initial: InputAction,
}
fn exceeds_output_limit(stdout: usize, stderr: usize, maximum: Option<usize>) -> bool {
    maximum.is_some_and(|limit| stdout.checked_add(stderr).is_none_or(|total| total > limit))
}
fn stage_input(
    action: InputAction,
    stdin: &mut Option<tokio::process::ChildStdin>,
) -> Option<InputAction> {
    if stdin.is_none() {
        return None;
    }
    if action.bytes.is_empty() {
        if action.close {
            stdin.take();
        }
        None
    } else {
        Some(action)
    }
}
pub async fn run_child(
    command: &mut Command,
    options: RunOptions<'_>,
    mut output: impl FnMut(&[u8]) -> InputAction,
) -> Value {
    command
        .stdin(if options.interactive {
            Stdio::inherit()
        } else if options.initial.close && options.initial.bytes.is_empty() {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let started = Instant::now();
    let Ok(mut child) = command.spawn() else {
        return json!({"spawn_error":true,"exit_code":null,"exit_signal":null,"timed_out":false,"output_limit_exceeded":false,"duration_ms":0,"stdout_bytes":0,"stderr_bytes":0});
    };
    let group = Group(child.id().expect("spawned child PID"));
    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let deadline = tokio::time::Instant::now() + options.timeout;
    // Retain at most one owned action. Suspend further stdout callbacks until
    // it finishes, while the main loop still polls stderr, exit and stop bounds.
    let mut pending_input = stage_input(options.initial, &mut stdin);
    let mut input_offset = 0;
    let (mut outbuf, mut errbuf) = ([0; 8192], [0; 8192]);
    let (mut outdone, mut errdone) = (false, false);
    let (mut outbytes, mut errbytes) = (0usize, 0usize);
    let (mut timed_out, mut output_limit, mut cancelled, mut response_received) =
        (false, false, false, false);
    let mut status = None;
    let mut stopping = None;
    let mut response_at = None;
    loop {
        if status.is_some() && outdone && errdone {
            break;
        }
        let next = stopping
            .map(|at: tokio::time::Instant| at + options.grace)
            .unwrap_or(deadline);
        tokio::select! {biased;
                    _=options.cancel.cancelled(),if !cancelled&&stopping.is_none()=>{cancelled=true;signal_group(group.0,false);stopping=Some(tokio::time::Instant::now());pending_input.take();stdin.take();},
                    _=options.response.cancelled(),if options.interactive&&!response_received=>{response_received=true;response_at=Some(tokio::time::Instant::now()+Duration::from_secs(1));},
                    _=tokio::time::sleep_until(response_at.unwrap_or(deadline)),if response_at.is_some()&&stopping.is_none()=>{signal_group(group.0,false);stopping=Some(tokio::time::Instant::now());pending_input.take();stdin.take();},
                    _=tokio::time::sleep_until(next)=>{if stopping.is_none(){timed_out=true;signal_group(group.0,false);stopping=Some(tokio::time::Instant::now());pending_input.take();stdin.take();}else{signal_group(group.0,true);
        let _=child.start_kill();
        if status.is_none(){status=child.wait().await.ok();}break;}},
                    result=child.wait(),if status.is_none()=>{status=result.ok();pending_input.take();stdin.take();},
                    result=stdout.read(&mut outbuf),if !outdone&&pending_input.is_none()=>{match result{Ok(0)|Err(_)=>outdone=true,Ok(n)=>{outbytes=outbytes.saturating_add(n);
        if exceeds_output_limit(outbytes,errbytes,options.max_stdout){output_limit=true;
        if stopping.is_none(){signal_group(group.0,false);stopping=Some(tokio::time::Instant::now());pending_input.take();stdin.take();}}else{pending_input=stage_input(output(&outbuf[..n]),&mut stdin);input_offset=0;}}}},
                    result=stderr.read(&mut errbuf),if !errdone=>{match result{Ok(0)|Err(_)=>errdone=true,Ok(n)=>{errbytes=errbytes.saturating_add(n);
        if exceeds_output_limit(outbytes,errbytes,options.max_stdout){output_limit=true;
        if stopping.is_none(){signal_group(group.0,false);stopping=Some(tokio::time::Instant::now());pending_input.take();stdin.take();}}}}},
                    result=async {
                        let action=pending_input.as_ref().expect("pending input action");
                        stdin.as_mut().expect("pending input pipe").write(&action.bytes[input_offset..]).await
                    },if pending_input.is_some()&&stopping.is_none()=>{
                        let action=pending_input.as_ref().expect("pending input action");
                        let complete=match result{Ok(n)if n>0=>{input_offset+=n;input_offset==action.bytes.len()},_=>true};
                        if complete{if action.close{stdin.take();}pending_input.take();input_offset=0;}
                    },
                }
    }
    #[cfg(unix)]
    let signal = status
        .as_ref()
        .and_then(|s| {
            use std::os::unix::process::ExitStatusExt;
            s.signal()
        })
        .and_then(|n| nix::sys::signal::Signal::try_from(n).ok())
        .map(|s| format!("{s:?}"));
    #[cfg(not(unix))]
    let signal: Option<String> = None;
    json!({"exit_code":status.and_then(|s|s.code()),"exit_signal":signal,"timed_out":timed_out,"output_limit_exceeded":output_limit,"cancelled":cancelled,"duration_ms":started.elapsed().as_millis(),"stdout_bytes":outbytes,"stderr_bytes":errbytes,"response_received":response_received,"controlled_stop":response_received&&!timed_out&&!cancelled})
}
pub async fn capture(
    command: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
    cancel: &CancellationToken,
) -> (Value, String) {
    let mut bytes = Vec::new();
    let response = CancellationToken::new();
    let result = run_child(
        Command::new(command).args(args).current_dir(cwd),
        RunOptions {
            timeout,
            grace: Duration::from_secs(1),
            max_stdout: Some(100000),
            interactive: false,
            response: &response,
            cancel,
            initial: InputAction {
                close: true,
                ..Default::default()
            },
        },
        |chunk| {
            bytes.extend_from_slice(chunk);
            InputAction::default()
        },
    )
    .await;
    (result, String::from_utf8_lossy(&bytes).into_owned())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    #[ignore = "isolated subprocess entry; invoked by the signal ownership tests"]
    fn signal_startup_child() {
        let case = std::env::var("AUTOROUTER_TEST_SIGNAL_CASE").expect("isolated signal case");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let signals = Signals::new().unwrap();
            let token = signals.token.clone();
            let task = signals.task.abort_handle();
            let dropped = case.starts_with("drop-");
            let signal = match case.as_str() {
                "interrupt" | "drop-interrupt" => nix::sys::signal::Signal::SIGINT,
                "terminate" | "drop-terminate" => nix::sys::signal::Signal::SIGTERM,
                _ => panic!("Unknown isolated signal case"),
            };
            if dropped {
                drop(signals);
            }
            // There is deliberately no runtime yield between construction and
            // the real process signal. The receiver task cannot have run yet.
            nix::sys::signal::kill(nix::unistd::Pid::this(), signal).unwrap();
            if !dropped {
                tokio::time::timeout(Duration::from_secs(1), token.cancelled())
                    .await
                    .expect("registered signal must cancel the owner");
            }
            tokio::time::timeout(Duration::from_secs(1), async {
                while !task.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("signal receiver task must finish");
            assert_eq!(token.is_cancelled(), !dropped);
        });
    }
    #[cfg(unix)]
    async fn isolated_signal_case(case: &str) {
        let cancel = CancellationToken::new();
        let response = CancellationToken::new();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "tool_process::tests::signal_startup_child",
                "--ignored",
                "--nocapture",
            ])
            .env("AUTOROUTER_TEST_SIGNAL_CASE", case);
        let result = run_child(
            &mut command,
            RunOptions {
                timeout: Duration::from_secs(3),
                grace: Duration::from_millis(100),
                max_stdout: Some(4096),
                interactive: false,
                response: &response,
                cancel: &cancel,
                initial: InputAction {
                    close: true,
                    ..Default::default()
                },
            },
            |_| InputAction::default(),
        )
        .await;
        assert_eq!(result["exit_code"], 0, "{case}: {result}");
        assert_eq!(result["exit_signal"], Value::Null, "{case}: {result}");
        assert_eq!(result["timed_out"], false, "{case}: {result}");
        assert_eq!(result["output_limit_exceeded"], false);
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn signals_are_registered_before_first_runtime_yield() {
        for case in ["interrupt", "terminate"] {
            isolated_signal_case(case).await;
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_signal_owner_stops_unpolled_receiver() {
        for case in ["drop-interrupt", "drop-terminate"] {
            isolated_signal_case(case).await;
        }
    }
    #[tokio::test]
    async fn completed_child_output_is_captured_and_omitted_from_metadata() {
        let token = CancellationToken::new();
        let (result, output) = capture(
            "/bin/sh",
            &[
                "-c",
                "printf private-canary; printf private-error-canary >&2",
            ],
            Path::new("/tmp"),
            Duration::from_secs(3),
            &token,
        )
        .await;
        assert_eq!(output, "private-canary");
        assert_eq!(result["exit_code"], 0);
        assert_eq!(result["timed_out"], false);
        assert_eq!(result["stdout_bytes"], "private-canary".len());
        assert_eq!(result["stderr_bytes"], "private-error-canary".len());
        assert!(!result.to_string().contains("private-canary"));
        assert!(!result.to_string().contains("private-error-canary"));
    }
    fn assert_child_reaped(pid_file: &Path) {
        use nix::sys::signal::kill;
        use nix::sys::wait::{WaitPidFlag, waitpid};
        use nix::unistd::Pid;
        let pid = Pid::from_raw(std::fs::read_to_string(pid_file).unwrap().parse().unwrap());
        assert_eq!(kill(pid, None), Err(nix::errno::Errno::ESRCH));
        assert_eq!(
            waitpid(pid, Some(WaitPidFlag::WNOHANG)),
            Err(nix::errno::Errno::ECHILD)
        );
    }
    async fn output_case(
        stdout: usize,
        stderr: usize,
        maximum: Option<usize>,
        stall: bool,
    ) -> (Value, Vec<u8>) {
        let scratch = Scratch::new("tool-output-bound").unwrap();
        let pid_file = scratch.0.join("child.pid");
        let stdout = "O".repeat(stdout);
        let stderr = "E".repeat(stderr);
        let ending = if stall {
            // Ignore TERM so an overflow exercises grace, forced group cleanup
            // and reaping. exec retains the recorded leader PID without a child.
            "exec /bin/sleep 30"
        } else {
            "exit 17"
        };
        let script = format!(
            "trap '' TERM; printf '%s' \"$$\" > \"$1\"; printf '%s' \"$2\"; printf '%s' \"$3\" >&2; {ending}"
        );
        let cancel = CancellationToken::new();
        let response = CancellationToken::new();
        let mut captured = Vec::new();
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            run_child(
                Command::new("/bin/sh").args([
                    std::ffi::OsStr::new("-c"),
                    script.as_ref(),
                    std::ffi::OsStr::new("synthetic-output-bound"),
                    pid_file.as_os_str(),
                    stdout.as_ref(),
                    stderr.as_ref(),
                ]),
                RunOptions {
                    timeout: Duration::from_secs(5),
                    grace: Duration::from_millis(30),
                    max_stdout: maximum,
                    interactive: false,
                    response: &response,
                    cancel: &cancel,
                    initial: InputAction {
                        close: true,
                        ..Default::default()
                    },
                },
                |chunk| {
                    captured.extend_from_slice(chunk);
                    InputAction::default()
                },
            ),
        )
        .await
        .expect("bounded output must terminate and reap the owned subprocess");
        assert_child_reaped(&pid_file);
        assert_eq!(result["timed_out"], false);
        assert_eq!(result["cancelled"], false);
        for private in [&stdout, &stderr]
            .into_iter()
            .filter(|text| text.len() >= 16)
        {
            assert!(!result.to_string().contains(&private[..16]));
        }
        (result, captured)
    }
    #[tokio::test]
    async fn bounded_children_accept_exact_combined_output_on_either_pipe() {
        for (stdout, stderr) in [(256, 0), (0, 256), (128, 128), (0, 0)] {
            let (result, output) = output_case(stdout, stderr, Some(stdout + stderr), false).await;
            assert_eq!(result["exit_code"], 17);
            assert_eq!(result["exit_signal"], Value::Null);
            assert_eq!(result["output_limit_exceeded"], false);
            assert_eq!(result["stdout_bytes"], stdout);
            assert_eq!(result["stderr_bytes"], stderr);
            assert_eq!(output, vec![b'O'; stdout]);
        }
    }
    #[tokio::test]
    async fn bounded_children_stop_and_reap_on_stderr_or_combined_output_overflow() {
        for (stdout, stderr) in [(257, 0), (0, 257), (128, 129), (1, 0), (0, 1)] {
            let maximum = if stdout + stderr == 1 { 0 } else { 256 };
            let (result, output) = output_case(stdout, stderr, Some(maximum), true).await;
            assert_eq!(result["exit_code"], Value::Null);
            assert_eq!(result["exit_signal"], "SIGKILL");
            assert_eq!(result["output_limit_exceeded"], true);
            assert_eq!(result["stdout_bytes"], stdout);
            assert_eq!(result["stderr_bytes"], stderr);
            assert!(output.len() <= maximum);
        }
    }
    #[tokio::test]
    async fn explicitly_streaming_children_keep_their_output_without_an_aggregate_ceiling() {
        let (result, output) = output_case(257, 257, None, false).await;
        assert_eq!(result["exit_code"], 17);
        assert_eq!(result["exit_signal"], Value::Null);
        assert_eq!(result["output_limit_exceeded"], false);
        assert_eq!(result["stdout_bytes"], 257);
        assert_eq!(result["stderr_bytes"], 257);
        assert_eq!(output, vec![b'O'; 257]);
    }
    #[tokio::test]
    async fn callback_input_backpressure_keeps_deadline_cancellation_and_stderr_bounds_live() {
        for stop in ["deadline", "cancel", "output"] {
            let scratch = Scratch::new("tool-callback-input").unwrap();
            let pid_file = scratch.0.join("child.pid");
            let callback_file = scratch.0.join("callback-ready");
            let stderr = if stop == "output" {
                "E".repeat(60)
            } else {
                String::new()
            };
            let mut command = Command::new("/bin/sh");
            command.args([
                std::ffi::OsStr::new("-c"),
                std::ffi::OsStr::new(
                    "trap '' TERM; printf '%s' \"$$\" > \"$1\"; printf ready; while [ ! -f \"$2\" ]; do :; done; printf '%s' \"$3\" >&2; exec /bin/sleep 30",
                ),
                std::ffi::OsStr::new("synthetic-callback-input"),
                pid_file.as_os_str(),
                callback_file.as_os_str(),
                stderr.as_ref(),
            ]);
            let cancel = CancellationToken::new();
            let response = CancellationToken::new();
            let callback_started = CancellationToken::new();
            let mut next_input = Some(vec![b'I'; 1024 * 1024]);
            let mut captured = Vec::new();
            let run = run_child(
                &mut command,
                RunOptions {
                    timeout: Duration::from_secs(if stop == "deadline" { 1 } else { 5 }),
                    grace: Duration::from_millis(30),
                    max_stdout: Some(64),
                    interactive: false,
                    response: &response,
                    cancel: &cancel,
                    initial: InputAction::default(),
                },
                |chunk| {
                    captured.extend_from_slice(chunk);
                    std::fs::write(&callback_file, b"ready").unwrap();
                    callback_started.cancel();
                    InputAction {
                        bytes: next_input.take().expect("only one pending input action"),
                        close: true,
                    }
                },
            );
            let interrupt = async {
                callback_started.cancelled().await;
                if stop == "cancel" {
                    cancel.cancel();
                }
            };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
                tokio::join!(run, interrupt)
            })
            .await
            .expect("blocked callback input must retain stop bounds and cleanup");
            assert_child_reaped(&pid_file);
            assert!(next_input.is_none());
            assert_eq!(captured, b"ready");
            assert_eq!(result["stdout_bytes"], 5);
            assert_eq!(result["stderr_bytes"], stderr.len());
            assert_eq!(result["exit_code"], Value::Null);
            assert_eq!(result["exit_signal"], "SIGKILL");
            assert_eq!(result["timed_out"], stop == "deadline");
            assert_eq!(result["cancelled"], stop == "cancel");
            assert_eq!(result["output_limit_exceeded"], stop == "output");
            assert!(!result.to_string().contains("EEEEEEEEEEEEEEEE"));
            assert!(!result.to_string().contains("IIIIIIIIIIIIIIII"));
        }
    }
    #[tokio::test]
    async fn initial_and_callback_actions_close_stdin_only_after_all_bytes_are_written() {
        for callback in [false, true] {
            let scratch = Scratch::new("tool-close-after-input").unwrap();
            let pid_file = scratch.0.join("child.pid");
            let input_file = scratch.0.join("received-input");
            let input = vec![b'I'; 1024 * 1024];
            let mut command = Command::new("/bin/sh");
            command.args([
                std::ffi::OsStr::new("-c"),
                std::ffi::OsStr::new(
                    "printf '%s' \"$$\" > \"$1\"; printf ready; /bin/cat > \"$2\"; printf done",
                ),
                std::ffi::OsStr::new("synthetic-close-after-input"),
                pid_file.as_os_str(),
                input_file.as_os_str(),
            ]);
            let cancel = CancellationToken::new();
            let response = CancellationToken::new();
            let mut next_input = callback.then(|| input.clone());
            let mut captured = Vec::new();
            let result = tokio::time::timeout(
                Duration::from_secs(10),
                run_child(
                    &mut command,
                    RunOptions {
                        timeout: Duration::from_secs(5),
                        grace: Duration::from_millis(30),
                        max_stdout: Some(64),
                        interactive: false,
                        response: &response,
                        cancel: &cancel,
                        initial: InputAction {
                            bytes: if callback { Vec::new() } else { input.clone() },
                            close: !callback,
                        },
                    },
                    |chunk| {
                        captured.extend_from_slice(chunk);
                        next_input
                            .take()
                            .map_or_else(InputAction::default, |bytes| InputAction {
                                bytes,
                                close: true,
                            })
                    },
                ),
            )
            .await
            .expect("a complete action must close stdin and allow clean child exit");
            assert_child_reaped(&pid_file);
            assert_eq!(result["exit_code"], 0);
            assert_eq!(result["exit_signal"], Value::Null);
            assert_eq!(result["timed_out"], false);
            assert_eq!(result["cancelled"], false);
            assert_eq!(result["output_limit_exceeded"], false);
            assert_eq!(result["stdout_bytes"], 9);
            assert_eq!(result["stderr_bytes"], 0);
            assert_eq!(captured, b"readydone");
            assert_eq!(std::fs::read(&input_file).unwrap(), input);
            assert!(next_input.is_none());
        }
    }
    #[tokio::test]
    async fn deadline_terminates_child_without_assuming_output_readiness() {
        let token = CancellationToken::new();
        // A deadline includes subprocess startup. Waiting for a shell to print
        // within that deadline would make this assertion depend on host load.
        // Output capture/privacy is verified separately above; retain the short
        // runtime deadline and the bounded termination/reaping behavior here.
        let (result, output) = tokio::time::timeout(
            Duration::from_secs(3),
            capture(
                "/bin/sh",
                &["-c", "exec sleep 5"],
                Path::new("/tmp"),
                Duration::from_millis(30),
                &token,
            ),
        )
        .await
        .expect("deadline must terminate and reap the owned subprocess");
        assert_eq!(output, "");
        assert_eq!(result["timed_out"], true);
        assert_eq!(result["cancelled"], false);
        assert_eq!(result["exit_code"], Value::Null);
        assert_eq!(result["exit_signal"], "SIGTERM");
    }
    #[tokio::test]
    async fn failed_launch_and_exit_are_not_success() {
        let token = CancellationToken::new();
        let (result, _) = capture(
            "/path/does/not/exist",
            &[],
            Path::new("/tmp"),
            Duration::from_secs(1),
            &token,
        )
        .await;
        assert_eq!(result["spawn_error"], true);
        let (result, _) = capture(
            "/bin/sh",
            &["-c", "exit 17"],
            Path::new("/tmp"),
            Duration::from_secs(1),
            &token,
        )
        .await;
        assert_eq!(result["exit_code"], 17);
    }
    #[tokio::test]
    async fn initial_input_backpressure_does_not_disable_timeout_or_cancellation() {
        for cancelled in [false, true] {
            let token = CancellationToken::new();
            if cancelled {
                token.cancel();
            }
            let response = CancellationToken::new();
            let result = run_child(
                Command::new("/bin/sh").args(["-c", "exec /bin/sleep 5"]),
                RunOptions {
                    timeout: Duration::from_millis(30),
                    grace: Duration::from_millis(10),
                    max_stdout: Some(1024),
                    interactive: false,
                    response: &response,
                    cancel: &token,
                    initial: InputAction {
                        bytes: vec![b'x'; 1024 * 1024],
                        close: false,
                    },
                },
                |_| InputAction::default(),
            )
            .await;
            assert_eq!(
                result[if cancelled { "cancelled" } else { "timed_out" }],
                true
            );
            assert_eq!(result["exit_code"], Value::Null);
        }
    }
    #[test]
    fn scratch_removed_and_private() {
        let path;
        {
            let dir = Scratch::new("test").unwrap();
            path = dir.0.clone();
            let file = dir.file("fixture", b"private").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
        assert!(!path.exists());
    }
}
