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
    pub fn new() -> Self {
        let token = CancellationToken::new();
        let copy = token.clone();
        let task = tokio::spawn(async move {
            #[cfg(unix)]
            {
                if let Ok(mut term) =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                {
                    tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
                } else {
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
            copy.cancel();
        });
        Self { token, task }
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
    pub max_stdout: Option<usize>,
    pub interactive: bool,
    pub response: &'a CancellationToken,
    pub cancel: &'a CancellationToken,
    pub initial: InputAction,
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
    if let Some(input) = &mut stdin {
        let result = tokio::select! {
            result = input.write_all(&options.initial.bytes) => result.map_err(|_| "stdin_error"),
            _ = options.cancel.cancelled() => Err("cancelled"),
            _ = tokio::time::sleep_until(deadline) => Err("timed_out"),
        };
        if let Err(reason) = result
            && reason != "stdin_error"
        {
            signal_group(group.0, true);
            let _ = child.kill().await;
            let _ = child.wait().await;
            return json!({"exit_code":null,"exit_signal":"SIGKILL","timed_out":reason=="timed_out","cancelled":reason=="cancelled","stdin_error":reason=="stdin_error","output_limit_exceeded":false,"duration_ms":started.elapsed().as_millis(),"stdout_bytes":0,"stderr_bytes":0});
        }
    }
    if options.initial.close {
        stdin.take();
    }
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
                    _=options.cancel.cancelled(),if !cancelled&&stopping.is_none()=>{cancelled=true;signal_group(group.0,false);stopping=Some(tokio::time::Instant::now());stdin.take();},
                    _=options.response.cancelled(),if options.interactive&&!response_received=>{response_received=true;response_at=Some(tokio::time::Instant::now()+Duration::from_secs(1));},
                    _=tokio::time::sleep_until(response_at.unwrap_or(deadline)),if response_at.is_some()&&stopping.is_none()=>{signal_group(group.0,false);stopping=Some(tokio::time::Instant::now());stdin.take();},
                    _=tokio::time::sleep_until(next)=>{if stopping.is_none(){timed_out=true;signal_group(group.0,false);stopping=Some(tokio::time::Instant::now());stdin.take();}else{signal_group(group.0,true);
        let _=child.start_kill();
        if status.is_none(){status=child.wait().await.ok();}break;}},
                    result=child.wait(),if status.is_none()=>{status=result.ok();stdin.take();},
                    result=stdout.read(&mut outbuf),if !outdone=>{match result{Ok(0)|Err(_)=>outdone=true,Ok(n)=>{outbytes=outbytes.saturating_add(n);
        if options.max_stdout.is_some_and(|max|outbytes>max){output_limit=true;
        if stopping.is_none(){signal_group(group.0,false);stopping=Some(tokio::time::Instant::now());stdin.take();}}else{let action=output(&outbuf[..n]);
        if let Some(input)=&mut stdin{let _=input.write_all(&action.bytes).await;}
        if action.close{stdin.take();}}}}},
                    result=stderr.read(&mut errbuf),if !errdone=>{match result{Ok(0)|Err(_)=>errdone=true,Ok(n)=>errbytes=errbytes.saturating_add(n)}}
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
    #[tokio::test]
    async fn output_and_deadline_are_bounded_and_metadata_never_contains_output() {
        let token = CancellationToken::new();
        let (result, output) = capture(
            "/bin/sh",
            &["-c", "printf private-canary; exec sleep 5"],
            Path::new("/tmp"),
            Duration::from_millis(30),
            &token,
        )
        .await;
        assert_eq!(output, "private-canary");
        assert_eq!(result["timed_out"], true);
        assert!(!result.to_string().contains("private-canary"));
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
