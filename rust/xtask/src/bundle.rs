//! Explicit public-source transfer allowlist; never copies a checkout wholesale.
use crate::package::archive::{Entry, SOURCE, decode_root, encode_root};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
const ROOT: &str = "autorouter-benchmark";
const LARGE_SYNTHETIC_CORPORA: &[&str] = &[
    "rust/parity/cases/router-contracts.jsonl",
    "rust/parity/cases/ollama-router-contracts.jsonl",
    "rust/parity/cases/response-observer-contracts.jsonl",
    "rust/parity/cases/telemetry-statusline-contracts.jsonl",
    "rust/parity/cases/server-streaming-routing-contracts.jsonl",
];
const FIXED: &[&str] = &[
    "LICENSE",
    "rust/Cargo.toml",
    "rust/Cargo.lock",
    "rust/rust-toolchain.toml",
    "rust/crates/autorouter-runtime/src/server_deadline_contracts.rs",
    "rust/.cargo/config.toml",
    "rust/parity/local-benchmark-v1.json",
    "rust/parity/local-benchmark-v2.json",
    "rust/parity/local-benchmark-v3.json",
    "rust/parity/storage-benchmark-v1.json",
    "rust/parity/storage-contracts.capture.json",
    "rust/parity/storage-reference.mjs",
    "scripts/rust-reference.mjs",
    "rust/xtask/tests/benchmark_storage.rs",
    "rust/docs/storage-benchmark.md",
    "rust/xtask/tests/benchmark_resources.rs",
    "rust/parity/live-gateway.mjs",
    "rust/parity/cases/router-contracts.jsonl",
    "rust/parity/cases/router-contracts.capture.json",
    "rust/parity/cases/router-concurrency-contracts.jsonl",
    "rust/parity/cases/router-concurrency-contracts.capture.json",
    "rust/parity/cases/evaluation-report-contracts.jsonl",
    "rust/parity/cases/evaluation-report-contracts.capture.json",
    "rust/parity/cases/local-setup-diagnostic.jsonl",
    "rust/parity/cases/local-setup-diagnostic.capture.json",
    "rust/parity/cases/jev-routing-original-contracts.jsonl",
    "rust/parity/cases/jev-routing-original-contracts.capture.json",
    "rust/parity/cases/local-setup-pull-contracts.jsonl",
    "rust/parity/cases/local-setup-pull-contracts.capture.json",
    "rust/parity/cases/policy-command-contracts.jsonl",
    "rust/parity/cases/policy-command-contracts.capture.json",
    "rust/parity/cases/local-finite-contracts.jsonl",
    "rust/parity/cases/local-finite-contracts.capture.json",
    "rust/parity/cases/local-diagnostic-contracts.jsonl",
    "rust/parity/cases/local-diagnostic-contracts.capture.json",
    "rust/parity/cases/server-response-contracts.jsonl",
    "rust/parity/cases/server-response-contracts.capture.json",
    "rust/parity/cases/server-streaming-routing-contracts.jsonl",
    "rust/parity/cases/server-streaming-routing-contracts.capture.json",
    "rust/parity/cases/server-identity-safety-contracts.jsonl",
    "rust/parity/cases/server-identity-safety-contracts.capture.json",
    "rust/parity/cases/ollama-evaluator-finite-contracts.jsonl",
    "rust/parity/cases/ollama-evaluator-finite-contracts.capture.json",
    "rust/parity/cases/ollama-evaluator-timed-contracts.jsonl",
    "rust/parity/cases/ollama-evaluator-timed-contracts.capture.json",
    "rust/parity/cases/redaction-contracts.jsonl",
    "rust/parity/cases/redaction-contracts.capture.json",
    "rust/parity/cases/ollama-router-contracts.jsonl",
    "rust/parity/cases/ollama-router-contracts.capture.json",
    "rust/parity/cases/ollama-routing-contracts.jsonl",
    "rust/parity/cases/ollama-routing-contracts.capture.json",
    "rust/parity/cases/ollama-routing-tool-contracts.jsonl",
    "rust/parity/cases/ollama-routing-tool-contracts.capture.json",
    "rust/parity/cases/savings-finite-contracts.jsonl",
    "rust/parity/cases/savings-finite-contracts.capture.json",
    "rust/parity/cases/ollama-routing-timeout-contracts.jsonl",
    "rust/parity/cases/ollama-routing-timeout-contracts.capture.json",
    "rust/parity/cases/savings-capacity-contracts.jsonl",
    "rust/parity/cases/savings-capacity-contracts.capture.json",
    "rust/parity/cases/statusline-boundary-contracts.jsonl",
    "rust/parity/cases/statusline-boundary-contracts.capture.json",
    "rust/parity/cases/local-diagnostic-timeout-contracts.jsonl",
    "rust/parity/cases/local-diagnostic-timeout-contracts.capture.json",
    "rust/parity/cases/local-diagnostic-boundary-contracts.jsonl",
    "rust/parity/cases/local-diagnostic-boundary-contracts.capture.json",
    "rust/parity/cases/server-subscription-contracts.jsonl",
    "rust/parity/cases/server-subscription-contracts.capture.json",
    "rust/parity/cases/benchmark-router-contracts.jsonl",
    "rust/parity/cases/benchmark-router-contracts.capture.json",
    "rust/parity/cases/release-contracts.jsonl",
    "rust/parity/cases/release-contracts.capture.json",
    "rust/parity/cases/response-observer-contracts.jsonl",
    "rust/parity/cases/response-observer-contracts.capture.json",
    "rust/parity/cases/telemetry-statusline-contracts.jsonl",
    "rust/parity/cases/telemetry-statusline-contracts.capture.json",
    "rust/parity/cases/status-state-contracts.jsonl",
    "rust/parity/cases/status-state-contracts.capture.json",
    "rust/parity/cases/session-log-contracts.jsonl",
    "rust/parity/cases/session-log-contracts.capture.json",
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
    "rust/vendor/hyper-util-provenance.json",
    "rust/vendor/hyper-util-request-lease.patch",
    "rust/vendor/verify-hyper-util.mjs",
    "rust/vendor/verify-production-features.mjs",
    "rust/vendor/openssl-provenance.json",
    "rust/vendor/openssl-node-trust.patch",
    "rust/vendor/verify-openssl.mjs",
    "rust/vendor/TLS-TRUST.md",
    "rust/vendor/node-ca/node-v22.14.0.pem",
    "rust/vendor/node-ca/provenance.json",
    "rust/vendor/node-ca/LICENSE",
    "rust/distribution/licenses/alloc-stdlib-0.3.0-LICENSE",
    "rust/distribution/licenses/provenance.json",
];
// Exact public upstream archive inventory, including its synthetic certificate
// fixtures. Adjacent files are never admitted by a directory-wide wildcard.
const OPENSSL_FILES: &[&str] = &[
    ".cargo_vcs_info.json",
    "CHANGELOG.md",
    "Cargo.lock",
    "Cargo.toml",
    "Cargo.toml.orig",
    "LICENSE",
    "LICENSE-APACHE",
    "README.md",
    "build.rs",
    "examples/mk_certs.rs",
    "src/aes.rs",
    "src/asn1.rs",
    "src/base64.rs",
    "src/bio.rs",
    "src/bn.rs",
    "src/cipher.rs",
    "src/cipher_ctx.rs",
    "src/cms.rs",
    "src/conf.rs",
    "src/derive.rs",
    "src/dh.rs",
    "src/dsa.rs",
    "src/ec.rs",
    "src/ecdsa.rs",
    "src/encrypt.rs",
    "src/envelope.rs",
    "src/error.rs",
    "src/ex_data.rs",
    "src/fips.rs",
    "src/hash.rs",
    "src/kdf.rs",
    "src/lib.rs",
    "src/lib_ctx.rs",
    "src/macros.rs",
    "src/md.rs",
    "src/md_ctx.rs",
    "src/memcmp.rs",
    "src/nid.rs",
    "src/ocsp.rs",
    "src/ossl_param.rs",
    "src/pkcs12.rs",
    "src/pkcs5.rs",
    "src/pkcs7.rs",
    "src/pkey.rs",
    "src/pkey_ctx.rs",
    "src/provider.rs",
    "src/rand.rs",
    "src/rsa.rs",
    "src/sha.rs",
    "src/sign.rs",
    "src/srtp.rs",
    "src/ssl/bio.rs",
    "src/ssl/callbacks.rs",
    "src/ssl/connector.rs",
    "src/ssl/error.rs",
    "src/ssl/mod.rs",
    "src/ssl/test/mod.rs",
    "src/ssl/test/server.rs",
    "src/stack.rs",
    "src/string.rs",
    "src/symm.rs",
    "src/util.rs",
    "src/version.rs",
    "src/x509/extension.rs",
    "src/x509/mod.rs",
    "src/x509/store.rs",
    "src/x509/tests.rs",
    "src/x509/verify.rs",
    "test/aia_bad_utf8_cert.pem",
    "test/aia_test_cert.pem",
    "test/alt_name_cert.pem",
    "test/authority_key_identifier.pem",
    "test/ca.crt",
    "test/cert.pem",
    "test/certs.pem",
    "test/certv3.pem",
    "test/certv3_extfile",
    "test/cms.p12",
    "test/cms_pubkey.der",
    "test/corrupted-rsa.pem",
    "test/crl-ca.crt",
    "test/csr.pem",
    "test/dhparams.pem",
    "test/dsa.pem",
    "test/dsa.pem.pub",
    "test/dsaparam.pem",
    "test/entry_extensions.crl",
    "test/identity.p12",
    "test/intermediate-ca.key",
    "test/intermediate-ca.pem",
    "test/key.der",
    "test/key.der.pub",
    "test/key.pem",
    "test/key.pem.pub",
    "test/keystore-empty-chain.p12",
    "test/leaf.pem",
    "test/nid_test_cert.pem",
    "test/nid_uid_test_cert.pem",
    "test/ocsp_ca_cert.der",
    "test/ocsp_resp_no_nextupdate.der",
    "test/ocsp_resp_revoked.der",
    "test/ocsp_subject_cert.der",
    "test/pkcs1.pem.pub",
    "test/pkcs8-nocrypt.der",
    "test/pkcs8.der",
    "test/root-ca.key",
    "test/root-ca.pem",
    "test/rsa-encrypted.pem",
    "test/rsa.pem",
    "test/rsa.pem.pub",
    "test/subca.crt",
    "test/test.crl",
];
// Pinned hyper-util archive inventory plus the reviewed opt-in lease module.
// An adjacent file is never included merely because its extension is Rust.
const HYPER_UTIL_FILES: &[&str] = &[
    ".cargo_vcs_info.json",
    ".github/workflows/CI.yml",
    ".github/workflows/rustdoc-preview.yml",
    ".gitignore",
    "CHANGELOG.md",
    "Cargo.lock",
    "Cargo.toml",
    "Cargo.toml.orig",
    "LICENSE",
    "README.md",
    "examples/client.rs",
    "examples/client_tracing.rs",
    "examples/server.rs",
    "examples/server_graceful.rs",
    "src/client/legacy/client.rs",
    "src/client/legacy/connect/capture.rs",
    "src/client/legacy/connect/dns.rs",
    "src/client/legacy/connect/http.rs",
    "src/client/legacy/connect/mod.rs",
    "src/client/legacy/connect/proxy/mod.rs",
    "src/client/legacy/connect/proxy/socks/mod.rs",
    "src/client/legacy/connect/proxy/socks/v4/errors.rs",
    "src/client/legacy/connect/proxy/socks/v4/messages.rs",
    "src/client/legacy/connect/proxy/socks/v4/mod.rs",
    "src/client/legacy/connect/proxy/socks/v5/errors.rs",
    "src/client/legacy/connect/proxy/socks/v5/messages.rs",
    "src/client/legacy/connect/proxy/socks/v5/mod.rs",
    "src/client/legacy/connect/proxy/tunnel.rs",
    "src/client/legacy/connect/request_lease.rs",
    "src/client/legacy/mod.rs",
    "src/client/legacy/pool.rs",
    "src/client/mod.rs",
    "src/client/pool/cache.rs",
    "src/client/pool/map.rs",
    "src/client/pool/mod.rs",
    "src/client/pool/negotiate.rs",
    "src/client/pool/singleton.rs",
    "src/client/proxy/matcher.rs",
    "src/client/proxy/mod.rs",
    "src/common/exec.rs",
    "src/common/lazy.rs",
    "src/common/mod.rs",
    "src/common/rewind.rs",
    "src/common/sync.rs",
    "src/common/timer.rs",
    "src/lib.rs",
    "src/rt/io.rs",
    "src/rt/mod.rs",
    "src/rt/tokio.rs",
    "src/rt/tokio/with_hyper_io.rs",
    "src/rt/tokio/with_tokio_io.rs",
    "src/rt/tracing.rs",
    "src/server/conn/auto/mod.rs",
    "src/server/conn/auto/upgrade.rs",
    "src/server/conn/mod.rs",
    "src/server/graceful.rs",
    "src/server/mod.rs",
    "src/service/glue.rs",
    "src/service/mod.rs",
    "src/service/oneshot.rs",
    "tests/legacy_client.rs",
    "tests/proxy.rs",
    "tests/test_utils/mod.rs",
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
    if path
        .strip_prefix("rust/vendor/openssl/")
        .is_some_and(|relative| OPENSSL_FILES.contains(&relative))
    {
        return true;
    }
    if path
        .strip_prefix("rust/vendor/hyper-util/")
        .is_some_and(|relative| HYPER_UTIL_FILES.contains(&relative))
    {
        return true;
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
    // These exact synthetic corpora are compile-time test dependencies. Keep
    // other source files at1MiB and the whole SOURCE archive at32MiB expanded.
    let limit = if LARGE_SYNTHETIC_CORPORA.contains(&relative) {
        4 * 1024 * 1024
    } else {
        1024 * 1024
    };
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
    if !info.is_file() || info.nlink() != 1 || info.len() > limit {
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
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read bundle source")?;
    if bytes.len() as u64 > limit {
        return Err("Benchmark source exceeds byte limit".into());
    }
    Ok(bytes)
}
fn collect(root: &Path) -> Result<BTreeMap<String, Entry>, String> {
    let mut paths = FIXED.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    paths.extend(
        OPENSSL_FILES
            .iter()
            .map(|relative| format!("rust/vendor/openssl/{relative}")),
    );
    paths.extend(
        HYPER_UTIL_FILES
            .iter()
            .map(|relative| format!("rust/vendor/hyper-util/{relative}")),
    );
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
    let readme=b"# AutoRouter native benchmark source bundle\n\nPublic source and synthetic fixtures only. Install the pinned Rust toolchain, a C compiler, Make and Perl for the bundled OpenSSL build, and fetch the Cargo.lock dependencies (or provide an existing Cargo cache), then run from rust/:\n\n    cargo build --locked --release --package xtask\n    cargo xtask evaluate-ollama --help\n\nReading help makes no evaluator calls. Hardware evaluator runs are explicitly opt-in, do not download models, and must retain fixture hashes, model digest, residency, cold/warm conditions and the unchanged acceptance rubric. Existing experimental quality failures remain visible. The historical docs describe the original Node commands; use native xtask help for the replacement interface.\n\nThe runtime-neutral Node/Rust HTTP benchmark requires its separately verified frozen Node reference; no frozen Node product source/runtime, private reports, credentials, user configuration, Cargo cache or binaries are included here. This source bundle itself does not establish build provenance, platform qualification, task quality or performance acceptance.\n".to_vec();
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
    let bytes = encode_root(ROOT, &files, SOURCE)?;
    let (verified, expanded) = decode_root(&bytes, ROOT, SOURCE)?.into_parts();
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
            "cargo xtask benchmark-bundle [NEW_DESTINATION.tar.gz]\nCreate an allowlisted public Rust source and synthetic-fixture bundle; no credentials, private reports, binaries or frozen Node product sources. Requires pinned Rust and Cargo dependencies on destination host; performs no evaluator calls."
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
            "rust/vendor/openssl/test/private-session.pem",
            "rust/vendor/openssl/.env",
            "rust/vendor/hyper-util/.env",
            "rust/vendor/hyper-util/src/private.rs",
            "rust/vendor/hyper-util/tests/private-session.json",
            "rust/xtask/src/../secret.rs",
            "rust/crates/autorouter-core/src/nested/.env",
        ] {
            assert!(!allowed(path), "{path}");
        }
        assert!(allowed("rust/xtask/src/bundle.rs"));
        assert!(allowed("rust/vendor/hyper/src/lib.rs"));
        assert!(allowed("rust/vendor/hyper-util/LICENSE"));
        assert!(allowed(
            "rust/vendor/hyper-util/src/client/legacy/connect/request_lease.rs"
        ));
        assert!(allowed("rust/vendor/openssl/build.rs"));
        assert!(allowed("rust/vendor/openssl/test/root-ca.key"));
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
        let (files, _) = decode_root(&bytes, ROOT, SOURCE).unwrap().into_parts();
        let manifest: Value = serde_json::from_slice(&files["source-manifest.json"].bytes).unwrap();
        for (path, hash) in manifest["files"].as_object().unwrap() {
            assert_eq!(hash, &json!(sha(&files[path].bytes)));
        }
        // Compile-time embedded inputs are build dependencies even when the
        // Rust file itself is allowlisted. Inspect the actual packed sources.
        let embedded = regex::Regex::new(r#"include_(?:str|bytes)!\s*\(\s*"([^"]+)""#).unwrap();
        for (path, entry) in files.iter().filter(|(path, _)| path.ends_with(".rs")) {
            let source = String::from_utf8_lossy(&entry.bytes);
            for capture in embedded.captures_iter(&source) {
                let mut parts: Vec<_> = path.split('/').collect();
                parts.pop();
                for part in capture[1].split('/') {
                    match part {
                        ".." => {
                            assert!(parts.pop().is_some());
                        }
                        "." => {}
                        value => parts.push(value),
                    }
                }
                let required = parts.join("/");
                assert!(
                    files.contains_key(&required),
                    "Packed {path} is missing embedded source {required}"
                );
            }
        }
        // A packed Node oracle must retain its static relative imports even
        // though the frozen product modules are supplied separately at runtime.
        let imports =
            regex::Regex::new(r#"(?m)^import[^\n;]*\bfrom\s*['"](\.[^'"]+)['"]"#).unwrap();
        for (path, entry) in files.iter().filter(|(path, _)| path.ends_with(".mjs")) {
            let source = String::from_utf8_lossy(&entry.bytes);
            for capture in imports.captures_iter(&source) {
                let mut parts: Vec<_> = path.split('/').collect();
                parts.pop();
                for part in capture[1].split('/') {
                    match part {
                        ".." => assert!(parts.pop().is_some()),
                        "." => {}
                        value => parts.push(value),
                    }
                }
                let required = parts.join("/");
                assert!(
                    files.contains_key(&required),
                    "Packed {path} is missing imported helper {required}"
                );
            }
        }
        assert!(
            files.keys().all(|p| allowed(p)
                || ["source-manifest.json", "README.native.md"].contains(&p.as_str()))
        );
        for (name, allowed_files) in [("openssl", OPENSSL_FILES), ("hyper-util", HYPER_UTIL_FILES)]
        {
            let provenance: Value = serde_json::from_slice(
                &files[&format!("rust/vendor/{name}-provenance.json")].bytes,
            )
            .unwrap();
            let declared = provenance["patched_files"].as_object().unwrap();
            assert_eq!(declared.len(), allowed_files.len());
            for (relative, expected) in declared {
                assert!(allowed_files.contains(&relative.as_str()));
                assert_eq!(
                    *expected,
                    json!(sha(&files[&format!("rust/vendor/{name}/{relative}")].bytes))
                );
            }
        }
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
        fs::create_dir_all(scratch.0.join("rust/parity/cases")).unwrap();
        for path in LARGE_SYNTHETIC_CORPORA {
            let file = scratch.file(path, &vec![b'x'; 4 * 1024 * 1024]).unwrap();
            assert_eq!(regular(&scratch.0, path).unwrap().len(), 4 * 1024 * 1024);
            OpenOptions::new()
                .append(true)
                .open(file)
                .unwrap()
                .write_all(b"x")
                .unwrap();
            assert!(regular(&scratch.0, path).is_err());
        }
        scratch
            .file(
                "rust/parity/cases/adjacent.jsonl",
                &vec![b'x'; 1024 * 1024 + 1],
            )
            .unwrap();
        assert!(regular(&scratch.0, "rust/parity/cases/adjacent.jsonl").is_err());
    }
}
