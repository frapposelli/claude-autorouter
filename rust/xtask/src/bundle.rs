//! Explicit public-source transfer allowlist; never copies a checkout wholesale.
use crate::package::archive::{Entry, decode_root, encode_root};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
const ROOT: &str = "autorouter-benchmark";
const FIXED: &[&str] = &[
    "LICENSE",
    "rust/Cargo.toml",
    "rust/Cargo.lock",
    "rust/rust-toolchain.toml",
    "rust/.cargo/config.toml",
    "rust/parity/local-benchmark-v1.json",
    "rust/parity/performance-gates.json",
    "rust/parity/benchmark-protocol.json",
    "rust/parity/baseline.json",
    "rust/distribution/autorouter.sh",
    "rust/distribution/platforms.json",
    "rust/distribution/README.md",
    "rust/xtask/Cargo.toml",
    "rust/xtask/fixtures/live-cases-v2.json",
    "rust/xtask/fixtures/merge-intervals.rs",
    "rust/xtask/fixtures/merge-intervals.test.rs",
    "rust/xtask/fixtures/repair-cargo.toml",
    "test/fixtures/ollama-routing.json",
    "test/fixtures/claude-protocol-v1.json",
    "docs/hardware-benchmark.md",
    "docs/hardware-results-16gb.md",
    "docs/hardware-results-16gb.json",
    "docs/hardware-results-64gb.json",
    "docs/hardware-comparison.md",
    "rust/vendor/hyper/Cargo.toml",
    "rust/vendor/hyper/Cargo.toml.orig",
    "rust/vendor/hyper/LICENSE",
    "rust/vendor/hyper/README.md",
    "rust/vendor/README.md",
    "rust/vendor/hyper-provenance.json",
    "rust/vendor/hyper-node-http1-compat.patch",
    "rust/vendor/verify-hyper.mjs",
    "rust/vendor/node-ca/node-v22.14.0.pem",
    "rust/vendor/node-ca/provenance.json",
    "rust/vendor/node-ca/LICENSE",
    "rust/distribution/licenses/alloc-stdlib-0.3.0-LICENSE",
    "rust/distribution/licenses/provenance.json",
];
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn safe(path: &str) -> bool {
    !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != ".." && !s.chars().any(char::is_control))
}
fn allowed(path: &str) -> bool {
    if !safe(path) {
        return false;
    }
    if FIXED.contains(&path) {
        return true;
    }
    for name in ["autorouter-core", "autorouter-runtime", "claude-autorouter"] {
        let prefix = format!("rust/crates/{name}/");
        if let Some(relative) = path.strip_prefix(&prefix) {
            return relative == "Cargo.toml"
                || ((relative.starts_with("src/") || relative.starts_with("tests/"))
                    && relative.ends_with(".rs"))
                || (relative.starts_with("src/help/") && relative.ends_with(".txt"));
        }
    }
    ["rust/xtask/src/", "rust/vendor/hyper/src/"]
        .iter()
        .any(|prefix| path.starts_with(prefix) && path.ends_with(".rs"))
}
fn walk(directory: &Path, root: &Path, paths: &mut Vec<String>) -> Result<(), String> {
    for entry in fs::read_dir(directory).map_err(|_| "Cannot inspect bundle sources")? {
        let entry = entry.map_err(|_| "Cannot inspect bundle entry")?;
        let kind = entry
            .file_type()
            .map_err(|_| "Cannot inspect bundle source type")?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "Source outside repository")?
            .to_str()
            .ok_or("Invalid bundle source filename")?;
        if kind.is_symlink() {
            if allowed(relative) {
                return Err("Bundle sources must not be symbolic links".into());
            }
            continue;
        }
        if kind.is_dir() {
            let name = entry.file_name();
            if name.to_str().is_none_or(|n| {
                n.starts_with('.') || ["target", "artifacts", "node_modules"].contains(&n)
            }) {
                continue;
            }
            walk(&path, root, paths)?;
        } else if kind.is_file() && allowed(relative) {
            paths.push(relative.to_owned());
        }
    }
    Ok(())
}
fn regular(root: &Path, relative: &str) -> Result<Vec<u8>, String> {
    let mut path = root.to_path_buf();
    for part in Path::new(relative).components() {
        path.push(part);
        if fs::symlink_metadata(&path)
            .map_err(|_| "Missing bundle source")?
            .file_type()
            .is_symlink()
        {
            return Err("Bundle sources must not contain symbolic links".into());
        }
    }
    let info = fs::symlink_metadata(&path).map_err(|_| "Cannot inspect bundle source")?;
    if !info.is_file() || info.nlink() != 1 || info.len() > 1024 * 1024 {
        return Err("Benchmark source must be a bounded regular file".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits())
        .open(&path)
        .map_err(|_| "Cannot open bundle source")?;
    let opened = file
        .metadata()
        .map_err(|_| "Cannot inspect open bundle source")?;
    if !opened.is_file() || opened.dev() != info.dev() || opened.ino() != info.ino() {
        return Err("Bundle source changed during inspection".into());
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read bundle source")?;
    if bytes.len() > 1024 * 1024 {
        return Err("Benchmark source exceeds byte limit".into());
    }
    Ok(bytes)
}
fn collect(root: &Path) -> Result<BTreeMap<String, Entry>, String> {
    let mut paths = FIXED.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    for directory in ["rust/crates", "rust/xtask/src", "rust/vendor/hyper/src"] {
        walk(&root.join(directory), root, &mut paths)?;
    }
    paths.sort();
    paths.dedup();
    let mut files = BTreeMap::new();
    let mut manifest = BTreeMap::new();
    for path in paths {
        if !allowed(&path) {
            return Err("Bundle source escaped its allowlist".into());
        }
        let bytes = regular(root, &path)?;
        manifest.insert(path.clone(), sha(&bytes));
        files.insert(path, Entry { bytes, mode: 0o644 });
    }
    let readme=b"# AutoRouter native benchmark source bundle\n\nPublic source and synthetic fixtures only. Install the pinned Rust toolchain and fetch the Cargo.lock dependencies (or provide an existing Cargo cache), then run from rust/:\n\n    cargo build --locked --release --package xtask\n    cargo xtask evaluate-ollama --help\n\nReading help makes no evaluator calls. Hardware evaluator runs are explicitly opt-in, do not download models, and must retain fixture hashes, model digest, residency, cold/warm conditions and the unchanged acceptance rubric. Existing experimental quality failures remain visible. The historical docs describe the original Node commands; use native xtask help for the replacement interface.\n\nThe runtime-neutral Node/Rust HTTP benchmark requires its separately verified frozen Node reference; no Node source/runtime, private reports, credentials, user configuration, Cargo cache or binaries are included here. This source bundle itself does not establish build provenance, platform qualification, task quality or performance acceptance.\n".to_vec();
    manifest.insert("README.native.md".into(), sha(&readme));
    files.insert(
        "README.native.md".into(),
        Entry {
            bytes: readme,
            mode: 0o644,
        },
    );
    let record = json!({"schema_version":2,"kind":"native_benchmark_public_source_bundle","fixture_sha256":manifest.get("test/fixtures/ollama-routing.json"),"files":manifest,"build_requires":"Pinned Rust toolchain and locked dependencies via network or prepopulated Cargo cache; no Node required for native evaluator tools.","private_material_included":false,"live_calls_performed":false});
    files.insert(
        "source-manifest.json".into(),
        Entry {
            bytes: serde_json::to_vec_pretty(&record)
                .map_err(|_| "Cannot serialize bundle manifest")?,
            mode: 0o644,
        },
    );
    Ok(files)
}
pub fn create(root: &Path, destination: &Path) -> Result<Value, String> {
    let sidecar = PathBuf::from(format!("{}.sha256", destination.display()));
    if fs::symlink_metadata(destination).is_ok() || fs::symlink_metadata(&sidecar).is_ok() {
        return Err("Refusing to overwrite an existing bundle or checksum".into());
    }
    let files = collect(root)?;
    let bytes = encode_root(ROOT, &files)?;
    let (verified, expanded) = decode_root(&bytes, ROOT)?;
    if verified.len() != files.len()
        || files.iter().any(|(path, file)| {
            verified
                .get(path)
                .is_none_or(|found| found.bytes != file.bytes || found.mode != file.mode)
        })
    {
        return Err("Bundle archive verification failed".into());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|_| "Cannot create bundle destination directory")?;
    }
    let checksum = sha(&bytes);
    let mut archive = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)
        .map_err(|_| "Cannot create source bundle")?;
    archive
        .write_all(&bytes)
        .map_err(|_| "Cannot write source bundle")?;
    let mut sum = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&sidecar)
        .map_err(|_| "Cannot create bundle checksum")?;
    writeln!(
        sum,
        "{checksum}  {}",
        destination
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("Invalid bundle destination filename")?
    )
    .map_err(|_| "Cannot write bundle checksum")?;
    Ok(
        json!({"bundle":destination.to_string_lossy(),"sha256":checksum,"files":files.len(),"compressed_bytes":bytes.len(),"expanded_tar_bytes":expanded,"source_only":true,"requires_node":false,"dependencies_bundled":false,"release_approved":false}),
    )
}
pub fn run(args: &[String], root: &Path) -> Result<bool, String> {
    if args == ["--help"] {
        println!(
            "cargo xtask benchmark-bundle [NEW_DESTINATION.tar.gz]\nCreate an allowlisted public Rust source and synthetic-fixture bundle; no credentials, private reports, binaries or Node sources. Requires pinned Rust and Cargo dependencies on destination host; performs no evaluator calls."
        );
        return Ok(true);
    }
    if args.len() > 1 {
        return Err("Usage: cargo xtask benchmark-bundle [NEW_DESTINATION.tar.gz]".into());
    }
    let destination = root.join(
        args.first()
            .map(String::as_str)
            .unwrap_or("artifacts/autorouter-native-hardware-benchmark.tar.gz"),
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&create(root, &destination)?)
            .map_err(|_| "Cannot serialize bundle result")?
    );
    Ok(true)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_process::Scratch;
    #[test]
    fn explicit_allowlist_rejects_private_adjacent_material() {
        for path in [
            ".env",
            "rust/.env",
            "rust/xtask/src/config.json",
            "rust/xtask/fixtures/credentials.json",
            "rust/crates/autorouter-core/src/session.json",
            "rust/crates/autorouter-core/target/main.rs",
            "artifacts/result.json",
            ".git/config",
            "rust/xtask/src/../secret.rs",
            "rust/crates/autorouter-core/src/nested/.env",
        ] {
            assert!(!allowed(path), "{path}");
        }
        assert!(allowed("rust/xtask/src/bundle.rs"));
        assert!(allowed("rust/vendor/hyper/src/lib.rs"));
    }
    #[test]
    fn actual_bundle_checks_every_hash_excludes_canary_and_refuses_overwrite() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let scratch = Scratch::new("bundle-test").unwrap();
        let destination = scratch.0.join("bundle.tar.gz");
        let result = create(&root, &destination).unwrap();
        let bytes = fs::read(&destination).unwrap();
        assert_eq!(result["sha256"], sha(&bytes));
        let (files, _) = decode_root(&bytes, ROOT).unwrap();
        let manifest: Value = serde_json::from_slice(&files["source-manifest.json"].bytes).unwrap();
        for (path, hash) in manifest["files"].as_object().unwrap() {
            assert_eq!(hash, &json!(sha(&files[path].bytes)));
        }
        assert!(
            files.keys().all(|p| allowed(p)
                || ["source-manifest.json", "README.native.md"].contains(&p.as_str()))
        );
        assert!(create(&root, &destination).is_err());
        assert_eq!(fs::read(&destination).unwrap(), bytes);
    }
    #[test]
    fn source_symlink_and_oversize_are_rejected() {
        let scratch = Scratch::new("bundle-source-test").unwrap();
        scratch
            .file("external", b"synthetic-private-canary")
            .unwrap();
        std::os::unix::fs::symlink(scratch.0.join("external"), scratch.0.join("link.rs")).unwrap();
        assert!(regular(&scratch.0, "link.rs").is_err());
        scratch
            .file("large.rs", &vec![b'x'; 1024 * 1024 + 1])
            .unwrap();
        assert!(regular(&scratch.0, "large.rs").is_err());
    }
}
