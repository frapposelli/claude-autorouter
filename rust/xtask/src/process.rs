//! Bounded subprocess protocol without pipe deadlocks or inherited Node hooks.
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_STDOUT: u64 = 128 * 1024 * 1024;
const MAX_STDERR: u64 = 64 * 1024;

struct Scratch(PathBuf);

struct OwnedChild(Child);

impl std::ops::Deref for OwnedChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl std::ops::DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Scratch {
    fn new() -> Result<Self, String> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "Invalid system clock")?
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("autorouter-parity-{}-{nonce}", std::process::id()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&directory)
            .map_err(|_| "Cannot create private fixture directory")?;
        Ok(Self(directory))
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
    let scratch = Scratch::new()?;
    scratch
        .file("stdin")?
        .write_all(input)
        .map_err(|_| "Cannot write fixture input")?;
    let stdout = scratch.file("stdout")?;
    let stderr = scratch.file("stderr")?;
    let input = File::open(scratch.0.join("stdin")).map_err(|_| "Cannot read fixture input")?;
    let mut child = OwnedChild(
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
            let _ = child.kill();
            let _ = child.wait();
            return Err(if oversized {
                "Fixture executable exceeded output limit"
            } else {
                "Fixture executable timed out"
            }
            .into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Cannot wait for fixture executable".into());
            }
        }
    };
    if !status.success() {
        // Report exit state only: subprocess diagnostics can contain fixtures.
        return Err(format!(
            "Fixture executable failed ({status}); run the reference baseline check separately"
        ));
    }
    read_bounded(&scratch.0.join("stdout"), MAX_STDOUT)
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
    fn failed_process_is_not_an_empty_passing_run() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 17"]);
        let error = capture(&mut command, b"", Duration::from_secs(1)).unwrap_err();
        assert!(error.starts_with("Fixture executable failed"));
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

    #[test]
    fn reads_are_bounded() {
        let scratch = Scratch::new().unwrap();
        scratch.file("large").unwrap().write_all(b"12345").unwrap();
        assert!(read_bounded(&scratch.0.join("large"), 4).is_err());
        assert_eq!(read_bounded(&scratch.0.join("large"), 5).unwrap(), b"12345");
    }
}
