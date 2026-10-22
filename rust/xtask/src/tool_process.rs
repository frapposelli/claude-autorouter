//! Opt-in tool subprocesses: bounded pipes, private fixtures and group cleanup.
use autorouter_core::auth::Environment;
use autorouter_core::js_json::JsDocument;
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
    crate::audit_support::random_hex::<32>()
        .map_err(|_| "Cannot obtain local authentication entropy".into())
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
fn signal_group(pid: u32, kill: bool) -> Result<(), i32> {
    #[cfg(unix)]
    {
        nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pid as i32),
            if kill {
                nix::sys::signal::Signal::SIGKILL
            } else {
                nix::sys::signal::Signal::SIGTERM
            },
        )
        .map_err(|error| error as i32)
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, kill);
        Ok(())
    }
}

// Own the Child as well as its numeric lease: Drop signals before Tokio's
// reaper can consume it. No consuming wait is polled until terminate retires
// this lease. Linux ECHILD instead retires an already lost identity.
struct ToolChild {
    child: tokio::process::Child,
    group: Option<u32>,
    signal_observations: Vec<Value>,
}
impl ToolChild {
    fn signal_ready_leader(&self, signal: nix::sys::signal::Signal) -> bool {
        // A readiness value supplies no identity. This is the same owned,
        // unreaped direct-child reservation, not the cleanup process group.
        #[cfg(unix)]
        {
            self.group.is_some_and(|pid| {
                nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), signal).is_ok()
            })
        }
        #[cfg(not(unix))]
        {
            let _ = signal;
            false
        }
    }
    fn signal(&mut self, kill: bool) {
        if let Some(pid) = self.group
            && let Err(errno) = signal_group(pid, kill)
        {
            self.record_signal("group", kill, Some(errno), None);
        }
    }
    fn record_signal(
        &mut self,
        target: &str,
        kill: bool,
        errno: Option<i32>,
        kind: Option<String>,
    ) {
        // Only TERM, final group KILL and direct KILL can occur. Keep the
        // first three exceptional observations; never retain command payloads.
        if self.signal_observations.len() < 3 {
            let absent = cfg!(unix) && errno == Some(nix::libc::ESRCH);
            self.signal_observations.push(json!({
                "target":target,"signal":if kill{"SIGKILL"}else{"SIGTERM"},
                "outcome":if absent{"absent"}else{"error"},"errno":errno,"kind":kind
            }));
        }
    }
    #[cfg(target_os = "linux")]
    fn retire(&mut self) {
        self.group = None;
    }
    fn terminate(&mut self) {
        let Some(pid) = self.group.take() else {
            return;
        };
        if let Err(errno) = signal_group(pid, true) {
            self.record_signal("group", true, Some(errno), None);
        }
        // The direct-child fallback is still protected by the same unreaped
        // reservation. Unix kill_on_drop is disabled, so it cannot run later
        // on a lost identity or after this first consuming wait.
        if let Err(error) = self.child.start_kill() {
            self.record_signal(
                "child",
                true,
                error.raw_os_error(),
                Some("direct_kill_error".into()),
            );
        }
    }
    async fn finish(&mut self, status: &mut Option<std::process::ExitStatus>) -> Result<(), ()> {
        // The production finalization seam: group cleanup/retirement is fully
        // synchronous, before the first poll of Tokio's consuming wait.
        self.terminate();
        if status.is_none() {
            *status = Some(self.child.wait().await.map_err(|_| ())?);
        }
        Ok(())
    }
}
impl Drop for ToolChild {
    fn drop(&mut self) {
        // Nonblocking signal then Tokio's normal best-effort orphan reaping.
        // Awaited completion below remains the normal joined cleanup path.
        self.terminate();
    }
}

enum ExitFailure {
    #[cfg(target_os = "linux")]
    OwnershipLost,
    Observation,
}
async fn wait_for_exit(
    _child: &mut ToolChild,
    observer: &mut Result<crate::process::ExitObserver, String>,
    pid: u32,
) -> Result<Option<std::process::ExitStatus>, ExitFailure> {
    #[cfg(unix)]
    {
        let observer = observer.as_mut().map_err(|_| ExitFailure::Observation)?;
        loop {
            match observer
                .observe_pid(pid)
                .map_err(|_| ExitFailure::Observation)?
            {
                crate::process::ExitObservation::Pending => {
                    tokio::time::sleep(Duration::from_millis(5)).await
                }
                crate::process::ExitObservation::Exited => return Ok(None),
                #[cfg(target_os = "linux")]
                crate::process::ExitObservation::OwnershipLost => {
                    return Err(ExitFailure::OwnershipLost);
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (observer, pid);
        _child
            .child
            .wait()
            .await
            .map(Some)
            .map_err(|_| ExitFailure::Observation)
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
#[derive(Clone, Copy)]
enum PipeSide {
    Stdout,
    Stderr,
}
#[derive(Default)]
struct OutputAction {
    input: InputAction,
    ready_signal: Option<nix::sys::signal::Signal>,
    overflow: bool,
}
struct OwnedRun {
    report: Value,
    ready_signal_attempts: u8,
    ready_signal_call_succeeded: bool,
    streams_eof: bool,
}
pub async fn run_child(
    command: &mut Command,
    options: RunOptions<'_>,
    mut output: impl FnMut(&[u8]) -> InputAction,
) -> Value {
    run_child_inner(command, options, |side, chunk| OutputAction {
        input: match side {
            PipeSide::Stdout => output(chunk),
            PipeSide::Stderr => InputAction::default(),
        },
        ..Default::default()
    })
    .await
    .report
}
async fn run_child_inner(
    command: &mut Command,
    options: RunOptions<'_>,
    mut observe: impl FnMut(PipeSide, &[u8]) -> OutputAction,
) -> OwnedRun {
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
        // Unix cleanup is owned by ToolChild before any Tokio reap. Keep the
        // existing automatic fallback on platforms without process groups.
        .kill_on_drop(cfg!(not(unix)));
    #[cfg(unix)]
    command.process_group(0);
    let started = Instant::now();
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            // Retain only the closed portable cause, never an OS message,
            // executable path, arguments, environment or captured payload.
            let kind = match error.kind() {
                std::io::ErrorKind::NotFound => "not_found",
                std::io::ErrorKind::PermissionDenied => "permission_denied",
                _ => "other",
            };
            return OwnedRun {
                report: json!({"spawn_error":true,"spawn_error_kind":kind,"exit_code":null,"exit_signal":null,"timed_out":false,"output_limit_exceeded":false,"duration_ms":0,"stdout_bytes":0,"stderr_bytes":0}),
                ready_signal_attempts: 0,
                ready_signal_call_succeeded: false,
                streams_eof: false,
            };
        }
    };
    let pid = child.id().expect("spawned child PID");
    let mut child = ToolChild {
        child,
        group: Some(pid),
        signal_observations: Vec::new(),
    };
    let mut observer = crate::process::ExitObserver::new(pid);
    let mut stdin = child.child.stdin.take();
    let mut stdout = child.child.stdout.take().unwrap();
    let mut stderr = child.child.stderr.take().unwrap();
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
    let mut exit_observed = false;
    let mut observation_error = false;
    let mut read_error = false;
    let mut normal_completion = false;
    let wait_error;
    #[cfg(target_os = "linux")]
    let mut ownership_lost = false;
    #[cfg(not(target_os = "linux"))]
    let ownership_lost = false;
    let mut stopping = None;
    let mut response_at = None;
    let mut ready_signal_attempts = 0;
    let mut ready_signal_call_succeeded = false;
    loop {
        if exit_observed && outdone && errdone {
            // A factual task exit is separate from process-group cleanup.
            // Read errors mark their pipe done but never count as clean EOF.
            normal_completion =
                stopping.is_none() && !timed_out && !output_limit && !cancelled && !read_error;
            wait_error = child.finish(&mut status).await.is_err();
            break;
        }
        let next = stopping
            .map(|at: tokio::time::Instant| at + options.grace)
            .unwrap_or(deadline);
        tokio::select! {biased;
                    _=options.cancel.cancelled(),if !cancelled&&stopping.is_none()=>{cancelled=true;child.signal(false);stopping=Some(tokio::time::Instant::now());pending_input.take();stdin.take();},
                    _=options.response.cancelled(),if options.interactive&&!response_received=>{response_received=true;response_at=Some(tokio::time::Instant::now()+Duration::from_secs(1));},
                    _=tokio::time::sleep_until(response_at.unwrap_or(deadline)),if response_at.is_some()&&stopping.is_none()=>{child.signal(false);stopping=Some(tokio::time::Instant::now());pending_input.take();stdin.take();},
                    _=tokio::time::sleep_until(next)=>{if stopping.is_none(){timed_out=true;child.signal(false);stopping=Some(tokio::time::Instant::now());pending_input.take();stdin.take();}else{wait_error=child.finish(&mut status).await.is_err();break;}},
                    result=wait_for_exit(&mut child,&mut observer,pid),if !exit_observed=>{
                        match result {
                            Ok(reaped)=>{exit_observed=true;status=reaped;pending_input.take();stdin.take();},
                            #[cfg(target_os = "linux")]
                            Err(ExitFailure::OwnershipLost)=>{ownership_lost=true;wait_error=false;child.retire();break;},
                            Err(ExitFailure::Observation)=>{observation_error=true;wait_error=child.finish(&mut status).await.is_err();break;},
                        }
                    },
                    result=stdout.read(&mut outbuf),if !outdone&&pending_input.is_none()=>{match result{Ok(0)=>outdone=true,Err(_)=>{read_error=true;outdone=true;},Ok(n)=>{outbytes=outbytes.saturating_add(n);
        if exceeds_output_limit(outbytes,errbytes,options.max_stdout){output_limit=true;
        if stopping.is_none(){child.signal(false);stopping=Some(tokio::time::Instant::now());pending_input.take();stdin.take();}}else{
            let action=observe(PipeSide::Stdout,&outbuf[..n]);
            if action.overflow {
                output_limit=true;
                if stopping.is_none(){child.signal(false);stopping=Some(tokio::time::Instant::now());pending_input.take();stdin.take();}
            } else {
                if let Some(signal)=action.ready_signal
                    && ready_signal_attempts==0 && stopping.is_none() && !options.cancel.is_cancelled()
                {
                    ready_signal_attempts+=1;
                    ready_signal_call_succeeded=child.signal_ready_leader(signal);
                }
                pending_input=stage_input(action.input,&mut stdin);input_offset=0;
            }
        }}}},
                    result=stderr.read(&mut errbuf),if !errdone=>{match result{Ok(0)=>errdone=true,Err(_)=>{read_error=true;errdone=true;},Ok(n)=>{errbytes=errbytes.saturating_add(n);
        if exceeds_output_limit(outbytes,errbytes,options.max_stdout)
            || observe(PipeSide::Stderr,&errbuf[..n]).overflow {
            output_limit=true;
            if stopping.is_none(){child.signal(false);stopping=Some(tokio::time::Instant::now());pending_input.take();stdin.take();}
        }}}},
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
    let mut report = json!({"exit_code":status.and_then(|s|s.code()),"exit_signal":signal,"timed_out":timed_out,"output_limit_exceeded":output_limit,"cancelled":cancelled,"duration_ms":started.elapsed().as_millis(),"stdout_bytes":outbytes,"stderr_bytes":errbytes,"response_received":response_received,"controlled_stop":response_received&&!timed_out&&!cancelled});
    if observation_error
        || wait_error
        || ownership_lost
        || read_error
        || !child.signal_observations.is_empty()
    {
        let signal_error = child
            .signal_observations
            .iter()
            .any(|event| event["outcome"] == "error");
        let supervision_error =
            observation_error || wait_error || ownership_lost || read_error || signal_error;
        // Only normal observed exit plus both actual EOFs can preserve task
        // exit code despite the final group KILL failing. Factual non-success
        // signals also survive other failures. This is not successful
        // cleanup: raw errors survive and no cleanup_confirmed:true is emitted.
        let postexit_group_error_only = normal_completion
            && !observation_error
            && !wait_error
            && !ownership_lost
            && !read_error
            && signal_error
            && child.signal_observations.iter().all(|event| {
                event["outcome"] != "error"
                    || (event["target"] == "group" && event["signal"] == "SIGKILL")
            });
        if supervision_error {
            report["controlled_stop"] = Value::Bool(false);
            report["cleanup_confirmed"] = Value::Bool(false);
            if !postexit_group_error_only {
                let observed_code = report["exit_code"].take();
                let observed_signal = report["exit_signal"].clone();
                report["observed_exit_code"] = observed_code;
                report["observed_exit_signal"] = observed_signal;
            }
        }
        report["supervision"] = json!({"observation_error":observation_error,"wait_error":wait_error,"ownership_lost":ownership_lost,"read_error":read_error,"signals":child.signal_observations});
    }
    OwnedRun {
        report,
        ready_signal_attempts,
        ready_signal_call_succeeded,
        streams_eof: outdone && errdone && !read_error,
    }
}

const LIFECYCLE_UTF16_LIMIT: usize = 1024 * 1024;
#[derive(Clone, Copy)]
enum CaptureMode {
    Bytes(usize),
    SourceLifecycle,
}
/// The Source lifecycle helper's defaults. Only SIGINT or SIGTERM may be
/// requested on readiness; cleanup cancellation is independently owned.
pub struct LifecycleOptions {
    pub timeout: Duration,
    pub signal_on_ready: Option<nix::sys::signal::Signal>,
    mode: CaptureMode,
}
impl Default for LifecycleOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            signal_on_ready: None,
            mode: CaptureMode::SourceLifecycle,
        }
    }
}
impl LifecycleOptions {
    fn capture_bytes(timeout: Duration) -> Self {
        Self {
            timeout,
            signal_on_ready: None,
            mode: CaptureMode::Bytes(100000),
        }
    }
}
/// Readiness and payloads stay separate from the existing safe process report.
/// JsDocument retains the complete parsed marker, including unknown fields,
/// non-finite Numbers and isolated UTF-16 surrogates, without a serde projection.
pub struct LifecycleCapture {
    pub process: Value,
    pub stdout: String,
    pub stderr: String,
    pub ready: Option<JsDocument>,
    pub observations: Value,
}
impl LifecycleCapture {
    pub fn into_parts(self) -> (Value, String, String, Option<JsDocument>, Value) {
        (
            self.process,
            self.stdout,
            self.stderr,
            self.ready,
            self.observations,
        )
    }
}
struct CapturedOutput {
    mode: CaptureMode,
    stdout_bytes: Vec<u8>,
    stdout: String,
    stderr: String,
    utf16_units: usize,
    overflow: bool,
    ready: Option<JsDocument>,
    signal_on_ready: Option<nix::sys::signal::Signal>,
}
impl CapturedOutput {
    fn new(options: &LifecycleOptions) -> Self {
        Self {
            mode: options.mode,
            stdout_bytes: Vec::new(),
            stdout: String::new(),
            stderr: String::new(),
            utf16_units: 0,
            overflow: false,
            ready: None,
            signal_on_ready: options.signal_on_ready,
        }
    }
    fn observe(&mut self, side: PipeSide, chunk: &[u8]) -> OutputAction {
        if matches!(self.mode, CaptureMode::Bytes(_)) {
            // The raw owner applies the aggregate byte ceiling BEFORE this
            // callback, including the rejected crossing chunk. Decode once at
            // completion, exactly as the existing capture consumer does.
            if matches!(side, PipeSide::Stdout) {
                self.stdout_bytes.extend_from_slice(chunk);
            }
            return OutputAction::default();
        }
        if self.overflow {
            return OutputAction {
                overflow: true,
                ..Default::default()
            };
        }
        // Node's `text += Buffer` decodes every observed data chunk separately.
        // Match that contract for our observed reads, not an asserted identity
        // between Node's event boundaries and Tokio's pipe read boundaries.
        let text = String::from_utf8_lossy(chunk);
        self.utf16_units = self.utf16_units.saturating_add(text.encode_utf16().count());
        match side {
            PipeSide::Stdout => self.stdout.push_str(&text),
            PipeSide::Stderr => self.stderr.push_str(&text),
        }
        self.overflow = self.utf16_units > LIFECYCLE_UTF16_LIMIT;
        if self.overflow {
            return OutputAction {
                overflow: true,
                ..Default::default()
            };
        }
        if matches!(side, PipeSide::Stdout) {
            // Re-scan all accumulated segments, INCLUDING the final segment.
            // A complete marker without LF is eligible, and a later chunk may
            // invalidate an earlier partial segment while exposing an older
            // complete marker. Last matching parsed value wins each scan.
            for line in self.stdout.split('\n') {
                if let Ok(document) = JsDocument::parse(line.as_bytes())
                    && document
                        .string_is(document.get(document.root(), "marker"), "lifecycle_ready")
                {
                    self.ready = Some(document);
                }
            }
        }
        OutputAction {
            ready_signal: if matches!(side, PipeSide::Stdout) && self.ready.is_some() {
                self.signal_on_ready
            } else {
                None
            },
            ..Default::default()
        }
    }
}
pub async fn capture_lifecycle(
    command: &mut Command,
    options: LifecycleOptions,
    cancel: &CancellationToken,
) -> Result<LifecycleCapture, String> {
    if options.signal_on_ready.is_some_and(|signal| {
        !matches!(
            signal,
            nix::sys::signal::Signal::SIGINT | nix::sys::signal::Signal::SIGTERM
        )
    }) {
        return Err("Readiness signal must be SIGINT or SIGTERM".into());
    }
    let mut captured = CapturedOutput::new(&options);
    let response = CancellationToken::new();
    let result = run_child_inner(
        command,
        RunOptions {
            timeout: options.timeout,
            grace: Duration::from_secs(1),
            max_stdout: match options.mode {
                CaptureMode::Bytes(limit) => Some(limit),
                CaptureMode::SourceLifecycle => None,
            },
            interactive: false,
            response: &response,
            cancel,
            initial: InputAction {
                close: true,
                ..Default::default()
            },
        },
        |side, chunk| captured.observe(side, chunk),
    )
    .await;
    if matches!(options.mode, CaptureMode::Bytes(_)) {
        captured.stdout = String::from_utf8_lossy(&captured.stdout_bytes).into_owned();
    }
    let observations = json!({
        "ready_signal_attempts":result.ready_signal_attempts,
        "ready_signal_call_succeeded":result.ready_signal_call_succeeded,
        "streams_eof":result.streams_eof,
        "utf16_units":matches!(options.mode,CaptureMode::SourceLifecycle).then_some(captured.utf16_units)
    });
    Ok(LifecycleCapture {
        process: result.report,
        stdout: captured.stdout,
        stderr: captured.stderr,
        ready: captured.ready,
        observations,
    })
}
pub async fn capture(
    command: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
    cancel: &CancellationToken,
) -> (Value, String) {
    let captured = capture_lifecycle(
        Command::new(command).args(args).current_dir(cwd),
        LifecycleOptions::capture_bytes(timeout),
        cancel,
    )
    .await
    .expect("byte capture never requests a readiness signal");
    let (process, stdout, _, _, _) = captured.into_parts();
    (process, stdout)
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
    async fn callback_interrupt(
        stop: &str,
        callback_started: &CancellationToken,
        run_finished: &CancellationToken,
        cancel: &CancellationToken,
    ) {
        if stop == "cancel" {
            tokio::select! { biased;
                _ = callback_started.cancelled() => cancel.cancel(),
                _ = run_finished.cancelled() => {},
            }
        }
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
            // Metadata only: no child output, arguments or input bytes.
            let completed = std::cell::Cell::new(None);
            let run_finished = CancellationToken::new();
            let run = async {
                let result = run_child(
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
                )
                .await;
                completed.set(Some((
                    result["timed_out"].as_bool(),
                    result["cancelled"].as_bool(),
                    result["output_limit_exceeded"].as_bool(),
                    result["exit_code"].as_i64(),
                    result["exit_signal"] == "SIGKILL",
                )));
                run_finished.cancel();
                result
            };
            let interrupt = callback_interrupt(stop, &callback_started, &run_finished, &cancel);
            let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
                tokio::join!(run, interrupt)
            })
            .await
            .unwrap_or_else(|_| panic!(
                "blocked callback stop={stop}, callback_started={}, child_marker={}, callback_marker={}, completed={:?}",
                callback_started.is_cancelled(), pid_file.exists(), callback_file.exists(), completed.get(),
            ));
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
    async fn callback_interrupt_finishes_when_child_exits_without_requesting_input() {
        for stop in ["deadline", "cancel", "output"] {
            let scratch = Scratch::new("tool-callback-silent-exit").unwrap();
            let pid_file = scratch.0.join("child.pid");
            let cancel = CancellationToken::new();
            let response = CancellationToken::new();
            let callback_started = CancellationToken::new();
            let run_finished = CancellationToken::new();
            let mut command = Command::new("/bin/sh");
            command.args([
                std::ffi::OsStr::new("-c"),
                std::ffi::OsStr::new("printf '%s' \"$$\" > \"$1\"; exit 17"),
                std::ffi::OsStr::new("synthetic-callback-silent-exit"),
                pid_file.as_os_str(),
            ]);
            let run = async {
                let result = run_child(
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
                    |_| panic!("silent child must not request callback input"),
                )
                .await;
                run_finished.cancel();
                result
            };
            let interrupt = callback_interrupt(stop, &callback_started, &run_finished, &cancel);
            let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
                tokio::join!(run, interrupt)
            })
            .await
            .expect("completed child must not leave a callback waiter");
            assert_child_reaped(&pid_file);
            assert!(run_finished.is_cancelled());
            assert!(!callback_started.is_cancelled());
            assert!(!cancel.is_cancelled());
            assert_eq!(result["exit_code"], 17);
            assert_eq!(result["exit_signal"], Value::Null);
            assert_eq!(result["stdout_bytes"], 0);
            assert_eq!(result["stderr_bytes"], 0);
            for flag in ["timed_out", "cancelled", "output_limit_exceeded"] {
                assert_eq!(result[flag], false, "{stop}: {flag}");
            }
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    mod ownership_regressions {
        use super::*;
        use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
        use nix::unistd::Pid;

        const READY: &[u8] = b"owner-ready\n";
        const TAIL_OUT: &[u8] = b"owner-tail-out\n";
        const TAIL_ERR: &[u8] = b"owner-tail-err\n";
        const LEADER_SCRIPT: &str = r#"
printf '%s' "$$" > "$1"
(exec /bin/sh "$6" "$2" "$3" "$5") &
while [ ! -f "$3" ]; do /bin/sleep 0.01; done
while [ ! -f "$4" ]; do /bin/sleep 0.01; done
exit 23
"#;
        const DESCENDANT_SCRIPT: &str = r#"
trap '' TERM
printf '%s' "$$" > "$1"
printf 'owner-ready\n'
: > "$2"
while [ ! -f "$3" ]; do /bin/sleep 0.01; done
printf 'owner-tail-out\n'
printf 'owner-tail-err\n' >&2
"#;

        struct GatedFixture {
            _scratch: Scratch,
            leader_script: PathBuf,
            descendant_script: PathBuf,
            leader_pid: PathBuf,
            descendant_pid: PathBuf,
            descendant_ready: PathBuf,
            leader_exit: PathBuf,
            descendant_tail: PathBuf,
        }
        impl GatedFixture {
            fn new() -> Self {
                let scratch = Scratch::new("tool-owned-exit-gates").unwrap();
                Self {
                    leader_script: scratch.file("leader.sh", LEADER_SCRIPT.as_bytes()).unwrap(),
                    descendant_script: scratch
                        .file("descendant.sh", DESCENDANT_SCRIPT.as_bytes())
                        .unwrap(),
                    leader_pid: scratch.0.join("leader.pid"),
                    descendant_pid: scratch.0.join("descendant.pid"),
                    descendant_ready: scratch.0.join("descendant.ready"),
                    leader_exit: scratch.0.join("leader-exit.gate"),
                    descendant_tail: scratch.0.join("descendant-tail.gate"),
                    _scratch: scratch,
                }
            }
            fn command(&self) -> Command {
                let mut command = Command::new("/bin/sh");
                command.args([
                    self.leader_script.as_os_str(),
                    self.leader_pid.as_os_str(),
                    self.descendant_pid.as_os_str(),
                    self.descendant_ready.as_os_str(),
                    self.leader_exit.as_os_str(),
                    self.descendant_tail.as_os_str(),
                    self.descendant_script.as_os_str(),
                ]);
                command
            }
            fn identities(&self) -> (u32, u32) {
                let read = |path: &Path| std::fs::read_to_string(path).unwrap().parse().unwrap();
                (read(&self.leader_pid), read(&self.descendant_pid))
            }
            fn release_leader(&self) {
                std::fs::write(&self.leader_exit, b"release").unwrap();
            }
            fn release_tail(&self) {
                std::fs::write(&self.descendant_tail, b"release").unwrap();
            }
        }

        fn options<'a>(
            response: &'a CancellationToken,
            cancel: &'a CancellationToken,
        ) -> RunOptions<'a> {
            RunOptions {
                timeout: Duration::from_secs(8),
                grace: Duration::from_millis(50),
                max_stdout: Some(256),
                interactive: false,
                response,
                cancel,
                initial: InputAction {
                    close: true,
                    ..Default::default()
                },
            }
        }

        async fn observed_exit(observer: &mut crate::process::ExitObserver, pid: u32) {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let observation = observer.observe_pid(pid).expect("owned child observation");
                    #[cfg(target_os = "linux")]
                    assert!(
                        !matches!(observation, crate::process::ExitObservation::OwnershipLost),
                        "test leader was reaped before its nonconsuming exit observation"
                    );
                    if matches!(observation, crate::process::ExitObservation::Exited) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("gated leader must exit without a consuming wait");
        }

        async fn direct_child_reaped_by_owner(pid: u32) {
            // Do not poll waitpid while Tokio's asynchronous orphan reaper is
            // pending: an Exited result would reap the child in the test itself.
            tokio::time::timeout(Duration::from_secs(4), async {
                loop {
                    match nix::sys::signal::kill(Pid::from_raw(pid as i32), None) {
                        Err(nix::errno::Errno::ESRCH) => break,
                        Ok(()) => tokio::time::sleep(Duration::from_millis(5)).await,
                        other => panic!("cannot inspect owned direct child: {other:?}"),
                    }
                }
            })
            .await
            .expect("live Tokio runtime must reap its dropped or joined direct child");
            assert_eq!(
                waitpid(Pid::from_raw(pid as i32), Some(WaitPidFlag::WNOHANG)),
                Err(nix::errno::Errno::ECHILD),
                "the test must not be the operation that consumes child status",
            );
        }

        async fn descendant_inactive_or_zombie(pid: u32) {
            // A grandchild can remain a zombie under a non-reaping Linux PID1.
            // Inspect state with bounded capture, rather than requiring ESRCH.
            let pid_text = pid.to_string();
            let cancel = CancellationToken::new();
            tokio::time::timeout(Duration::from_secs(4), async {
                loop {
                    let (report, state) = capture(
                        "/bin/ps",
                        &["-o", "stat=", "-p", &pid_text],
                        Path::new("/tmp"),
                        Duration::from_secs(1),
                        &cancel,
                    )
                    .await;
                    let state = state.trim();
                    if (report["exit_code"] == 1 && state.is_empty())
                        || (report["exit_code"] == 0 && state.starts_with('Z'))
                    {
                        break;
                    }
                    assert_eq!(report["timed_out"], false, "state inspection: {report}");
                    assert_eq!(
                        report["output_limit_exceeded"], false,
                        "state inspection: {report}"
                    );
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("owned descendant must be absent or a non-running zombie");
        }

        #[tokio::test]
        async fn terminate_retires_the_group_before_independent_waitpid_consumes_status() {
            for consuming_wait_first in [false, true] {
                let mut command = Command::new("/bin/sh");
                command
                    .args(["-c", "exit 23"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .process_group(0)
                    .kill_on_drop(false);
                let child = command.spawn().unwrap();
                let pid = child.id().unwrap();
                let mut owner = ToolChild {
                    child,
                    group: Some(pid),
                    signal_observations: Vec::new(),
                };
                let mut observer = crate::process::ExitObserver::new(pid).unwrap();
                observed_exit(&mut observer, pid).await;

                // Real production retirement precedes every consuming wait.
                // The positive schedule performs no Tokio wait/try_wait before
                // independent waitpid proves that status23 is still available.
                owner.terminate();
                assert!(owner.group.is_none());
                if consuming_wait_first {
                    // Negative control: deliberately consume through Tokio;
                    // the independent OS proof must now distinguish ECHILD.
                    let status = tokio::time::timeout(Duration::from_secs(3), owner.child.wait())
                        .await
                        .expect("negative-control Tokio wait must finish")
                        .unwrap();
                    assert_eq!(status.code(), Some(23));
                }
                let outcome = tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        match waitpid(Pid::from_raw(pid as i32), Some(WaitPidFlag::WNOHANG)) {
                            // Darwin NOTE_EXIT can precede waitable SZOMB.
                            // EINTR is also not a terminal ownership verdict.
                            Ok(WaitStatus::StillAlive) | Err(nix::errno::Errno::EINTR) => {
                                tokio::time::sleep(Duration::from_millis(1)).await;
                            }
                            terminal => break terminal,
                        }
                    }
                })
                .await
                .expect("independent waitpid must reach a terminal ownership result");
                if consuming_wait_first {
                    assert_eq!(outcome, Err(nix::errno::Errno::ECHILD));
                } else {
                    assert_eq!(
                        outcome,
                        Ok(WaitStatus::Exited(Pid::from_raw(pid as i32), 23)),
                        "production terminate must leave the owned leader unreaped",
                    );
                }
                drop(owner); // Retired lease cannot signal after either wait.
                assert_eq!(
                    waitpid(Pid::from_raw(pid as i32), Some(WaitPidFlag::WNOHANG)),
                    Err(nix::errno::Errno::ECHILD),
                );
            }
        }

        #[tokio::test]
        async fn leader_exit_does_not_discard_later_same_group_pipe_tails() {
            let fixture = GatedFixture::new();
            let mut command = fixture.command();
            let cancel = CancellationToken::new();
            let response = CancellationToken::new();
            let ready = CancellationToken::new();
            let mut captured = Vec::new();
            let run = run_child(&mut command, options(&response, &cancel), |chunk| {
                captured.extend_from_slice(chunk);
                if captured.starts_with(READY) {
                    ready.cancel();
                }
                InputAction::default()
            });
            let release = async {
                ready.cancelled().await;
                let (leader, descendant) = fixture.identities();
                let mut observer = crate::process::ExitObserver::new(leader).unwrap();
                fixture.release_leader();
                observed_exit(&mut observer, leader).await;
                fixture.release_tail(); // Deliberately after independently observed leader exit.
                (leader, descendant)
            };
            let (report, (leader, descendant)) =
                tokio::time::timeout(Duration::from_secs(12), async {
                    tokio::join!(run, release)
                })
                .await
                .expect("leader exit and delayed pipe EOF must finish together");
            assert_eq!(captured, [READY, TAIL_OUT].concat());
            assert_eq!(report["stdout_bytes"], READY.len() + TAIL_OUT.len());
            assert_eq!(report["stderr_bytes"], TAIL_ERR.len());
            assert_eq!(
                report["exit_code"], 23,
                "actual leader status must survive cleanup: {report}"
            );
            assert_eq!(report["exit_signal"], Value::Null);
            for flag in ["timed_out", "cancelled", "output_limit_exceeded"] {
                assert_eq!(report[flag], false, "{flag}: {report}");
            }
            // A factual status23 is not proof of group cleanup; raw post-exit
            // EPERM and cleanup_confirmed:false must remain visible if present.
            direct_child_reaped_by_owner(leader).await;
            descendant_inactive_or_zombie(descendant).await;
        }

        #[tokio::test]
        async fn cancellation_after_observed_leader_exit_forces_the_term_ignoring_descendant() {
            let fixture = GatedFixture::new();
            let mut command = fixture.command();
            let cancel = CancellationToken::new();
            let response = CancellationToken::new();
            let ready = CancellationToken::new();
            let mut captured = Vec::new();
            let run = run_child(&mut command, options(&response, &cancel), |chunk| {
                captured.extend_from_slice(chunk);
                if captured.starts_with(READY) {
                    ready.cancel();
                }
                InputAction::default()
            });
            let interrupt = async {
                ready.cancelled().await;
                let (leader, descendant) = fixture.identities();
                let mut observer = crate::process::ExitObserver::new(leader).unwrap();
                fixture.release_leader();
                observed_exit(&mut observer, leader).await;
                assert!(!fixture.descendant_tail.exists());
                cancel.cancel(); // Readiness and exit, never a startup-duration guess.
                (leader, descendant)
            };
            let (report, (leader, descendant)) =
                tokio::time::timeout(Duration::from_secs(12), async {
                    tokio::join!(run, interrupt)
                })
                .await
                .expect("cancellation must force cleanup despite already exited leader");
            assert_eq!(captured, READY);
            assert_eq!(report["stdout_bytes"], READY.len());
            assert_eq!(report["stderr_bytes"], 0);
            assert_eq!(report["cancelled"], true);
            assert_eq!(report["timed_out"], false);
            assert_eq!(report["output_limit_exceeded"], false);
            assert_eq!(
                report["exit_code"], 23,
                "cancellation must join the actual leader: {report}"
            );
            assert_eq!(report["exit_signal"], Value::Null);
            assert!(!fixture.descendant_tail.exists());
            direct_child_reaped_by_owner(leader).await;
            descendant_inactive_or_zombie(descendant).await;
        }

        #[tokio::test]
        async fn dropping_boxed_run_future_cleans_group_and_reaps_before_or_after_leader_exit() {
            for after_exit in [false, true] {
                let fixture = GatedFixture::new();
                let mut command = fixture.command();
                let cancel = CancellationToken::new();
                let response = CancellationToken::new();
                let ready = CancellationToken::new();
                let mut captured = Vec::new();
                let mut run = Box::pin(run_child(
                    &mut command,
                    options(&response, &cancel),
                    |chunk| {
                        captured.extend_from_slice(chunk);
                        if captured.starts_with(READY) {
                            ready.cancel();
                        }
                        InputAction::default()
                    },
                ));
                tokio::time::timeout(Duration::from_secs(4), async {
                    tokio::select! {
                        _ = ready.cancelled() => {},
                        report = run.as_mut() => panic!("gated run finished before readiness: {report}"),
                    }
                }).await.expect("boxed run must reach actual output readiness");
                let (leader, descendant) = fixture.identities();
                let mut observer = crate::process::ExitObserver::new(leader).unwrap();
                if after_exit {
                    fixture.release_leader();
                    tokio::select! {
                        _ = observed_exit(&mut observer, leader) => {},
                        report = run.as_mut() => panic!("tail-gated run finished before drop: {report}"),
                    }
                } else {
                    assert!(matches!(
                        observer.observe_pid(leader).unwrap(),
                        crate::process::ExitObservation::Pending,
                    ));
                    assert!(!fixture.leader_exit.exists());
                }
                drop(run); // Drops the actual owned future on a live Tokio runtime.
                assert_eq!(captured, READY);
                assert!(!fixture.descendant_tail.exists());
                direct_child_reaped_by_owner(leader).await;
                descendant_inactive_or_zombie(descendant).await;
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_failures_preserve_closed_not_found_permission_denied_and_other_kinds() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("spawn-kind").unwrap();
        let missing = scratch.0.join("PRIVATE-missing-program");
        let denied_bytes = b"#!/bin/sh\nexit 99\n";
        let denied = scratch
            .file("PRIVATE-non-executable", denied_bytes)
            .unwrap();
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(!missing.exists());
        assert!(std::fs::symlink_metadata(&denied).unwrap().is_file());
        assert_eq!(
            std::fs::metadata(&denied).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let cancel = CancellationToken::new();
        let response = CancellationToken::new();
        let mut observed = Vec::new();
        for (program, argument, expected_kind) in [
            (missing.as_path(), None, "not_found"),
            (denied.as_path(), None, "permission_denied"),
            (Path::new("/bin/sh"), Some("PRIVATE\0argument"), "other"),
        ] {
            let mut callbacks = 0;
            let mut command = Command::new(program);
            if let Some(argument) = argument {
                command.arg(argument);
            }
            command.current_dir(&scratch.0).env_clear();
            let result = tokio::time::timeout(
                Duration::from_secs(3),
                run_child(
                    &mut command,
                    RunOptions {
                        timeout: Duration::from_millis(100),
                        grace: Duration::from_millis(100),
                        max_stdout: Some(1024),
                        interactive: false,
                        response: &response,
                        cancel: &cancel,
                        initial: InputAction {
                            close: true,
                            ..Default::default()
                        },
                    },
                    |_| {
                        callbacks += 1;
                        InputAction::default()
                    },
                ),
            )
            .await
            .expect("spawn failure must complete within independent watchdog");
            assert_eq!(callbacks, 0);
            assert!(!cancel.is_cancelled() && !response.is_cancelled());
            assert!(!result.to_string().contains("PRIVATE"));
            observed.push((expected_kind, result));
        }
        assert!(!missing.exists());
        assert_eq!(std::fs::read(&denied).unwrap(), denied_bytes);
        assert_eq!(
            std::fs::metadata(&denied).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let directory = scratch.0.clone();
        drop(scratch);
        assert!(!directory.exists());
        // All three real failures and private-fixture cleanup are observed before
        // asserting the new metadata, so the unchanged implementation emits
        // all RED controls rather than stopping after the first missing field.
        for (expected_kind, actual) in &observed {
            eprintln!(
                "SPAWN_KIND_OBSERVATION {}",
                json!({"expected_kind":expected_kind,"actual":actual})
            );
        }
        for (expected_kind, actual) in observed {
            assert_eq!(
                actual,
                json!({
                    "spawn_error":true, "spawn_error_kind":expected_kind,
                    "exit_code":null, "exit_signal":null, "timed_out":false,
                    "output_limit_exceeded":false, "duration_ms":0,
                    "stdout_bytes":0, "stderr_bytes":0
                })
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn postspawn_exit_and_original_hundred_ms_deadline_omit_spawn_causes() {
        let cancel = CancellationToken::new();
        for (script, timeout_ms, timed_out) in
            [("exit 17", 3000, false), ("exec /bin/sleep 5", 100, true)]
        {
            let (report, output) = tokio::time::timeout(
                Duration::from_secs(5),
                capture(
                    "/bin/sh",
                    &["-c", script],
                    Path::new("/"),
                    Duration::from_millis(timeout_ms),
                    &cancel,
                ),
            )
            .await
            .expect("postspawn control must terminate and join within watchdog");
            assert_eq!(output, "");
            assert_eq!(report["stderr_bytes"], 0);
            assert_eq!(report["timed_out"], timed_out);
            assert_eq!(report["cancelled"], false);
            assert_eq!(report["output_limit_exceeded"], false);
            assert_eq!(
                report["exit_code"],
                if timed_out { Value::Null } else { json!(17) }
            );
            if timed_out {
                assert_eq!(report["exit_signal"], "SIGTERM");
            }
            assert!(report.get("spawn_error").is_none());
            assert!(report.get("spawn_error_kind").is_none());
            assert!(!cancel.is_cancelled());
            eprintln!(
                "POSTSPAWN_CAUSE_ABSENCE {}",
                json!({"timeout_ms":timeout_ms,"report":report})
            );
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn lifecycle_readiness_signals_the_owned_leader_once_and_keeps_the_last_marker() {
        use nix::sys::signal::Signal;
        for (signal, name) in [(Signal::SIGINT, "INT"), (Signal::SIGTERM, "TERM")] {
            let scratch = Scratch::new("lifecycle-ready").unwrap();
            let receipt = scratch.file("signal-received", b"").unwrap();
            let cancel = CancellationToken::new();
            let first = r#"{"marker":"lifecycle_ready","gateway":"synthetic","phase":1,"pid":"not-an-owned-pid"}"#;
            let second = r#"{"marker":"lifecycle_ready","gateway":"synthetic","phase":2,"opaque":"\ud800","pid":"not-an-owned-pid"}"#;
            // The second marker can only be emitted AFTER the first marker
            // has triggered the signal. Initial readiness has no trailing LF.
            let script = format!(
                "trap 'printf received >> \"$1\"; printf \"\\n%s\\n\" \"$2\"; exit 23' {name}\nprintf '%s' \"$3\"\nwhile :; do :; done"
            );
            let mut command = Command::new("/bin/sh");
            command.env_clear().current_dir(&scratch.0).args([
                std::ffi::OsStr::new("-c"),
                script.as_ref(),
                std::ffi::OsStr::new("synthetic-readiness"),
                receipt.as_os_str(),
                second.as_ref(),
                first.as_ref(),
            ]);
            let defaults = LifecycleOptions::default();
            assert_eq!(defaults.timeout, Duration::from_secs(10));
            assert!(matches!(defaults.mode, CaptureMode::SourceLifecycle));
            let options = LifecycleOptions {
                signal_on_ready: Some(signal),
                ..defaults
            };
            let captured = tokio::time::timeout(
                Duration::from_secs(12),
                capture_lifecycle(&mut command, options, &cancel),
            )
            .await
            .expect("owned lifecycle child must finish")
            .unwrap();
            assert_eq!(captured.process["exit_code"], 23, "{}", captured.process);
            let ready = captured.ready.as_ref().expect("full readiness document");
            assert!(ready.string_is(ready.get(ready.root(), "gateway"), "synthetic"));
            assert_eq!(captured.stderr, "");
            assert_eq!(captured.process["stderr_bytes"], 0);
            assert_eq!(captured.stdout, format!("{first}\n{second}\n"));
            assert_eq!(ready.stringify(), second);
            assert_eq!(captured.observations["ready_signal_attempts"], 1);
            assert_eq!(captured.observations["ready_signal_call_succeeded"], true);
            assert_eq!(captured.observations["streams_eof"], true);
            assert_eq!(std::fs::read(&receipt).unwrap(), b"received");
            assert_eq!(captured.process["timed_out"], false);
            assert_eq!(captured.process["output_limit_exceeded"], false);
            assert_eq!(captured.process["cancelled"], false);
            assert!(!cancel.is_cancelled());
            assert!(!captured.process.to_string().contains("not-an-owned-pid"));
            eprintln!(
                "LIFECYCLE_READY_SIGNAL {}",
                json!({
                    "requested":name,"process":captured.process,
                    "observations":captured.observations,
                    "ready":ready.stringify(),"stderr":captured.stderr
                })
            );
        }
    }

    #[test]
    fn lifecycle_readiness_rescans_split_and_final_segments_without_losing_js_values() {
        use autorouter_core::js_json::JsNode;
        let options = LifecycleOptions::default();
        let mut captured = CapturedOutput::new(&options);
        captured.observe(PipeSide::Stdout, br#"{"marker":"lifecycle_"#);
        assert!(captured.ready.is_none());
        let suffix = format!(
            r#"ready","gateway":"synthetic","opaque":"\ud800","n":1e400,"dup":"old","dup":"last","deep":{}{}}}"#,
            "[".repeat(512),
            "]".repeat(512)
        );
        captured.observe(PipeSide::Stdout, suffix.as_bytes());
        let ready = captured.ready.as_ref().unwrap();
        assert!(ready.string_is(ready.get(ready.root(), "gateway"), "synthetic"));
        assert!(ready.string_is(ready.get(ready.root(), "dup"), "last"));
        assert_eq!(
            ready
                .string(ready.get(ready.root(), "opaque").unwrap())
                .unwrap()
                .units(),
            &[0xd800]
        );
        match ready.node(ready.get(ready.root(), "n").unwrap()).unwrap() {
            JsNode::Number(value) => assert_eq!(*value, f64::INFINITY),
            _ => panic!("original overflow Number must remain owned"),
        }
        let retained = captured.ready.take().unwrap();
        drop(captured);
        assert!(retained.stringify().contains(r#""opaque":"\ud800""#));
        assert!(retained.stringify().contains(r#""n":null"#));

        let mut captured = CapturedOutput::new(&options);
        captured.observe(PipeSide::Stdout,
            b"{\"marker\":\"lifecycle_ready\",\"gateway\":\"older\"}\n{\"marker\":\"lifecycle_ready\",\"gateway\":\"partial\"}");
        assert!(
            captured
                .ready
                .as_ref()
                .unwrap()
                .stringify()
                .contains("partial")
        );
        // The pending final segment was valid, then becomes invalid. Source
        // re-scans the earlier complete marker and returns it again.
        captured.observe(PipeSide::Stdout, b"x");
        assert!(
            captured
                .ready
                .as_ref()
                .unwrap()
                .stringify()
                .contains("older")
        );
        captured.observe(
            PipeSide::Stdout,
            b"\n{\"marker\":\"lifecycle_ready\",\"gateway\":\"last\"}",
        );
        assert!(
            captured
                .ready
                .as_ref()
                .unwrap()
                .stringify()
                .contains("last")
        );
    }

    #[test]
    fn lifecycle_capture_keeps_distinct_utf16_chunk_and_legacy_byte_budgets() {
        let options = LifecycleOptions::default();
        let mut captured = CapturedOutput::new(&options);
        let ascii_multibyte = "€".repeat(LIFECYCLE_UTF16_LIMIT / 2);
        let supplementary = "🦀".repeat(LIFECYCLE_UTF16_LIMIT / 4);
        assert!(
            !captured
                .observe(PipeSide::Stdout, ascii_multibyte.as_bytes())
                .overflow
        );
        assert!(
            !captured
                .observe(PipeSide::Stderr, supplementary.as_bytes())
                .overflow
        );
        assert_eq!(captured.utf16_units, LIFECYCLE_UTF16_LIMIT);
        assert!(captured.stdout.len() + captured.stderr.len() > LIFECYCLE_UTF16_LIMIT);
        assert!(captured.observe(PipeSide::Stderr, b"x").overflow);
        assert_eq!(captured.utf16_units, LIFECYCLE_UTF16_LIMIT + 1);
        let before = (captured.stdout.len(), captured.stderr.len());
        assert!(
            captured
                .observe(PipeSide::Stdout, b"not-retained-after-overflow")
                .overflow
        );
        assert_eq!((captured.stdout.len(), captured.stderr.len()), before);

        let mut source = CapturedOutput::new(&options);
        source.observe(PipeSide::Stderr, &[0xf0, 0x9f]);
        source.observe(PipeSide::Stderr, &[0x98, 0x80]);
        assert_eq!(source.stderr, "\u{fffd}\u{fffd}\u{fffd}");
        assert_eq!(source.utf16_units, 3);
        let mut legacy =
            CapturedOutput::new(&LifecycleOptions::capture_bytes(Duration::from_secs(1)));
        legacy.observe(PipeSide::Stdout, &[0xf0, 0x9f]);
        legacy.observe(PipeSide::Stdout, &[0x98, 0x80]);
        assert_eq!(String::from_utf8_lossy(&legacy.stdout_bytes), "😀");
        assert!(legacy.stderr.is_empty());
        assert!(legacy.ready.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lifecycle_readiness_cannot_bypass_cancellation_deadline_or_output_bounds() {
        let cancel = CancellationToken::new();
        let rejected = capture_lifecycle(
            &mut Command::new("/nonexistent/synthetic-invalid-readiness-signal"),
            LifecycleOptions {
                signal_on_ready: Some(nix::sys::signal::Signal::SIGKILL),
                ..Default::default()
            },
            &cancel,
        )
        .await;
        assert!(
            matches!(rejected, Err(error) if error == "Readiness signal must be SIGINT or SIGTERM")
        );
        assert!(!cancel.is_cancelled());
        for (name, script, deadline, precancel) in [
            (
                "cancel",
                "printf '%s' '{\"marker\":\"lifecycle_ready\"}'; exec /bin/sleep 30",
                Duration::from_secs(10),
                true,
            ),
            (
                "deadline",
                "exec /bin/sleep 30",
                Duration::from_millis(100),
                false,
            ),
            (
                "output",
                "/usr/bin/head -c 524288 /dev/zero; /usr/bin/head -c 524289 /dev/zero >&2; exec /bin/sleep 30",
                Duration::from_secs(10),
                false,
            ),
        ] {
            let cancel = CancellationToken::new();
            if precancel {
                cancel.cancel();
            }
            let options = LifecycleOptions {
                timeout: deadline,
                signal_on_ready: Some(nix::sys::signal::Signal::SIGINT),
                ..Default::default()
            };
            let captured = tokio::time::timeout(
                Duration::from_secs(12),
                capture_lifecycle(
                    Command::new("/bin/sh").env_clear().args(["-c", script]),
                    options,
                    &cancel,
                ),
            )
            .await
            .expect("bounded lifecycle stop")
            .unwrap();
            assert_eq!(
                captured.process["cancelled"], precancel,
                "{name}: {}",
                captured.process
            );
            assert_eq!(
                captured.process["timed_out"],
                name == "deadline",
                "{name}: {}",
                captured.process
            );
            assert_eq!(
                captured.process["output_limit_exceeded"],
                name == "output",
                "{name}: {}",
                captured.process
            );
            assert_eq!(captured.observations["ready_signal_attempts"], 0);
            assert_eq!(captured.observations["ready_signal_call_succeeded"], false);
            assert_eq!(cancel.is_cancelled(), precancel);
            if name == "output" {
                assert_eq!(
                    captured.observations["utf16_units"],
                    LIFECYCLE_UTF16_LIMIT + 1
                );
            }
            eprintln!(
                "LIFECYCLE_READY_STOP {}",
                json!({"case":name,"process":captured.process,"observations":captured.observations})
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn integrated_byte_capture_does_not_append_the_rejected_crossing_chunk() {
        let cancel = CancellationToken::new();
        let (report, stdout) = tokio::time::timeout(
            Duration::from_secs(5),
            capture(
                "/bin/sh",
                &[
                    "-c",
                    "/usr/bin/head -c 100001 /dev/zero; exec /bin/sleep 30",
                ],
                Path::new("/tmp"),
                Duration::from_secs(3),
                &cancel,
            ),
        )
        .await
        .expect("bounded existing capture consumer");
        assert_eq!(report["output_limit_exceeded"], true, "{report}");
        assert_eq!(report["timed_out"], false);
        assert!(report["stdout_bytes"].as_u64().unwrap() > 100000);
        assert!(stdout.len() <= 100000);
        assert!(stdout.bytes().all(|byte| byte == 0));
        assert!(report.get("ready_signal_attempts").is_none());
        assert!(report.get("ready").is_none());
        assert!(!cancel.is_cancelled());
    }
}
