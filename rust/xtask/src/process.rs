//! Bounded subprocess protocol without pipe deadlocks or inherited Node hooks.
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const MAX_STDOUT: u64 = 128 * 1024 * 1024;
const MAX_STDERR: u64 = 64 * 1024;
const SCRATCH_ATTEMPTS: usize = 8;

struct Scratch(PathBuf);

struct OwnedChild {
    child: Child,
    cleaned: bool,
    #[cfg(unix)]
    group: Option<nix::unistd::Pid>,
}

impl OwnedChild {
    fn new(child: Child) -> Self {
        Self {
            cleaned: false,
            #[cfg(unix)]
            group: Some(nix::unistd::Pid::from_raw(child.id() as i32)),
            child,
        }
    }

    fn cleanup(&mut self) {
        if self.cleaned {
            return;
        }
        self.cleaned = true;
        // The leader may already have exited while fixture descendants still
        // own output files or sockets. End the owned group on every path,
        // including successful completion, before returning captured bytes.
        #[cfg(unix)]
        if let Some(group) = self.group.take() {
            let _ = nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// Observe completion without reaping the leader. Keeping its PID reserved
// until after killpg prevents signaling a reused process-group identifier.
struct ExitObserver {
    #[cfg(target_os = "macos")]
    queue: nix::sys::event::Kqueue,
    #[cfg(target_os = "macos")]
    exited: bool,
}

impl ExitObserver {
    fn new(pid: u32) -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        {
            use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent, Kqueue};
            let queue = Kqueue::new().map_err(|_| "Cannot observe fixture executable")?;
            let change = KEvent::new(
                pid as usize,
                EventFilter::EVFILT_PROC,
                EvFlags::EV_ADD | EvFlags::EV_ONESHOT,
                FilterFlag::NOTE_EXIT,
                0,
                0,
            );
            // With no output slots, registration errors are returned directly.
            // ESRCH means this owned, unreaped child exited before registration.
            let exited = match queue.kevent(&[change], &mut [], None) {
                Ok(_) => false,
                Err(nix::errno::Errno::ESRCH) => true,
                Err(_) => return Err("Cannot observe fixture executable".into()),
            };
            Ok(Self { queue, exited })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = pid;
            Ok(Self {})
        }
    }

    fn exited(&mut self, child: &mut OwnedChild) -> Result<bool, String> {
        #[cfg(target_os = "macos")]
        {
            use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent};
            if self.exited {
                return Ok(true);
            }
            let mut events = [KEvent::new(
                child.id() as usize,
                EventFilter::EVFILT_PROC,
                EvFlags::empty(),
                FilterFlag::empty(),
                0,
                0,
            )];
            match self.queue.kevent(
                &[],
                &mut events,
                Some(nix::libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                }),
            ) {
                Ok(0) | Err(nix::errno::Errno::EINTR) => Ok(false),
                Ok(1)
                    if !events[0].flags().contains(EvFlags::EV_ERROR)
                        && events[0].ident() == child.id() as usize
                        && events[0].fflags().contains(FilterFlag::NOTE_EXIT) =>
                {
                    self.exited = true;
                    Ok(true)
                }
                _ => Err("Cannot wait for fixture executable".into()),
            }
        }
        #[cfg(target_os = "linux")]
        {
            use nix::sys::wait::{Id, WaitPidFlag, WaitStatus, waitid};
            match waitid(
                Id::Pid(nix::unistd::Pid::from_raw(child.id() as i32)),
                WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT,
            ) {
                Ok(WaitStatus::Exited(_, _) | WaitStatus::Signaled(_, _, _)) => Ok(true),
                Ok(WaitStatus::StillAlive) | Err(nix::errno::Errno::EINTR) => Ok(false),
                Err(nix::errno::Errno::ECHILD) => {
                    // An unexpected external reaper has ended our ownership.
                    // Do not signal a numeric PID/group that can now be reused.
                    child.cleaned = true;
                    child.group = None;
                    Err("Cannot wait for fixture executable".into())
                }
                _ => Err("Cannot wait for fixture executable".into()),
            }
        }
        #[cfg(not(unix))]
        {
            child
                .try_wait()
                .map(|status| status.is_some())
                .map_err(|_| "Cannot wait for fixture executable".into())
        }
        #[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
        {
            let _ = child;
            Err("Fixture process observation is supported only on macOS and Linux".into())
        }
    }
}

impl std::ops::Deref for OwnedChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}
impl std::ops::DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.cleanup();
    }
}

impl Scratch {
    fn new() -> Result<Self, String> {
        Self::new_in(&std::env::temp_dir(), || {
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce)
                .map_err(|_| "Cannot obtain private fixture directory entropy")?;
            Ok(nonce)
        })
    }

    fn new_in(
        root: &Path,
        mut nonce: impl FnMut() -> Result<[u8; 16], String>,
    ) -> Result<Self, String> {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        for _ in 0..SCRATCH_ATTEMPTS {
            let nonce: String = nonce()?.iter().map(|byte| format!("{byte:02x}")).collect();
            let directory = root.join(format!("autorouter-parity-{}-{nonce}", std::process::id()));
            match builder.create(&directory) {
                Ok(()) => return Ok(Self(directory)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "Cannot create private fixture directory ({:?})",
                        error.kind()
                    ));
                }
            }
        }
        Err("Cannot create private fixture directory (name collisions)".into())
    }

    fn file(&self, name: &str) -> Result<File, String> {
        let mut options = OpenOptions::new();
        options.create_new(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(self.0.join(name))
            .map_err(|_| "Cannot create private fixture file".into())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn capture(command: &mut Command, input: &[u8], timeout: Duration) -> Result<Vec<u8>, String> {
    let result = capture_result(command, input, timeout)?;
    if !result.status.success() {
        // Default diagnostics never disclose subprocess payloads.
        return Err(format!(
            "Fixture executable failed ({}); run the reference baseline check separately",
            result.status
        ));
    }
    Ok(result.stdout)
}

/// Bounded output for explicit fixture/report consumers. Callers decide which
/// synthetic diagnostics to retain; routine errors must not print these bytes.
pub struct CapturedProcess {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub fn capture_result(
    command: &mut Command,
    input: &[u8],
    timeout: Duration,
) -> Result<CapturedProcess, String> {
    let scratch = Scratch::new()?;
    scratch
        .file("stdin")?
        .write_all(input)
        .map_err(|_| "Cannot write fixture input")?;
    let stdout = scratch.file("stdout")?;
    let stderr = scratch.file("stderr")?;
    let input = File::open(scratch.0.join("stdin")).map_err(|_| "Cannot read fixture input")?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = OwnedChild::new(
        command
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(
                stdout
                    .try_clone()
                    .map_err(|_| "Cannot attach fixture output")?,
            ))
            .stderr(Stdio::from(
                stderr
                    .try_clone()
                    .map_err(|_| "Cannot attach fixture error output")?,
            ))
            .env_remove("NODE_OPTIONS")
            .env_remove("NODE_PATH")
            .spawn()
            .map_err(|_| "Cannot start fixture executable")?,
    );
    let mut observer = ExitObserver::new(child.id())?;
    let started = Instant::now();
    let status = loop {
        let oversized = stdout
            .metadata()
            .map_err(|_| "Cannot inspect fixture output")?
            .len()
            > MAX_STDOUT
            || stderr
                .metadata()
                .map_err(|_| "Cannot inspect fixture errors")?
                .len()
                > MAX_STDERR;
        if oversized || started.elapsed() >= timeout {
            child.cleanup();
            return Err(if oversized {
                "Fixture executable exceeded output limit"
            } else {
                "Fixture executable timed out"
            }
            .into());
        }
        match observer.exited(&mut child) {
            Ok(true) => {
                child.cleanup();
                break child
                    .wait()
                    .map_err(|_| "Cannot wait for fixture executable")?;
            }
            Ok(false) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                child.cleanup();
                return Err(error);
            }
        }
    };
    child.cleanup();
    Ok(CapturedProcess {
        status,
        stdout: read_bounded(&scratch.0.join("stdout"), MAX_STDOUT)?,
        stderr: read_bounded(&scratch.0.join("stderr"), MAX_STDERR)?,
    })
}

pub fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    if !fs::metadata(path)
        .map_err(|_| "Cannot inspect fixture file")?
        .is_file()
    {
        return Err("Fixture input must be a regular file".into());
    }
    let file = File::open(path).map_err(|_| "Cannot open fixture file")?;
    if !file
        .metadata()
        .map_err(|_| "Cannot inspect fixture file")?
        .is_file()
    {
        return Err("Fixture input must be a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read fixture file")?;
    if bytes.len() as u64 > max {
        return Err("Fixture file exceeds byte limit".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_collisions_preserve_existing_directories_and_files() {
        for existing_file in [false, true] {
            let root = Scratch::new().unwrap();
            let existing = Scratch::new_in(&root.0, || Ok([0; 16])).unwrap();
            let existing_path = existing.0.clone();
            drop(existing);
            let marker = if existing_file {
                existing_path.clone()
            } else {
                fs::create_dir(&existing_path).unwrap();
                existing_path.join("marker")
            };
            fs::write(&marker, b"existing owner").unwrap();
            let mut attempts = 0;
            let created = Scratch::new_in(&root.0, || {
                let value = attempts;
                attempts += 1;
                Ok([value; 16])
            })
            .unwrap();
            assert_eq!(attempts, 2);
            assert_ne!(created.0, existing_path);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&created.0).unwrap().permissions().mode() & 0o777,
                    0o700
                );
                let file = created.file("private").unwrap();
                assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
            }
            let created_path = created.0.clone();
            drop(created);
            assert!(!created_path.exists());
            assert_eq!(fs::read(marker).unwrap(), b"existing owner");
        }
    }

    #[test]
    fn scratch_repeated_collision_is_bounded_and_preserves_existing_owner() {
        let root = Scratch::new().unwrap();
        let existing = Scratch::new_in(&root.0, || Ok([0; 16])).unwrap();
        fs::write(existing.0.join("marker"), b"existing owner").unwrap();
        let mut attempts = 0;
        let error = Scratch::new_in(&root.0, || {
            attempts += 1;
            Ok([0; 16])
        })
        .err()
        .unwrap();
        assert_eq!(attempts, SCRATCH_ATTEMPTS);
        assert_eq!(
            error,
            "Cannot create private fixture directory (name collisions)"
        );
        assert_eq!(
            fs::read(existing.0.join("marker")).unwrap(),
            b"existing owner"
        );
    }

    #[test]
    #[cfg(unix)]
    fn scratch_symlink_collision_does_not_follow_or_remove_link() {
        let root = Scratch::new().unwrap();
        let existing = Scratch::new_in(&root.0, || Ok([0; 16])).unwrap();
        let link = existing.0.clone();
        drop(existing);
        let target = root.0.join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("marker"), b"existing owner").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let mut attempts = 0;
        let created = Scratch::new_in(&root.0, || {
            let value = attempts;
            attempts += 1;
            Ok([value; 16])
        })
        .unwrap();
        assert_eq!(attempts, 2);
        assert_ne!(created.0, link);
        drop(created);
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(target.join("marker")).unwrap(), b"existing owner");
    }

    #[test]
    fn scratch_non_collision_failure_is_reported_without_retry_or_path() {
        let root = Scratch::new().unwrap();
        let missing = root.0.join("sensitive-fixture-parent");
        let mut attempts = 0;
        let error = Scratch::new_in(&missing, || {
            attempts += 1;
            Ok([0; 16])
        })
        .err()
        .unwrap();
        assert_eq!(attempts, 1);
        assert_eq!(error, "Cannot create private fixture directory (NotFound)");
        assert!(!error.contains("sensitive-fixture-parent"));
        assert!(!missing.exists());
        assert_eq!(
            Scratch::new_in(&root.0, || Err("synthetic entropy failure".into()))
                .err()
                .unwrap(),
            "synthetic entropy failure"
        );
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
    }

    #[test]
    fn failed_process_is_not_an_empty_passing_run() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 17"]);
        let error = capture(&mut command, b"", Duration::from_secs(1)).unwrap_err();
        assert!(error.starts_with("Fixture executable failed"));
    }

    #[test]
    fn completed_fixture_capture_retains_exit_status_and_separate_streams() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "cat; printf synthetic-error >&2; exit 17"]);
        let output =
            capture_result(&mut command, b"synthetic-input", Duration::from_secs(1)).unwrap();
        assert_eq!(output.status.code(), Some(17));
        assert_eq!(output.stdout, b"synthetic-input");
        assert_eq!(output.stderr, b"synthetic-error");
        let error = capture(&mut command, b"synthetic-input", Duration::from_secs(1)).unwrap_err();
        assert!(!error.contains("synthetic-input"));
        assert!(!error.contains("synthetic-error"));
    }

    #[test]
    fn stalled_process_has_a_deadline() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exec sleep 30"]);
        let started = Instant::now();
        assert_eq!(
            capture(&mut command, b"", Duration::from_millis(30)).unwrap_err(),
            "Fixture executable timed out"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    fn descendant_fixture(mode: &str) -> (Scratch, Result<CapturedProcess, String>) {
        let scratch = Scratch::new().unwrap();
        // The descendant announces that it is running, ignores shell HUP,
        // and waits for a release file created only after capture returns.
        // Without group cleanup it can then write the escaped marker.
        let script = r#"
            (
                trap '' HUP
                printf ready > "$1/started"
                while [ ! -f "$1/release" ]; do sleep 0.01; done
                printf escaped > "$1/escaped"
            ) &
            while [ ! -f "$1/started" ]; do sleep 0.01; done
            case "$2" in
                timeout) wait ;;
                overflow) dd if=/dev/zero bs=65536 count=2 1>&2 2>/dev/null; wait ;;
                success) printf synthetic-completion ;;
                failure) printf synthetic-failure >&2; exit 17 ;;
            esac
        "#;
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", script, "owned-descendant-fixture"])
            .arg(&scratch.0)
            .arg(mode);
        let result = capture_result(&mut command, b"", Duration::from_secs(2));
        // Release even if the assertion below fails, so a regressed helper
        // cannot leave this synthetic descendant waiting indefinitely.
        fs::write(scratch.0.join("release"), b"released").unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(fs::read(scratch.0.join("started")).unwrap(), b"ready");
        assert!(
            !scratch.0.join("escaped").exists(),
            "fixture descendant continued after capture returned ({mode})"
        );
        (scratch, result)
    }

    #[test]
    #[cfg(unix)]
    fn timeout_stops_spawned_descendants_before_returning() {
        let (_scratch, result) = descendant_fixture("timeout");
        assert_eq!(result.err().unwrap(), "Fixture executable timed out");
    }

    #[test]
    #[cfg(unix)]
    fn output_overflow_stops_spawned_descendants_before_returning() {
        let (_scratch, result) = descendant_fixture("overflow");
        assert_eq!(
            result.err().unwrap(),
            "Fixture executable exceeded output limit"
        );
    }

    #[test]
    #[cfg(unix)]
    fn completed_capture_cleans_descendants_and_preserves_the_leader_status() {
        for mode in ["success", "failure"] {
            let (_scratch, result) = descendant_fixture(mode);
            let result = result.unwrap();
            if mode == "success" {
                assert!(result.status.success());
                assert_eq!(result.stdout, b"synthetic-completion");
                assert!(result.stderr.is_empty());
            } else {
                assert_eq!(result.status.code(), Some(17));
                assert!(result.stdout.is_empty());
                assert_eq!(result.stderr, b"synthetic-failure");
            }
        }
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn exit_observation_keeps_the_leader_waitable_until_group_cleanup() {
        use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
        use nix::unistd::Pid;
        use std::os::unix::process::CommandExt;
        for already_exited in [false, true] {
            for _ in 0..8 {
                let child = Command::new("/bin/sh")
                    .args(["-c", "exit 23"])
                    .process_group(0)
                    .spawn()
                    .unwrap();
                let mut child = OwnedChild::new(child);
                if already_exited {
                    std::thread::sleep(Duration::from_millis(10));
                }
                let mut observer = ExitObserver::new(child.id()).unwrap();
                let deadline = Instant::now() + Duration::from_secs(2);
                while !observer.exited(&mut child).unwrap() {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
                // An independent OS wait proves observation did not consume
                // exit status or release the PID; Child's cached status alone
                // would not detect an accidental try_wait implementation.
                let pid = Pid::from_raw(child.id() as i32);
                let result = waitpid(pid, Some(WaitPidFlag::WNOHANG));
                if matches!(
                    result,
                    Ok(WaitStatus::Exited(_, _) | WaitStatus::Signaled(_, _, _))
                        | Err(nix::errno::Errno::ECHILD)
                ) {
                    // This test intentionally reaped it: never signal its
                    // numeric ID after ending ownership.
                    child.cleaned = true;
                    child.group = None;
                }
                assert_eq!(result.unwrap(), WaitStatus::Exited(pid, 23));
            }
        }
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn capture_cleanup_does_not_signal_a_separate_owned_fixture_group() {
        use std::os::unix::process::CommandExt;
        let scratch = Scratch::new().unwrap();
        let child = Command::new("/bin/sh")
            .args([
                "-c",
                "while [ ! -f \"$1/release\" ]; do sleep 0.01; done; printf survived > \"$1/survived\"",
                "separate-owned-fixture",
            ])
            .arg(&scratch.0)
            .process_group(0)
            .spawn()
            .unwrap();
        let mut other = OwnedChild::new(child);
        assert_eq!(
            nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(other.id() as i32))).unwrap(),
            nix::unistd::Pid::from_raw(other.id() as i32)
        );
        assert_ne!(
            nix::unistd::getpgrp(),
            nix::unistd::Pid::from_raw(other.id() as i32)
        );
        let error = capture(
            Command::new("/bin/sh").args(["-c", "exec sleep 30"]),
            b"",
            Duration::from_millis(30),
        )
        .unwrap_err();
        assert_eq!(error, "Fixture executable timed out");
        fs::write(scratch.0.join("release"), b"released").unwrap();
        let mut observer = ExitObserver::new(other.id()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !observer.exited(&mut other).unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        other.cleanup();
        assert!(other.wait().unwrap().success());
        assert_eq!(fs::read(scratch.0.join("survived")).unwrap(), b"survived");
    }

    #[test]
    fn reads_are_bounded() {
        let scratch = Scratch::new().unwrap();
        scratch.file("large").unwrap().write_all(b"12345").unwrap();
        assert!(read_bounded(&scratch.0.join("large"), 4).is_err());
        assert_eq!(read_bounded(&scratch.0.join("large"), 5).unwrap(), b"12345");
    }
}
