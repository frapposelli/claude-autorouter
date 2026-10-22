//! Reviewed GNU/Mac/Musl CI build provenance collector. Never builds or runs the product.
//! A reviewed workflow and original GitHub artifact identity remain the trust boundary.
use crate::audit_support::{digest, regular};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const INPUT_MAX: u64 = 2 * 1024 * 1024;
const BUILD_MAX: u64 = 64 * 1024 * 1024;
const BINARY_MAX: u64 = 32 * 1024 * 1024;
const TOOL_MAX: u64 = 64 * 1024 * 1024;
const OUTPUT_MAX: usize = 2 * 1024 * 1024;
const REPOSITORY: &str = "frapposelli/claude-autorouter";
const WORKFLOW: &str = ".github/workflows/rust-portability.yml";
const CARGO_CONFIG: &str = "783fdc447abb5b8f8b5652157aa9ede64da640813fa89679c29c5251c8e0cbe6";
const PUBLIC_GITHUB: &[&str] = &[
    "GITHUB_ACTIONS",
    "GITHUB_REPOSITORY",
    "GITHUB_SERVER_URL",
    "GITHUB_SHA",
    "GITHUB_JOB",
    "GITHUB_WORKFLOW_REF",
    "GITHUB_WORKFLOW_SHA",
    "GITHUB_RUN_ID",
    "GITHUB_RUN_ATTEMPT",
];

#[derive(Clone, Copy)]
struct Profile {
    target: &'static str,
    machine: &'static str,
    image: &'static str,
    rust_archive: &'static str,
    rustflags: &'static str,
    cflags: &'static str,
}

fn profile(target: &str) -> Result<Profile, String> {
    match target {
        "x86_64-unknown-linux-gnu" => Ok(Profile {
            target: "x86_64-unknown-linux-gnu",
            machine: "x86_64",
            image: "quay.io/pypa/manylinux_2_28_x86_64@sha256:39df0042d5cc900b085aa25a0659368b42a0006c54c474299b785b44c1b4ff82",
            rust_archive: "891c6366d7100feda0bca4c03ce63f3c9ac827cbebbc283e7433061d42c6a376",
            rustflags: "-C target-cpu=x86-64 -C linker=gcc",
            cflags: "-march=x86-64 -mtune=generic ",
        }),
        "aarch64-unknown-linux-gnu" => Ok(Profile {
            target: "aarch64-unknown-linux-gnu",
            machine: "aarch64",
            image: "quay.io/pypa/manylinux_2_28_aarch64@sha256:f5f03dedf61b47d69d0742d36047db78adeee3bdb7a9910ff52304750ecf2514",
            rust_archive: "5a30ce742be0835d9b23fc862db5cbbbc71464e1c9b1fca22917e10e4ba32a92",
            rustflags: "-C target-cpu=generic -C linker=gcc",
            cflags: "-march=armv8-a ",
        }),
        _ => Err("Native provenance target has no reviewed GNU build profile".into()),
    }
}

#[path = "native_provenance_macos.rs"]
mod macos;

#[path = "native_provenance_musl.rs"]
mod musl;

#[derive(Clone, Copy)]
enum BuildProfile {
    Gnu(Profile),
    Mac(macos::MacProfile),
    Musl(musl::MuslProfile),
}

impl BuildProfile {
    fn reviewed(target: &str) -> Result<Self, String> {
        if target.ends_with("apple-darwin") {
            macos::profile(target).map(Self::Mac)
        } else if target.ends_with("-linux-musl") {
            musl::profile(target).map(Self::Musl)
        } else {
            profile(target).map(Self::Gnu)
        }
    }

    fn from_inputs(before: &Value) -> Result<Self, String> {
        let p = Self::reviewed(text(before, "target")?)?;
        let expected = match p {
            Self::Gnu(_) => "gnu-glibc228-v1",
            Self::Mac(_) => "macos-hosted-v1",
            Self::Musl(_) => "musl-alpine321-v1",
        };
        if before["profile"] != expected {
            return Err("Unrecognized native build-input target/profile pair".into());
        }
        if matches!(p, Self::Mac(_)) && before["builder"] != Value::Null {
            return Err("Mac input record must not claim a GNU builder report".into());
        }
        Ok(p)
    }

    fn target(self) -> &'static str {
        match self {
            Self::Gnu(p) => p.target,
            Self::Mac(p) => p.target,
            Self::Musl(p) => p.target,
        }
    }

    fn rustflags(self, root: &Path) -> Result<String, String> {
        match self {
            Self::Gnu(p) => Ok(p.rustflags.to_owned()),
            Self::Mac(p) => Ok(p.rustflags.to_owned()),
            Self::Musl(p) => musl::rustflags(p, root),
        }
    }

    fn require_cold_release(self, root: &Path, state: &Value) -> Result<(), String> {
        match self {
            Self::Musl(p) => musl::fresh_release_tree(root, state, p),
            _ => fresh_release_tree(root, state),
        }
    }

    fn executable(self, root: &Path, state: &Value, inspected: &Value) -> Result<PathBuf, String> {
        match self {
            Self::Musl(p) => musl::admitted_executable(root, state, inspected, p),
            _ => admitted_executable(root, state, inspected),
        }
    }

    fn begin_snapshot(
        self,
        root: &Path,
        options: &BTreeMap<String, String>,
    ) -> Result<Value, String> {
        match self {
            Self::Gnu(p) => snapshot(root, p, Path::new(&options["--builder-report"])),
            Self::Mac(p) => macos::snapshot(root, p),
            Self::Musl(p) => musl::snapshot(root, p, Path::new(&options["--builder-report"])),
        }
    }

    fn observed_again(self, root: &Path, before: &Value) -> Result<Value, String> {
        match self {
            Self::Gnu(p) => snapshot(root, p, Path::new(text(&before["builder"], "path")?)),
            Self::Mac(p) => macos::snapshot(root, p),
            Self::Musl(p) => musl::snapshot(root, p, Path::new(text(&before["builder"], "path")?)),
        }
    }

    fn inspect(self, binary: &[u8]) -> Result<Value, String> {
        match self {
            Self::Gnu(p) => inspected_gnu_binary(p, binary),
            Self::Mac(p) => macos::inspect(p, binary),
            Self::Musl(p) => musl::inspect(p, binary),
        }
    }
}

fn begin_options(args: &[String]) -> Result<(BuildProfile, BTreeMap<String, String>), String> {
    let names: &[&str] = match args.len() {
        8 => &["--target", "--build-report", "--feature-report", "--out"],
        10 => &[
            "--target",
            "--builder-report",
            "--build-report",
            "--feature-report",
            "--out",
        ],
        _ => return Err("Invalid native provenance options".into()),
    };
    let options = options(args, names)?;
    let p = BuildProfile::reviewed(&options["--target"])?;
    let needs_builder = matches!(p, BuildProfile::Gnu(_) | BuildProfile::Musl(_));
    if needs_builder != options.contains_key("--builder-report") {
        return Err("GNU/Musl begin requires a builder report; Mac begin must omit it".into());
    }
    Ok((p, options))
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("Missing provenance field: {key}"))
}

fn hex(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn decimal(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 20
        && !value.starts_with('0')
        && value.bytes().all(|b| b.is_ascii_digit())
}

fn json_read(bytes: &[u8]) -> Result<Value, String> {
    serde_json::from_slice(bytes).map_err(|_| "Invalid native provenance JSON".into())
}

fn json_bytes(value: &Value) -> Result<Vec<u8>, String> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|_| "Cannot encode native provenance")?;
    bytes.push(b'\n');
    if bytes.len() > OUTPUT_MAX {
        return Err("Native provenance output exceeds bound".into());
    }
    Ok(bytes)
}

// `audit_support::regular` supplies no-follow/nonblocking bounded reads. This
// extra pre/post witness rejects replacement, growth, mode/link changes and
// writes during the read; it is not a claim that no transient mutation occurred.
fn stable(path: &Path, maximum: u64) -> Result<Vec<u8>, String> {
    let before = fs::symlink_metadata(path).map_err(|_| "Cannot inspect provenance input")?;
    // SDK tools and Cargo's public/deps executable aliases may be hardlinked.
    // Link count participates in the witness; it does not establish mutability.
    if !before.is_file() || before.nlink() == 0 {
        return Err("Provenance input must be a regular file".into());
    }
    let bytes = regular(path, maximum)?;
    let after = fs::symlink_metadata(path).map_err(|_| "Provenance input vanished")?;
    if [
        before.dev(),
        before.ino(),
        before.len(),
        before.mode() as u64,
        before.nlink(),
        before.mtime() as u64,
        before.mtime_nsec() as u64,
        before.ctime() as u64,
        before.ctime_nsec() as u64,
    ] != [
        after.dev(),
        after.ino(),
        after.len(),
        after.mode() as u64,
        after.nlink(),
        after.mtime() as u64,
        after.mtime_nsec() as u64,
        after.ctime() as u64,
        after.ctime_nsec() as u64,
    ] || bytes.len() as u64 != before.len()
    {
        return Err("Provenance input changed while reading".into());
    }
    Ok(bytes)
}

fn canonical(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err("Provenance inputs require absolute lexical paths".into());
    }
    let resolved = path
        .canonicalize()
        .map_err(|_| "Cannot resolve provenance input")?;
    if resolved != path {
        return Err("Provenance input path contains a symlink".into());
    }
    Ok(resolved)
}

fn descriptor(path: &Path, maximum: u64) -> Result<Value, String> {
    let path = canonical(path)?;
    let bytes = stable(&path, maximum)?;
    let metadata =
        fs::symlink_metadata(&path).map_err(|_| "Cannot inspect immutable provenance input")?;
    Ok(
        json!({"path":path,"bytes":bytes.len(),"sha256":digest(&bytes),"identity":{
        "device":metadata.dev(),"inode":metadata.ino(),"mode":metadata.mode(),"links":metadata.nlink(),
        "mtime_seconds":metadata.mtime(),"mtime_nanoseconds":metadata.mtime_nsec(),
        "ctime_seconds":metadata.ctime(),"ctime_nanoseconds":metadata.ctime_nsec()}}),
    )
}

fn single_report(path: &Path, maximum: u64) -> Result<Vec<u8>, String> {
    if fs::symlink_metadata(path)
        .map_err(|_| "Cannot inspect owned provenance report")?
        .nlink()
        != 1
    {
        return Err("Owned provenance reports require exactly one link".into());
    }
    stable(path, maximum)
}

fn env_string(env: &BTreeMap<String, String>, name: &str) -> Result<String, String> {
    env.get(name)
        .filter(|value| {
            !value.is_empty() && value.len() <= 8192 && !value.contains(['\0', '\r', '\n'])
        })
        .cloned()
        .ok_or_else(|| format!("Missing or invalid public build variable: {name}"))
}

fn select_env(
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Result<BTreeMap<String, String>, String> {
    // Never retain the whole environment or print rejected values.
    let mut selected = BTreeMap::new();
    let mut total = 0usize;
    for (name, value) in vars {
        let Some(name) = name.to_str() else {
            return Err("Non-UTF8 build environment name".into());
        };
        let relevant = name.starts_with("CARGO_")
            || name.starts_with("RUST")
            || name.contains("OPENSSL")
            || PUBLIC_GITHUB.contains(&name)
            || name.starts_with("AUTOROUTER_BUILD_")
            || [
                "AUTOROUTER_RUST_ARCHIVE_SHA256",
                "AUTOROUTER_PERL_VERSION",
                "AUTOROUTER_PERL_ARCHIVE_SHA256",
            ]
            .contains(&name)
            || [
                "PATH",
                "CC",
                "CXX",
                "CFLAGS",
                "CXXFLAGS",
                "CPPFLAGS",
                "LDFLAGS",
                "AR",
                "ARFLAGS",
                "CROSS_COMPILE",
                "HOST_CC",
                "HOST_CXX",
                "HOST_CFLAGS",
                "HOST_CXXFLAGS",
                "TARGET_CC",
                "TARGET_CXX",
                "TARGET_CFLAGS",
                "TARGET_CXXFLAGS",
                "LD_PRELOAD",
                "LD_LIBRARY_PATH",
                "BASH_ENV",
                "ENV",
                "MACOSX_DEPLOYMENT_TARGET",
                "CRATE_CC_NO_DEFAULTS",
                "MAKEFLAGS",
                "MFLAGS",
                "LIBRARY_PATH",
                "CPATH",
                "C_INCLUDE_PATH",
                "CPLUS_INCLUDE_PATH",
                "GCC_EXEC_PREFIX",
                "COMPILER_PATH",
                "LD",
                "LDSHARED",
                "AS",
                "ASMFLAGS",
                "RANLIB",
                "LIBCLANG_PATH",
                "SHELLOPTS",
                "BASHOPTS",
            ]
            .contains(&name)
            || name.starts_with("PKG_CONFIG")
            || name.starts_with("DYLD_")
            || name.starts_with("PERL")
            || [
                "CC_",
                "CXX_",
                "CFLAGS_",
                "CXXFLAGS_",
                "AR_",
                "ARFLAGS_",
                "RANLIB_",
                "CMAKE_",
            ]
            .iter()
            .any(|&prefix| name.starts_with(prefix))
            || [
                "_CC",
                "_CXX",
                "_CFLAGS",
                "_CXXFLAGS",
                "_AR",
                "_ARFLAGS",
                "_RANLIB",
            ]
            .iter()
            .any(|&suffix| name.ends_with(suffix));
        if relevant {
            let Some(value) = value.to_str() else {
                return Err("Non-UTF8 public build variable".into());
            };
            total = total
                .checked_add(name.len())
                .and_then(|bytes| bytes.checked_add(value.len()))
                .ok_or("Public build environment size overflow")?;
            if name.len() > 128
                || value.len() > 8192
                || value.contains(['\0', '\r', '\n'])
                || selected.len() >= 64
                || total > 65536
            {
                return Err("Invalid or oversized public build environment".into());
            }
            selected.insert(name.to_owned(), value.to_owned());
        }
    }
    Ok(selected)
}

fn collect_env() -> Result<BTreeMap<String, String>, String> {
    select_env(std::env::vars_os())
}

fn validate_env(env: &BTreeMap<String, String>, p: Profile, commit: &str) -> Result<Value, String> {
    let expected = BTreeMap::from([
        ("CARGO_HOME", "/opt/autorouter-cargo"),
        ("CARGO_INCREMENTAL", "0"),
        ("CARGO_NET_RETRY", "3"),
        ("CC", "/opt/autorouter-c-compilers/cc"),
        ("CXX", "/opt/autorouter-c-compilers/cxx"),
        ("CFLAGS", "-O2"),
        ("CXXFLAGS", "-O2"),
        ("RUSTFLAGS", p.rustflags),
        ("OPENSSL_NO_VENDOR", "0"),
        ("OPENSSL_STATIC", "1"),
        ("OPENSSL_SRC_PERL", "/opt/autorouter-perl/bin/perl"),
        ("AUTOROUTER_BUILD_TARGET", p.target),
        ("AUTOROUTER_BUILD_MACHINE", p.machine),
        ("AUTOROUTER_BUILD_IMAGE", p.image),
    ]);
    for (name, value) in &expected {
        if env.get(*name).map(String::as_str) != Some(*value) {
            return Err(format!("Unreviewed build variable: {name}"));
        }
    }
    let permitted = BTreeSet::from([
        "PATH",
        "CARGO_BUILD_JOBS",
        "CARGO_NET_OFFLINE",
        "CARGO_TARGET_DIR",
    ]);
    for name in env.keys() {
        if PUBLIC_GITHUB.contains(&name.as_str())
            || expected.contains_key(name.as_str())
            || permitted.contains(name.as_str())
        {
            continue;
        }
        // GNU workflow's two supplier pin variables are separately checked in
        // the builder record; all other effective override surfaces fail shut.
        if matches!(
            name.as_str(),
            "AUTOROUTER_RUST_ARCHIVE_SHA256"
                | "AUTOROUTER_PERL_VERSION"
                | "AUTOROUTER_PERL_ARCHIVE_SHA256"
        ) {
            continue;
        }
        return Err(format!("Unexpected compiler/build override: {name}"));
    }
    let path = env_string(env, "PATH")?;
    if std::env::split_paths(&path).any(|part| !part.is_absolute()) {
        return Err("Compiler PATH must contain only absolute directories".into());
    }
    if let Some(jobs) = env.get("CARGO_BUILD_JOBS")
        && !decimal(jobs)
    {
        return Err("Invalid bounded build jobs".into());
    }
    if let Some(value) = env.get("CARGO_NET_OFFLINE")
        && value != "true"
    {
        return Err("Unexpected offline override".into());
    }
    if let Some(value) = env.get("CARGO_TARGET_DIR")
        && !Path::new(value).is_absolute()
    {
        return Err("GNU target directory must be absolute".into());
    }
    if env_string(env, "GITHUB_ACTIONS")? != "true"
        || env_string(env, "GITHUB_REPOSITORY")? != REPOSITORY
        || env_string(env, "GITHUB_SERVER_URL")? != "https://github.com"
        || env_string(env, "GITHUB_SHA")? != commit
        || env_string(env, "GITHUB_JOB")? != "glibc228"
    {
        return Err(
            "Native release provenance requires matching original GNU workflow context".into(),
        );
    }
    let workflow_ref = env_string(env, "GITHUB_WORKFLOW_REF")?;
    let suffix = workflow_ref
        .strip_prefix(&format!("{REPOSITORY}/{WORKFLOW}@"))
        .ok_or("Unreviewed provenance workflow identity")?;
    if !["refs/heads/", "refs/tags/", "refs/pull/"]
        .iter()
        .any(|&prefix| suffix.starts_with(prefix))
        || suffix.ends_with('/')
        || suffix.contains("..")
        || suffix.contains("//")
        || !suffix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
    {
        return Err("Unreviewed provenance workflow identity".into());
    }
    let workflow_sha = env_string(env, "GITHUB_WORKFLOW_SHA")?;
    let run_id = env_string(env, "GITHUB_RUN_ID")?;
    let run_attempt = env_string(env, "GITHUB_RUN_ATTEMPT")?;
    if !hex(&workflow_sha, 40) || !decimal(&run_id) || !decimal(&run_attempt) {
        return Err("Invalid workflow/run metadata".into());
    }
    Ok(
        json!({"repository":REPOSITORY,"checkout_commit":commit,"workflow_ref":workflow_ref,"workflow_sha":workflow_sha,
        "workflow_run":format!("https://github.com/{REPOSITORY}/actions/runs/{run_id}/attempts/{run_attempt}"),"run_id":run_id,"run_attempt":run_attempt,"job":"glibc228"}),
    )
}

fn resolve_tool(env: &BTreeMap<String, String>, name: &str) -> Result<PathBuf, String> {
    for parent in std::env::split_paths(&env_string(env, "PATH")?) {
        if !parent.is_absolute() {
            return Err("Compiler PATH must contain only absolute directories".into());
        }
        let path = parent.join(name);
        if path.is_file() {
            let resolved = path
                .canonicalize()
                .map_err(|_| "Cannot resolve compiler tool")?;
            if fs::metadata(&resolved)
                .map_err(|_| "Cannot inspect compiler tool")?
                .mode()
                & 0o111
                == 0
            {
                return Err("Compiler tool is not executable".into());
            }
            return Ok(resolved);
        }
    }
    Err("Missing reviewed build tool".into())
}

fn capture(
    env: &BTreeMap<String, String>,
    root: &Path,
    program: &Path,
    args: &[&str],
    maximum: usize,
) -> Result<String, String> {
    let mut command = Command::new(program);
    command
        .current_dir(root)
        .env_clear()
        .env("PATH", env_string(env, "PATH")?)
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_REPLACE_OBJECTS", "1");
    command.args(args);
    let bytes = crate::process::capture(&mut command, b"", Duration::from_secs(15))?;
    if bytes.len() > maximum {
        return Err("Public compiler metadata exceeds bound".into());
    }
    String::from_utf8(bytes)
        .map(|value| value.trim().to_owned())
        .map_err(|_| "Invalid compiler metadata UTF-8".into())
}

fn git(env: &BTreeMap<String, String>, root: &Path, args: &[&str]) -> Result<String, String> {
    let safe = format!("safe.directory={}", root.display());
    let mut all = vec![
        "--no-optional-locks",
        "-c",
        safe.as_str(),
        "-c",
        "core.fsmonitor=false",
    ];
    all.extend_from_slice(args);
    capture(env, root, &resolve_tool(env, "git")?, &all, 65536)
}

fn no_config_overrides(root: &Path) -> Result<Value, String> {
    let admitted = root.join("rust/.cargo/config.toml");
    if digest(&stable(&admitted, 65536)?) != CARGO_CONFIG {
        return Err("Unreviewed workspace Cargo configuration".into());
    }
    let mut paths = Vec::new();
    for parent in root.join("rust").ancestors() {
        paths.push(parent.join(".cargo/config"));
        paths.push(parent.join(".cargo/config.toml"));
    }
    paths.push(PathBuf::from("/opt/autorouter-cargo/config"));
    paths.push(PathBuf::from("/opt/autorouter-cargo/config.toml"));
    for path in paths {
        if path == admitted {
            continue;
        }
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err("Unreviewed effective Cargo configuration path".into()),
        }
    }
    descriptor(&admitted, 65536)
}

fn wrapper(
    env: &BTreeMap<String, String>,
    root: &Path,
    p: Profile,
    name: &str,
) -> Result<Value, String> {
    let path = PathBuf::from(env_string(env, name)?);
    canonical(&path)?;
    let bytes = stable(&path, 4096)?;
    let contents =
        String::from_utf8(bytes.clone()).map_err(|_| "Invalid C compiler wrapper".to_owned())?;
    let middle = contents
        .strip_prefix("#!/bin/bash\nexec ")
        .and_then(|s| s.strip_suffix(&format!(" {}\"$@\"\n", p.cflags)))
        .ok_or("C compiler wrapper does not enforce reviewed baseline")?;
    let compiler = PathBuf::from(middle);
    if !compiler.is_absolute()
        || middle.is_empty()
        || !middle
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-.+".contains(&b))
        || compiler.file_name().and_then(|s| s.to_str())
            != Some(if name == "CC" { "gcc" } else { "g++" })
    {
        return Err("Unreviewed wrapped compiler executable".into());
    }
    let compiler = compiler
        .canonicalize()
        .map_err(|_| "Cannot resolve wrapped compiler")?;
    Ok(
        json!({"path":path,"contents":contents,"sha256":digest(&bytes),
        "effective_target_options":capture(env,root,&path,&["-Q","--help=target"],65536)?,
        "baseline_macros":capture(env,root,&path,&["-dM","-E","-x",if name=="CC" {"c"} else {"c++"},"-"],65536)?,
        "identity":descriptor(&path,4096)?,
        "compiler":descriptor(&compiler,TOOL_MAX)?}),
    )
}

fn validate_builder(builder: &Value, state: &Value, p: Profile, root: &Path) -> Result<(), String> {
    if builder["schema_version"] != 1
        || builder["kind"] != "gnu_glibc228_builder_provenance"
        || builder["source_dirty"] != false
        || builder["source_commit"] != state["source_commit"]
        || builder["cargo_lock_sha256"] != state["cargo_lock_sha256"]
        || builder["workflow_sha256"] != state["files"][WORKFLOW]["sha256"]
        || builder["target"] != p.target
        || builder["image"] != p.image
        || builder["rust_archive_sha256"] != p.rust_archive
        || builder["rustc"] != state["compiler"]["rustc_text"]
        || builder["cargo"] != state["compiler"]["cargo_text"]
        || builder["gcc"] != state["compiler"]["gcc_text"]
        || builder["linker"] != state["compiler"]["linker_text"]
        || builder["libc"] != "glibc 2.28"
        || builder["rustflags"] != p.rustflags
        || builder["cflags"] != "-O2"
        || builder["openssl_no_vendor"] != "0"
        || builder["openssl_static"] != "1"
    {
        return Err("GNU builder record does not match current admitted inputs".into());
    }
    for name in ["CC", "CXX"] {
        for field in [
            "path",
            "contents",
            "sha256",
            "effective_target_options",
            "baseline_macros",
        ] {
            if builder["compiler_wrappers"][name][field]
                != state["compiler"]["wrappers"][name][field]
            {
                return Err("GNU wrapper record does not match actual compiler inputs".into());
            }
        }
    }
    if text(builder, "perl_archive_sha256")?
        != "c9387e1473a1866935cb047ece7c2e0a80767a3acdecb79d4a375f8a95970ddc"
        || text(builder, "perl_version")? != "5.42.3"
        || builder["perl"] != state["compiler"]["perl_text"]
        || builder["source_date_epoch"] != state["source_date_epoch"]
        || builder["kernel"] != state["compiler"]["kernel_text"]
    {
        return Err("GNU SDK metadata does not match admitted inputs".into());
    }
    if root.to_str().is_none() {
        return Err("Non-UTF8 source root".into());
    }
    Ok(())
}

fn snapshot(root: &Path, p: Profile, builder_path: &Path) -> Result<Value, String> {
    let env = collect_env()?;
    let claimed = env_string(&env, "GITHUB_SHA")?;
    if !hex(&claimed, 40) {
        return Err("Invalid checkout commit metadata".into());
    }
    let ci = validate_env(&env, p, &claimed)?;
    // Admission and public context checks happen before any metadata subprocess.
    let commit = git(&env, root, &["rev-parse", "--verify", "HEAD"])?;
    if commit != claimed
        || !git(
            &env,
            root,
            &["status", "--porcelain", "--untracked-files=normal"],
        )?
        .is_empty()
    {
        return Err("Native CI provenance requires a clean exact source checkout".into());
    }
    for (name, value) in [
        ("AUTOROUTER_RUST_ARCHIVE_SHA256", p.rust_archive),
        ("AUTOROUTER_PERL_VERSION", "5.42.3"),
        (
            "AUTOROUTER_PERL_ARCHIVE_SHA256",
            "c9387e1473a1866935cb047ece7c2e0a80767a3acdecb79d4a375f8a95970ddc",
        ),
    ] {
        if env_string(&env, name)? != value {
            return Err("GNU SDK environment pin mismatch".into());
        }
    }
    let config = no_config_overrides(root)?;
    let rustc = resolve_tool(&env, "rustc")?;
    let cargo = resolve_tool(&env, "cargo")?;
    if rustc != Path::new("/opt/autorouter-rust/bin/rustc")
        || cargo != Path::new("/opt/autorouter-rust/bin/cargo")
    {
        return Err("GNU provenance requires checksum-pinned standalone Rust tools".into());
    }
    let rustc_text = capture(&env, root, &rustc, &["-vV"], 16384)?;
    if !rustc_text.lines().any(|line| line == "release: 1.99.0")
        || !rustc_text
            .lines()
            .any(|line| line == format!("host: {}", p.target))
    {
        return Err("Rust compiler does not match pinned GNU toolchain/host".into());
    }
    let mut tools = serde_json::Map::new();
    for name in [
        "git", "rustc", "cargo", "gcc", "g++", "ld", "getconf", "uname",
    ] {
        tools.insert(
            name.to_owned(),
            descriptor(&resolve_tool(&env, name)?, TOOL_MAX)?,
        );
    }
    tools.insert(
        "perl".into(),
        descriptor(Path::new("/opt/autorouter-perl/bin/perl"), TOOL_MAX)?,
    );
    let mut files = serde_json::Map::new();
    for name in [
        "rust/Cargo.lock",
        "rust/Cargo.toml",
        "rust/rust-toolchain.toml",
        WORKFLOW,
        "rust/.cargo/config.toml",
    ] {
        files.insert(name.to_owned(), descriptor(&root.join(name), INPUT_MAX)?);
    }
    let epoch = git(&env, root, &["show", "-s", "--format=%ct", "HEAD"])?;
    if !decimal(&epoch)
        || std::env::var("SOURCE_DATE_EPOCH").ok().as_deref() != Some(epoch.as_str())
    {
        return Err("Source epoch does not match clean source commit".into());
    }
    if capture(
        &env,
        root,
        &resolve_tool(&env, "getconf")?,
        &["GNU_LIBC_VERSION"],
        4096,
    )? != "glibc 2.28"
        || capture(&env, root, &resolve_tool(&env, "uname")?, &["-m"], 4096)? != p.machine
    {
        return Err("GNU provenance host/container does not match reviewed profile".into());
    }
    let compiler = json!({"tools":tools,"rustc_text":rustc_text,"cargo_text":capture(&env,root,&cargo,&["--version"],4096)?,
        "gcc_text":capture(&env,root,&resolve_tool(&env,"gcc")?,&["--version"],16384)?,
        "linker_text":capture(&env,root,&resolve_tool(&env,"ld")?,&["--version"],16384)?,
        "kernel_text":capture(&env,root,&resolve_tool(&env,"uname")?,&["-srmo"],4096)?,
        "perl_text":capture(&env,root,Path::new("/opt/autorouter-perl/bin/perl"),&["-V"],65536)?,
        "wrappers":{"CC":wrapper(&env,root,p,"CC")?,"CXX":wrapper(&env,root,p,"CXX")?}});
    let builder_path = canonical(builder_path)?;
    let builder_bytes = single_report(&builder_path, INPUT_MAX)?;
    let builder = json_read(&builder_bytes)?;
    let lock_sha = files["rust/Cargo.lock"]["sha256"].clone();
    let state = json!({"schema_version":1,"kind":"native_binary_build_inputs","profile":"gnu-glibc228-v1","target":p.target,
        "source_commit":commit,"source_dirty":false,"cargo_lock_sha256":lock_sha,"source_date_epoch":epoch,
        "ci":ci,"environment":env,"files":files,"cargo_config":config,"compiler":compiler,
        "builder":descriptor(&builder_path,INPUT_MAX)?,
        "command":["cargo","build","--release","--offline","--locked","--package","claude-autorouter","--message-format=json"],
        "working_directory":root.join("rust")});
    validate_builder(&builder, &state, p, root)?;
    Ok(state)
}

fn options(args: &[String], names: &[&str]) -> Result<BTreeMap<String, String>, String> {
    if args.len() != names.len() * 2 {
        return Err("Invalid native provenance options".into());
    }
    let mut result = BTreeMap::new();
    for pair in args.as_chunks::<2>().0 {
        if !names.contains(&pair[0].as_str())
            || pair[1].is_empty()
            || result.insert(pair[0].clone(), pair[1].clone()).is_some()
        {
            return Err("Unknown or duplicate native provenance option".into());
        }
    }
    Ok(result)
}

#[derive(Clone, Copy)]
struct Identity {
    device: u64,
    inode: u64,
}
impl Identity {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
    fn matches(self, path: &Path, directory: bool) -> bool {
        fs::symlink_metadata(path).is_ok_and(|metadata| {
            metadata.dev() == self.device
                && metadata.ino() == self.inode
                && if directory {
                    metadata.is_dir()
                } else {
                    metadata.is_file()
                }
        })
    }
}
struct OwnedFile {
    path: PathBuf,
    identity: Identity,
    armed: bool,
}
impl OwnedFile {
    fn keep(&mut self) {
        self.armed = false;
    }
    fn matches(&self) -> bool {
        self.identity.matches(&self.path, false)
    }
}
impl Drop for OwnedFile {
    fn drop(&mut self) {
        if self.armed && self.matches() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn persist_owned_file(
    path: &Path,
    mode: u32,
    persist: impl FnOnce(&mut fs::File) -> std::io::Result<()>,
) -> Result<OwnedFile, String> {
    if !path.is_absolute() || path.file_name().is_none() {
        return Err("Provenance output requires an absolute lexical file path".into());
    }
    let parent = path
        .parent()
        .ok_or("Missing native provenance output parent")?;
    canonical(parent)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "Native provenance output already exists or cannot be created".to_owned())?;
    let identity = Identity::of(
        &file
            .metadata()
            .map_err(|_| "Cannot inspect created provenance file")?,
    );
    let owned = OwnedFile {
        path: path.to_path_buf(),
        identity,
        armed: true,
    };
    persist(&mut file).map_err(|_| "Cannot persist native provenance output")?;
    if !owned.matches() {
        return Err("Created provenance file path was replaced".into());
    }
    Ok(owned)
}

fn fresh_file(path: &Path, bytes: &[u8], mode: u32) -> Result<OwnedFile, String> {
    persist_owned_file(path, mode, |file| {
        file.write_all(bytes)
            .and_then(|_| file.set_permissions(fs::Permissions::from_mode(mode)))
            .and_then(|_| file.sync_all())
    })
}

fn read_build_inputs(path: &Path) -> Result<(Vec<u8>, Value), String> {
    let bytes = single_report(path, INPUT_MAX)?;
    let before = json_read(&bytes)?;
    if json_bytes(&before)? != bytes {
        return Err("Build inputs are not canonical producer output".into());
    }
    if before["schema_version"] != 1 || before["kind"] != "native_binary_build_inputs" {
        return Err("Unrecognized native build-input schema/profile".into());
    }
    BuildProfile::from_inputs(&before)?;
    Ok((bytes, before))
}

fn declared_outputs(
    before: &Value,
    build_report: &str,
    feature_report: &str,
) -> Result<Value, String> {
    let outputs = before["outputs"].clone();
    if outputs != json!({"build_report":build_report,"feature_report":feature_report}) {
        return Err("Provenance reports are not the outputs declared before the build".into());
    }
    Ok(outputs)
}

fn bound_state(before: &Value, mut after: Value, outputs: &Value) -> Result<(), String> {
    after["outputs"] = outputs.clone();
    if before != &after {
        return Err("Native source/compiler/workflow/build inputs changed since begin".into());
    }
    Ok(())
}

fn require_fresh_artifacts(build_bytes: &[u8]) -> Result<(), String> {
    for line in std::str::from_utf8(build_bytes)
        .map_err(|_| "Invalid Cargo build UTF-8")?
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let row: Value = serde_json::from_str(line).map_err(|_| "Invalid Cargo build record")?;
        if row["reason"] == "compiler-artifact" && row["fresh"] != false {
            return Err(
                "Cached/unspecified compiler artifacts lack current compiler provenance".into(),
            );
        }
    }
    Ok(())
}

fn matching_feature_report(bytes: &[u8], exact: &Value) -> Result<(), String> {
    if bytes != json_bytes(exact)? {
        return Err("Feature audit is not the actual exact production build audit".into());
    }
    Ok(())
}

fn admitted_executable(root: &Path, before: &Value, inspected: &Value) -> Result<PathBuf, String> {
    let executable = canonical(Path::new(text(&inspected["binary"], "executable")?))?;
    let expected = before["environment"]["CARGO_TARGET_DIR"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("rust/target"))
        .join("release/claude-autorouter");
    if executable != expected {
        return Err("Cargo executable is not at the admitted native production path".into());
    }
    Ok(executable)
}

fn inspected_gnu_binary(p: Profile, binary: &[u8]) -> Result<Value, String> {
    let inspection = crate::provenance_binary::inspect(p.target, binary)?;
    crate::provenance_binary::enforce_glibc_baseline(&inspection, "2.28")?;
    let libraries = inspection["dynamic_libraries"]
        .as_array()
        .ok_or("Missing GNU dependency inspection")?;
    for library in libraries {
        let name = library
            .as_str()
            .ok_or("Invalid GNU dependency inspection")?
            .rsplit('/')
            .next()
            .unwrap_or("");
        if ["libssl", "libcrypto"].iter().any(|base| {
            name == *base
                || name.starts_with(&format!("{base}."))
                || name.starts_with(&format!("{base}-"))
        }) {
            return Err("GNU artifact depends on host OpenSSL".into());
        }
    }
    if inspection["rpath"] != Value::Null || inspection["runpath"] != Value::Null {
        return Err("GNU artifact has a builder library search path".into());
    }
    Ok(inspection)
}

fn fresh_release_tree(root: &Path, state: &Value) -> Result<(), String> {
    let directory = state["environment"]["CARGO_TARGET_DIR"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("rust/target"))
        .join("release");
    match fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err("Native provenance requires a prospective fresh release output tree".into()),
    }
}

fn finish(root: &Path, args: &[String]) -> Result<bool, String> {
    let options = options(
        args,
        &["--inputs", "--build-report", "--feature-report", "--out"],
    )?;
    let input_path = canonical(Path::new(&options["--inputs"]))?;
    let (input_bytes, before) = read_build_inputs(&input_path)?;
    let p = BuildProfile::from_inputs(&before)?;
    let expected_outputs = declared_outputs(
        &before,
        &options["--build-report"],
        &options["--feature-report"],
    )?;
    bound_state(&before, p.observed_again(root, &before)?, &expected_outputs)?;
    let build_path = canonical(Path::new(&options["--build-report"]))?;
    let build_bytes = single_report(&build_path, BUILD_MAX)?;
    let inspected = crate::production_features::inspect(&build_bytes, root)?;
    require_fresh_artifacts(&build_bytes)?;
    let exact_feature_report = crate::production_features::verified_report(&build_bytes, root)?;
    let feature_path = canonical(Path::new(&options["--feature-report"]))?;
    let feature_bytes = single_report(&feature_path, INPUT_MAX)?;
    matching_feature_report(&feature_bytes, &exact_feature_report)?;
    let executable = p.executable(root, &before, &inspected)?;
    let binary = stable(&executable, BINARY_MAX)?;
    if digest(&binary) != text(&exact_feature_report, "binary_sha256")? {
        return Err("Production executable changed after feature audit".into());
    }
    let inspection = p.inspect(&binary)?;
    let rustflags = p.rustflags(root)?;
    let provenance = json!({"schema_version":1,"kind":"native_binary_provenance","provenance":"ci-source-build",
        "source_commit":before["source_commit"],"cargo_lock_sha256":before["cargo_lock_sha256"],"target":p.target(),
        "binary_sha256":digest(&binary),"repository":REPOSITORY,"profile":"release","portable_cpu":true,
        "rustc":before["compiler"]["rustc_text"],"workflow_run":before["ci"]["workflow_run"],
        "rustflags":rustflags.split_ascii_whitespace().collect::<Vec<_>>(),"workflow":before["ci"],
        "inputs_sha256":digest(&input_bytes),"cargo_report_sha256":digest(&build_bytes),"feature_report_sha256":digest(&feature_bytes),
        "builder_report_sha256":before["builder"]["sha256"],"inspection":inspection,
        "qualification":"Build provenance only. No platform/TLS/performance/lifecycle/release approval or private-artifact promotion."});
    let provenance_bytes = json_bytes(&provenance)?;
    // Verify every observed file again before committing output. This closes
    // ordinary concurrent mutation windows; the reviewed job is still trusted
    // to execute the recorded command without a transient hidden mutation.
    let mut final_state = p.observed_again(root, &before)?;
    final_state["outputs"] = expected_outputs;
    if before != final_state
        || single_report(&input_path, INPUT_MAX)? != input_bytes
        || single_report(&build_path, BUILD_MAX)? != build_bytes
        || single_report(&feature_path, INPUT_MAX)? != feature_bytes
        || stable(&executable, BINARY_MAX)? != binary
    {
        return Err("Native provenance input drift before output".into());
    }
    let out = PathBuf::from(&options["--out"]);
    if !out.is_absolute() {
        return Err("Native provenance output requires an absolute fresh directory".into());
    }
    canonical(
        out.parent()
            .ok_or("Missing native provenance output parent")?,
    )?;
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&out)
        .map_err(|_| "Native provenance output already exists or cannot be created".to_owned())?;
    let output_identity = Identity::of(
        &fs::symlink_metadata(&out).map_err(|_| "Cannot inspect fresh provenance directory")?,
    );
    let mut owned_files = Vec::new();
    let result = (|| {
        fs::set_permissions(&out, fs::Permissions::from_mode(0o700))
            .map_err(|_| "Cannot protect native provenance directory")?;
        owned_files.push(fresh_file(&out.join("claude-autorouter"), &binary, 0o755)?);
        owned_files.push(fresh_file(
            &out.join("native-binary-provenance.json"),
            &provenance_bytes,
            0o600,
        )?);
        if !output_identity.matches(&out, true)
            || owned_files.iter().any(|file| !file.matches())
            || single_report(&out.join("claude-autorouter"), BINARY_MAX)? != binary
            || single_report(&out.join("native-binary-provenance.json"), INPUT_MAX)?
                != provenance_bytes
            || fs::metadata(out.join("claude-autorouter"))
                .map_err(|_| "Cannot inspect retained binary mode")?
                .mode()
                & 0o7777
                != 0o755
            || fs::metadata(out.join("native-binary-provenance.json"))
                .map_err(|_| "Cannot inspect retained provenance mode")?
                .mode()
                & 0o7777
                != 0o600
            || fs::symlink_metadata(&out)
                .map_err(|_| "Cannot inspect retained output directory mode")?
                .mode()
                & 0o7777
                != 0o700
            || !output_identity.matches(&out, true)
        {
            return Err("Retained native provenance output changed".into());
        }
        Ok::<(), String>(())
    })();
    if let Err(error) = result {
        // Only attempt named-file cleanup while this exact fresh directory
        // remains owned. Never recursively remove unknown or replaced paths.
        drop(owned_files);
        if output_identity.matches(&out, true) {
            let _ = fs::remove_dir(&out);
        }
        return Err(error);
    }
    for file in &mut owned_files {
        file.keep();
    }
    println!("{}",serde_json::to_string_pretty(&json!({"kind":"native_binary_provenance_written","passed":true,
        "target":p.target(),"binary_sha256":digest(&binary),"provenance_sha256":digest(&provenance_bytes),
        "release_approved":false,"output":out})).map_err(|_|"Cannot encode provenance result")?);
    Ok(true)
}

pub(crate) fn run(args: &[String], root: &Path) -> Result<bool, String> {
    match args.first().map(String::as_str) {
        Some("begin") => {
            let (p, options) = begin_options(&args[1..])?;
            let mut state = p.begin_snapshot(root, &options)?;
            let output_paths = [
                &options["--build-report"],
                &options["--feature-report"],
                &options["--out"],
            ];
            if output_paths.iter().collect::<BTreeSet<_>>().len() != 3 {
                return Err("Provenance inputs/reports require separate fresh paths".into());
            }
            for value in output_paths {
                let path = Path::new(value);
                if !path.is_absolute() || path.file_name().is_none() {
                    return Err("Declared provenance reports require absolute paths".into());
                }
                canonical(
                    path.parent()
                        .ok_or("Missing declared provenance report parent")?,
                )?;
                match fs::symlink_metadata(path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    _ => return Err("Declared provenance report/output already exists".into()),
                }
            }
            p.require_cold_release(root, &state)?;
            state["outputs"] = json!({"build_report":options["--build-report"],"feature_report":options["--feature-report"]});
            let mut owned = fresh_file(Path::new(&options["--out"]), &json_bytes(&state)?, 0o600)?;
            owned.keep();
            Ok(true)
        }
        Some("finish") => finish(root, &args[1..]),
        Some("--help") => {
            println!(
                "Reviewed GNU/Mac/Musl CI only: binary-provenance begin --target TARGET [--builder-report ABSOLUTE_PATH (GNU/Musl only)] --build-report NEW_ABSOLUTE_PATH --feature-report NEW_ABSOLUTE_PATH --out NEW_ABSOLUTE_FILE\n              binary-provenance finish --inputs ABSOLUTE_PATH --build-report ABSOLUTE_PATH --feature-report ABSOLUTE_PATH --out NEW_ABSOLUTE_DIRECTORY\nNo builds, product execution, downloads, provider calls, qualification or release approval. Local/unreviewed profiles fail closed."
            );
            Ok(true)
        }
        _ => Err("Expected binary-provenance begin|finish|--help".into()),
    }
}

#[cfg(test)]
#[path = "native_provenance_tests.rs"]
mod tests;
