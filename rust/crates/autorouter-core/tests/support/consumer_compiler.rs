//! Exact executed-test context for synthetic metadata-only Rust consumers.
//! Cargo fingerprints are internal: missing/unsupported layouts fail explicitly.
//! This is test dependency selection, not shipping/source-to-binary attestation.
use serde_json::Value;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct CompileContext {
    library: PathBuf,
    dependencies: Vec<PathBuf>,
    target: Option<String>,
    rustflags: Vec<String>,
}

fn read_bounded(path: &Path, cap: usize) -> Vec<u8> {
    assert!(fs::symlink_metadata(path).unwrap().file_type().is_file());
    let mut bytes = Vec::new();
    fs::File::open(path)
        .unwrap()
        .take((cap + 1) as u64)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(
        bytes.len() <= cap,
        "Compiler context metadata exceeds its bound"
    );
    bytes
}

fn fingerprint_value(path: &Path) -> u64 {
    let bytes = read_bounded(path, 64);
    let text = std::str::from_utf8(&bytes).unwrap().trim();
    assert_eq!(text.len(), 16, "Unsupported Cargo fingerprint encoding");
    let mut decoded = [0u8; 8];
    for (index, slot) in decoded.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).unwrap();
    }
    u64::from_le_bytes(decoded)
}

fn path_argument(prefix: &str, path: &Path) -> OsString {
    let mut argument = OsString::from(prefix);
    argument.push(path);
    argument
}

impl CompileContext {
    pub fn for_test(package: &str, test: &str) -> Self {
        Self::from_executable(&std::env::current_exe().unwrap(), package, test)
    }

    fn from_executable(executable: &Path, package: &str, test: &str) -> Self {
        let name = executable.file_stem().unwrap().to_str().unwrap();
        let hash = name.strip_prefix(&format!("{test}-")).unwrap();
        assert!(hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()));
        let dependencies = executable.parent().unwrap();
        assert_eq!(dependencies.file_name().unwrap(), "deps");
        let profile = dependencies.parent().unwrap();
        let fingerprints = profile.join(".fingerprint");
        let record: Value = serde_json::from_slice(&read_bounded(
            &fingerprints
                .join(format!("{package}-{hash}"))
                .join(format!("test-integration-test-{test}.json")),
            1024 * 1024,
        ))
        .unwrap();
        let core: Vec<_> = record["deps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row[1] == "autorouter_core")
            .collect();
        assert_eq!(core.len(), 1);
        let expected = core[0][3].as_u64().unwrap();
        let mut matches = Vec::new();
        let mut candidates = 0usize;
        for entry in fs::read_dir(&fingerprints).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let Some(hash) = name
                .to_str()
                .and_then(|n| n.strip_prefix("autorouter-core-"))
            else {
                continue;
            };
            if !entry.file_type().unwrap().is_dir() {
                continue;
            }
            let stamp = entry.path().join("lib-autorouter_core");
            if !stamp.is_file() {
                continue;
            }
            candidates += 1;
            assert!(candidates <= 32, "Too many Cargo core library contexts");
            if fingerprint_value(&stamp) == expected {
                let library_record: Value = serde_json::from_slice(&read_bounded(
                    &entry.path().join("lib-autorouter_core.json"),
                    1024 * 1024,
                ))
                .unwrap();
                for field in ["rustc", "compile_kind", "rustflags"] {
                    assert_eq!(library_record[field], record[field]);
                }
                matches.push(dependencies.join(format!("libautorouter_core-{hash}.rlib")));
            }
        }
        assert_eq!(
            matches.len(),
            1,
            "Exact executed-test core dependency is unavailable or ambiguous"
        );
        let library = matches.pop().unwrap();
        assert!(
            fs::symlink_metadata(&library)
                .unwrap()
                .file_type()
                .is_file()
        );
        let mut directories = vec![dependencies.to_path_buf()];
        let target = if record["compile_kind"].as_u64().unwrap() == 0 {
            None
        } else {
            // Standard built-in Cargo target layout; custom JSON targets and a
            // separately configured build-dir are not guessed or reconstructed.
            let target_root = profile.parent().unwrap();
            let target = target_root.file_name().unwrap().to_str().unwrap();
            assert!(
                !target.is_empty()
                    && target
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            );
            let host = target_root
                .parent()
                .unwrap()
                .join(profile.file_name().unwrap())
                .join("deps");
            assert!(
                host.is_dir(),
                "Explicit Cargo target requires host dependency context"
            );
            directories.push(host);
            Some(target.to_owned())
        };
        let rustflags = record["rustflags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|flag| flag.as_str().unwrap().to_owned())
            .collect();
        Self {
            library,
            dependencies: directories,
            target,
            rustflags,
        }
    }

    pub fn library(&self) -> &Path {
        &self.library
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()));
        command.args(&self.rustflags);
        if let Some(target) = &self.target {
            command.arg("--target").arg(target);
        }
        command.args([
            "--crate-type=lib",
            "--edition=2024",
            "--emit=metadata",
            "--error-format=json",
        ]);
        command
            .arg("--extern")
            .arg(path_argument("autorouter_core=", &self.library));
        for directory in &self.dependencies {
            command
                .arg("-L")
                .arg(path_argument("dependency=", directory));
        }
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "autorouter-consumer-context-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn library(profile: &Path, hash: &str, stamp: u64, kind: u64, flags: &[&str]) -> PathBuf {
            let directory = profile
                .join(".fingerprint")
                .join(format!("autorouter-core-{hash}"));
            fs::create_dir_all(&directory).unwrap();
            let encoded: String = stamp
                .to_le_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            fs::write(directory.join("lib-autorouter_core"), encoded).unwrap();
            fs::write(
                directory.join("lib-autorouter_core.json"),
                json!({"rustc":7,"compile_kind":kind,"rustflags":flags}).to_string(),
            )
            .unwrap();
            let library = profile
                .join("deps")
                .join(format!("libautorouter_core-{hash}.rlib"));
            fs::create_dir_all(library.parent().unwrap()).unwrap();
            fs::write(
                &library,
                b"synthetic metadata fixture, never loaded by rustc",
            )
            .unwrap();
            library
        }
        fn executable(&self, target: Option<&str>, flags: &[&str]) -> (PathBuf, PathBuf) {
            let root = target.map_or_else(|| self.0.clone(), |target| self.0.join(target));
            let profile = root.join("debug");
            let hash = "0123456789abcdef";
            let directory = profile
                .join(".fingerprint")
                .join(format!("autorouter-core-{hash}"));
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("test-integration-test-context.json"), json!({"deps":[[1,"autorouter_core",false,42]],"rustc":7,"compile_kind":u64::from(target.is_some()),"rustflags":flags}).to_string()).unwrap();
            let exact = Self::library(
                &profile,
                "1111111111111111",
                42,
                u64::from(target.is_some()),
                flags,
            );
            Self::library(
                &profile,
                "2222222222222222",
                99,
                u64::from(target.is_some()),
                flags,
            );
            (profile.join("deps/context-0123456789abcdef"), exact)
        }
    }

    #[test]
    fn exact_dependency_selection_ignores_other_available_libraries_and_keeps_flag_tokens() {
        let fixture = Fixture::new();
        let flags = [
            "--remap-path-prefix=/a path=/source",
            "-C",
            "target-cpu=generic",
        ];
        let (executable, exact) = fixture.executable(None, &flags);
        let context = CompileContext::from_executable(&executable, "autorouter-core", "context");
        assert_eq!(context.library(), exact.as_path());
        assert_eq!(
            context.dependencies,
            [executable.parent().unwrap().to_path_buf()]
        );
        assert!(context.target.is_none());
        let arguments: Vec<_> = context
            .command()
            .get_args()
            .map(|arg| arg.to_owned())
            .collect();
        assert_eq!(&arguments[..3], &flags.map(OsString::from));
        assert_eq!(arguments.iter().filter(|arg| *arg == "-L").count(), 1);
        assert!(!arguments.iter().any(|arg| arg == "--target"));
    }

    #[test]
    fn explicit_target_replays_target_and_both_target_and_host_dependencies() {
        let fixture = Fixture::new();
        let target = "aarch64-unknown-linux-musl";
        let (executable, _) = fixture.executable(Some(target), &["-Ctarget-feature=+crt-static"]);
        let host = fixture.0.join("debug/deps");
        fs::create_dir_all(&host).unwrap();
        let context = CompileContext::from_executable(&executable, "autorouter-core", "context");
        assert_eq!(context.target.as_deref(), Some(target));
        assert_eq!(
            context.dependencies,
            [executable.parent().unwrap().to_path_buf(), host]
        );
        let arguments: Vec<_> = context
            .command()
            .get_args()
            .map(|arg| arg.to_owned())
            .collect();
        assert!(
            arguments
                .windows(2)
                .any(|args| args == [OsString::from("--target"), OsString::from(target)])
        );
        assert_eq!(arguments.iter().filter(|arg| *arg == "-L").count(), 2);
    }

    #[test]
    fn unavailable_or_ambiguous_exact_dependency_is_rejected_instead_of_substituted() {
        for duplicate in [false, true] {
            let fixture = Fixture::new();
            let (executable, exact) = fixture.executable(None, &[]);
            if duplicate {
                Fixture::library(
                    exact.parent().unwrap().parent().unwrap(),
                    "3333333333333333",
                    42,
                    0,
                    &[],
                );
            } else {
                fs::remove_file(exact).unwrap();
            }
            assert!(
                std::panic::catch_unwind(|| CompileContext::from_executable(
                    &executable,
                    "autorouter-core",
                    "context"
                ))
                .is_err()
            );
        }
    }
}
