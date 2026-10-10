//! One fresh collector process and one child, so RUSAGE_CHILDREN never includes
//! driver mocks, ps samplers, preceding workloads, or the collector itself.
use super::{ExitObserver, OwnedChild};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::sys::resource::{UsageWho, getrusage};
use nix::sys::signal::{Signal, kill, killpg};
use nix::sys::time::TimeValLike;
use nix::unistd::{Pid, getpgrp, getpid};
use serde_json::{Value, json};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const RECORD_LIMIT: usize = 4096;
const CLEANUP_LIMIT: Duration = Duration::from_secs(10);
const LIFETIME_LIMIT: Duration = Duration::from_secs(30 * 60);

/// The collector and measured process share this group. Its leader stays
/// unreaped in the driver, so even a stopped collector cannot strand children.
/// On owner loss the collector itself ends the group. Normal completion sends
/// the complete report first; SIGKILL is then the declared collector protocol,
/// not the measured executable's exit status.
struct CollectorGroup(Pid);
impl CollectorGroup {
    fn new() -> Result<Self, String> {
        let pid = getpid();
        if getpgrp() != pid {
            return Err("Resource collector must own its process group".into());
        }
        Ok(Self(pid))
    }
    fn terminate(self) -> ! {
        drop(self);
        std::process::exit(1); // Fail closed if the owned group signal fails.
    }
}
impl Drop for CollectorGroup {
    fn drop(&mut self) {
        let _ = killpg(self.0, Signal::SIGKILL);
    }
}

fn usage() -> Result<Value, String> {
    let value = getrusage(UsageWho::RUSAGE_CHILDREN)
        .map_err(|_| "Cannot inspect benchmark child resource usage")?;
    let raw = u64::try_from(value.max_rss()).map_err(|_| "Invalid child RSS")?;
    let unit = if cfg!(target_os = "macos") { 1 } else { 1024 };
    let bytes = raw.checked_mul(unit).ok_or("Child RSS overflow")?;
    Ok(json!({
        "method":"fresh_single_child_getrusage_after_wait",
        "scope":"complete child lifetime including startup, readiness, warmup, measured requests and shutdown; waited descendants contribute CPU and maximum individual RSS, never aggregate process-tree RSS",
        "raw_max_rss":raw,
        "raw_max_rss_unit":if cfg!(target_os="macos") {"bytes"} else {"KiB"},
        "peak_rss_bytes":bytes,
        "user_cpu_microseconds":value.user_time().num_microseconds(),
        "system_cpu_microseconds":value.system_time().num_microseconds()
    }))
}

fn emit(value: &Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| "Cannot encode resource report")?;
    bytes.push(b'\n');
    if bytes.len() > RECORD_LIMIT {
        return Err("Resource report exceeds bound".into());
    }
    let mut output = std::io::stdout().lock();
    output
        .write_all(&bytes)
        .map_err(|_| "Cannot send resource report")?;
    output
        .flush()
        .map_err(|_| "Cannot flush resource report".into())
}

/// Internal executable entry point. No Tokio runtime, command discovery or
/// environment-file processing may precede it or launch another child here.
pub(crate) fn run_helper() -> Result<(), String> {
    let mut arguments = std::env::args_os().skip(2);
    let program = arguments.next().ok_or("Missing measured executable")?;
    let group = CollectorGroup::new()?;
    let initial = usage()?;
    if [
        "raw_max_rss",
        "user_cpu_microseconds",
        "system_cpu_microseconds",
    ]
    .iter()
    .any(|key| initial[key] != 0)
    {
        return Err("Resource collector already has child usage".into());
    }
    let input = std::io::stdin();
    let flags =
        fcntl(&input, FcntlArg::F_GETFL).map_err(|_| "Cannot inspect collector control channel")?;
    fcntl(
        &input,
        FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK),
    )
    .map_err(|_| "Cannot configure collector control channel")?;
    let child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|_| "Cannot spawn measured executable")?;
    let mut child = OwnedChild::new(child);
    // The collector is the group leader. This child guard must only reap its
    // direct child; CollectorGroup and the driver own whole-group cleanup.
    child.group = None;
    let mut observer = ExitObserver::new(child.id())?;
    emit(&json!({"kind":"started","pid":child.id()}))?;
    let started = Instant::now();
    let mut stopping = None;
    let mut input = input.lock();
    loop {
        if observer.exited(&mut child)? {
            // Reap the direct child before sampling usage. The collector group
            // stays armed until the report is flushed, including on owner loss.
            child.cleanup();
            let status = child
                .wait()
                .map_err(|_| "Cannot reap measured executable")?;
            let resources = usage()?;
            emit(
                &json!({"kind":"finished","success":status.success(),"exit_code":status.code(),"signal":status.signal(),"stop_requested":stopping.is_some(),"collector_termination":"owned_group_sigkill_after_report","resources":resources}),
            )?;
            group.terminate();
        }
        let mut command = [0; 1];
        match input.read(&mut command) {
            Ok(0) => return Err("Resource collector lost its owner".into()),
            Ok(1) if command[0] == b'T' && stopping.is_none() => {
                kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM)
                    .map_err(|_| "Cannot stop measured executable")?;
                stopping = Some(Instant::now());
            }
            Ok(_) => return Err("Invalid resource control command".into()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return Err("Cannot read resource control command".into()),
        }
        if started.elapsed() > LIFETIME_LIMIT
            || stopping.is_some_and(|value| value.elapsed() > CLEANUP_LIMIT)
        {
            return Err("Measured executable exceeded resource deadline".into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Parent-side ownership reserves the helper/group leader until group cleanup.
/// The measured PID is only observed here, never used as signal authority.
pub(crate) struct ResourceChild {
    helper: OwnedChild,
    observer: ExitObserver,
    control: Option<tokio::net::UnixStream>,
    pid: u32,
}

async fn read_record(control: &mut tokio::net::UnixStream) -> Result<Value, String> {
    let mut bytes = Vec::new();
    loop {
        let byte = control
            .read_u8()
            .await
            .map_err(|_| "Resource collector closed without a report")?;
        if byte == b'\n' {
            return serde_json::from_slice(&bytes).map_err(|_| "Invalid resource report".into());
        }
        if bytes.len() == RECORD_LIMIT {
            return Err("Resource report exceeds bound".into());
        }
        bytes.push(byte);
    }
}

impl ResourceChild {
    pub(crate) async fn spawn(command: &Command, stderr: File) -> Result<Self, String> {
        let (parent, child) =
            UnixStream::pair().map_err(|_| "Cannot create collector control channel")?;
        parent
            .set_nonblocking(true)
            .map_err(|_| "Cannot configure resource reader")?;
        let child_input = child
            .try_clone()
            .map_err(|_| "Cannot clone collector control channel")?;
        let control = tokio::net::UnixStream::from_std(parent)
            .map_err(|_| "Cannot register resource channel")?;
        let mut helper =
            Command::new(std::env::current_exe().map_err(|_| "Cannot locate resource collector")?);
        helper
            .arg("__benchmark-resource-child")
            .arg(command.get_program())
            .args(command.get_args())
            .env_clear();
        for (key, value) in command.get_envs() {
            if let Some(value) = value {
                helper.env(key, value);
            }
        }
        if let Some(directory) = command.get_current_dir() {
            helper.current_dir(directory);
        }
        let helper = helper
            .stdin(Stdio::from(OwnedFd::from(child_input)))
            .stdout(Stdio::from(OwnedFd::from(child)))
            .stderr(Stdio::from(stderr))
            // A terminal signal to the driver closes its channel; the collector
            // and measured process are isolated together from that terminal.
            .process_group(0)
            .spawn()
            .map_err(|_| "Cannot launch resource collector")?;
        let mut helper = OwnedChild::new(helper);
        let observer = match ExitObserver::new(helper.id()) {
            Ok(observer) => observer,
            Err(error) => {
                helper.cleanup();
                return Err(error);
            }
        };
        let mut owned = Self {
            helper,
            observer,
            control: Some(control),
            pid: 0,
        };
        let record =
            tokio::time::timeout(CLEANUP_LIMIT, read_record(owned.control.as_mut().unwrap()))
                .await
                .map_err(|_| "Resource collector startup timed out")??;
        if record["kind"] != "started" {
            return Err("Missing resource start report".into());
        }
        owned.pid = record["pid"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or("Invalid resource child PID")?;
        Ok(owned)
    }
    pub(crate) fn id(&self) -> u32 {
        self.pid
    }
    pub(crate) fn exited(&mut self) -> Result<bool, String> {
        self.observer.exited(&mut self.helper)
    }
    pub(crate) async fn stop(&mut self) -> Result<Value, String> {
        let control = self
            .control
            .as_mut()
            .ok_or("Missing resource control channel")?;
        control
            .write_all(b"T")
            .await
            .map_err(|_| "Cannot stop benchmark resource child")?;
        let report =
            tokio::time::timeout(CLEANUP_LIMIT + Duration::from_secs(2), read_record(control))
                .await
                .map_err(|_| "Resource collector cleanup timed out")??;
        if report["kind"] != "finished"
            || report["success"] != true
            || report["stop_requested"] != true
            || report["collector_termination"] != "owned_group_sigkill_after_report"
            || report["resources"]["peak_rss_bytes"]
                .as_u64()
                .is_none_or(|value| value == 0)
        {
            return Err("Measured gateway did not produce a clean resource report".into());
        }
        let deadline = Instant::now();
        loop {
            if self.observer.exited(&mut self.helper)? {
                self.helper.cleanup();
                let status = self
                    .helper
                    .wait()
                    .map_err(|_| "Cannot reap resource collector")?;
                if status.signal() != Some(Signal::SIGKILL as i32) {
                    return Err("Resource collector did not use owned-group termination".into());
                }
                self.control.take();
                return Ok(report["resources"].clone());
            }
            if deadline.elapsed() > Duration::from_secs(2) {
                return Err("Resource collector did not exit".into());
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
}

impl Drop for ResourceChild {
    fn drop(&mut self) {
        self.control.take();
        // The helper PID has never been reaped. Its group remains ours even if
        // it is stopped or unable to process EOF, and includes the gateway.
        self.helper.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dropping_a_stopped_collector_ends_its_entire_owned_group() {
        let (mut output, inherited) = UnixStream::pair().unwrap();
        output
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        // Both leader and descendant keep this descriptor open. EOF proves
        // neither can continue, without relying on orphan PID reaping timing.
        let helper = Command::new("/bin/sh")
            .args(["-c", "exec 3<&0; printf 'leader\\n'; /bin/sh -c 'printf \"descendant\\n\"; exec /bin/cat >/dev/null' <&3 & wait"])
            .env_clear()
            .stdin(Stdio::from(OwnedFd::from(inherited.try_clone().unwrap())))
            .stdout(Stdio::from(OwnedFd::from(inherited)))
            .stderr(Stdio::null())
            .process_group(0)
            .spawn().unwrap();
        let helper = OwnedChild::new(helper);
        let observer = ExitObserver::new(helper.id()).unwrap();
        let mut ready = [0; 18];
        output.read_exact(&mut ready).unwrap();
        assert_eq!(&ready, b"leader\ndescendant\n");
        let pid = Pid::from_raw(helper.id() as i32);
        assert_eq!(nix::unistd::getpgid(Some(pid)).unwrap(), pid);
        killpg(pid, None).expect("owned group must permit cleanup signals");
        kill(pid, Signal::SIGSTOP).unwrap();
        // waitpid(WUNTRACED) could also consume an unexpected exit. Inspect the
        // owned, unreaped process read-only so cleanup always reserves its PGID.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = super::super::capture(
                Command::new("/bin/ps")
                    .env_clear()
                    .args(["-o", "stat=", "-p", &pid.to_string()]),
                b"",
                Duration::from_secs(1),
            )
            .unwrap();
            if status
                .iter()
                .copied()
                .find(|byte| !byte.is_ascii_whitespace())
                == Some(b'T')
            {
                break;
            }
            assert!(Instant::now() < deadline, "collector did not stop");
            std::thread::sleep(Duration::from_millis(2));
        }
        let (control, peer) = UnixStream::pair().unwrap();
        control.set_nonblocking(true).unwrap();
        let child = ResourceChild {
            helper,
            observer,
            control: Some(tokio::net::UnixStream::from_std(control).unwrap()),
            pid: 0,
        };
        drop(child);
        assert_eq!(output.read(&mut [0]).unwrap(), 0);
        assert_eq!(
            nix::sys::wait::waitpid(pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG)),
            Err(nix::errno::Errno::ECHILD)
        );
        drop(peer);
    }
}
