//! Owned synthetic children exercise the production pipe/clock/cleanup body.
//! No test invokes /usr/bin/security or creates a Keychain account.
use super::{TOOL_ERROR, child_command, run_child};
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use std::future::{Future, poll_fn};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::Poll;
use std::time::{Duration, Instant};

const PRIVATE: &str = "synthetic-private-security-pipe-marker-271828";

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let suffix = crate::random_hex::<12>().unwrap();
        let path = std::env::temp_dir().join(format!("autorouter-security-process-{suffix}"));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// This guard only addresses the exact owned child PID. Checking parentage
/// through waitpid avoids signaling a reused PID after Tokio reaps the child.
struct OwnedChild(Pid);
impl OwnedChild {
    fn new(child: &tokio::process::Child) -> Self {
        Self(Pid::from_raw(i32::try_from(child.id().unwrap()).unwrap()))
    }
    fn assert_reaped(&self) {
        // A returned exit status would mean this assertion performed the reap
        // itself, rather than observing cleanup by the production runner.
        assert_eq!(
            waitpid(self.0, Some(WaitPidFlag::WNOHANG)),
            Err(Errno::ECHILD)
        );
        assert_eq!(kill(self.0, None), Err(Errno::ESRCH));
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if matches!(
            waitpid(self.0, Some(WaitPidFlag::WNOHANG)),
            Ok(WaitStatus::StillAlive)
        ) {
            let _ = kill(self.0, Signal::SIGKILL);
            let _ = waitpid(self.0, None);
        }
    }
}

fn spawn(script: &str, args: &[&str]) -> (tokio::process::Child, OwnedChild) {
    let mut command_args = vec!["-c", script, "autorouter-owned-child"];
    command_args.extend_from_slice(args);
    let child = child_command(std::ffi::OsStr::new("/bin/sh"), &command_args)
        .env_clear()
        .spawn()
        .unwrap();
    let owner = OwnedChild::new(&child);
    (child, owner)
}

fn gate(root: &Root) -> (PathBuf, std::fs::File) {
    let path = root.0.join("gate");
    nix::unistd::mkfifo(
        &path,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    // Holding both ends prevents the child's FIFO open from waiting for a
    // reader/writer. The shell then blocks in its builtin read, with no helper
    // descendants that could keep the output pipes open.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(&path)
        .unwrap();
    (path, file)
}

async fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Poll<F::Output> {
    poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
}

async fn finish<F: Future>(mut future: Pin<&mut F>) -> F::Output {
    let wall = Instant::now() + Duration::from_secs(5);
    loop {
        if let Poll::Ready(output) = poll_once(future.as_mut()).await {
            return output;
        }
        assert!(
            Instant::now() < wall,
            "Owned synthetic child exceeded test bound"
        );
        // Keeping this task runnable also prevents Tokio's paused clock from
        // jumping to the production deadline before a real child can start.
        tokio::task::yield_now().await;
    }
}

async fn started<F: Future>(mut future: Pin<&mut F>, ready: &Path) {
    let wall = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(poll_once(future.as_mut()).await.is_pending());
        assert!(Instant::now() < wall, "Owned synthetic child did not start");
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn shared_output_boundary_is_65536_and_65537_across_both_pipes() {
    for (stdout, stderr, accepted) in [(32_768, 32_768, true), (32_768, 32_769, false)] {
        let script = format!("printf '%{stdout}s' ''; printf '%{stderr}s' '' >&2");
        let (child, owner) = spawn(&script, &[]);
        let output = run_child(child, None).await;
        if accepted {
            let output = output.unwrap();
            assert_eq!(output.status, Some(0));
            assert_eq!(output.stdout.len(), stdout);
        } else {
            assert!(matches!(output, Err(ref error) if error == TOOL_ERROR));
        }
        owner.assert_reaped();
    }
}

#[tokio::test]
async fn each_pipe_has_exact_byte_boundary_and_overflow_kills_and_reaps() {
    for (stream, bytes, accepted) in [
        ("stdout", 65_536, true),
        ("stdout", 65_537, false),
        ("stderr", 65_536, true),
        ("stderr", 65_537, false),
    ] {
        let root = Root::new();
        let ready = root.0.join("ready");
        let (gate_path, mut release) = gate(&root);
        let done = root.0.join("done");
        let redirect = if stream == "stderr" { " >&2" } else { "" };
        let script = format!(
            "printf ready > \"$1\"; IFS= read -r begin < \"$2\"; printf '%{bytes}s' ''{redirect}; printf done > \"$3\"; IFS= read -r release < \"$2\""
        );
        let (child, owner) = spawn(
            &script,
            &[
                ready.to_str().unwrap(),
                gate_path.to_str().unwrap(),
                done.to_str().unwrap(),
            ],
        );
        let mut future = Box::pin(run_child(child, None));
        started(future.as_mut(), &ready).await;
        if accepted {
            release.write_all(b"begin\n").unwrap();
            started(future.as_mut(), &done).await;
            release.write_all(b"finish\n").unwrap();
            let output = finish(future.as_mut()).await.unwrap();
            assert_eq!(output.status, Some(0));
            assert_eq!(
                output.stdout.len(),
                if stream == "stdout" { bytes } else { 0 }
            );
        } else {
            release.write_all(b"begin\n").unwrap();
            // The final gate remains closed: normal child exit cannot explain this
            // result or satisfy the direct reaping proof.
            assert!(matches!(finish(future.as_mut()).await, Err(ref error) if error == TOOL_ERROR));
        }
        owner.assert_reaped();
    }
}

#[tokio::test]
async fn stdin_is_written_then_closed_and_child_status_is_preserved() {
    let script = "IFS= read -r line; IFS= read -r extra && exit 73; printf '%s' \"$line\"; exit 7";
    let (child, owner) = spawn(script, &[]);
    let input = format!("{PRIVATE}\n");
    let output = run_child(child, Some(&input)).await.unwrap();
    assert_eq!(output.status, Some(7));
    assert_eq!(output.stdout, PRIVATE.as_bytes());
    owner.assert_reaped();

    let (child, owner) = spawn("IFS= read -r line && exit 73; exit 0", &[]);
    assert_eq!(run_child(child, None).await.unwrap().status, Some(0));
    owner.assert_reaped();
}

#[tokio::test(start_paused = true)]
async fn production_15_second_deadline_covers_wait_and_blocked_stdin_and_reaps() {
    for input in [None, Some("x".repeat(1_048_576))] {
        let root = Root::new();
        let ready = root.0.join("ready");
        let (gate_path, _hold) = gate(&root);
        let script = format!(
            "printf ready > \"$1\"; printf '%s' '{PRIVATE}' >&2; IFS= read -r release < \"$2\""
        );
        let (child, owner) = spawn(
            &script,
            &[ready.to_str().unwrap(), gate_path.to_str().unwrap()],
        );
        let mut future = Box::pin(run_child(child, input.as_deref()));
        assert!(poll_once(future.as_mut()).await.is_pending());
        let began = tokio::time::Instant::now();
        started(future.as_mut(), &ready).await;
        assert_eq!(tokio::time::Instant::now(), began);
        tokio::time::advance(Duration::from_millis(14_999)).await;
        assert!(poll_once(future.as_mut()).await.is_pending());
        assert_eq!(kill(owner.0, None), Ok(()));
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(matches!(finish(future.as_mut()).await, Err(ref error) if error == TOOL_ERROR));
        assert_eq!(tokio::time::Instant::now() - began, Duration::from_secs(15));
        owner.assert_reaped();
    }
}

#[tokio::test(start_paused = true)]
async fn dropped_future_eventually_kills_and_reaps_owned_child_on_live_runtime() {
    let root = Root::new();
    let ready = root.0.join("ready");
    let (gate_path, _hold) = gate(&root);
    let (child, owner) = spawn(
        "printf ready > \"$1\"; IFS= read -r release < \"$2\"",
        &[ready.to_str().unwrap(), gate_path.to_str().unwrap()],
    );
    let mut future = Box::pin(run_child(child, None));
    started(future.as_mut(), &ready).await;
    let began = tokio::time::Instant::now();
    drop(future);
    let wall = Instant::now() + Duration::from_secs(5);
    while kill(owner.0, None) != Err(Errno::ESRCH) {
        assert!(Instant::now() < wall, "Dropped owned child was not reaped");
        tokio::task::yield_now().await;
    }
    assert_eq!(tokio::time::Instant::now(), began);
    owner.assert_reaped();
}
