//! Frozen release-check #4–16 and release-verification #14 obligations.
//! All approvals below are synthetic parser fixtures, never release evidence.
//! Production admission stays native-only; historical reader assertions are
//! deliberately distinguished from release-check/submitted acceptance.
use super::*;
use crate::process::{CapturedProcess, capture, capture_result};
use crate::release_install::{CommandRunner, Invocation, NativeCommand};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const CHILD: &str = "release_pack::tests::archive_contracts::isolated_release_entrypoint";
const ARCHIVE: &str = "claude-autorouter-0.4.0.tgz";

fn isolated(command: &mut Command, home: &Path) {
    let path = std::env::var_os("PATH").expect("contributor PATH");
    command
        .env_clear()
        .env("PATH", path)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_COUNT", "0")
        .env("LC_ALL", "C");
}
fn git(root: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    isolated(&mut command, root);
    command
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(root);
    String::from_utf8(capture(&mut command, b"", Duration::from_secs(10)).unwrap())
        .unwrap()
        .trim()
        .into()
}
fn write(root: &Path, name: &str, bytes: &[u8]) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}
fn init(root: &Path, tag: &str) {
    git(root, &["init", "--initial-branch=main"]);
    git(root, &["config", "user.email", "synthetic@example.invalid"]);
    git(root, &["config", "user.name", "Synthetic release fixture"]);
    git(root, &["config", "core.autocrlf", "false"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "synthetic initial source"]);
    git(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(root, &["tag", "-a", tag, "-m", "synthetic release"]);
}
fn source_fixture() -> Scratch {
    let dir = Scratch::new("release source with spaces").unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&Fixture::new().public["package.json"].bytes).unwrap();
    manifest["version"] = json!("1.2.3");
    write(&dir.0, "package.json", &json_bytes(&manifest));
    write(&dir.0, "README.md", b"Synthetic release source.\n");
    write(&dir.0, ".gitignore", b"dist/\n");
    init(&dir.0, "v1.2.3");
    dir
}
fn entry(root: &Path, mode: &str, args: &[&str], output: Option<&Path>) -> CapturedProcess {
    entry_environment(root, mode, args, output, false)
}
fn entry_environment(
    root: &Path,
    mode: &str,
    args: &[&str],
    output: Option<&Path>,
    github: bool,
) -> CapturedProcess {
    let mut command = Command::new(std::env::current_exe().unwrap());
    isolated(&mut command, root);
    command
        .args(["--exact", CHILD, "--nocapture"])
        .env("AUTOROUTER_RELEASE_CONTRACT_ROOT", root)
        .env("AUTOROUTER_RELEASE_CONTRACT_MODE", mode)
        .env(
            "AUTOROUTER_RELEASE_CONTRACT_ARGS",
            serde_json::to_string(args).unwrap(),
        );
    if let Some(path) = output {
        command.env("GITHUB_OUTPUT", path);
    }
    if github {
        command
            .env("GITHUB_ACTIONS", "true")
            .env("GITHUB_REPOSITORY", "frapposelli/claude-autorouter")
            .env("GITHUB_REF_TYPE", "tag")
            .env("GITHUB_REF", "refs/tags/v1.2.3");
    }
    capture_result(&mut command, b"", Duration::from_secs(20)).unwrap()
}
fn checked(root: &Path, args: &[&str]) -> String {
    let result = entry(root, "check", args, None);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}
fn rejected(root: &Path, args: &[&str], message: &str) {
    let result = entry(root, "check", args, None);
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(result.stderr).unwrap(),
        format!("{message}\n")
    );
}
/// Child isolates inherited Git/GitHub state without mutating parallel tests.
/// It calls the production command parser with an explicit fixture checkout;
/// the installed xtask compile-time checkout selection is not being tested.
#[test]
fn isolated_release_entrypoint() {
    let Some(root) = std::env::var_os("AUTOROUTER_RELEASE_CONTRACT_ROOT") else {
        return;
    };
    let args: Vec<String> =
        serde_json::from_str(&std::env::var("AUTOROUTER_RELEASE_CONTRACT_ARGS").unwrap()).unwrap();
    let result = match std::env::var("AUTOROUTER_RELEASE_CONTRACT_MODE")
        .unwrap()
        .as_str()
    {
        "check" => release::run(&args, Path::new(&root)),
        "verify" => crate::release_verify::run(&args, Path::new(&root)),
        _ => panic!("unknown owned fixture entrypoint"),
    };
    match result {
        Ok(passed) => std::process::exit(if passed { 0 } else { 1 }),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

#[test]
fn annotated_lightweight_ignored_dirty_and_untracked_source_contract() {
    let f = source_fixture();
    let expected = "Verified source release: {\"version\":\"1.2.3\",\"archive\":\"dist/claude-autorouter-1.2.3.tgz\",\"dist_tag\":\"latest\"}\n";
    let annotated = entry_environment(&f.0, "check", &["source", "v1.2.3"], None, true);
    assert_eq!(annotated.status.code(), Some(0));
    assert!(
        String::from_utf8(annotated.stdout)
            .unwrap()
            .ends_with(expected)
    );
    git(&f.0, &["tag", "-d", "v1.2.3"]);
    git(&f.0, &["tag", "v1.2.3"]);
    assert!(checked(&f.0, &["source", "v1.2.3"]).ends_with(expected));
    write(&f.0, "dist/ignored.tgz", b"ignored synthetic artifact");
    assert!(checked(&f.0, &["source", "v1.2.3"]).ends_with(expected));
    write(&f.0, "README.md", b"modified synthetic source\n");
    let message = "Release checkout must have no modified or untracked source files";
    rejected(&f.0, &["source", "v1.2.3"], message);
    git(&f.0, &["restore", "README.md"]);
    write(&f.0, "untracked.txt", b"synthetic untracked source\n");
    rejected(&f.0, &["source", "v1.2.3"], message);
}

#[test]
fn source_exact_head_main_ancestry_detached_ancestor_and_missing_ref_contract() {
    let f = source_fixture();
    let original = git(&f.0, &["rev-parse", "HEAD"]);
    write(&f.0, "README.md", b"synthetic second commit\n");
    git(&f.0, &["add", "README.md"]);
    git(&f.0, &["commit", "-m", "synthetic second source"]);
    rejected(
        &f.0,
        &["source", "v1.2.3"],
        "Release tag does not resolve to the checked-out HEAD commit",
    );
    git(&f.0, &["tag", "-f", "v1.2.3"]);
    let ancestry = "Release commit must be reachable from origin/main; fetch the full main branch";
    rejected(&f.0, &["source", "v1.2.3"], ancestry);
    git(&f.0, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let expected = "Verified source release: {\"version\":\"1.2.3\",\"archive\":\"dist/claude-autorouter-1.2.3.tgz\",\"dist_tag\":\"latest\"}\n";
    assert!(checked(&f.0, &["source", "v1.2.3"]).ends_with(expected));
    git(&f.0, &["tag", "-f", "v1.2.3", &original]);
    git(&f.0, &["checkout", "--detach", &original]);
    assert!(checked(&f.0, &["source", "v1.2.3"]).ends_with(expected));
    git(&f.0, &["update-ref", "-d", "refs/remotes/origin/main"]);
    rejected(&f.0, &["source", "v1.2.3"], ancestry);
}

#[test]
fn source_entrypoint_validates_before_exact_github_output_append() {
    let f = source_fixture();
    let output = Scratch::new("release github output").unwrap();
    let path = output.0.join("output");
    let result = entry_environment(&f.0, "check", &["source", "v1.2.3"], Some(&path), true);
    assert_eq!(result.status.code(), Some(0));
    assert!(
        String::from_utf8(result.stdout)
            .unwrap()
            .contains("Verified source release")
    );
    let expected = b"version=1.2.3\narchive=dist/claude-autorouter-1.2.3.tgz\ndist_tag=latest\n";
    assert_eq!(std::fs::read(&path).unwrap(), expected);
    let result = entry_environment(
        &f.0,
        "check",
        &["source", "v1.2.3\narchive=malicious"],
        Some(&path),
        true,
    );
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(result.stderr).unwrap(),
        "Release tag must contain strict SemVer without build metadata\n"
    );
    assert_eq!(std::fs::read(&path).unwrap(), expected);
}

fn authorize(f: &Fixture, output: &Path) {
    let candidate = authorization::candidate(output).unwrap();
    let evidence = material(
        &f.dir,
        "installed-contract-evidence.json",
        b"synthetic declarations only, not release qualification",
    );
    let rows: Vec<Value> = candidate
        .expected
        .values()
        .map(|expected| {
            let mut row = expected.clone();
            row["passed"] = json!(true);
            row["checks"] = json!({});
            for key in authorization::SHARED
                .iter()
                .chain(["offline_install_no_scripts", "upgrade_rollback"].iter())
            {
                row["checks"][key] = json!(true);
            }
            row["evidence"] = json!([evidence]);
            row
        })
        .collect();
    let report = json!({"schema_version":1,"kind":"native_release_final_qualification","passed":true,"complete":true,"source":f.source,"release_index_sha256":candidate.index_hash,"instances":rows});
    let report_desc = document(&f.dir, "contract-final-report.json", &report);
    let approval = json!({"schema_version":1,"kind":"native_release_final_approval","approved":true,"source":f.source,"release_index_sha256":candidate.index_hash,"report_sha256":report_desc["sha256"],"reviewer":"synthetic fixture","review_reference":"synthetic:contract"});
    let approval_desc = document(&f.dir, "contract-final-approval.json", &approval);
    let input = json!({"schema_version":1,"kind":"native_release_final_inputs","report":report_desc,"approval":approval_desc});
    let auth = authorization::authorize(&f.dir.0, &input, &candidate).unwrap();
    write(output, "release-authorization.json", &json_bytes(&auth));
    write(
        output,
        "release-authorization.json.sha256",
        format!(
            "{}  release-authorization.json\n",
            digest(&json_bytes(&auth))
        )
        .as_bytes(),
    );
}
fn native_source() -> (Scratch, Fixture) {
    let root = Scratch::new("release archive source with spaces").unwrap();
    let provisional = Fixture::new();
    for (path, item) in &provisional.public {
        write(&root.0, path, &item.bytes);
    }
    write(&root.0, ".gitignore", b"dist/\n");
    write(
        &root.0,
        "rust/Cargo.lock",
        b"# synthetic lock identity, not a build lock\n",
    );
    write(
        &root.0,
        "rust/distribution/platforms.json",
        &json_bytes(&provisional.baseline),
    );
    write(
        &root.0,
        "rust/distribution/autorouter.sh",
        include_bytes!("../../distribution/autorouter.sh"),
    );
    init(&root.0, "v0.4.0");
    let source = json!({"commit":git(&root.0,&["rev-parse","HEAD"]),"dirty":false,"cargo_lock_sha256":digest(&std::fs::read(root.0.join("rust/Cargo.lock")).unwrap()),"provenance":"ci-source-build"});
    let f = Fixture::with_source(source);
    restore_native(&root.0, &f);
    (root, f)
}
fn restore_native(root: &Path, f: &Fixture) {
    let output = root.join("dist");
    if output.exists() {
        std::fs::remove_dir_all(&output).unwrap();
    }
    write_output(&output, &f.assembled()).unwrap();
    authorize(f, &output);
}
fn replace_archive(root: &Path, files: &BTreeMap<String, Entry>) {
    let bytes = archive::encode_root("package", files, archive::NATIVE).unwrap();
    write(root, ARCHIVE, &bytes);
    write(
        root,
        &format!("{ARCHIVE}.sha256"),
        format!("{}  {ARCHIVE}\n", digest(&bytes)).as_bytes(),
    );
}

#[test]
fn native_archive_admission_rejects_checksum_filename_stale_private_and_symlink_mutations() {
    let (root, f) = native_source();
    checked(&root.0, &["archive", "v0.4.0"]);
    let output = root.0.join("dist");
    let path = output.join(ARCHIVE);
    let original = std::fs::read(&path).unwrap();
    let files = archive::decode(&original, archive::NATIVE).unwrap().files;
    for mutation in [
        "checksum",
        "checksum-path",
        "stale",
        "manifest",
        "runtime",
        "additional",
        "missing",
        "private",
        "symlink",
    ] {
        restore_native(&root.0, &f);
        assert!(
            release::inspect_artifact(&path, "v0.4.0", false)
                .unwrap()
                .native
        );
        let reason = match mutation {
            "checksum" => {
                write(
                    &output,
                    &format!("{ARCHIVE}.sha256"),
                    format!("{}  {ARCHIVE}\n", "0".repeat(64)).as_bytes(),
                );
                "checksum_mismatch"
            }
            "checksum-path" => {
                write(
                    &output,
                    &format!("{ARCHIVE}.sha256"),
                    format!("{}  ../{ARCHIVE}\n", digest(&original)).as_bytes(),
                );
                "checksum_filename_mismatch"
            }
            "stale" => {
                write(&output, "old.tgz", b"synthetic stale artifact");
                "unexpected_release_artifacts"
            }
            "symlink" => {
                write(&output, "other.tgz", &original);
                std::fs::remove_file(&path).unwrap();
                #[cfg(unix)]
                std::os::unix::fs::symlink("other.tgz", &path).unwrap();
                "archive_not_regular_or_bounded"
            }
            _ => {
                let mut changed = files.clone();
                match mutation {
                    "manifest" => {
                        let mut m: Value =
                            serde_json::from_slice(&changed["package.json"].bytes).unwrap();
                        m["description"] = json!("changed synthetic metadata");
                        changed.get_mut("package.json").unwrap().bytes = json_bytes(&m);
                    }
                    "runtime" => changed
                        .get_mut("native/aarch64-apple-darwin/claude-autorouter")
                        .unwrap()
                        .bytes
                        .push(0),
                    "additional" => {
                        changed.insert(
                            "docs/injected.md".into(),
                            Entry {
                                bytes: b"synthetic extra documentation".to_vec(),
                                mode: 0o644,
                            },
                        );
                    }
                    "missing" => {
                        changed.remove("LICENSE");
                    }
                    "private" => {
                        changed.insert(
                            ".env".into(),
                            Entry {
                                bytes: b"SYNTHETIC_SECRET=not-a-real-credential\n".to_vec(),
                                mode: 0o644,
                            },
                        );
                    }
                    _ => unreachable!(),
                }
                replace_archive(&output, &changed);
                if matches!(mutation, "additional" | "private") {
                    "unexpected_file"
                } else {
                    "native_release_not_qualified"
                }
            }
        };
        let error = release::inspect_artifact(&path, "v0.4.0", false)
            .err()
            .unwrap();
        assert_eq!(error.code, "invalid_archive", "{mutation}");
        assert_eq!(error.detail, json!({"reason":reason}), "{mutation}");
        let message = match mutation {
            "checksum" => "Release check failed: Release archive SHA256 does not match",
            "checksum-path" => {
                "Release check failed: SHA256 file must identify the exact release archive"
            }
            "symlink" => "Release check failed: Release artifact must be a bounded regular file",
            "additional" | "private" => {
                "Release check failed: Unexpected file in native production package"
            }
            "stale" => {
                "Release check failed: dist must contain only the expected native release archive set and authorization pair"
            }
            _ => "Release check failed: invalid_archive",
        };
        rejected(&root.0, &["archive", "v0.4.0"], message);
    }
    restore_native(&root.0, &f);
    checked(&root.0, &["archive", "v0.4.0"]);
}

#[test]
fn native_archive_source_identity_and_bytes_are_compared_after_qualification() {
    let (root, _f) = native_source();
    checked(&root.0, &["archive", "v0.4.0"]);
    let manifest_path = root.0.join("package.json");
    let original = std::fs::read(&manifest_path).unwrap();
    let mut changed: Value = serde_json::from_slice(&original).unwrap();
    changed["description"] = json!("changed checkout metadata");
    std::fs::write(&manifest_path, json_bytes(&changed)).unwrap();
    rejected(
        &root.0,
        &["archive", "v0.4.0"],
        "Archive package.json differs from the checkout",
    );
    std::fs::write(&manifest_path, &original).unwrap();
    write(&root.0, "README.md", b"changed synthetic documentation\n");
    rejected(
        &root.0,
        &["archive", "v0.4.0"],
        "Archive content differs from checkout: README.md",
    );
    git(&root.0, &["restore", "README.md"]);
    write(&root.0, "rust/Cargo.lock", b"changed synthetic lock\n");
    rejected(
        &root.0,
        &["archive", "v0.4.0"],
        "Release archive source identity differs from checkout",
    );
}

#[test]
fn submitted_report_preserves_checksum_failure_contract_and_native_admission_reason() {
    let (root, _f) = native_source();
    let path = root.0.join("dist").join(ARCHIVE);
    let report = root.0.join("dist/report.json");
    // Report/output live outside dist so they cannot become stale release artifacts.
    let diagnostics = Scratch::new("submitted synthetic diagnostics").unwrap();
    let report = diagnostics.0.join(report.file_name().unwrap());
    let output = diagnostics.0.join("github-output");
    let args = [
        "submitted",
        "v0.4.0",
        "--archive",
        path.to_str().unwrap(),
        "--report",
        report.to_str().unwrap(),
    ];
    let result = entry(&root.0, "verify", &args, Some(&output));
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    let inspected = release::inspect_artifact(&path, "v0.4.0", false).unwrap();
    assert_eq!(value["state"], "submitted");
    assert_eq!(value["sha256"], inspected.sha256);
    assert_eq!(std::fs::read(&output).unwrap(), b"state=submitted\n");
    std::fs::write(
        path.with_extension("tgz.sha256"),
        format!("{}  {ARCHIVE}\n", "0".repeat(64)),
    )
    .unwrap();
    let typed = release::inspect_artifact(&path, "v0.4.0", false)
        .err()
        .unwrap();
    assert_eq!(typed.code, "invalid_archive");
    assert_eq!(typed.detail["reason"], "checksum_mismatch");
    let result = entry(&root.0, "verify", &args, None);
    assert_eq!(result.status.code(), Some(1));
    let value: Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert_eq!(
        value,
        json!({"schema_version":1,"state":"failed","phase":"submitted","reason":"invalid_archive","error_code":"invalid_archive"})
    );
    // Other admission reasons remain distinguishable, never hidden by the report fix.
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(
        path.with_extension("tgz.sha256"),
        format!("{}  {ARCHIVE}\n", digest(&bytes)),
    )
    .unwrap();
    std::fs::remove_file(root.0.join("dist/release-authorization.json")).unwrap();
    let result = entry(&root.0, "verify", &args, None);
    assert_eq!(result.status.code(), Some(1));
    let value: Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert_eq!(value["reason"], "unexpected_release_artifacts");
    // A lone checksum is an invalid artifact set. With the whole pair absent,
    // the separate final-authorization requirement still retains its category.
    std::fs::remove_file(root.0.join("dist/release-authorization.json.sha256")).unwrap();
    let result = entry(&root.0, "verify", &args, None);
    assert_eq!(result.status.code(), Some(1));
    let value: Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert_eq!(value["reason"], "native_final_authorization_required");
}

struct OfflineInstall {
    archive: PathBuf,
    commands: Mutex<Vec<String>>,
}
impl CommandRunner for OfflineInstall {
    async fn run(
        &self,
        mut command: Invocation,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<String, release::ReleaseError> {
        self.commands
            .lock()
            .unwrap()
            .push(if command.program == Path::new("npm") {
                "npm".into()
            } else {
                command.args[0].to_string_lossy().into_owned()
            });
        if command.program == Path::new("npm") {
            assert_eq!(
                command.args.last().unwrap(),
                OsStr::new("claude-autorouter@0.4.0")
            );
            command.args.pop();
            command.args.push("--offline".into());
            command.args.push(self.archive.clone().into_os_string());
        }
        NativeCommand.run(command, timeout, cancel).await
    }
}

#[tokio::test]
async fn historical_canonical_npm_bytes_install_offline_but_cannot_be_submitted() {
    let f = Scratch::new("historical release contracts").unwrap();
    let root = f.0.join("source");
    std::fs::create_dir(&root).unwrap();
    let manifest = json!({"name":PACKAGE,"version":"0.4.0","license":"Apache-2.0","type":"module","repository":{"type":"git","url":format!("git+https://github.com/{}.git",release::REPOSITORY)},"publishConfig":{"access":"public","registry":format!("{}/",release::REGISTRY)},"bin":{PACKAGE:"bin/autorouter.mjs"},"files":["bin/*.mjs","src/*.mjs","docs/*.md","LICENSE"]});
    write(&root, "package.json", &json_bytes(&manifest));
    for path in [
        "README.md",
        "LICENSE",
        "bin/autorouter.mjs",
        "bin/statusline.mjs",
        "src/config.mjs",
        "src/router.mjs",
        "src/server.mjs",
        "docs/reference.md",
        "docs/development.md",
        "docs/releasing.md",
        "docs/ollama-evaluation.md",
    ] {
        write(
            &root,
            path,
            if path == "bin/autorouter.mjs" {
                b"#!/usr/bin/env node\nconsole.log(process.argv.includes(\"--version\") ? \"0.4.0\" : \"claude-autorouter setup help\");\n"
            } else if path.ends_with(".mjs") {
                b"export const synthetic = true;\n"
            } else {
                b"Synthetic release documentation.\n"
            },
        );
    }
    let dist = f.0.join("dist");
    std::fs::create_dir(&dist).unwrap();
    let env = crate::release_install::npm_environment(&f.0.join("npm")).unwrap();
    let mut command = Command::new("npm");
    command
        .env_clear()
        .envs(env)
        .current_dir(&root)
        .args([
            "pack",
            "--ignore-scripts",
            "--offline",
            "--json",
            "--pack-destination",
        ])
        .arg(&dist);
    let packed: Value =
        serde_json::from_slice(&capture(&mut command, b"", Duration::from_secs(30)).unwrap())
            .unwrap();
    let path = dist.join(ARCHIVE);
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(
        path.with_extension("tgz.sha256"),
        format!("{}  {ARCHIVE}\n", digest(&bytes)),
    )
    .unwrap();
    let artifact = release::inspect_artifact(&path, "v0.4.0", true).unwrap();
    assert!(!artifact.native);
    assert_eq!(artifact.sha256, digest(&bytes));
    // npm computes this independently of the native archive reader.
    assert!(
        packed[0]["integrity"]
            .as_str()
            .unwrap()
            .starts_with("sha512-")
    );
    assert_eq!(artifact.integrity, packed[0]["integrity"].as_str().unwrap());
    let files = archive::decode(&bytes, archive::HISTORICAL).unwrap().files;
    let runner = OfflineInstall {
        archive: path.clone(),
        commands: Mutex::new(Vec::new()),
    };
    let result = crate::release_install::install_with(
        &artifact,
        &files,
        &runner,
        Duration::from_secs(30),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result["version"], "0.4.0");
    assert_eq!(
        *runner.commands.lock().unwrap(),
        ["npm", "--version", "--help"]
    );
    let rejected = release::inspect_artifact(&path, "v0.4.0", false)
        .err()
        .unwrap();
    assert_eq!(rejected.code, "invalid_archive");
    assert_eq!(rejected.detail["reason"], "native_release_required");
    let report = f.0.join("report.json");
    let args = [
        "submitted",
        "v0.4.0",
        "--archive",
        path.to_str().unwrap(),
        "--report",
        report.to_str().unwrap(),
    ];
    assert_eq!(entry(&root, "verify", &args, None).status.code(), Some(1));
    let value: Value = serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
    assert_eq!(value["reason"], "native_release_required");
}

#[test]
fn checksum_sidecar_and_regular_file_failures_keep_safe_original_command_diagnostics() {
    let (root, f) = native_source();
    checked(&root.0, &["archive", "v0.4.0"]);
    let output = root.0.join("dist");
    let path = output.join(ARCHIVE);
    let checksum_path = path.with_extension("tgz.sha256");
    let original = std::fs::read(&path).unwrap();
    let diagnostics = Scratch::new("PRIVATE_CHECKSUM_DIAGNOSTIC").unwrap();
    let github = diagnostics.0.join("PRIVATE_OUTPUT");
    for (name, checksum, reason, message) in [
        (
            "hash",
            format!("{}  {ARCHIVE}\n", "0".repeat(64)),
            "checksum_mismatch",
            "Release check failed: Release archive SHA256 does not match",
        ),
        (
            "hash-and-filename",
            format!("{}  ../{ARCHIVE}\n", "0".repeat(64)),
            "checksum_filename_mismatch",
            "Release check failed: SHA256 file must identify the exact release archive",
        ),
        (
            "filename",
            format!("{}  ../PRIVATE_ARCHIVE.tgz\n", digest(&original)),
            "checksum_filename_mismatch",
            "Release check failed: SHA256 file must identify the exact release archive",
        ),
        (
            "trailing-data",
            format!("{}  {ARCHIVE}\r\n", digest(&original)),
            "checksum_filename_mismatch",
            "Release check failed: SHA256 file must identify the exact release archive",
        ),
    ] {
        std::fs::write(&checksum_path, checksum.as_bytes()).unwrap();
        std::fs::write(&github, b"unchanged\n").unwrap();
        let failure = release::inspect_artifact(&path, "v0.4.0", false)
            .err()
            .unwrap();
        assert_eq!(failure.code, "invalid_archive", "{name}");
        assert_eq!(failure.detail, json!({"reason":reason}), "{name}");
        let result = entry(&root.0, "check", &["archive", "v0.4.0"], Some(&github));
        assert_eq!(result.status.code(), Some(1), "{name}");
        assert_eq!(
            String::from_utf8(result.stderr).unwrap(),
            format!("{message}\n"),
            "{name}"
        );
        assert!(
            !String::from_utf8(result.stdout)
                .unwrap()
                .contains("Verified archive release")
        );
        assert_eq!(std::fs::read(&github).unwrap(), b"unchanged\n");
        assert_eq!(std::fs::read(&checksum_path).unwrap(), checksum.as_bytes());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    // Historical sidecar grammar allows an optional LF, but native final
    // authorization deliberately still requires its canonical LF-terminated bytes.
    restore_native(&root.0, &f);
    std::fs::write(&checksum_path, format!("{}  {ARCHIVE}", digest(&original))).unwrap();
    let failure = release::inspect_artifact(&path, "v0.4.0", false)
        .err()
        .unwrap();
    assert_eq!(
        failure.detail,
        json!({"reason":"native_final_authorization_required"})
    );
    rejected(
        &root.0,
        &["archive", "v0.4.0"],
        "Release check failed: invalid_archive",
    );
    restore_native(&root.0, &f);
    let target = diagnostics.0.join("PRIVATE_TARGET.tgz");
    std::fs::write(&target, &original).unwrap();
    std::fs::remove_file(&path).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &path).unwrap();
    let failure = release::inspect_artifact(&path, "v0.4.0", false)
        .err()
        .unwrap();
    assert_eq!(failure.code, "invalid_archive");
    assert_eq!(
        failure.detail,
        json!({"reason":"archive_not_regular_or_bounded"})
    );
    let result = entry(&root.0, "check", &["archive", "v0.4.0"], Some(&github));
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(result.stderr).unwrap(),
        "Release check failed: Release artifact must be a bounded regular file\n"
    );
    assert_eq!(std::fs::read(&github).unwrap(), b"unchanged\n");
    assert_eq!(std::fs::read(&target).unwrap(), original);
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    restore_native(&root.0, &f);
    checked(&root.0, &["archive", "v0.4.0"]);
}

fn policy_dist_bytes(directory: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            let bytes = release::regular(&entry.path(), archive::MAX_COMPRESSED as u64).unwrap();
            (name, bytes)
        })
        .collect()
}

fn policy_source_bytes(root: &Path, fixture: &Fixture) -> BTreeMap<String, Vec<u8>> {
    fixture
        .public
        .keys()
        .cloned()
        .chain(
            [
                ".gitignore",
                "rust/Cargo.lock",
                "rust/distribution/platforms.json",
                "rust/distribution/autorouter.sh",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .map(|name| {
            let bytes = release::regular(&root.join(&name), archive::MAX_FILE as u64).unwrap();
            (name, bytes)
        })
        .collect()
}

fn policy_positive_metadata(root: &Path) {
    let stdout = checked(root, &["archive", "v0.4.0"]);
    let marker = "Verified archive release: ";
    assert_eq!(stdout.matches(marker).count(), 1);
    let (_, metadata) = stdout.split_once(marker).unwrap();
    let metadata: Value = serde_json::from_str(metadata.trim_end()).unwrap();
    assert_eq!(
        metadata,
        json!({"version":"0.4.0","archive":"dist/claude-autorouter-0.4.0.tgz","dist_tag":"latest"})
    );
}

fn policy_command_observation(root: &Path, github: &Path, id: &str, origin: &str) -> String {
    let failure = release::inspect_artifact(&root.join("dist").join(ARCHIVE), "v0.4.0", false)
        .err()
        .unwrap();
    assert_eq!(failure.code, "invalid_archive");
    assert_eq!(failure.detail.as_object().unwrap().len(), 1);
    let reason = failure.detail["reason"].as_str().unwrap();
    assert!(!reason.is_empty() && reason.len() <= 64);
    assert!(reason.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
    let result = entry(root, "check", &["archive", "v0.4.0"], Some(github));
    assert_eq!(result.status.code(), Some(1));
    let stderr = String::from_utf8(result.stderr).unwrap();
    assert!(stderr.len() <= 512);
    for private in [
        root.to_str().unwrap(),
        "PRIVATE_",
        "SYNTHETIC_SENTINEL",
        "not-a-real-secret",
        ".env",
        "old.tgz",
    ] {
        assert!(!stderr.contains(private));
    }
    assert!(!String::from_utf8_lossy(&result.stdout).contains("Verified archive release"));
    // Observe the current typed reason without freezing the old broad category.
    // This is emitted before the desired command diagnostic assertion.
    eprintln!(
        "RELEASE_POLICY_OBSERVATION {}",
        json!({"id":id,"origin":origin,"typed_error":{"code":failure.code,"detail":failure.detail},"exit_code":result.status.code(),"stderr":stderr,"stdout_bytes":result.stdout.len(),"stdout_sha256":digest(&result.stdout)})
    );
    stderr
}

#[test]
fn original_file_rejections_private_file_keeps_safe_diagnostic() {
    let (root, fixture) = native_source();
    policy_positive_metadata(&root.0);
    let output = root.0.join("dist");
    let source_before = policy_source_bytes(&root.0, &fixture);
    let artifacts_before = policy_dist_bytes(&output);
    let diagnostics = Scratch::new("PRIVATE_POLICY_DIAGNOSTIC").unwrap();
    let github = diagnostics.0.join("PRIVATE_OUTPUT");
    std::fs::write(&github, b"unchanged\n").unwrap();
    let mut changed = archive::decode(&artifacts_before[ARCHIVE], archive::NATIVE)
        .unwrap()
        .files;
    changed.insert(
        ".env".into(),
        Entry {
            bytes: b"SYNTHETIC_SENTINEL=not-a-real-secret\n".to_vec(),
            mode: 0o644,
        },
    );
    let manifest: Value = serde_json::from_slice(&changed["package.json"].bytes).unwrap();
    let origin = release::native_files(&changed, &manifest).err().unwrap();
    assert_eq!(origin, "Unexpected file in native production package");
    replace_archive(&output, &changed);
    let mutated = policy_dist_bytes(&output);
    let actual = archive::decode(&mutated[ARCHIVE], archive::NATIVE).unwrap();
    assert_eq!(
        actual.files[".env"].bytes,
        b"SYNTHETIC_SENTINEL=not-a-real-secret\n"
    );
    assert_eq!(actual.files[".env"].mode, 0o644);
    let stderr = policy_command_observation(&root.0, &github, "private-file", &origin);
    assert_eq!(policy_dist_bytes(&output), mutated);
    assert_eq!(policy_source_bytes(&root.0, &fixture), source_before);
    assert_eq!(std::fs::read(&github).unwrap(), b"unchanged\n");
    restore_native(&root.0, &fixture);
    policy_positive_metadata(&root.0);
    assert_eq!(policy_dist_bytes(&output), artifacts_before);
    assert_eq!(policy_source_bytes(&root.0, &fixture), source_before);
    assert_eq!(std::fs::read(&github).unwrap(), b"unchanged\n");
    assert_eq!(
        stderr,
        "Release check failed: Unexpected file in native production package\n"
    );
}

#[test]
fn original_file_rejections_stale_archive_keeps_safe_diagnostic() {
    let (root, fixture) = native_source();
    policy_positive_metadata(&root.0);
    let output = root.0.join("dist");
    let source_before = policy_source_bytes(&root.0, &fixture);
    let artifacts_before = policy_dist_bytes(&output);
    let diagnostics = Scratch::new("PRIVATE_POLICY_DIAGNOSTIC").unwrap();
    let github = diagnostics.0.join("PRIVATE_OUTPUT");
    std::fs::write(&github, b"unchanged\n").unwrap();
    write(&output, "old.tgz", b"old");
    let origin = authorization::candidate(&output).err().unwrap();
    assert_eq!(
        origin,
        "Release directory must contain only the exact immutable archive set and authorization pair"
    );
    let mutated = policy_dist_bytes(&output);
    assert_eq!(mutated["old.tgz"], b"old");
    let stderr = policy_command_observation(&root.0, &github, "stale-archive", &origin);
    assert_eq!(policy_dist_bytes(&output), mutated);
    assert_eq!(policy_source_bytes(&root.0, &fixture), source_before);
    assert_eq!(std::fs::read(&github).unwrap(), b"unchanged\n");
    std::fs::remove_file(output.join("old.tgz")).unwrap();
    policy_positive_metadata(&root.0);
    assert_eq!(policy_dist_bytes(&output), artifacts_before);
    assert_eq!(policy_source_bytes(&root.0, &fixture), source_before);
    assert_eq!(std::fs::read(&github).unwrap(), b"unchanged\n");
    assert_eq!(
        stderr,
        "Release check failed: dist must contain only the expected native release archive set and authorization pair\n"
    );
}

// These controls isolate independent native integrity layers. All binary
// headers and approvals are synthetic parser fixtures, never qualification.
fn layered_file_declaration(files: &mut BTreeMap<String, Entry>, path: &str, inserted: bool) {
    let actual = files.get(path).unwrap();
    let declared = json!({"path":path,"sha256":digest(&actual.bytes),"bytes":actual.bytes.len(),"mode":actual.mode});
    let mut build = json_read(&files["build-manifest.json"].bytes).unwrap();
    let rows = build["files"].as_array_mut().unwrap();
    let matches = rows.iter().filter(|row| row["path"] == path).count();
    assert_eq!(matches, usize::from(!inserted));
    if inserted {
        rows.push(declared);
    } else {
        *rows.iter_mut().find(|row| row["path"] == path).unwrap() = declared;
    }
    files.get_mut("build-manifest.json").unwrap().bytes = json_bytes(&build);
}

#[test]
fn layered_runtime_tamper_recomputes_file_hash_without_reauthorizing_artifact_or_index() {
    let (root, fixture) = native_source();
    policy_positive_metadata(&root.0);
    let output = root.0.join("dist");
    let original = policy_dist_bytes(&output);
    let source = policy_source_bytes(&root.0, &fixture);
    let diagnostics = Scratch::new("PRIVATE_LAYERED_RUNTIME").unwrap();
    let github = diagnostics.0.join("PRIVATE_OUTPUT");
    std::fs::write(&github, b"unchanged\n").unwrap();
    let mut files = archive::decode(&original[ARCHIVE], archive::NATIVE)
        .unwrap()
        .files;
    let manifest = json_read(&files["package.json"].bytes).unwrap();
    let before = release::native_files(&files, &manifest).unwrap();
    let target = "aarch64-apple-darwin";
    let binary = "native/aarch64-apple-darwin/claude-autorouter";
    let inspection = crate::package::binary::inspect(target, &files[binary].bytes).unwrap();
    let old_binary_hash = digest(&files[binary].bytes);
    files.get_mut(binary).unwrap().bytes.push(0);
    assert_eq!(files[binary].mode, 0o755);
    assert_ne!(digest(&files[binary].bytes), old_binary_hash);
    assert_eq!(
        crate::package::binary::inspect(target, &files[binary].bytes).unwrap(),
        inspection
    );
    layered_file_declaration(&mut files, binary, false);
    let after = json_read(&files["build-manifest.json"].bytes).unwrap();
    for key in ["artifacts", "source", "qualification", "approval"] {
        assert_eq!(
            after[key], before[key],
            "Untrusted file row must not rewrite {key}"
        );
    }
    let cause = release::native_files(&files, &manifest).err().unwrap();
    assert_eq!(cause, "Native artifact bytes, target or provenance differ");
    replace_archive(&output, &files);
    assert_eq!(
        policy_command_observation(&root.0, &github, "runtime-file-redeclared", &cause),
        "Release check failed: invalid_archive\n"
    );
    let public_failure = release::inspect_artifact(&output.join(ARCHIVE), "v0.4.0", false)
        .err()
        .unwrap();
    assert_eq!(
        public_failure.detail,
        json!({"reason":"native_release_not_qualified"})
    );

    // Even a forged internal artifact hash cannot alter the already-reviewed
    // outer candidate index. No index, approval or authorization is regenerated.
    let mut forged = after;
    let artifacts = forged["artifacts"].as_array_mut().unwrap();
    assert_eq!(artifacts.len(), 1);
    artifacts[0]["sha256"] = json!(digest(&files[binary].bytes));
    files.get_mut("build-manifest.json").unwrap().bytes = json_bytes(&forged);
    assert!(
        release::native_files(&files, &manifest).is_ok(),
        "Reach the independent immutable-index guard"
    );
    replace_archive(&output, &files);
    let index_cause = authorization::candidate(&output).err().unwrap();
    assert_eq!(index_cause, "Candidate archive changed after qualification");
    assert_eq!(
        policy_command_observation(
            &root.0,
            &github,
            "runtime-artifact-forged-index-preserved",
            &index_cause
        ),
        "Release check failed: invalid_archive\n"
    );
    let final_failure = release::inspect_artifact(&output.join(ARCHIVE), "v0.4.0", false)
        .err()
        .unwrap();
    assert_eq!(
        final_failure.detail,
        json!({"reason":"native_final_authorization_required"})
    );
    let mutated = policy_dist_bytes(&output);
    assert_eq!(
        mutated.keys().collect::<Vec<_>>(),
        original.keys().collect::<Vec<_>>()
    );
    for (name, bytes) in &original {
        if name != ARCHIVE && name != &format!("{ARCHIVE}.sha256") {
            assert_eq!(
                mutated[name], *bytes,
                "Immutable reviewed set member {name}"
            );
        }
    }
    assert_eq!(policy_source_bytes(&root.0, &fixture), source);
    assert_eq!(std::fs::read(&github).unwrap(), b"unchanged\n");
    restore_native(&root.0, &fixture);
    policy_positive_metadata(&root.0);
    assert_eq!(policy_dist_bytes(&output), original);
    assert_eq!(policy_source_bytes(&root.0, &fixture), source);
}

#[test]
fn layered_exact_plan_rejects_valid_known_native_target_outside_selected_matrix() {
    let (root, fixture) = native_source();
    policy_positive_metadata(&root.0);
    let output = root.0.join("dist");
    let original = policy_dist_bytes(&output);
    let source = policy_source_bytes(&root.0, &fixture);
    let diagnostics = Scratch::new("PRIVATE_LAYERED_TARGET").unwrap();
    let github = diagnostics.0.join("PRIVATE_OUTPUT");
    std::fs::write(&github, b"unchanged\n").unwrap();
    let mut files = archive::decode(&original[ARCHIVE], archive::NATIVE)
        .unwrap()
        .files;
    let manifest = json_read(&files["package.json"].bytes).unwrap();
    let target = "x86_64-apple-darwin";
    let path = "native/x86_64-apple-darwin/claude-autorouter";
    let project_matrix = json_read(include_bytes!("../../distribution/platforms.json")).unwrap();
    assert!(
        project_matrix["targets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["target"] == target)
    );
    let selected = json_read(&files["platforms.json"].bytes).unwrap();
    assert!(
        !selected["targets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["target"] == target)
    );
    let mut executable = vec![0u8; 32];
    executable[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
    executable[4..8].copy_from_slice(&0x01000007u32.to_le_bytes());
    executable[12..16].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(
        crate::package::binary::inspect(target, &executable).unwrap()["target"],
        target
    );
    assert!(
        files
            .insert(
                path.into(),
                Entry {
                    bytes: executable,
                    mode: 0o755
                }
            )
            .is_none()
    );
    layered_file_declaration(&mut files, path, true);
    let cause = release::native_files(&files, &manifest).err().unwrap();
    assert_eq!(cause, "Unexpected file in native production package");
    replace_archive(&output, &files);
    assert_eq!(
        policy_command_observation(&root.0, &github, "known-unselected-native-target", &cause),
        "Release check failed: Unexpected file in native production package\n"
    );
    let mutated = policy_dist_bytes(&output);
    assert_eq!(
        mutated.keys().collect::<Vec<_>>(),
        original.keys().collect::<Vec<_>>()
    );
    for (name, bytes) in &original {
        if name != ARCHIVE && name != &format!("{ARCHIVE}.sha256") {
            assert_eq!(
                mutated[name], *bytes,
                "Immutable reviewed set member {name}"
            );
        }
    }
    assert_eq!(policy_source_bytes(&root.0, &fixture), source);
    assert_eq!(std::fs::read(&github).unwrap(), b"unchanged\n");
    restore_native(&root.0, &fixture);
    policy_positive_metadata(&root.0);
    assert_eq!(policy_dist_bytes(&output), original);
}

#[test]
fn layered_qualified_changed_archive_manifest_is_compared_to_unchanged_checkout() {
    let (root, original_fixture) = native_source();
    policy_positive_metadata(&root.0);
    let original = policy_dist_bytes(&root.0.join("dist"));
    let source = policy_source_bytes(&root.0, &original_fixture);
    let diagnostics = Scratch::new("PRIVATE_LAYERED_MANIFEST").unwrap();
    let github = diagnostics.0.join("PRIVATE_OUTPUT");
    std::fs::write(&github, b"unchanged\n").unwrap();
    let mut changed = Fixture::with_source(original_fixture.source.clone());
    let mut manifest = json_read(&changed.public["package.json"].bytes).unwrap();
    manifest["description"] = json!("Changed synthetic archive metadata");
    changed.public.get_mut("package.json").unwrap().bytes = json_bytes(&manifest);
    changed.public_sha = digest(&json_bytes(&declarations(&changed.public)));
    let descriptor_path = changed.inputs["approval"]["path"].as_str().unwrap();
    let mut approval =
        json_read(&std::fs::read(changed.dir.0.join(descriptor_path)).unwrap()).unwrap();
    approval["public_files_sha256"] = Value::String(changed.public_sha.clone());
    changed.inputs["approval"] = document(&changed.dir, "changed-public-approval.json", &approval);
    assert!(
        changed.qualified().is_ok(),
        "Synthetic admission fixture must be coherent before archive/check comparison"
    );
    restore_native(&root.0, &changed);
    assert!(
        release::inspect_artifact(&root.0.join("dist").join(ARCHIVE), "v0.4.0", false)
            .unwrap()
            .native
    );
    let altered = policy_dist_bytes(&root.0.join("dist"));
    let result = entry(&root.0, "check", &["archive", "v0.4.0"], Some(&github));
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(result.stderr).unwrap(),
        "Archive package.json differs from the checkout\n"
    );
    assert!(!String::from_utf8_lossy(&result.stdout).contains("Verified archive release"));
    assert_eq!(policy_dist_bytes(&root.0.join("dist")), altered);
    assert_eq!(policy_source_bytes(&root.0, &original_fixture), source);
    assert_eq!(std::fs::read(&github).unwrap(), b"unchanged\n");
    restore_native(&root.0, &original_fixture);
    policy_positive_metadata(&root.0);
    assert_eq!(policy_dist_bytes(&root.0.join("dist")), original);
    assert_eq!(policy_source_bytes(&root.0, &original_fixture), source);
}
