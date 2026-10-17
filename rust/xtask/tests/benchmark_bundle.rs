//! Required release-profile executable handoff test. All evaluator I/O is local
//! synthetic HTTP; the receiver has no Node/Cargo/Git on PATH or credentials.
// Reuse the same bounded process owner used by the tooling. Its module tests
// are not the executable-bundle evidence selected by the required CI command.
#[allow(dead_code)]
#[path = "../src/audit_support.rs"]
mod audit_support;
#[allow(dead_code)]
#[path = "../src/process.rs"]
mod process;

use autorouter_core::js_json::JsDocument;
use autorouter_core::prompt_state::build_ollama_state_document;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;

const FIXTURE: &str = "test/fixtures/ollama-routing.json";
const EXECUTABLE: &str = "bin/autorouter-benchmark";
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn run(command: &mut Command) -> process::CapturedProcess {
    process::capture_result(command, b"", Duration::from_secs(120)).unwrap()
}
fn successful(command: &mut Command) -> Vec<u8> {
    let result = run(command);
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}
fn target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "x86") => "i686-unknown-linux-gnu",
        _ => panic!("Required executable-bundle suite needs an admitted native CI target"),
    }
}
struct Artifact {
    _scratch: audit_support::Scratch,
    extracted: PathBuf,
    unrelated: PathBuf,
    archive: PathBuf,
}
impl Artifact {
    fn create() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let scratch = audit_support::Scratch::new("executable-bundle-cli").unwrap();
        let archive = scratch.0.join("benchmark.tar.gz");
        let helper = Path::new(env!("CARGO_BIN_EXE_xtask"));
        assert!(
            fs::metadata(helper).unwrap().len() < 32 * 1024 * 1024,
            "Helper exceeds unchanged 32 MiB bound; run cargo test --release --package xtask --features executable-bundle-tests --test benchmark_bundle required_executable_bundle"
        );
        // Match Cargo's Linux hardlink layout with private files on one
        // filesystem, leaving the actual Cargo output untouched.
        let supplied = scratch.0.join("supplied-release-helper");
        fs::copy(helper, &supplied).unwrap();
        fs::hard_link(&supplied, scratch.0.join("supplied-helper-deps-alias")).unwrap();
        assert_eq!(fs::metadata(&supplied).unwrap().nlink(), 2);
        let report = successful(
            Command::new(helper)
                .current_dir(&root)
                .args(["benchmark-bundle", "--executable"])
                .arg(format!("{}={}", target(), supplied.display()))
                .arg(&archive),
        );
        let report: Value = serde_json::from_slice(&report).unwrap();
        assert_eq!(report["source_only"], false);
        assert_eq!(report["requires_node"], false);
        assert_eq!(report["requires_cargo"], false);
        assert_eq!(report["source_to_binary_provenance"], "unverified_prebuilt");
        assert!(report["expanded_tar_bytes"].as_u64().unwrap() <= 32 * 1024 * 1024);
        let checksum = hash(&fs::read(&archive).unwrap());
        assert_eq!(report["sha256"], checksum);
        assert_eq!(
            fs::read_to_string(scratch.0.join("benchmark.tar.gz.sha256")).unwrap(),
            format!("{checksum}  benchmark.tar.gz\n")
        );
        let listing = successful(Command::new("/usr/bin/tar").args(["-tzf"]).arg(&archive));
        let names: BTreeSet<_> = std::str::from_utf8(&listing).unwrap().lines().collect();
        let expected: BTreeSet<_> = [
            EXECUTABLE,
            FIXTURE,
            "LICENSE",
            "THIRD-PARTY-LICENSES.txt",
            "README.native.md",
            "executable-manifest.json",
        ]
        .into_iter()
        .map(|name| format!("autorouter-benchmark/{name}"))
        .collect();
        assert_eq!(names, expected.iter().map(String::as_str).collect());
        // Change only the extraction child umask; preserve the manifest modes
        // under ordinary private-shell conditions without mutating test globals.
        successful(
            Command::new("/bin/sh")
                .args([
                    "-c",
                    r#"umask 077; exec /usr/bin/tar -xzpf "$1" -C "$2""#,
                    "bundle-extract",
                ])
                .arg(&archive)
                .arg(&scratch.0),
        );
        let extracted = scratch.0.join("relocated evaluator with spaces");
        fs::rename(scratch.0.join("autorouter-benchmark"), &extracted).unwrap();
        let unrelated = scratch.0.join("unrelated working directory");
        fs::create_dir(&unrelated).unwrap();
        let manifest: Value =
            serde_json::from_slice(&fs::read(extracted.join("executable-manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["schema_version"], 1);
        assert_eq!(manifest["kind"], "native_benchmark_executable_bundle");
        assert_eq!(manifest["target"], target());
        assert_eq!(manifest["private_material_included"], false);
        assert_eq!(manifest["live_calls_performed"], false);
        assert_eq!(manifest["downloads_performed"], 0);
        assert!(
            manifest["license_files_missing"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(!manifest["dependencies"].as_array().unwrap().is_empty());
        assert_eq!(manifest["files"].as_array().unwrap().len() + 1, names.len());
        for declared in manifest["files"].as_array().unwrap() {
            let file = extracted.join(declared["path"].as_str().unwrap());
            assert_eq!(declared["sha256"], hash(&fs::read(&file).unwrap()));
            assert_eq!(declared["bytes"], fs::metadata(&file).unwrap().len());
            assert_eq!(
                declared["mode"],
                fs::metadata(&file).unwrap().mode() & 0o7777
            );
        }
        assert_eq!(
            manifest["fixture_sha256"],
            hash(&fs::read(extracted.join(FIXTURE)).unwrap())
        );
        assert_eq!(
            fs::read(extracted.join(FIXTURE)).unwrap(),
            fs::read(root.join(FIXTURE)).unwrap()
        );
        let before = fs::read(&archive).unwrap();
        assert!(
            !run(Command::new(helper)
                .args(["benchmark-bundle", "--executable"])
                .arg(format!("{}={}", target(), helper.display()))
                .arg(&archive))
            .status
            .success()
        );
        assert_eq!(fs::read(&archive).unwrap(), before);
        // Refuse incomplete existing outputs, wrong/unsupported target labels
        // and a symlinked binary at the public constructor boundary.
        let checksum_only = scratch.0.join("checksum-only.tar.gz");
        fs::write(
            scratch.0.join("checksum-only.tar.gz.sha256"),
            b"synthetic-owned-checksum",
        )
        .unwrap();
        assert!(
            !run(Command::new(helper)
                .args(["benchmark-bundle", "--executable"])
                .arg(format!("{}={}", target(), helper.display()))
                .arg(&checksum_only))
            .status
            .success()
        );
        assert!(!checksum_only.exists());
        assert_eq!(
            fs::read(scratch.0.join("checksum-only.tar.gz.sha256")).unwrap(),
            b"synthetic-owned-checksum"
        );
        let wrong = if target().starts_with("aarch64") {
            "x86_64-apple-darwin"
        } else {
            "aarch64-apple-darwin"
        };
        for label in [wrong, "unsupported-private-target"] {
            let rejected = scratch.0.join(format!("rejected-{label}.tar.gz"));
            assert!(
                !run(Command::new(helper)
                    .args(["benchmark-bundle", "--executable"])
                    .arg(format!("{label}={}", helper.display()))
                    .arg(&rejected))
                .status
                .success()
            );
            assert!(!rejected.exists());
        }
        let linked = scratch.0.join("symlinked-helper");
        std::os::unix::fs::symlink(helper, &linked).unwrap();
        let rejected = scratch.0.join("rejected-link.tar.gz");
        assert!(
            !run(Command::new(helper)
                .args(["benchmark-bundle", "--executable"])
                .arg(format!("{}={}", target(), linked.display()))
                .arg(&rejected))
            .status
            .success()
        );
        assert!(!rejected.exists());
        Self {
            _scratch: scratch,
            extracted,
            unrelated,
            archive,
        }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(self.extracted.join(EXECUTABLE));
        command
            .env_clear()
            .env("PATH", "")
            .current_dir(&self.unrelated);
        command
    }
}

#[test]
fn required_executable_bundle_extracts_relocates_and_help_needs_no_installed_tools() {
    let artifact = Artifact::create();
    let help = successful(artifact.command().args([
        "evaluate-ollama",
        "--help",
        "--endpoint",
        "http://127.0.0.1:0",
    ]));
    let help = String::from_utf8(help).unwrap();
    assert!(help.starts_with("Usage: autorouter-benchmark evaluate-ollama"));
    assert!(help.contains("Downloads nothing and does not contact Claude/Jev"));
    assert!(help.contains("--timeout-ms 0 disables only the warm deadline"));
    assert!(
        successful(artifact.command().arg("--help"))
            .starts_with(b"AutoRouter executable evaluator bundle")
    );
    for args in [
        vec!["benchmark-storage", "--help"],
        vec!["release-check", "source", "v1.2.3"],
        vec!["__storage-child"],
        vec!["__benchmark-resource-child"],
        vec!["--env-file", "private.env", "evaluate-ollama", "--help"],
    ] {
        let result = run(artifact.command().args(args));
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("supports only evaluate-ollama"));
    }
    let fixture = artifact.extracted.join(FIXTURE);
    let receiver_alias = artifact.unrelated.join("fixture-alias");
    fs::hard_link(&fixture, &receiver_alias).unwrap();
    assert!(
        !run(artifact.command().args(["evaluate-ollama", "--help"]))
            .status
            .success()
    );
    fs::remove_file(receiver_alias).unwrap();
    let original = fs::read(&fixture).unwrap();
    fs::remove_file(&fixture).unwrap();
    assert!(
        !run(artifact.command().args(["evaluate-ollama", "--help"]))
            .status
            .success()
    );
    fs::write(&fixture, b"[]").unwrap();
    assert!(
        !run(artifact.command().args(["evaluate-ollama", "--help"]))
            .status
            .success()
    );
    fs::write(&fixture, &original).unwrap();
    fs::set_permissions(&fixture, fs::Permissions::from_mode(0o644)).unwrap();
    let manifest = artifact.extracted.join("executable-manifest.json");
    let original_manifest = fs::read(&manifest).unwrap();
    fs::remove_file(&manifest).unwrap();
    assert!(
        !run(artifact.command().args(["evaluate-ollama", "--help"]))
            .status
            .success()
    );
    fs::write(&manifest, &original_manifest).unwrap();
    fs::set_permissions(&manifest, fs::Permissions::from_mode(0o644)).unwrap();
    let executable = artifact.extracted.join(EXECUTABLE);
    let mut modified = fs::OpenOptions::new()
        .append(true)
        .open(&executable)
        .unwrap();
    std::io::Write::write_all(&mut modified, b"synthetic-appended-tamper").unwrap();
    drop(modified);
    assert!(
        !run(artifact.command().args(["evaluate-ollama", "--help"]))
            .status
            .success()
    );
    assert!(artifact.archive.is_file());
}

struct Mock {
    expected: BTreeMap<String, String>,
    calls: Mutex<Vec<String>>,
    resident: Mutex<bool>,
}
impl Mock {
    fn new(fixture: &[u8]) -> Self {
        let cases: Value = serde_json::from_slice(fixture).unwrap();
        let expected = cases
            .as_array()
            .unwrap()
            .iter()
            .filter(|case| case["split"] == "heldout")
            .map(|case| {
                let messages = case.get("messages").cloned().unwrap_or_else(|| {
                    let mut history = case["history"].as_array().cloned().unwrap_or_default();
                    history.push(json!({"role":"user","content":case["prompt"]}));
                    json!(history)
                });
                let body = JsDocument::parse(json!({"model":"claude-haiku-4-5-20251001","max_tokens":4096,"messages":messages}).to_string().as_bytes()).unwrap();
                let state = build_ollama_state_document(&body, 3000);
                let state: Value = serde_json::from_str(&state.stringify()).unwrap();
                (
                    state.to_string(),
                    case["expected"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        Self {
            expected,
            calls: Mutex::new(Vec::new()),
            resident: Mutex::new(false),
        }
    }
    async fn request(
        self: Arc<Self>,
        request: Request<Incoming>,
    ) -> Result<Response<Full<Bytes>>, Infallible> {
        assert!(request.headers().get("authorization").is_none());
        assert!(request.headers().get("x-api-key").is_none());
        let path = request.uri().path().to_owned();
        self.calls.lock().unwrap().push(path.clone());
        let body = request.into_body().collect().await.unwrap().to_bytes();
        assert!(body.len() <= 1024 * 1024);
        let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let value = match path.as_str() {
            "/api/ps" => {
                json!({"models": if *self.resident.lock().unwrap() { vec![json!({"name":"synthetic:1","size":1234,"size_vram":1234,"context_length":2048,"details":{"parameter_size":"1B","quantization_level":"Q4_K_M"}})] } else { vec![] }})
            }
            "/api/tags" => {
                json!({"models":[{"name":"synthetic:1","digest":"synthetic-digest","size":1234}]})
            }
            "/api/version" => json!({"version":"0.35.0"}),
            "/api/show" => json!({"details":{"parameter_size":"1B"}}),
            "/v1/systemone" => {
                assert_eq!(body["model"], "synthetic:1");
                *self.resident.lock().unwrap() = true;
                let tier = self
                    .expected
                    .get(&body["state"].to_string())
                    .expect("Mock received an unknown frozen task");
                json!({"model":"synthetic:1","answers":{"tier":{"type":"choice","choice":tier,"confidence":1,"probabilities":{"haiku":u8::from(tier=="haiku"),"sonnet":u8::from(tier=="sonnet"),"opus":u8::from(tier=="opus")}}},"usage":{"input_tokens":100,"output_tokens":1}})
            }
            "/api/generate" => {
                assert_eq!(
                    body,
                    json!({"model":"synthetic:1","stream":false,"keep_alive":0})
                );
                *self.resident.lock().unwrap() = false;
                json!({})
            }
            _ => panic!("Undeclared executable-bundle HTTP request: {path}"),
        };
        Ok(Response::new(Full::new(Bytes::from(value.to_string()))))
    }
}
struct Server {
    task: JoinHandle<()>,
    cancel: CancellationToken,
    url: String,
}
impl Server {
    async fn start(mock: Arc<Mock>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.unwrap();
                        let mock = mock.clone();
                        connections.spawn(async move {
                            hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service_fn(move |request| mock.clone().request(request))).await.unwrap();
                        });
                    }
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        Self { task, cancel, url }
    }
    async fn close(self) {
        self.cancel.cancel();
        self.task.await.unwrap();
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn required_executable_bundle_runs_frozen_heldout_evaluation_over_owned_loopback_only() {
    let artifact = Artifact::create();
    let fixture = fs::read(artifact.extracted.join(FIXTURE)).unwrap();
    let mock = Arc::new(Mock::new(&fixture));
    assert_eq!(mock.expected.len(), 30);
    let server = Server::start(mock.clone()).await;
    let report_path = artifact.unrelated.join("evaluation.json");
    let mut command = artifact.command();
    command
        .args([
            "evaluate-ollama",
            "--models",
            "synthetic:1",
            "--split",
            "heldout",
            "--rounds",
            "1",
            "--endpoint",
            &server.url,
            "--min-agreement",
            "1",
            "--max-under-route-rate",
            "0",
            "--output",
        ])
        .arg(&report_path);
    let result = tokio::task::spawn_blocking(move || run(&mut command))
        .await
        .unwrap();
    server.close().await;
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(report["fixture_sha256"], hash(&fixture));
    assert_eq!(report["complete"], true);
    assert_eq!(report["passed"], true);
    assert_eq!(report["models"][0]["rows"].as_array().unwrap().len(), 30);
    let tiers: BTreeSet<_> = report["models"][0]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["actual"].as_str().unwrap())
        .collect();
    assert_eq!(tiers, ["haiku", "sonnet", "opus"].into_iter().collect());
    let calls = mock.calls.lock().unwrap();
    assert_eq!(
        calls.iter().filter(|path| *path == "/v1/systemone").count(),
        31
    );
    assert_eq!(
        calls.iter().filter(|path| *path == "/api/generate").count(),
        1
    );
    assert!(
        !calls
            .iter()
            .any(|path| path.contains("pull") || path.contains("messages"))
    );
    assert!(!*mock.resident.lock().unwrap());
}
