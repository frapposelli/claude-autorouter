//! Secret input without echo or command-line values. Restore descriptor and
//! terminal state on every return, including SIGINT/SIGTERM cancellation.
//! Leaving hidden input first discards bytes already in the unread input queue,
//! then restores attributes immediately without waiting for output to drain.
//! These are separate syscalls: typing after the flush is outside that discard
//! boundary, including typing after cancellation.
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::sys::termios::{
    FlushArg, LocalFlags, SetArg, Termios, cfmakeraw, tcflush, tcgetattr, tcsetattr,
};
use std::fs::File;
use std::io::{IsTerminal, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use tokio::io::unix::AsyncFd;
use tokio::signal::unix::{SignalKind, signal};
#[path = "secret_edit.rs"]
mod edit;

struct Input {
    file: File,
    flags: OFlag,
    terminal: Option<Termios>,
}
impl AsRawFd for Input {
    fn as_raw_fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }
}
impl AsFd for Input {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}
impl Drop for Input {
    fn drop(&mut self) {
        let _ = self.restore_terminal();
        let _ = fcntl(&self.file, FcntlArg::F_SETFL(self.flags));
    }
}
impl Input {
    fn restore_terminal(&mut self) -> nix::Result<()> {
        match self.terminal.take() {
            Some(terminal) => restore_hidden_terminal(&self.file, &terminal),
            None => Ok(()),
        }
    }
}

fn restore_hidden_terminal(fd: &impl AsFd, terminal: &Termios) -> nix::Result<()> {
    // TCSAFLUSH also drains output, which can stall cancellation indefinitely
    // when the terminal is stopped or backpressured. Flush only input, then
    // restore immediately even when flushing fails. Do not retry EINTR in an
    // unbounded loop. A restoration failure takes precedence over a flush error.
    let flushed = tcflush(fd, FlushArg::TCIFLUSH);
    let restored = tcsetattr(fd, SetArg::TCSANOW, terminal);
    restored.and(flushed)
}
const READ_ERROR: &str = "Could not read a bounded secret from stdin.";
fn hidden_mode(terminal: &Termios) -> Termios {
    let mut hidden = terminal.clone();
    cfmakeraw(&mut hidden);
    hidden
        .local_flags
        .remove(LocalFlags::ECHO | LocalFlags::ECHONL);
    hidden
}

pub async fn read_secret(label: &str, from_stdin: bool) -> Result<String, String> {
    let stdin = std::io::stdin();
    if from_stdin && stdin.is_terminal() {
        return Err("Pipe the secret to --stdin, or omit --stdin to use the hidden prompt.".into());
    }
    if !from_stdin && (!stdin.is_terminal() || !std::io::stderr().is_terminal()) {
        return Err(format!(
            "Set {label} in the environment for noninteractive setup"
        ));
    }
    let mut interrupt = signal(SignalKind::interrupt()).map_err(|_| READ_ERROR)?;
    let mut terminate = signal(SignalKind::terminate()).map_err(|_| READ_ERROR)?;
    let file = File::from(nix::unistd::dup(&stdin).map_err(|_| READ_ERROR)?);
    // Regular redirected files need no readiness registration (epoll rejects
    // them). Reads are bounded and EOF is immediate, as with Node's stdin.
    if from_stdin && file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        let mut bytes = Vec::new();
        file.take(16_385)
            .read_to_end(&mut bytes)
            .map_err(|_| READ_ERROR)?;
        return finish(bytes, true);
    }
    let flags = OFlag::from_bits_truncate(fcntl(&file, FcntlArg::F_GETFL).map_err(|_| READ_ERROR)?);
    let terminal = if from_stdin {
        None
    } else {
        Some(tcgetattr(&file).map_err(|_| "Could not read a hidden secret.")?)
    };
    let input = Input {
        file,
        flags,
        terminal,
    };
    if let Some(terminal) = &input.terminal {
        tcsetattr(&input.file, SetArg::TCSANOW, &hidden_mode(terminal))
            .map_err(|_| "Could not read a hidden secret.")?;
    }
    fcntl(&input.file, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).map_err(|_| READ_ERROR)?;
    let input = AsyncFd::new(input).map_err(|_| READ_ERROR)?;
    if !from_stdin {
        eprint!("{label} (hidden): ");
        let _ = std::io::stderr().flush();
    }
    let result = tokio::select! {
        result = async {
            if from_stdin { collect(&input).await } else { collect_hidden(&input).await }
        } => result,
        _ = interrupt.recv() => Err("Setup cancelled".into()),
        _ = terminate.recv() => Err("Setup cancelled".into()),
    };
    let mut input = input.into_inner();
    let restored = input
        .restore_terminal()
        .map_err(|_| "Could not restore the hidden input terminal.".to_owned());
    drop(input);
    if !from_stdin {
        eprintln!();
    }
    result.and_then(|value| restored.map(|()| value))
}
async fn collect(input: &AsyncFd<Input>) -> Result<String, String> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let mut ready = input.readable().await.map_err(|_| READ_ERROR)?;
        let count = match ready.try_io(|input| (&input.get_ref().file).read(&mut buffer)) {
            Ok(result) => result.map_err(|_| READ_ERROR)?,
            Err(_) => continue,
        };
        if count == 0 {
            return finish(bytes, true);
        }
        bytes.extend_from_slice(&buffer[..count]);
        if bytes.len() > 16_384 {
            return Err(READ_ERROR.into());
        }
    }
}
async fn collect_hidden(input: &AsyncFd<Input>) -> Result<String, String> {
    let mut editor = edit::Editor::default();
    let mut buffer = [0; 4096];
    let mut action = edit::Action::Continue;
    loop {
        match action {
            edit::Action::Line(line) => return Ok(line),
            edit::Action::Cancel => return Err("Setup cancelled".into()),
            edit::Action::Suspend => {
                // Ctrl-Z must restore the shell's terminal before suspension.
                // Continue restores hidden editing without printing the line.
                let terminal = input.get_ref().terminal.as_ref().expect("hidden terminal");
                restore_hidden_terminal(input.get_ref(), terminal).map_err(|_| READ_ERROR)?;
                nix::sys::signal::kill(nix::unistd::getpid(), nix::sys::signal::Signal::SIGTSTP)
                    .map_err(|_| READ_ERROR)?;
                tcsetattr(input.get_ref(), SetArg::TCSANOW, &hidden_mode(terminal))
                    .map_err(|_| READ_ERROR)?;
                action = editor.feed(&[]);
                continue;
            }
            edit::Action::Continue => {}
        }
        let ready = if editor.escape_pending() {
            tokio::select! {
                result = input.readable() => result,
                _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
                    editor.expire_escape();
                    continue;
                }
            }
        } else {
            input.readable().await
        };
        let mut ready = ready.map_err(|_| READ_ERROR)?;
        let count = match ready.try_io(|input| (&input.get_ref().file).read(&mut buffer)) {
            Ok(result) => result.map_err(|_| READ_ERROR)?,
            Err(_) => continue,
        };
        action = if count == 0 {
            editor.eof()
        } else {
            editor.feed(&buffer[..count])
        };
    }
}
fn finish(bytes: Vec<u8>, bounded: bool) -> Result<String, String> {
    if bounded && bytes.len() > 16_384 {
        return Err(READ_ERROR.into());
    }
    let mut value = String::from_utf8_lossy(&bytes).into_owned();
    if value.ends_with('\n') {
        value.pop();
        if value.ends_with('\r') {
            value.pop();
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::pty::openpty;
    use nix::sys::termios::{FlowArg, tcflow};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    #[test]
    fn restoration_does_not_wait_for_stopped_terminal_output() {
        const CHILD: &str = "AUTOROUTER_TEST_STOPPED_TERMINAL_CHILD";
        if std::env::var_os(CHILD).is_some() {
            for explicit in [false, true] {
                let pair = openpty(None, None).unwrap();
                let _master = File::from(pair.master);
                let mut slave = File::from(pair.slave);
                let terminal = tcgetattr(&slave).unwrap();
                let flags = OFlag::from_bits_truncate(fcntl(&slave, FcntlArg::F_GETFL).unwrap());
                let mut input = Input {
                    file: slave.try_clone().unwrap(),
                    flags,
                    terminal: Some(terminal.clone()),
                };
                tcsetattr(&input, SetArg::TCSANOW, &hidden_mode(&terminal)).unwrap();
                fcntl(&input, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).unwrap();
                tcflow(&slave, FlowArg::TCOOFF).unwrap();
                // macOS queues this output while stopped; Linux PTYs may
                // immediately reject it with EAGAIN. Neither permits us to
                // depend on output progress to restore hidden input.
                let queued = match slave.write(b"synthetic stopped terminal output") {
                    Ok(count) => {
                        assert!(count > 0);
                        count
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => 0,
                    Err(error) => panic!("Could not queue stopped output: {error}"),
                };
                println!("explicit={explicit}; queued_output_bytes={queued}");
                std::io::stdout().flush().unwrap();
                if explicit {
                    input.restore_terminal().unwrap();
                }
                drop(input);
                let restored = tcgetattr(&slave).unwrap();
                assert_eq!(
                    restored.local_flags.difference(LocalFlags::PENDIN),
                    terminal.local_flags.difference(LocalFlags::PENDIN)
                );
                assert_eq!(restored.input_flags, terminal.input_flags);
                assert_eq!(restored.output_flags, terminal.output_flags);
                assert_eq!(restored.control_flags, terminal.control_flags);
                assert_eq!(restored.control_chars, terminal.control_chars);
                assert_eq!(
                    OFlag::from_bits_truncate(fcntl(&slave, FcntlArg::F_GETFL).unwrap()),
                    flags
                );
                // This is deliberately after restoration and its assertions.
                // Nothing reads the master or resumes output while it runs.
                tcflow(&slave, FlowArg::TCOON).unwrap();
            }
            return;
        }

        // A draining-regression must fail with bounded cleanup, not hang the
        // test runner. This child owns only its synthetic PTYs, no descendants.
        let mut child = Command::new(std::env::current_exe().unwrap())
            .env_clear()
            .env(CHILD, "1")
            .args([
                "--exact",
                "secret_input::tests::restoration_does_not_wait_for_stopped_terminal_output",
                "--nocapture",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let completed = loop {
            if child.try_wait().unwrap().is_some() {
                break true;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                break false;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let output = child.wait_with_output().unwrap();
        assert!(
            completed && output.status.success(),
            "Stopped-output restoration did not complete successfully; completed={completed}; status={}; stdout={}; stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn leaving_hidden_input_discards_queued_bytes_before_restoring_echo() {
        let pair = openpty(None, None).unwrap();
        let mut master = File::from(pair.master);
        let slave = File::from(pair.slave);
        let terminal = tcgetattr(&slave).unwrap();
        assert!(
            terminal
                .local_flags
                .contains(LocalFlags::ECHO | LocalFlags::ICANON)
        );
        let flags = OFlag::from_bits_truncate(fcntl(&slave, FcntlArg::F_GETFL).unwrap());
        let input = Input {
            file: slave.try_clone().unwrap(),
            flags,
            terminal: Some(terminal.clone()),
        };
        tcsetattr(&input, SetArg::TCSANOW, &hidden_mode(&terminal)).unwrap();
        fcntl(&input, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).unwrap();
        let input = AsyncFd::new(input).unwrap();
        master
            .write_all(b"synthetic-PRIVATE-queued-secret")
            .unwrap();
        // Readiness is an observable barrier: bytes have reached the hidden
        // input queue, but the cancelled reader has not consumed any of them.
        drop(
            tokio::time::timeout(Duration::from_secs(2), input.readable())
                .await
                .unwrap()
                .unwrap(),
        );
        drop(input);
        let restored = tcgetattr(&slave).unwrap();
        assert_eq!(
            restored.local_flags.difference(LocalFlags::PENDIN),
            terminal.local_flags.difference(LocalFlags::PENDIN)
        );
        assert_eq!(restored.input_flags, terminal.input_flags);
        assert_eq!(restored.output_flags, terminal.output_flags);
        assert_eq!(restored.control_flags, terminal.control_flags);
        assert_eq!(restored.control_chars, terminal.control_chars);
        assert_eq!(fcntl(&slave, FcntlArg::F_GETFL).unwrap(), flags.bits());

        // A later shell keystroke must neither reveal nor read the discarded
        // secret. It must still behave like ordinary restored terminal input.
        fcntl(&slave, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).unwrap();
        let slave = AsyncFd::new(slave).unwrap();
        master.write_all(b"\r").unwrap();
        let mut ready = tokio::time::timeout(Duration::from_secs(2), slave.readable())
            .await
            .unwrap()
            .unwrap();
        let mut bytes = [0; 128];
        let count = ready
            .try_io(|input| input.get_ref().read(&mut bytes))
            .unwrap()
            .unwrap();
        assert_eq!(&bytes[..count], b"\n");
        let flags = OFlag::from_bits_truncate(fcntl(&master, FcntlArg::F_GETFL).unwrap());
        fcntl(&master, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).unwrap();
        let master = AsyncFd::new(master).unwrap();
        let mut ready = tokio::time::timeout(Duration::from_secs(2), master.readable())
            .await
            .unwrap()
            .unwrap();
        let count = ready
            .try_io(|output| output.get_ref().read(&mut bytes))
            .unwrap()
            .unwrap();
        assert_eq!(&bytes[..count], b"\r\n");
    }
}
