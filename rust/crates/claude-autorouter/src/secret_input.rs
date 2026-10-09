//! Secret input without echo or command-line values. Restore descriptor and
//! terminal state on every return, including SIGINT/SIGTERM cancellation.
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::sys::termios::{LocalFlags, SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr};
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
        if let Some(terminal) = &self.terminal {
            let _ = tcsetattr(&self.file, SetArg::TCSANOW, terminal);
        }
        let _ = fcntl(&self.file, FcntlArg::F_SETFL(self.flags));
    }
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
    drop(input);
    if !from_stdin {
        eprintln!();
    }
    result
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
                tcsetattr(input.get_ref(), SetArg::TCSANOW, terminal).map_err(|_| READ_ERROR)?;
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
