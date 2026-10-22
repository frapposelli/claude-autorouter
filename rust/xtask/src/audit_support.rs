//! Runtime-independent bounded reads and private directories for bootstrap audits.
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn regular(path: &Path, maximum: u64) -> Result<Vec<u8>, String> {
    let before = fs::symlink_metadata(path).map_err(|_| "Cannot inspect audit input")?;
    if !before.is_file() || before.len() > maximum {
        return Err("Audit input must be a bounded regular file".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "Cannot open regular audit input")?;
    let actual = file
        .metadata()
        .map_err(|_| "Cannot inspect opened audit input")?;
    if !actual.is_file()
        || actual.len() > maximum
        || before.dev() != actual.dev()
        || before.ino() != actual.ino()
    {
        return Err("Audit input changed identity or bounds".into());
    }
    let mut bytes = Vec::new();
    file.take(maximum.checked_add(1).ok_or("Invalid audit input bound")?)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read audit input")?;
    if bytes.len() as u64 > maximum {
        return Err("Audit input exceeds byte limit".into());
    }
    Ok(bytes)
}

/// Fresh lowercase hex with the caller's original entropy width and error policy.
/// Filesystem naming, permissions, collision handling and cleanup stay local.
pub fn random_hex<const N: usize>() -> Result<String, getrandom::Error> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub struct Scratch(pub PathBuf);
impl Scratch {
    pub fn new(prefix: &str) -> Result<Self, String> {
        let suffix =
            random_hex::<16>().map_err(|_| "Cannot obtain private audit directory entropy")?;
        let path = std::env::temp_dir().join(format!("autorouter-{prefix}-{suffix}"));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| "Cannot create private audit directory")?;
        Ok(Self(
            path.canonicalize()
                .map_err(|_| "Cannot resolve private audit directory")?,
        ))
    }
    pub fn file(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        use std::io::Write;
        let path = self.0.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|_| "Cannot create private audit file")?;
        file.write_all(bytes)
            .map_err(|_| "Cannot write private audit file")?;
        Ok(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_audit_inputs_reject_links_fifo_and_growth_without_reading_payloads() {
        let scratch = Scratch::new("audit-read-contract").unwrap();
        let file = scratch.file("plain", b"synthetic").unwrap();
        assert_eq!(regular(&file, 9).unwrap(), b"synthetic");
        assert!(regular(&file, 8).is_err());
        std::os::unix::fs::symlink(&file, scratch.0.join("link")).unwrap();
        assert!(regular(&scratch.0.join("link"), 9).is_err());
        nix::unistd::mkfifo(&scratch.0.join("fifo"), nix::sys::stat::Mode::S_IRUSR).unwrap();
        assert!(regular(&scratch.0.join("fifo"), 9).is_err());
    }
}
