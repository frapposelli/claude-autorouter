//! The installed xtask entrypoint must check the invoking checkout, not the
//! source tree embedded by Cargo when the tool was compiled.
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        for _ in 0..8 {
            let mut nonce = [0_u8; 16];
            getrandom::fill(&mut nonce).unwrap();
            let suffix = nonce
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let path = std::env::temp_dir().join(format!("autorouter-release-cli-{suffix}"));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("Cannot create private release fixture: {error}"),
            }
        }
        panic!("Cannot create unique release fixture")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn isolated(command: &mut Command, home: &Path) {
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C");
}

fn git(root: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    isolated(&mut command, root);
    let output = command
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn check(cwd: &Path, home: &Path, output_path: &Path, tag: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    isolated(&mut command, home);
    command
        .current_dir(cwd)
        .args(["release-check", "source", tag])
        .env("GITHUB_ACTIONS", "true")
        .env("GITHUB_REPOSITORY", "frapposelli/claude-autorouter")
        .env("GITHUB_REF_TYPE", "tag")
        .env("GITHUB_REF", "refs/tags/v1.2.3")
        .env("GITHUB_OUTPUT", output_path)
        .output()
        .unwrap()
}

#[test]
fn built_xtask_checks_the_invoking_checkout_and_only_appends_validated_outputs() {
    let scratch = Scratch::new();
    let parent_project = scratch.0.join("parent npm project with spaces");
    fs::create_dir(&parent_project).unwrap();
    fs::write(
        parent_project.join("package.json"),
        br#"{"name":"wrong-parent-project","version":"9.9.9"}"#,
    )
    .unwrap();
    // This root is itself named rust and has a Cargo.toml. Without preferring
    // its own package.json, checkout_root would incorrectly select the parent.
    let checkout = parent_project.join("rust");
    fs::create_dir(&checkout).unwrap();
    fs::create_dir(checkout.join("rust")).unwrap();
    fs::write(
        checkout.join("package.json"),
        br#"{"name":"claude-autorouter","version":"1.2.3","repository":{"url":"git+https://github.com/frapposelli/claude-autorouter.git"},"publishConfig":{"access":"public","registry":"https://registry.npmjs.org/"}}"#,
    )
    .unwrap();
    fs::write(checkout.join("Cargo.toml"), b"[workspace]\n").unwrap();
    fs::write(checkout.join("rust/Cargo.toml"), b"[workspace]\n").unwrap();
    fs::write(checkout.join("README.md"), b"Synthetic release fixture.\n").unwrap();
    git(&checkout, &["init", "--initial-branch=main"]);
    git(&checkout, &["config", "user.name", "Release Fixture"]);
    git(
        &checkout,
        &["config", "user.email", "release@example.invalid"],
    );
    git(&checkout, &["add", "."]);
    git(&checkout, &["commit", "-m", "Synthetic release"]);
    git(
        &checkout,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    git(&checkout, &["tag", "-a", "v1.2.3", "-m", "Synthetic tag"]);

    let output_path = scratch.0.join("github output");
    let expected = b"version=1.2.3\narchive=dist/claude-autorouter-1.2.3.tgz\ndist_tag=latest\n";
    for (index, cwd) in [&checkout, &checkout.join("rust")].into_iter().enumerate() {
        let result = check(cwd, &scratch.0, &output_path, "v1.2.3");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("Verified source release:"));
        assert_eq!(fs::read(&output_path).unwrap(), expected.repeat(index + 1));
    }

    let malicious = check(
        &checkout,
        &scratch.0,
        &output_path,
        "v1.2.3\narchive=malicious",
    );
    assert!(!malicious.status.success());
    assert!(String::from_utf8_lossy(&malicious.stderr).contains("strict SemVer"));
    assert_eq!(fs::read(&output_path).unwrap(), expected.repeat(2));
}

#[path = "../src/release_native_fixture.rs"]
mod shared_release_native_fixture;

fn write_owned(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

/// File-backed output has no pipe readers or descendant EOF dependency.
/// These admitted public modes do not spawn children; this owns the direct xtask.
fn captured_xtask(
    home: &Path,
    cwd: &Path,
    label: &str,
    args: &[&str],
    github: Option<&Path>,
) -> Output {
    use std::process::{Child, Stdio};
    use std::time::{Duration, Instant};
    struct Direct(Option<Child>);
    impl Drop for Direct {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let out = home.join(format!("{label}.stdout"));
    let err = home.join(format!("{label}.stderr"));
    write_owned(&out, b"");
    write_owned(&err, b"");
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    isolated(&mut command, home);
    command
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(fs::OpenOptions::new().write(true).open(&out).unwrap())
        .stderr(fs::OpenOptions::new().write(true).open(&err).unwrap());
    if let Some(path) = github {
        command.env("GITHUB_OUTPUT", path);
    }
    let mut child = Direct(Some(command.spawn().unwrap()));
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.0.as_mut().unwrap().try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "Owned {label} xtask exceeded20s");
        assert!(
            fs::metadata(&out).unwrap().len() <= 256 * 1024,
            "Owned stdout exceeded observation cap"
        );
        assert!(
            fs::metadata(&err).unwrap().len() <= 32 * 1024,
            "Owned stderr exceeded observation cap"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let _ = child.0.take();
    assert!(fs::metadata(&out).unwrap().len() <= 256 * 1024);
    assert!(fs::metadata(&err).unwrap().len() <= 32 * 1024);
    Output {
        status,
        stdout: fs::read(out).unwrap(),
        stderr: fs::read(err).unwrap(),
    }
}

fn retained_release(
    directory: &Path,
) -> std::collections::BTreeMap<String, (Vec<u8>, u32, u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = fs::symlink_metadata(entry.path()).unwrap();
            assert!(metadata.is_file() && !metadata.file_type().is_symlink());
            let name = entry.file_name().into_string().unwrap();
            (
                name,
                (
                    fs::read(entry.path()).unwrap(),
                    metadata.mode(),
                    metadata.dev(),
                    metadata.ino(),
                ),
            )
        })
        .collect()
}

#[test]
fn built_xtask_submitted_preserves_complete_native_identity_and_report_write_order() {
    use serde_json::{Value, json};
    use shared_release_native_fixture::{MaterialFixture, digest, integrity, json_bytes};
    let scratch = Scratch::new();
    let home = scratch.0.canonicalize().unwrap();
    let unrelated = home.join("unrelated cwd with spaces");
    fs::create_dir(&unrelated).unwrap();
    write_owned(
        &unrelated.join("package.json"),
        br#"{"name":"unrelated-parent","version":"9.9.9"}"#,
    );
    let output = home.join("complete synthetic release");
    fs::create_dir(&output).unwrap();
    let inputs = home.join("synthetic final inputs");
    fs::create_dir(&inputs).unwrap();
    let diagnostics = home.join("external diagnostics");
    fs::create_dir(&diagnostics).unwrap();
    let fixture = MaterialFixture::new(
        json!({"commit":"a".repeat(40),"dirty":false,"cargo_lock_sha256":"b".repeat(64),"provenance":"ci-source-build"}),
    );
    assert_eq!(fixture.baseline["targets"].as_array().unwrap().len(), 1);
    assert_eq!(fixture.inputs["kind"], "native_release_inputs");
    assert_eq!(
        fixture.public_sha,
        digest(&json_bytes(&shared_release_native_fixture::declarations(
            &fixture.public
        )))
    );
    let archives = fixture.assembled();
    assert_eq!(archives.len(), 6);
    for (name, bytes) in &archives {
        write_owned(&output.join(name), bytes);
    }
    let final_materials = fixture.final_materials(&archives);
    for (name, bytes) in &final_materials {
        write_owned(&inputs.join(name), bytes);
    }
    let final_input = inputs.join("final-inputs.json");
    // This branch validates an already assembled candidate; it does not select
    // public source or call release-pack's build-checkout assembly path.
    let authorized = captured_xtask(
        &home,
        &unrelated,
        "authorize",
        &[
            "release-pack",
            "authorize",
            output.to_str().unwrap(),
            "--inputs",
            final_input.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(
        authorized.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&authorized.stderr)
    );
    assert!(authorized.stderr.is_empty());
    let authorization = fixture.expected_authorization(&archives, &final_materials);
    let mut authorization_stdout = json_bytes(&authorization);
    authorization_stdout.push(b'\n');
    assert_eq!(authorized.stdout, authorization_stdout);
    assert_eq!(
        fs::read(output.join("release-authorization.json")).unwrap(),
        json_bytes(&authorization)
    );
    let baseline = retained_release(&output);
    assert_eq!(baseline.len(), 8);
    let input_baseline = retained_release(&inputs);
    let archive_name = "claude-autorouter-0.4.0.tgz";
    let archive = output.join(archive_name);
    let checksum = output.join(format!("{archive_name}.sha256"));
    let report = diagnostics.join("report.json");
    let github = diagnostics.join("github output");
    write_owned(&github, b"existing=retained\n");
    let args = [
        "release-verify",
        "submitted",
        "v0.4.0",
        "--archive",
        archive.to_str().unwrap(),
        "--report",
        report.to_str().unwrap(),
    ];
    let submitted = captured_xtask(&home, &unrelated, "submitted", &args, Some(&github));
    assert_eq!(
        submitted.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&submitted.stderr)
    );
    assert!(submitted.stderr.is_empty());
    let bytes = fs::read(&report).unwrap();
    let actual: Value = serde_json::from_slice(&bytes).unwrap();
    let timestamp = actual["timestamp"].as_str().unwrap();
    assert!(
        regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$")
            .unwrap()
            .is_match(timestamp)
    );
    // Only the current UTC timestamp is observed. Every other value and the
    // complete ten-key shape are independently fixed by the retained fixture.
    let expected = json!({"schema_version":1,"package":"claude-autorouter","version":"0.4.0","tag":"v0.4.0","dist_tag":"latest","archive":archive_name,"sha256":digest(&archives[archive_name]),"integrity":integrity(&archives[archive_name]),"state":"submitted","timestamp":timestamp});
    assert_eq!(actual, expected);
    assert_eq!(bytes, json_bytes(&expected));
    assert_eq!(submitted.stdout, bytes);
    assert_eq!(
        fs::read(&github).unwrap(),
        b"existing=retained\nstate=submitted\n"
    );
    assert_eq!(retained_release(&output), baseline);
    assert_eq!(retained_release(&inputs), input_baseline);
    // Exact filename/LF remain valid. Only the digest changes.
    let corrupt = format!("{}  {archive_name}\n", "0".repeat(64)).into_bytes();
    fs::write(&checksum, &corrupt).unwrap();
    let rejected = captured_xtask(&home, &unrelated, "corrupt", &args, Some(&github));
    assert_eq!(rejected.status.code(), Some(1));
    assert!(rejected.stderr.is_empty());
    let failed = json!({"schema_version":1,"state":"failed","phase":"submitted","reason":"invalid_archive","error_code":"invalid_archive"});
    assert_eq!(fs::read(&report).unwrap(), json_bytes(&failed));
    assert_eq!(rejected.stdout, json_bytes(&failed));
    assert_eq!(
        fs::read(&github).unwrap(),
        b"existing=retained\nstate=submitted\nstate=failed\n"
    );
    let mut changed = baseline.clone();
    changed
        .get_mut(&format!("{archive_name}.sha256"))
        .unwrap()
        .0 = corrupt;
    assert_eq!(retained_release(&output), changed);
    // A report-write error must happen before GITHUB_OUTPUT append.
    let unwritable_args = [
        "release-verify",
        "submitted",
        "v0.4.0",
        "--archive",
        archive.to_str().unwrap(),
        "--report",
        diagnostics.to_str().unwrap(),
    ];
    let write_error = captured_xtask(
        &home,
        &unrelated,
        "write-error",
        &unwritable_args,
        Some(&github),
    );
    assert_eq!(write_error.status.code(), Some(1));
    assert!(write_error.stdout.is_empty());
    assert_eq!(write_error.stderr, b"Release check failed: report_failed\n");
    assert_eq!(
        fs::read(&github).unwrap(),
        b"existing=retained\nstate=submitted\nstate=failed\n"
    );
    assert_eq!(fs::read(&report).unwrap(), json_bytes(&failed));
    assert_eq!(retained_release(&output), changed);
    assert_eq!(retained_release(&inputs), input_baseline);
    assert_eq!(fs::read(archive).unwrap(), archives[archive_name]);
    assert_eq!(
        fs::read(unrelated.join("package.json")).unwrap(),
        br#"{"name":"unrelated-parent","version":"9.9.9"}"#
    );
    eprintln!(
        "RELEASE_VERIFY_BUILT_MAIN={}",
        json!({"synthetic_fixture_only":true,"native_archive_sha256":digest(&archives[archive_name]),"native_archive_bytes":archives[archive_name].len(),"release_files":baseline.len(),"authorize":{"exit_code":authorized.status.code(),"authorization":serde_json::from_slice::<Value>(&authorized.stdout).unwrap()},"submitted":{"exit_code":submitted.status.code(),"report":actual},"corrupt":{"exit_code":rejected.status.code(),"report":serde_json::from_slice::<Value>(&rejected.stdout).unwrap()},"report_write_error":{"exit_code":write_error.status.code(),"stdout_bytes":write_error.stdout.len(),"stderr":String::from_utf8(write_error.stderr).unwrap()},"github_output":String::from_utf8(fs::read(&github).unwrap()).unwrap(),"all_inputs_and_archive_set_preserved_except_declared_checksum":true})
    );
}

// One real Cargo-built native identity across inspect, offline install and main.
// All approvals/materials are synthetic parser fixtures, not release evidence.
fn runnable_fixture_target_for(
    os: &str,
    arch: &str,
    environment: &str,
    abi: &str,
    endian: &str,
    pointer_width: u8,
) -> Option<&'static str> {
    match (os, arch, environment, abi, endian, pointer_width) {
        ("macos", "aarch64", "", "", "little", 64) => Some("aarch64-apple-darwin"),
        ("macos", "x86_64", "", "", "little", 64) => Some("x86_64-apple-darwin"),
        ("linux", "aarch64", "gnu", "", "little", 64) => Some("aarch64-unknown-linux-gnu"),
        ("linux", "x86_64", "gnu", "", "little", 64) => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64", "musl", "", "little", 64) => Some("aarch64-unknown-linux-musl"),
        ("linux", "x86_64", "musl", "", "little", 64) => Some("x86_64-unknown-linux-musl"),
        ("linux", "x86", "gnu", "", "little", 32) => Some("i686-unknown-linux-gnu"),
        ("linux", "arm", "gnu", "eabihf", "little", 32) => Some("armv7-unknown-linux-gnueabihf"),
        ("linux", "powerpc64", "gnu", "elfv2", "little", 64) => {
            Some("powerpc64le-unknown-linux-gnu")
        }
        ("linux", "s390x", "gnu", "", "big", 64) => Some("s390x-unknown-linux-gnu"),
        ("linux", "loongarch64", "gnu", "", "little", 64) => Some("loongarch64-unknown-linux-gnu"),
        ("linux", "riscv64", "gnu", "", "little", 64) => Some("riscv64gc-unknown-linux-gnu"),
        _ => None,
    }
}

fn runnable_fixture_target() -> &'static str {
    let environment = if cfg!(target_env = "gnu") {
        "gnu"
    } else if cfg!(target_env = "musl") {
        "musl"
    } else if cfg!(target_env = "") {
        ""
    } else {
        "unsupported environment"
    };
    let abi = if cfg!(target_abi = "") {
        ""
    } else if cfg!(target_abi = "eabihf") {
        "eabihf"
    } else if cfg!(target_abi = "elfv2") {
        "elfv2"
    } else {
        "unsupported ABI"
    };
    let endian = if cfg!(target_endian = "little") {
        "little"
    } else if cfg!(target_endian = "big") {
        "big"
    } else {
        "unsupported endian"
    };
    let width = if cfg!(target_pointer_width = "32") {
        32
    } else if cfg!(target_pointer_width = "64") {
        64
    } else {
        0
    };
    runnable_fixture_target_for(
        std::env::consts::OS,
        std::env::consts::ARCH,
        environment,
        abi,
        endian,
        width,
    )
    .expect("Runnable release fixture requires an existing catalog target")
}

fn assert_runnable_fixture_target_catalog() {
    struct Case {
        os: &'static str,
        arch: &'static str,
        environment: &'static str,
        abi: &'static str,
        endian: &'static str,
        width: u8,
        target: &'static str,
    }
    // Literal compiler cfg observations are checked against the entire product
    // target catalog. This is mapping evidence, never hardware qualification.
    let cases = [
        Case {
            os: "macos",
            arch: "aarch64",
            environment: "",
            abi: "",
            endian: "little",
            width: 64,
            target: "aarch64-apple-darwin",
        },
        Case {
            os: "macos",
            arch: "x86_64",
            environment: "",
            abi: "",
            endian: "little",
            width: 64,
            target: "x86_64-apple-darwin",
        },
        Case {
            os: "linux",
            arch: "aarch64",
            environment: "gnu",
            abi: "",
            endian: "little",
            width: 64,
            target: "aarch64-unknown-linux-gnu",
        },
        Case {
            os: "linux",
            arch: "x86_64",
            environment: "gnu",
            abi: "",
            endian: "little",
            width: 64,
            target: "x86_64-unknown-linux-gnu",
        },
        Case {
            os: "linux",
            arch: "aarch64",
            environment: "musl",
            abi: "",
            endian: "little",
            width: 64,
            target: "aarch64-unknown-linux-musl",
        },
        Case {
            os: "linux",
            arch: "x86_64",
            environment: "musl",
            abi: "",
            endian: "little",
            width: 64,
            target: "x86_64-unknown-linux-musl",
        },
        Case {
            os: "linux",
            arch: "x86",
            environment: "gnu",
            abi: "",
            endian: "little",
            width: 32,
            target: "i686-unknown-linux-gnu",
        },
        Case {
            os: "linux",
            arch: "arm",
            environment: "gnu",
            abi: "eabihf",
            endian: "little",
            width: 32,
            target: "armv7-unknown-linux-gnueabihf",
        },
        Case {
            os: "linux",
            arch: "powerpc64",
            environment: "gnu",
            abi: "elfv2",
            endian: "little",
            width: 64,
            target: "powerpc64le-unknown-linux-gnu",
        },
        Case {
            os: "linux",
            arch: "s390x",
            environment: "gnu",
            abi: "",
            endian: "big",
            width: 64,
            target: "s390x-unknown-linux-gnu",
        },
        Case {
            os: "linux",
            arch: "loongarch64",
            environment: "gnu",
            abi: "",
            endian: "little",
            width: 64,
            target: "loongarch64-unknown-linux-gnu",
        },
        Case {
            os: "linux",
            arch: "riscv64",
            environment: "gnu",
            abi: "",
            endian: "little",
            width: 64,
            target: "riscv64gc-unknown-linux-gnu",
        },
    ];
    let mut mapped = std::collections::BTreeSet::new();
    for row in cases {
        assert_eq!(
            runnable_fixture_target_for(
                row.os,
                row.arch,
                row.environment,
                row.abi,
                row.endian,
                row.width
            ),
            Some(row.target)
        );
        assert!(mapped.insert(row.target));
        assert_eq!(
            runnable_fixture_target_for(
                row.os,
                row.arch,
                "future environment",
                row.abi,
                row.endian,
                row.width
            ),
            None
        );
        assert_eq!(
            runnable_fixture_target_for(
                row.os,
                row.arch,
                row.environment,
                "future ABI",
                row.endian,
                row.width
            ),
            None
        );
        assert_eq!(
            runnable_fixture_target_for(
                row.os,
                row.arch,
                row.environment,
                row.abi,
                if row.endian == "little" {
                    "big"
                } else {
                    "little"
                },
                row.width
            ),
            None
        );
        assert_eq!(
            runnable_fixture_target_for(
                row.os,
                row.arch,
                row.environment,
                row.abi,
                row.endian,
                if row.width == 32 { 64 } else { 32 }
            ),
            None
        );
    }
    let catalog: serde_json::Value =
        serde_json::from_str(include_str!("../../distribution/platforms.json")).unwrap();
    let catalog_rows = catalog["targets"].as_array().unwrap();
    let declared = catalog_rows
        .iter()
        .map(|row| row["target"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(mapped.len(), 12);
    assert_eq!(catalog_rows.len(), 12);
    assert_eq!(mapped, declared);
}

fn installed_native_facts(root: &Path) -> Vec<serde_json::Value> {
    use shared_release_native_fixture::digest;
    use std::os::unix::fs::MetadataExt;
    let mut pending = vec![PathBuf::new()];
    let mut facts = Vec::new();
    while let Some(relative) = pending.pop() {
        for item in fs::read_dir(root.join(&relative)).unwrap() {
            let item = item.unwrap();
            let path = relative.join(item.file_name());
            let metadata = fs::symlink_metadata(root.join(&path)).unwrap();
            assert!(!metadata.file_type().is_symlink());
            if metadata.is_dir() {
                pending.push(path);
            } else {
                assert!(metadata.is_file() && metadata.len() <= 32 * 1024 * 1024);
                assert!(facts.len() < 256);
                let bytes = fs::read(root.join(&path)).unwrap();
                facts.push(serde_json::json!({"path":path.to_str().unwrap(),"bytes":bytes.len(),"sha256":digest(&bytes),"mode":metadata.mode() & 0o777}));
            }
        }
    }
    facts.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    facts
}

struct OfflineNativeInstall {
    artifact: xtask::release_fixture_api::Artifact,
    home: PathBuf,
    target: &'static str,
    binary_sha: String,
    calls: std::sync::Mutex<Vec<serde_json::Value>>,
    owned: std::sync::Mutex<Option<(PathBuf, PathBuf)>>,
    environment: std::sync::Mutex<Option<autorouter_core::auth::Environment>>,
    installed: std::sync::Mutex<Vec<serde_json::Value>>,
}
impl xtask::release_fixture_api::CommandRunner for OfflineNativeInstall {
    async fn run(
        &self,
        mut invocation: xtask::release_fixture_api::Invocation,
        timeout: std::time::Duration,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<String, xtask::release_fixture_api::ReleaseError> {
        use shared_release_native_fixture::digest;
        use std::ffi::OsString;
        use std::os::unix::fs::MetadataExt;
        use xtask::release_fixture_api::NativeCommand;
        assert!(!cancel.is_cancelled() && !timeout.is_zero());
        assert!(timeout <= std::time::Duration::from_secs(30));
        // This is the declared Source-style local archive adapter, not a
        // registry substitute. Every other production option remains exact.
        invocation
            .env
            .insert("HOME".into(), self.home.clone().into_os_string());
        let allowed = [
            "PATH",
            "HOME",
            "USERPROFILE",
            "SystemRoot",
            "WINDIR",
            "TMPDIR",
            "TMP",
            "TEMP",
            "LANG",
            "LC_ALL",
            "npm_config_cache",
            "npm_config_userconfig",
            "npm_config_globalconfig",
            "npm_config_update_notifier",
            "npm_config_audit",
            "npm_config_fund",
            "npm_config_ignore_scripts",
        ];
        assert!(
            invocation
                .env
                .keys()
                .all(|key| allowed.iter().any(|name| key == std::ffi::OsStr::new(name)))
        );
        for (name, value) in [
            ("npm_config_update_notifier", "false"),
            ("npm_config_audit", "false"),
            ("npm_config_fund", "false"),
            ("npm_config_ignore_scripts", "true"),
        ] {
            assert_eq!(
                invocation.env.get(std::ffi::OsStr::new(name)),
                Some(&OsString::from(value))
            );
        }
        let index = { self.calls.lock().unwrap().len() };
        let original_args = invocation.args.clone();
        if index == 0 {
            assert_eq!(invocation.program, Path::new("npm"));
            assert_eq!(invocation.args.len(), 11);
            let prefix = PathBuf::from(&invocation.args[3]);
            let scratch = prefix.parent().unwrap();
            assert!(prefix.is_absolute() && !prefix.exists());
            assert_eq!(prefix.file_name().unwrap(), "install prefix");
            assert_eq!(invocation.cwd, scratch.join("unrelated cwd"));
            assert_eq!(invocation.cwd.canonicalize().unwrap(), invocation.cwd);
            let metadata = fs::symlink_metadata(scratch).unwrap();
            assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
            assert_eq!(metadata.mode() & 0o777, 0o700);
            assert_eq!(metadata.uid(), fs::metadata(&self.home).unwrap().uid());
            let expected: Vec<OsString> = ["install", "--global", "--prefix"]
                .into_iter()
                .map(Into::into)
                .chain([prefix.clone().into_os_string()])
                .chain(
                    [
                        "--ignore-scripts",
                        "--no-audit",
                        "--no-fund",
                        "--prefer-online",
                        "--registry",
                        "https://registry.npmjs.org/",
                        "claude-autorouter@0.4.0",
                    ]
                    .into_iter()
                    .map(Into::into),
                )
                .collect();
            assert_eq!(invocation.args, expected);
            for (key, path) in [
                ("npm_config_cache", scratch.join("npm/cache")),
                ("npm_config_userconfig", scratch.join("npm/user.npmrc")),
                ("npm_config_globalconfig", scratch.join("npm/global.npmrc")),
            ] {
                assert_eq!(
                    invocation.env.get(std::ffi::OsStr::new(key)),
                    Some(&path.into_os_string())
                );
            }
            for name in ["user.npmrc", "global.npmrc"] {
                let path = scratch.join("npm").join(name);
                let metadata = fs::symlink_metadata(&path).unwrap();
                assert!(metadata.is_file() && !metadata.file_type().is_symlink());
                assert_eq!(metadata.mode() & 0o777, 0o600);
                assert!(fs::read(path).unwrap().is_empty());
            }
            assert!(self.artifact.archive.is_absolute());
            assert_eq!(
                digest(&fs::read(&self.artifact.archive).unwrap()),
                self.artifact.sha256
            );
            invocation.args.pop();
            invocation.args.push("--offline".into());
            invocation
                .args
                .push(self.artifact.archive.clone().into_os_string());
            *self.owned.lock().unwrap() = Some((prefix, invocation.cwd.clone()));
            *self.environment.lock().unwrap() = Some(invocation.env.clone());
        } else {
            assert!(index <= 2, "Unexpected install command");
            let (prefix, cwd) = { self.owned.lock().unwrap().clone().unwrap() };
            assert_eq!(invocation.program, prefix.join("bin/claude-autorouter"));
            assert_eq!(invocation.cwd, cwd);
            assert_eq!(
                invocation.args,
                [OsString::from(if index == 1 {
                    "--version"
                } else {
                    "--help"
                })]
            );
            assert_eq!(invocation.env, {
                self.environment.lock().unwrap().clone().unwrap()
            });
            let root = prefix.join("lib/node_modules/claude-autorouter");
            assert_eq!(
                invocation.program.canonicalize().unwrap(),
                root.join("bin/autorouter").canonicalize().unwrap()
            );
            // Actual production byte/tree comparison before either executable
            // runs; this wrapper repeats its admission, never manufactures it.
            xtask::release_fixture_api::compare_installed_archive(&self.artifact, &root)?;
            let facts = installed_native_facts(&root);
            let binary_path = format!("native/{}/claude-autorouter", self.target);
            assert_eq!(
                facts.iter().find(|row| row["path"] == binary_path).unwrap()["sha256"],
                self.binary_sha
            );
            for executable in ["bin/autorouter", binary_path.as_str()] {
                let metadata = fs::symlink_metadata(root.join(executable)).unwrap();
                assert!(metadata.is_file() && !metadata.file_type().is_symlink());
                assert_ne!(metadata.mode() & 0o100, 0, "Installed owner execute bit");
            }
            if index == 1 {
                *self.installed.lock().unwrap() = facts;
            } else {
                assert_eq!(facts, { self.installed.lock().unwrap().clone() });
            }
        }
        let report = serde_json::json!({"phase":index,"program":invocation.program.to_str().unwrap(),"original_args":original_args.iter().map(|x|x.to_str().unwrap()).collect::<Vec<_>>(),"executed_args":invocation.args.iter().map(|x|x.to_str().unwrap()).collect::<Vec<_>>(),"cwd":invocation.cwd.to_str().unwrap(),"env_keys":invocation.env.keys().map(|x|x.to_str().unwrap()).collect::<Vec<_>>()});
        {
            self.calls.lock().unwrap().push(report);
        }
        let stdout = NativeCommand.run(invocation, timeout, cancel).await?;
        if index == 1 {
            assert_eq!(stdout, "0.4.0\n");
        }
        if index == 2 {
            assert_eq!(
                stdout,
                "claude-autorouter setup: synthetic native install fixture\n"
            );
        }
        {
            self.calls.lock().unwrap()[index]["stdout"] = serde_json::json!(stdout);
        }
        Ok(stdout)
    }
}

#[test]
fn one_runnable_native_archive_inspects_installs_and_reports_through_built_main() {
    use serde_json::{Value, json};
    use shared_release_native_fixture::{MaterialFixture, digest, integrity, json_bytes};
    use std::os::unix::fs::MetadataExt;
    use xtask::release_fixture_api as api;
    assert_runnable_fixture_target_catalog();
    let scratch = Scratch::new();
    let home = scratch.0.canonicalize().unwrap();
    let cwd = home.join("unrelated cwd");
    fs::create_dir(&cwd).unwrap();
    write_owned(
        &cwd.join("package.json"),
        br#"{"name":"unrelated","version":"9.9.9"}"#,
    );
    let directory = home.join("release");
    let inputs = home.join("final inputs");
    let diagnostics = home.join("reports");
    for path in [&directory, &inputs, &diagnostics] {
        fs::create_dir(path).unwrap();
    }
    let fixture_path = Path::new(env!("CARGO_BIN_EXE_autorouter-release-fixture"));
    let before = fs::symlink_metadata(fixture_path).unwrap();
    assert!(before.is_file() && !before.file_type().is_symlink());
    assert!(before.len() > 0 && before.len() <= 16 * 1024 * 1024);
    let binary = fs::read(fixture_path).unwrap();
    let binary_sha = digest(&binary);
    let target = runnable_fixture_target();
    let inspection = api::binary_inspection(target, &binary).unwrap();
    let fixture = MaterialFixture::with_binary(
        json!({"commit":"a".repeat(40),"dirty":false,"cargo_lock_sha256":"b".repeat(64),"provenance":"ci-source-build"}),
        target,
        binary,
        inspection,
    );
    let archives = fixture.assembled();
    for (name, bytes) in &archives {
        write_owned(&directory.join(name), bytes);
    }
    let final_materials = fixture.final_materials(&archives);
    for (name, bytes) in &final_materials {
        write_owned(&inputs.join(name), bytes);
    }
    let authorized = captured_xtask(
        &home,
        &cwd,
        "runnable-authorize",
        &[
            "release-pack",
            "authorize",
            directory.to_str().unwrap(),
            "--inputs",
            inputs.join("final-inputs.json").to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(
        authorized.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&authorized.stderr)
    );
    assert!(authorized.stderr.is_empty());
    let expected_authorization = fixture.expected_authorization(&archives, &final_materials);
    let mut authorization_stdout = json_bytes(&expected_authorization);
    authorization_stdout.push(b'\n');
    assert_eq!(authorized.stdout, authorization_stdout);
    assert_eq!(
        fs::read(directory.join("release-authorization.json")).unwrap(),
        json_bytes(&expected_authorization)
    );
    let archive_name = "claude-autorouter-0.4.0.tgz";
    let archive = directory.join(archive_name);
    let artifact = api::inspect_artifact(&archive, "v0.4.0", false).unwrap();
    assert!(artifact.native);
    assert_eq!(artifact.sha256, digest(&archives[archive_name]));
    assert_eq!(artifact.integrity, integrity(&archives[archive_name]));
    assert_eq!(artifact.bytes, archives[archive_name].len());
    let baseline = retained_release(&directory);
    let inputs_before = retained_release(&inputs);
    let runner = OfflineNativeInstall {
        artifact: artifact.clone(),
        home: home.clone(),
        target,
        binary_sha: binary_sha.clone(),
        calls: Default::default(),
        owned: Default::default(),
        environment: Default::default(),
        installed: Default::default(),
    };
    let cancel = tokio_util::sync::CancellationToken::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let installed = runtime
        .block_on(api::install_with_runner(
            &artifact,
            &runner,
            std::time::Duration::from_secs(30),
            &cancel,
        ))
        .unwrap();
    assert_eq!(
        installed,
        json!({"version":"0.4.0","help":true,"isolated":true,"registry":"https://registry.npmjs.org"})
    );
    assert!(!cancel.is_cancelled());
    drop(runtime);
    let calls = { runner.calls.lock().unwrap().clone() };
    assert_eq!(calls.len(), 3);
    let (prefix, _) = { runner.owned.lock().unwrap().clone().unwrap() };
    assert!(
        !prefix.parent().unwrap().exists(),
        "Production install scratch removed before return"
    );
    assert_eq!(retained_release(&directory), baseline);
    assert_eq!(retained_release(&inputs), inputs_before);
    let report = diagnostics.join("report.json");
    let github = diagnostics.join("github output");
    write_owned(&github, b"existing=retained\n");
    let args = [
        "release-verify",
        "submitted",
        "v0.4.0",
        "--archive",
        archive.to_str().unwrap(),
        "--report",
        report.to_str().unwrap(),
    ];
    let submitted = captured_xtask(&home, &cwd, "runnable-submitted", &args, Some(&github));
    assert_eq!(submitted.status.code(), Some(0));
    assert!(submitted.stderr.is_empty());
    let report_bytes = fs::read(&report).unwrap();
    let actual: Value = serde_json::from_slice(&report_bytes).unwrap();
    let timestamp = actual["timestamp"].as_str().unwrap();
    assert!(
        regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$")
            .unwrap()
            .is_match(timestamp)
    );
    let expected = json!({"schema_version":1,"package":"claude-autorouter","version":"0.4.0","tag":"v0.4.0","dist_tag":"latest","archive":archive_name,"sha256":digest(&archives[archive_name]),"integrity":integrity(&archives[archive_name]),"state":"submitted","timestamp":timestamp});
    assert_eq!(actual, expected);
    assert_eq!(report_bytes, json_bytes(&expected));
    assert_eq!(submitted.stdout, report_bytes);
    assert_eq!(
        fs::read(&github).unwrap(),
        b"existing=retained\nstate=submitted\n"
    );
    assert_eq!(retained_release(&directory), baseline);
    let checksum = directory.join(format!("{archive_name}.sha256"));
    let corrupt = format!("{}  {archive_name}\n", "0".repeat(64)).into_bytes();
    fs::write(&checksum, &corrupt).unwrap();
    let inspection_error = api::inspect_artifact(&archive, "v0.4.0", false)
        .err()
        .expect("Corrupt sidecar must be rejected");
    assert_eq!(inspection_error.code, "invalid_archive");
    assert_eq!(
        inspection_error.detail,
        json!({"reason":"checksum_mismatch"})
    );
    let rejected = captured_xtask(&home, &cwd, "runnable-corrupt", &args, Some(&github));
    assert_eq!(rejected.status.code(), Some(1));
    assert!(rejected.stderr.is_empty());
    let failed = json!({"schema_version":1,"state":"failed","phase":"submitted","reason":"invalid_archive","error_code":"invalid_archive"});
    assert_eq!(rejected.stdout, json_bytes(&failed));
    assert_eq!(fs::read(&report).unwrap(), json_bytes(&failed));
    assert_eq!(
        fs::read(&github).unwrap(),
        b"existing=retained\nstate=submitted\nstate=failed\n"
    );
    let mut changed = baseline.clone();
    changed
        .get_mut(&format!("{archive_name}.sha256"))
        .unwrap()
        .0 = corrupt;
    assert_eq!(retained_release(&directory), changed);
    assert_eq!(retained_release(&inputs), inputs_before);
    assert_eq!(fs::read(&archive).unwrap(), archives[archive_name]);
    assert_eq!(digest(&fs::read(fixture_path).unwrap()), binary_sha);
    let after = fs::symlink_metadata(fixture_path).unwrap();
    assert_eq!(
        (
            after.dev(),
            after.ino(),
            after.mode(),
            after.uid(),
            after.gid(),
            after.mtime(),
            after.mtime_nsec(),
            after.len()
        ),
        (
            before.dev(),
            before.ino(),
            before.mode(),
            before.uid(),
            before.gid(),
            before.mtime(),
            before.mtime_nsec(),
            before.len()
        )
    );
    eprintln!(
        "RELEASE_VERIFY_RUNNABLE_NATIVE={}",
        json!({"synthetic_fixture_only":true,"target":target,"fixture_binary_sha256":binary_sha,"fixture_binary_bytes":before.len(),"archive_sha256":artifact.sha256,"archive_integrity":artifact.integrity,"archive_bytes":artifact.bytes,"installed":installed,"commands":calls,"installed_files_with_observed_modes":*runner.installed.lock().unwrap(),"production_install_scratch_absent":true,"submitted":actual,"corrupt_inspect":{"code":inspection_error.code,"detail":inspection_error.detail},"corrupt_exit":rejected.status.code(),"corrupt_report":failed,"github_output":String::from_utf8(fs::read(&github).unwrap()).unwrap(),"same_archive_and_all_other_materials_preserved":true})
    );
}
