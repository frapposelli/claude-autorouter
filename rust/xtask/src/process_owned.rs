//! Developer-tool child owner: aggregate bounded pipes and unreaped group ID.
//! Intended as a child of process.rs, sharing its safe OwnedChild/ExitObserver.
use super::{ExitObserver, OwnedChild};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use serde_json::{Value, json};
use std::io::Read;
use std::os::fd::AsFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

pub(crate) const OUTPUT_LIMIT: usize = 2 * 1024 * 1024;
pub(crate) const DEADLINE: Duration = Duration::from_secs(900);
pub(crate) const TERM_GRACE: Duration = Duration::from_secs(5);

pub(crate) struct Output {
    pub status: Value,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
#[derive(Debug)]
pub(crate) struct Failure {
    pub spawned: bool,
    pub code: String,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.code)
    }
}
impl From<Failure> for String {
    fn from(failure: Failure) -> Self {
        failure.code
    }
}
impl Failure {
    pub fn evidence(&self) -> Value {
        json!({"spawned":self.spawned,"worker_joined":true,"complete":false,"worker_error":self.code,"output_capture_available":false})
    }
}

#[derive(Clone, Copy)]
struct Bounds {
    timeout: Duration,
    grace: Duration,
    output: usize,
}
impl Default for Bounds {
    fn default() -> Self {
        Self {
            timeout: DEADLINE,
            grace: TERM_GRACE,
            output: OUTPUT_LIMIT,
        }
    }
}

/// One blocking worker owns spawn, every signal, exit observation, reap and
/// pipe closure. Dropping a join waiter cancels that worker but preserves its
/// handle in this owner; close().await is the explicit joined cleanup barrier.
/// Drop is nonblocking: it cancels and detaches the same bounded worker, which
/// still owns the unreaped child and performs TERM then KILL before reaping.
pub(crate) struct Owner {
    cancel: CancellationToken,
    worker: Option<tokio::task::JoinHandle<Result<Output, String>>>,
    spawned: Arc<AtomicBool>,
}
struct CancelWaiter(CancellationToken, bool);
impl Drop for CancelWaiter {
    fn drop(&mut self) {
        if self.1 {
            self.0.cancel();
        }
    }
}
impl Owner {
    pub fn start(command: Command, caller: &CancellationToken) -> Self {
        Self::with_bounds(command, caller, Bounds::default())
    }
    pub fn metadata(command: Command, caller: &CancellationToken) -> Self {
        Self::with_bounds(
            command,
            caller,
            Bounds {
                timeout: Duration::from_secs(5),
                grace: Duration::from_secs(1),
                output: 65536,
            },
        )
    }
    fn with_bounds(command: Command, caller: &CancellationToken, bounds: Bounds) -> Self {
        let cancel = caller.child_token();
        let child_cancel = cancel.clone();
        let spawned = Arc::new(AtomicBool::new(false));
        let witness = spawned.clone();
        let worker =
            tokio::task::spawn_blocking(move || capture(command, &child_cancel, bounds, &witness));
        Self {
            cancel,
            worker: Some(worker),
            spawned,
        }
    }
    pub async fn join(&mut self) -> Result<Output, Failure> {
        let mut waiter = CancelWaiter(self.cancel.clone(), true);
        let result = self
            .worker
            .as_mut()
            .ok_or_else(|| Failure {
                spawned: self.spawned.load(Ordering::Acquire),
                code: "Owned child has already been joined".into(),
            })?
            .await
            .map_err(|_| Failure {
                spawned: self.spawned.load(Ordering::Acquire),
                code: "Owned child worker failed".into(),
            })?;
        self.worker.take();
        waiter.1 = false;
        result.map_err(|code| Failure {
            spawned: self.spawned.load(Ordering::Acquire),
            code,
        })
    }
    pub async fn close(mut self) -> Result<Output, Failure> {
        self.cancel.cancel();
        self.join().await
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn nonblocking(pipe: &impl AsFd) -> Result<(), String> {
    let flags = fcntl(pipe, FcntlArg::F_GETFL).map_err(|_| "Cannot inspect owned child pipe")?;
    fcntl(
        pipe,
        FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK),
    )
    .map_err(|_| "Cannot configure owned child pipe")?;
    Ok(())
}

struct Pipes {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_received: u64,
    stderr_received: u64,
    overflow: bool,
    read_error: bool,
}
impl Pipes {
    fn drain(&mut self, reader: &mut impl Read, stdout: bool, limit: usize) -> bool {
        let mut chunk = [0u8; 8192];
        // A noisy process cannot starve cancellation/exit/deadline observation.
        for _ in 0..8 {
            match reader.read(&mut chunk) {
                Ok(0) => return true,
                Ok(n) => {
                    let received = if stdout {
                        &mut self.stdout_received
                    } else {
                        &mut self.stderr_received
                    };
                    *received = received.saturating_add(n as u64);
                    let retain = n.min(limit.saturating_sub(self.stdout.len() + self.stderr.len()));
                    let target = if stdout {
                        &mut self.stdout
                    } else {
                        &mut self.stderr
                    };
                    target.extend_from_slice(&chunk[..retain]);
                    self.overflow |= retain != n;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return false,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => {
                    self.read_error = true;
                    return true;
                }
            }
        }
        false
    }
}
fn capture(
    mut command: Command,
    cancel: &CancellationToken,
    bounds: Bounds,
    spawned: &AtomicBool,
) -> Result<Output, String> {
    if cancel.is_cancelled() {
        return Ok(Output {
            status: json!({"spawned":false,"cancelled":true,"complete":false}),
            stdout: vec![],
            stderr: vec![],
        });
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("NODE_OPTIONS")
        .env_remove("NODE_PATH")
        .process_group(0);
    let started = Instant::now();
    let child = command
        .spawn()
        .map_err(|_| "Cannot start owned evaluator tool")?;
    let mut child = OwnedChild::new(child);
    spawned.store(true, Ordering::Release);
    let mut observer = ExitObserver::new(child.id())?;
    let mut stdout = child.stdout.take().ok_or("Missing owned child stdout")?;
    let mut stderr = child.stderr.take().ok_or("Missing owned child stderr")?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let mut pipes = Pipes {
        stdout: vec![],
        stderr: vec![],
        stdout_received: 0,
        stderr_received: 0,
        overflow: false,
        read_error: false,
    };
    let (mut out_done, mut err_done) = (false, false);
    let mut stopping = None;
    let (mut cancelled, mut timed_out, mut forced_kill) = (false, false, false);
    let status = loop {
        if !out_done {
            out_done = pipes.drain(&mut stdout, true, bounds.output);
        }
        if !err_done {
            err_done = pipes.drain(&mut stderr, false, bounds.output);
        }
        cancelled |= cancel.is_cancelled();
        timed_out |= started.elapsed() >= bounds.timeout;
        // Observe without reaping, then kill the still-owned group before wait.
        // This preserves the PID reservation even if the leader exits early.
        if observer.exited(&mut child)? {
            child.cleanup();
            break child
                .wait()
                .map_err(|_| "Cannot reap owned evaluator tool")?;
        }
        if stopping.is_none() && (cancelled || timed_out || pipes.overflow || pipes.read_error) {
            let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGTERM);
            stopping = Some(Instant::now());
        }
        if stopping.is_some_and(|at| at.elapsed() >= bounds.grace) {
            forced_kill = true;
            child.cleanup();
            break child
                .wait()
                .map_err(|_| "Cannot reap stopped evaluator tool")?;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // Group cleanup has already happened, and both pipes are nonblocking.
    // Retain remaining buffered output, bounded by the same aggregate quota.
    for _ in 0..64 {
        if !out_done {
            out_done = pipes.drain(&mut stdout, true, bounds.output);
        }
        if !err_done {
            err_done = pipes.drain(&mut stderr, false, bounds.output);
        }
        if out_done && err_done {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let complete =
        out_done && err_done && !cancelled && !timed_out && !pipes.overflow && !pipes.read_error;
    Ok(Output {
        status: json!({"spawned":true,"exit_code":status.code(),"exit_signal":status.signal(),"cancelled":cancelled,"timed_out":timed_out,"output_limit_exceeded":pipes.overflow,"output_read_error":pipes.read_error,"output_complete":out_done&&err_done,"forced_kill":forced_kill,"complete":complete,"stdout_bytes_observed":pipes.stdout_received,"stderr_bytes_observed":pipes.stderr_received,"aggregate_bytes_retained":pipes.stdout.len()+pipes.stderr.len(),"duration_ms":started.elapsed().as_millis(),"ownership":"unreaped leader kept reserved until group cleanup; worker is joined before returning"}),
        stdout: pipes.stdout,
        stderr: pipes.stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }
    fn bounds(output: usize) -> Bounds {
        Bounds {
            // Most tests here exercise output and ownership, not deadline
            // precision. Leave room for child scheduling in a parallel suite;
            // the timeout-specific case below sets its own shorter bound.
            timeout: Duration::from_secs(5),
            grace: Duration::from_millis(30),
            output,
        }
    }
    #[tokio::test]
    async fn completed_nonzero_status_retains_separate_private_streams_without_payload_diagnostics()
    {
        let token = CancellationToken::new();
        let mut owner = Owner::with_bounds(
            shell("printf synthetic-private-out; printf synthetic-private-err >&2; exit 17"),
            &token,
            bounds(1024),
        );
        let output = owner.join().await.unwrap();
        assert_eq!(
            output.status["exit_code"], 17,
            "owned child status: {}",
            output.status
        );
        assert_eq!(output.status["complete"], true);
        assert_eq!(output.stdout, b"synthetic-private-out");
        assert_eq!(output.stderr, b"synthetic-private-err");
        assert!(!output.status.to_string().contains("synthetic-private"));
    }
    #[tokio::test]
    async fn aggregate_quota_cannot_be_doubled_by_separate_stdout_stderr_limits() {
        let token = CancellationToken::new();
        let mut owner = Owner::with_bounds(
            shell("printf %0128d 0; printf %0129d 0 >&2; sleep 30"),
            &token,
            bounds(256),
        );
        let output = owner.join().await.unwrap();
        assert_eq!(
            output.status["output_limit_exceeded"], true,
            "owned child status: {}",
            output.status
        );
        assert!(!output.status["complete"].as_bool().unwrap());
        assert_eq!(output.stdout.len() + output.stderr.len(), 256);
        assert_eq!(output.status["aggregate_bytes_retained"], 256);
    }
    #[tokio::test]
    async fn disabled_evaluator_timer_cannot_disable_independent_process_timeout_or_forced_cleanup()
    {
        let token = CancellationToken::new();
        let mut owner = Owner::with_bounds(
            shell("trap '' TERM; while :; do sleep 1; done"),
            &token,
            Bounds {
                timeout: Duration::from_secs(1),
                ..bounds(1024)
            },
        );
        let output = owner.join().await.unwrap();
        assert_eq!(output.status["timed_out"], true);
        assert_eq!(output.status["forced_kill"], true);
        assert_eq!(output.status["exit_signal"], 9);
        assert_eq!(output.status["output_complete"], true);
    }
    #[tokio::test]
    async fn cancelled_join_waiter_preserves_owner_for_explicit_joined_close() {
        let token = CancellationToken::new();
        let scratch = crate::audit_support::Scratch::new("owned-waiter-test").unwrap();
        let mut command = shell("trap '' TERM; printf ready > \"$1\"; while :; do sleep 1; done");
        command
            .arg("synthetic-ready-waiter")
            .arg(scratch.0.join("ready"));
        let mut owner = Owner::with_bounds(
            command,
            &token,
            Bounds {
                timeout: Duration::from_secs(30),
                ..bounds(1024)
            },
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while !scratch.0.join("ready").exists() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("Owned child must announce readiness before waiter cancellation");
        assert!(
            tokio::time::timeout(Duration::from_millis(30), owner.join())
                .await
                .is_err()
        );
        let output = owner.close().await.unwrap();
        assert_eq!(output.status["cancelled"], true);
        assert_eq!(output.status["forced_kill"], true);
        assert_eq!(output.status["output_complete"], true);
    }
    #[tokio::test]
    async fn already_cancelled_admission_cannot_spawn_or_execute_shell() {
        let token = CancellationToken::new();
        token.cancel();
        let scratch = crate::audit_support::Scratch::new("owned-admission-test").unwrap();
        let mut command = shell("printf escaped > \"$1\"");
        command
            .arg("synthetic-admission")
            .arg(scratch.0.join("escaped"));
        let mut owner = Owner::with_bounds(command, &token, bounds(1024));
        let output = owner.join().await.unwrap();
        assert_eq!(output.status["spawned"], false);
        assert!(!scratch.0.join("escaped").exists());
    }
    #[tokio::test]
    async fn spawn_failure_retains_explicit_non_admission_and_safe_error_evidence() {
        let mut owner = Owner::start(
            Command::new("/synthetic-private-missing-program"),
            &CancellationToken::new(),
        );
        let error = owner.join().await.err().unwrap();
        assert!(!error.spawned);
        let evidence = error.evidence();
        assert_eq!(evidence["spawned"], false);
        assert_eq!(evidence["worker_joined"], true);
        assert_eq!(evidence["complete"], false);
        assert!(!evidence.to_string().contains("synthetic-private"));
    }
    #[tokio::test]
    async fn exited_leader_descendant_is_terminated_before_group_id_can_be_reused() {
        let scratch = crate::audit_support::Scratch::new("owned-descendant-test").unwrap();
        let script = r#"(trap '' HUP TERM; printf ready > "$1/ready"; while [ ! -f "$1/release" ]; do sleep 0.01; done; printf escaped > "$1/escaped") &
            while [ ! -f "$1/ready" ]; do sleep 0.01; done; exit 23"#;
        let mut command = shell(script);
        command.arg("synthetic-descendant").arg(&scratch.0);
        let mut owner = Owner::with_bounds(
            command,
            &CancellationToken::new(),
            Bounds {
                timeout: Duration::from_secs(2),
                ..bounds(1024)
            },
        );
        let output = owner.join().await.unwrap();
        assert_eq!(output.status["exit_code"], 23);
        assert_eq!(output.status["output_complete"], true);
        std::fs::write(scratch.0.join("release"), b"released").unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!scratch.0.join("escaped").exists());
    }
}
