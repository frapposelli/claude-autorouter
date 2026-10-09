//! Frozen pure release contracts and bounded synthetic registry/install schedules.
//! No command runner here executes npm, Git, a package binary or publication.
use crate::package::archive::Entry;
use crate::release::{self, Artifact, ReleaseError};
use crate::release_install::{self, CommandRunner, Installer, Invocation};
use crate::release_verify;
use autorouter_runtime::http_client::{HttpError, HttpTransport};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const CASES: &str = include_str!("../../parity/cases/release-contracts.jsonl");
const CAPTURE: &str = include_str!("../../parity/cases/release-contracts.capture.json");
const CASES_SHA: &str = "f938b7b0bb4819fe9e918bed6356d8afb72536da92297c18378222bf47699392";
const BYTES: &[u8] = b"synthetic canonical archive, already tested";

fn pure_outcome(row: &Value) -> Value {
    let args = row["input"].as_array().unwrap();
    let result: Result<Value, Value> = match row["op"].as_str().unwrap() {
        "release_metadata" => release::metadata(
            &args[0],
            args[1].as_str().unwrap(),
            args.get(2).unwrap_or(&json!({})),
        )
        .map_err(|message| json!({"message":message})),
        "parse_version" => release::parse_version(args[0].as_str().unwrap())
            .map(|v| json!({"numbers":v.numbers,"prerelease":v.prerelease}))
            .map_err(|_| json!({"rejected":true})),
        "compare_versions" => {
            release::compare_versions(args[0].as_str().unwrap(), args[1].as_str().unwrap())
                .map(|v| {
                    json!(match v {
                        std::cmp::Ordering::Less => -1,
                        std::cmp::Ordering::Equal => 0,
                        std::cmp::Ordering::Greater => 1,
                    })
                })
                .map_err(|_| json!({"rejected":true}))
        }
        "validate_artifact_source" => {
            let identity = &args[2];
            release_verify::validate_artifact_source(
                &args[0],
                &args[1],
                identity["runId"].as_str().unwrap(),
                identity["artifactId"].as_str().unwrap(),
                identity["commit"].as_str().unwrap(),
                identity["tag"].as_str().unwrap(),
            )
            .map_err(|_| json!({"rejected":true}))
        }
        op => panic!("unexpected pure operation {op}"),
    };
    match result {
        Ok(value) => json!({"ok":true,"result":value}),
        Err(error) => json!({"ok":false,"error":error}),
    }
}
fn asserted_outcome(row: &Value) -> Value {
    let expected = &row["node_expected"];
    if expected["ok"] == true {
        return expected.clone();
    }
    // Metadata source assertions use exact message-regex categories; the native
    // implementation also matches the complete captured message. The other
    // original assertions only demand rejection, not a JS Error class/string.
    json!({"ok":false,"error":if row["op"]=="release_metadata" {json!({"message":expected["error"]["message"]})}else{json!({"rejected":true})}})
}
#[test]
fn all_frozen_pure_release_arguments_preserve_results_and_asserted_errors() {
    assert_eq!(crate::evaluation::digest(CASES.as_bytes()), CASES_SHA);
    let capture: Value = serde_json::from_str(CAPTURE).unwrap();
    assert_eq!(capture["cases_sha256"], CASES_SHA);
    assert_eq!(capture["static_assertions"], 18);
    assert_eq!(capture["executed_assertions"], 66);
    let mut count = 0;
    for line in CASES.lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        let before = row["input"].clone();
        assert_eq!(pure_outcome(&row), asserted_outcome(&row), "{}", row["id"]);
        assert_eq!(row["input"], before);
        count += 1;
    }
    assert_eq!(count, 61);
}
#[test]
fn pure_comparison_rejects_wrong_order_wrong_identity_and_successful_invalid_input() {
    let rows: Vec<Value> = CASES
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    for op in ["compare_versions", "validate_artifact_source"] {
        let row = rows
            .iter()
            .find(|r| r["op"] == op && r["node_expected"]["ok"] == true)
            .unwrap();
        let mut wrong = asserted_outcome(row);
        wrong["result"] = json!("wrong");
        assert_ne!(pure_outcome(row), wrong);
    }
    let row = rows
        .iter()
        .find(|r| r["node_expected"]["ok"] == false)
        .unwrap();
    assert_ne!(pure_outcome(row), json!({"ok":true,"result":null}));
}

fn artifact() -> Artifact {
    Artifact {
        version: "0.4.0".into(),
        tag: "v0.4.0".into(),
        dist_tag: "latest".into(),
        archive: "synthetic-not-read".into(),
        filename: "claude-autorouter-0.4.0.tgz".into(),
        sha256: crate::evaluation::digest(BYTES),
        integrity: release::integrity(BYTES),
        bytes: BYTES.len(),
        native: true,
    }
}
fn metadata(target: &Artifact) -> Value {
    json!({"name":release::PACKAGE,"version":target.version,"dist":{"integrity":target.integrity,"tarball":format!("{}/{}/-/{}",release::REGISTRY,release::PACKAGE,target.filename)}})
}
#[derive(Default)]
struct Options {
    absent: bool,
    exact404: bool,
    status: Option<u16>,
    network_error: bool,
    latest: Option<String>,
    next: Option<String>,
    entry: Option<Value>,
    pack_entry: Option<Value>,
    wrong_bytes: bool,
    delayed: bool,
    stalled_metadata: bool,
}
struct Registry {
    options: Options,
    target: Artifact,
    reads: AtomicUsize,
    tarballs: AtomicUsize,
    calls: Mutex<Vec<(String, u64)>>,
    start: Instant,
}
impl Registry {
    fn new(options: Options) -> Self {
        Self::target(options, artifact())
    }
    fn target(options: Options, target: Artifact) -> Self {
        Self {
            options,
            target,
            reads: AtomicUsize::new(0),
            tarballs: AtomicUsize::new(0),
            calls: Mutex::new(Vec::new()),
            start: Instant::now(),
        }
    }
    fn attempts_at(&self) -> Vec<u64> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p == &format!("/{}/{}", release::PACKAGE, self.target.version))
            .map(|(_, at)| *at)
            .collect()
    }
}
impl HttpTransport for Registry {
    type ResponseBody = Full<Bytes>;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        assert_eq!(request.method(), "GET");
        assert_eq!(request.uri().scheme_str(), Some("https"));
        assert_eq!(
            request.uri().authority().unwrap().as_str(),
            "registry.npmjs.org"
        );
        assert!(
            request
                .uri()
                .query()
                .unwrap()
                .starts_with("autorouter_verify=")
        );
        assert!(
            request.headers()["cache-control"]
                .to_str()
                .unwrap()
                .contains("no-cache")
        );
        assert_eq!(request.headers()["pragma"], "no-cache");
        assert!(!request.headers().contains_key("authorization"));
        let path = request.uri().path().to_owned();
        assert!(
            request
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .is_empty()
        );
        self.calls
            .lock()
            .unwrap()
            .push((path.clone(), self.start.elapsed().as_millis() as u64));
        if self.options.network_error {
            return Err(HttpError::Network);
        }
        if self.options.stalled_metadata {
            std::future::pending::<()>().await;
        }
        let response = |status, bytes: Vec<u8>| {
            Ok(Response::builder()
                .status(status)
                .body(Full::new(Bytes::from(bytes)))
                .unwrap())
        };
        if let Some(status) = self.options.status {
            return response(status, Vec::new());
        }
        if path.contains("/-/") {
            let n = self.tarballs.fetch_add(1, Ordering::SeqCst);
            return if self.options.delayed && n == 0 {
                response(404, Vec::new())
            } else {
                response(
                    200,
                    if self.options.wrong_bytes {
                        b"different bytes".to_vec()
                    } else {
                        BYTES.to_vec()
                    },
                )
            };
        }
        let entry = self
            .options
            .entry
            .clone()
            .unwrap_or_else(|| metadata(&self.target));
        if path == format!("/{}/{}", release::PACKAGE, self.target.version) {
            let n = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
            return if self.options.absent
                || self.options.exact404
                || (self.options.delayed && n < 3)
            {
                response(404, Vec::new())
            } else {
                response(200, serde_json::to_vec(&entry).unwrap())
            };
        }
        assert_eq!(path, "/claude-autorouter");
        let exists = !self.options.absent
            && (!self.options.delayed || self.reads.load(Ordering::SeqCst) >= 3);
        let mut tags = json!({"latest":self.options.latest.as_deref().unwrap_or("0.4.0")});
        if let Some(next) = &self.options.next {
            tags["next"] = json!(next);
        }
        let mut versions = json!({});
        if exists {
            versions[&self.target.version] = self.options.pack_entry.clone().unwrap_or(entry);
        }
        response(
            200,
            serde_json::to_vec(
                &json!({"name":release::PACKAGE,"dist-tags":tags,"versions":versions}),
            )
            .unwrap(),
        )
    }
}
struct Install<'a> {
    calls: AtomicUsize,
    fail: bool,
    expected: &'a Artifact,
}
impl<'a> Install<'a> {
    fn new(fail: bool, expected: &'a Artifact) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            fail,
            expected,
        }
    }
}
impl Installer for Install<'_> {
    async fn install(
        &self,
        target: &Artifact,
        timeout: Duration,
        _: &CancellationToken,
    ) -> Result<Value, ReleaseError> {
        assert!(
            std::ptr::eq(target, self.expected),
            "Verifier must pass the exact admitted artifact reference"
        );
        assert_eq!(target.report_base(), self.expected.report_base());
        assert!(!timeout.is_zero());
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(release::error("command_failed", "installed_cli_failed"));
        }
        Ok(json!({"version":target.version,"isolated":true,"help":true}))
    }
}
async fn bounded(future: impl std::future::Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(700), future)
        .await
        .expect("synthetic release schedule exceeded independent deadline");
}
async fn verify(
    registry: &Registry,
    installer: &Install<'_>,
    budget: u64,
    states: &mut Vec<Value>,
) -> Value {
    release_verify::verify(
        registry,
        installer,
        &registry.target,
        budget,
        &CancellationToken::new(),
        &mut |row| {
            states.push(row.clone());
            Ok(())
        },
    )
    .await
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn preflight_original_absence_order_failure_retry_and_cached_metadata_cases() {
    bounded(async {
        let token = CancellationToken::new();
        let missing = Registry::new(Options {
            absent: true,
            latest: Some("0.3.7".into()),
            ..Default::default()
        });
        assert_eq!(
            release_verify::preflight(&missing, &missing.target, true, &token)
                .await
                .unwrap()["publish"],
            true
        );
        for latest in ["0.4.0", "0.5.0"] {
            let r = Registry::new(Options {
                absent: true,
                latest: Some(latest.into()),
                ..Default::default()
            });
            assert_eq!(
                release_verify::preflight(&r, &r.target, true, &token)
                    .await
                    .unwrap_err()
                    .code,
                "version_order"
            );
        }
        for status in [401, 429, 503] {
            let r = Registry::new(Options {
                status: Some(status),
                ..Default::default()
            });
            let e = release_verify::preflight(&r, &r.target, true, &token)
                .await
                .unwrap_err();
            assert_eq!(e.code, "registry_unavailable");
            assert_eq!(e.detail["http_status"], status);
        }
        let r = Registry::new(Options {
            network_error: true,
            ..Default::default()
        });
        let e = release_verify::preflight(&r, &r.target, true, &token)
            .await
            .unwrap_err();
        assert_eq!(e.code, "registry_unavailable");
        assert!(!e.to_string().contains("PRIVATE"));
        let cached = Registry::new(Options {
            exact404: true,
            latest: Some("0.5.0".into()),
            ..Default::default()
        });
        let result = release_verify::preflight(&cached, &cached.target, true, &token)
            .await
            .unwrap();
        for (k, v) in [
            ("publish", json!(false)),
            ("state", json!("submitted")),
            ("reason", json!("identical_version_exists")),
            ("metadata_source", json!("packument")),
            ("current_dist_tag", json!("0.5.0")),
        ] {
            assert_eq!(result[k], v);
        }
        assert_eq!(cached.calls.lock().unwrap().len(), 2);
        let retry = release_verify::preflight(&missing, &missing.target, false, &token)
            .await
            .unwrap();
        assert_eq!(retry["publish"], false);
        assert_eq!(retry["state"], "validating_unavailable");
        assert_eq!(retry["reason"], "retry_submission_unknown");
    })
    .await;
}
#[tokio::test(start_paused = true)]
async fn all_original_metadata_conflicts_reject_and_prerelease_targets_next() {
    bounded(async {
        let base = metadata(&artifact());
        let changes = [
            ("version", json!("9.0.0")),
            ("name", json!("other")),
            (
                "dist",
                json!({
                    "integrity": "sha512-not-the-canonical-archive",
                    "tarball": base["dist"]["tarball"],
                }),
            ),
            (
                "dist",
                json!({
                    "integrity": base["dist"]["integrity"],
                    "tarball": "https://private.example/archive.tgz",
                }),
            ),
        ];
        for (key, value) in changes {
            let mut changed = base.clone();
            changed[key] = value;
            let r = Registry::new(Options {
                entry: Some(changed),
                ..Default::default()
            });
            assert_eq!(
                release_verify::preflight(&r, &r.target, true, &CancellationToken::new())
                    .await
                    .unwrap_err()
                    .code,
                "release_mismatch",
            );
        }
        let mut conflict = base;
        conflict["dist"]["integrity"] = json!("sha512-conflicting");
        let r = Registry::new(Options {
            pack_entry: Some(conflict),
            ..Default::default()
        });
        assert_eq!(
            release_verify::preflight(&r, &r.target, true, &CancellationToken::new())
                .await
                .unwrap_err()
                .code,
            "release_mismatch",
        );
        let mut target = artifact();
        target.version = "0.4.0-beta.1".into();
        target.tag = "v0.4.0-beta.1".into();
        target.dist_tag = "next".into();
        let r = Registry::target(
            Options {
                absent: true,
                latest: Some("0.5.0".into()),
                next: Some("0.4.0-beta.0".into()),
                ..Default::default()
            },
            target,
        );
        let result = release_verify::preflight(&r, &r.target, true, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result["publish"], true);
        assert_eq!(result["dist_tag"], "next");
    })
    .await;
}
#[tokio::test(start_paused = true)]
async fn delayed_visibility_replays_all_waits_states_and_exact_single_install() {
    bounded(async {
        let r = Registry::new(Options {
            delayed: true,
            ..Default::default()
        });
        let i = Install::new(false, &r.target);
        let mut states = Vec::new();
        let result = verify(&r, &i, 15000, &mut states).await;
        assert_eq!(result["state"], "verified");
        assert_eq!(result["dist_tag_status"], "current");
        assert_eq!(i.calls.load(Ordering::SeqCst), 1);
        assert_eq!(r.attempts_at(), vec![0, 1000, 3000, 7000]);
        assert_eq!(
            r.attempts_at()
                .windows(2)
                .map(|v| v[1] - v[0])
                .collect::<Vec<_>>(),
            vec![1000, 2000, 4000]
        );
        assert_eq!(
            states
                .iter()
                .map(|v| v["state"].clone())
                .collect::<Vec<_>>(),
            vec![
                json!("validating_unavailable"),
                json!("validating_unavailable"),
                json!("validating_unavailable"),
                json!("verified")
            ]
        );
        assert_eq!(r.tarballs.load(Ordering::SeqCst), 2);
    })
    .await;
}
#[tokio::test(start_paused = true)]
async fn unavailable_timeout_preserves_reason_hash_http_status_and_no_install() {
    bounded(async {
        for (options, reason) in [
            (
                Options {
                    absent: true,
                    latest: Some("0.3.7".into()),
                    ..Default::default()
                },
                "version_not_available",
            ),
            (
                Options {
                    status: Some(503),
                    ..Default::default()
                },
                "http_error",
            ),
        ] {
            let r = Registry::new(options);
            let i = Install::new(false, &r.target);
            let result = verify(&r, &i, 5000, &mut Vec::new()).await;
            assert_eq!(result["state"], "validating_unavailable");
            assert_eq!(result["pending"], true);
            assert_eq!(result["reason"], reason);
            assert_eq!(result["sha256"], r.target.sha256);
            assert!(
                result["message"]
                    .as_str()
                    .unwrap()
                    .contains("do not submit a duplicate release")
            );
            if reason == "http_error" {
                assert_eq!(result["http_status"], 503);
            }
            assert_eq!(i.calls.load(Ordering::SeqCst), 0);
        }
    })
    .await;
}
#[tokio::test(start_paused = true)]
async fn tarball_mismatch_superseded_tag_older_tag_and_install_failure_stay_distinct() {
    bounded(async {
        let r = Registry::new(Options {
            wrong_bytes: true,
            ..Default::default()
        });
        let i = Install::new(false, &r.target);
        let failed = verify(&r, &i, 5000, &mut Vec::new()).await;
        assert_eq!(failed["state"], "failed");
        assert_eq!(failed["reason"], "tarball_bytes");
        assert_eq!(failed["attempts"], 1);
        assert_eq!(i.calls.load(Ordering::SeqCst), 0);
        let r = Registry::new(Options {
            latest: Some("0.5.0".into()),
            ..Default::default()
        });
        let result = verify(&r, &Install::new(false, &r.target), 5000, &mut Vec::new()).await;
        assert_eq!(result["state"], "verified");
        assert_eq!(result["current_dist_tag"], "0.5.0");
        assert_eq!(result["dist_tag_status"], "superseded_by_newer_version");
        assert_eq!(r.calls.lock().unwrap().len(), 3);
        let r = Registry::new(Options {
            latest: Some("0.3.7".into()),
            ..Default::default()
        });
        let i = Install::new(false, &r.target);
        let pending = verify(&r, &i, 2000, &mut Vec::new()).await;
        assert_eq!(pending["state"], "validating_unavailable");
        assert_eq!(pending["reason"], "dist_tag_not_updated");
        assert_eq!(i.calls.load(Ordering::SeqCst), 0);
        let r = Registry::new(Options::default());
        let failed = verify(&r, &Install::new(true, &r.target), 2000, &mut Vec::new()).await;
        assert_eq!(failed["state"], "failed");
        assert!(!failed.to_string().contains("PRIVATE"));
    })
    .await;
}
#[tokio::test(start_paused = true)]
async fn ten_minute_polling_preserves_budget_fifteen_second_backoff_and_32_events() {
    bounded(async {
        let r = Registry::new(Options {
            absent: true,
            ..Default::default()
        });
        let i = Install::new(false, &r.target);
        let start = Instant::now();
        let result = verify(&r, &i, 600000, &mut Vec::new()).await;
        assert_eq!(result["state"], "validating_unavailable");
        assert_eq!(result["elapsed_ms"], 600000);
        assert_eq!(start.elapsed(), Duration::from_secs(600));
        assert_eq!(result["events"].as_array().unwrap().len(), 32);
        let starts = r.attempts_at();
        let mut waits: Vec<_> = starts.windows(2).map(|v| v[1] - v[0]).collect();
        waits.push(600000 - starts.last().unwrap());
        assert_eq!(*waits.iter().max().unwrap(), 15000);
        assert_eq!(i.calls.load(Ordering::SeqCst), 0);
    })
    .await;
}

struct Commands {
    files: BTreeMap<String, Entry>,
    changed: bool,
    exhaust_budget: bool,
    calls: Mutex<Vec<Invocation>>,
}
impl CommandRunner for Commands {
    async fn run(
        &self,
        command: Invocation,
        _: Duration,
        _: &CancellationToken,
    ) -> Result<String, ReleaseError> {
        for key in ["TYPESAFE_API_KEY", "ANTHROPIC_API_KEY", "AUTOROUTER_CONFIG"] {
            assert!(!command.env.contains_key(std::ffi::OsStr::new(key)));
        }
        assert!(command.cwd.ends_with("unrelated cwd"));
        let output = if command.program == Path::new("npm") {
            let args = &command.args;
            let index = args.iter().position(|v| v == "--prefix").unwrap();
            let prefix = PathBuf::from(&args[index + 1]);
            let root = prefix.join("lib/node_modules/claude-autorouter");
            for (path, entry) in &self.files {
                std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
                std::fs::write(
                    root.join(path),
                    if self.changed {
                        b"// unreviewed content"
                    } else {
                        entry.bytes.as_slice()
                    },
                )
                .unwrap();
            }
            std::fs::create_dir_all(prefix.join("bin")).unwrap();
            std::os::unix::fs::symlink(
                root.join("bin/autorouter.mjs"),
                prefix.join("bin/claude-autorouter"),
            )
            .unwrap();
            if self.exhaust_budget {
                tokio::time::advance(Duration::from_millis(101)).await;
            }
            String::new()
        } else {
            assert!(
                !self.changed && !self.exhaust_budget,
                "Rejected/budget-exhausted installed code must never execute"
            );
            assert!(
                command
                    .program
                    .ends_with("install prefix/bin/claude-autorouter")
            );
            if command.args == ["--version"] {
                "0.4.0\n".into()
            } else {
                assert_eq!(command.args, ["--help"]);
                "claude-autorouter setup --help\n".into()
            }
        };
        self.calls.lock().unwrap().push(command);
        Ok(output)
    }
}
#[tokio::test(start_paused = true)]
async fn original_install_flags_environment_bytes_cleanup_and_remaining_budget() {
    bounded(async {
        for mode in ["valid", "changed", "exhausted"] {
            let mut files = BTreeMap::from([(
                "bin/autorouter.mjs".into(),
                Entry {
                    bytes: match mode {
                        "valid" => b"// synthetic CLI".to_vec(),
                        "changed" => b"// reviewed content".to_vec(),
                        _ => b"// canonical CLI".to_vec(),
                    },
                    mode: 0o755,
                },
            )]);
            if mode == "valid" {
                files.insert(
                    "package.json".into(),
                    Entry {
                        bytes: br#"{"name":"claude-autorouter"}"#.to_vec(),
                        mode: 0o644,
                    },
                );
            }
            let commands = Commands {
                files: files.clone(),
                changed: mode == "changed",
                exhaust_budget: mode == "exhausted",
                calls: Mutex::new(Vec::new()),
            };
            let mut target = artifact();
            target.native = false;
            let result = release_install::install_with(
                &target,
                &files,
                &commands,
                Duration::from_millis(100),
                &CancellationToken::new(),
            )
            .await;
            if mode == "valid" {
                assert_eq!(result.unwrap()["version"], "0.4.0");
            } else {
                let error = result.unwrap_err();
                assert_eq!(
                    error.code,
                    if mode == "changed" {
                        "release_mismatch"
                    } else {
                        "registry_unavailable"
                    }
                );
                assert_eq!(
                    error.detail["reason"],
                    if mode == "changed" {
                        "installed_archive_bytes"
                    } else {
                        "isolated_install_timeout"
                    }
                );
            }
            let calls = commands.calls.lock().unwrap();
            assert_eq!(calls.len(), if mode == "valid" { 3 } else { 1 });
            for flag in [
                "claude-autorouter@0.4.0",
                "--ignore-scripts",
                "--global",
                "--no-audit",
                "--no-fund",
            ] {
                assert!(calls[0].args.contains(&flag.into()));
            }
            if mode == "valid" {
                assert_eq!(
                    calls[1..]
                        .iter()
                        .map(|c| c.args.clone())
                        .collect::<Vec<_>>(),
                    vec![
                        vec![std::ffi::OsString::from("--version")],
                        vec![std::ffi::OsString::from("--help")]
                    ]
                );
            }
            assert_eq!(
                std::fs::metadata(&calls[0].cwd).unwrap_err().kind(),
                std::io::ErrorKind::NotFound
            );
        }
    })
    .await;
}
#[tokio::test(start_paused = true)]
async fn expired_metadata_deadline_never_starts_tarball_or_install() {
    bounded(async {
        let r = Registry::new(Options {
            stalled_metadata: true,
            ..Default::default()
        });
        let i = Install::new(false, &r.target);
        let result = verify(&r, &i, 1000, &mut Vec::new()).await;
        assert_eq!(r.calls.lock().unwrap().len(), 2);
        assert_eq!(r.tarballs.load(Ordering::SeqCst), 0);
        assert_eq!(i.calls.load(Ordering::SeqCst), 0);
        assert_eq!(result["state"], "validating_unavailable");
        assert_eq!(result["reason"], "timeout");
        // Frozen#16 independently jumps injected now() beyond still-live fetch
        // timers and expects verification_deadline. One native Tokio clock
        // cannot recreate that injection; this whole-definition mapping stays partial.
    })
    .await;
}
