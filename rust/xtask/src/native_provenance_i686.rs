//! Hosted i686 build observations; native CPU qualification remains separate.
use super::*;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::io::Read;

const WORKFLOW32: &str = ".github/workflows/rust-runtime32.yml";
const TARGET: &str = "i686-unknown-linux-gnu";
const HOST: &str = "x86_64-unknown-linux-gnu";
const HOST_TOOL_MAX: u64 = 512 * 1024 * 1024;
const RUSTFLAGS32: &str = "-C target-cpu=pentium4 -C linker=gcc";
const ROOT_PACKAGES: &[&str] = &["gcc-multilib", "libc6-dev-i386", "make", "perl"];
const EXTRA: &[&str] = &[
    "HOME",
    "RUNNER_OS",
    "RUNNER_ARCH",
    "RUNNER_ENVIRONMENT",
    "ImageOS",
    "ImageVersion",
    "SOURCE_DATE_EPOCH",
    "AUTOROUTER_RUNTIME32_RUST_HOST",
];

#[derive(Clone, Copy)]
pub(super) struct I686Profile {
    pub(super) target: &'static str,
    pub(super) rustflags: &'static str,
}
pub(super) fn profile(target: &str) -> Result<I686Profile, String> {
    if target != TARGET {
        return Err("No reviewed hosted i686 profile".into());
    }
    Ok(I686Profile {
        target: TARGET,
        rustflags: RUSTFLAGS32,
    })
}
fn selected_env(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    let mut total = 0usize;
    for (name, value) in vars {
        let name_text = name.to_str().ok_or("Non-UTF8 hosted32 variable name")?;
        let rows = if EXTRA.contains(&name_text) {
            BTreeMap::from([(
                name_text.to_owned(),
                value.into_string().map_err(|_| "Non-UTF8 hosted32 value")?,
            )])
        } else {
            super::select_env(std::iter::once((name, value)))?
        };
        for (name, value) in rows {
            total = total
                .checked_add(name.len())
                .and_then(|n| n.checked_add(value.len()))
                .ok_or("Hosted32 environment overflow")?;
            if name.len() > 128
                || value.len() > 8192
                || value.contains(['\0', '\r', '\n'])
                || out.len() >= 64
                || total > 65536
                || out.insert(name, value).is_some()
            {
                return Err("Invalid or oversized hosted32 environment".into());
            }
        }
    }
    Ok(out)
}
fn target_root(root: &Path, state: &Value) -> Result<PathBuf, String> {
    let wanted = root.join("artifacts/rust-rewrite/runtime32/target");
    if state["environment"]["CARGO_TARGET_DIR"].as_str() != wanted.to_str() {
        return Err("Hosted32 target root differs".into());
    }
    prospective_parents(&wanted)?;
    prospective_parents(&wanted.join(TARGET))?;
    Ok(wanted)
}
fn prospective_parents(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err("Invalid prospective hosted32 path".into());
    }
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            _ => return Err("Hosted32 target parent is not a real directory".into()),
        }
    }
    Ok(())
}
pub(super) fn fresh_release_tree(root: &Path, state: &Value, _: I686Profile) -> Result<(), String> {
    let target = target_root(root, state)?;
    for path in [target.join("release"), target.join(TARGET).join("release")] {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            _ => return Err("Hosted32 requires both host and target release trees cold".into()),
        }
    }
    Ok(())
}
pub(super) fn admitted_executable(
    root: &Path,
    state: &Value,
    inspected: &Value,
    _: I686Profile,
) -> Result<PathBuf, String> {
    let expected = target_root(root, state)?
        .join(TARGET)
        .join("release/claude-autorouter");
    prospective_parents(
        expected
            .parent()
            .ok_or("Missing target executable parent")?,
    )?;
    let actual = canonical(Path::new(text(&inspected["binary"], "executable")?))?;
    if actual != expected {
        return Err("Hosted32 Cargo executable is not the exact target release CLI".into());
    }
    Ok(actual)
}
fn validate_env(
    env: &BTreeMap<String, String>,
    root: &Path,
    p: I686Profile,
    commit: &str,
) -> Result<Value, String> {
    if root
        .to_str()
        .is_none_or(|s| s.chars().any(char::is_whitespace))
    {
        return Err("Hosted32 runner source root contains whitespace".into());
    }
    let runner = format!(
        "python3 {}/rust/distribution/ci/runtime32-proof.py run-test",
        root.display()
    );
    let target_dir = root.join("artifacts/rust-rewrite/runtime32/target");
    let expected = BTreeMap::from([
        ("CARGO_INCREMENTAL", "0"),
        ("CARGO_PROFILE_DEV_DEBUG", "0"),
        ("CARGO_PROFILE_TEST_DEBUG", "0"),
        ("CARGO_BUILD_JOBS", "4"),
        ("CARGO_BUILD_TARGET", TARGET),
        ("CARGO_NET_OFFLINE", "true"),
        ("CARGO_TERM_COLOR", "never"),
        ("CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_LINKER", "gcc"),
        ("RUSTUP_TOOLCHAIN", "1.99.0-x86_64-unknown-linux-gnu"),
        (
            "AUTOROUTER_RUNTIME32_RUST_HOST",
            "1.99.0-x86_64-unknown-linux-gnu",
        ),
        ("RUSTFLAGS", p.rustflags),
        ("OPENSSL_NO_VENDOR", "0"),
        ("OPENSSL_STATIC", "1"),
        ("OPENSSL_SRC_PERL", "/usr/bin/perl"),
        ("AUTOROUTER_BUILD_TARGET", TARGET),
        ("AUTOROUTER_BUILD_MACHINE", "x86_64"),
        ("AUTOROUTER_BUILD_RUNNER", "ubuntu-24.04"),
        ("RUNNER_OS", "Linux"),
        ("RUNNER_ARCH", "X64"),
        ("RUNNER_ENVIRONMENT", "github-hosted"),
    ]);
    for (key, value) in &expected {
        if env.get(*key).map(String::as_str) != Some(*value) {
            return Err(format!("Unreviewed hosted32 build variable: {key}"));
        }
    }
    let allowed = [
        "HOME",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "PATH",
        "CARGO_TARGET_DIR",
        "CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_RUNNER",
        "SOURCE_DATE_EPOCH",
        "ImageOS",
        "ImageVersion",
    ];
    if env.keys().any(|k| {
        !PUBLIC_GITHUB.contains(&k.as_str())
            && !expected.contains_key(k.as_str())
            && !allowed.contains(&k.as_str())
    }) {
        return Err("Unexpected hosted32 compiler/build override".into());
    }
    if env_string(env, "CARGO_TARGET_DIR")? != target_dir.to_str().ok_or("Non-UTF8 target root")?
        || env_string(env, "CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_RUNNER")? != runner
    {
        return Err("Hosted32 target root or exact source-owned runner differs".into());
    }
    let paths = env_string(env, "PATH")?;
    if std::env::split_paths(&paths).any(|p| {
        !p.is_absolute()
            || p.components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    }) {
        return Err("Unreviewed hosted32 PATH".into());
    }
    for key in ["HOME", "CARGO_HOME", "RUSTUP_HOME"] {
        if !Path::new(&env_string(env, key)?).is_absolute() {
            return Err("Hosted32 home must be absolute".into());
        }
    }
    if env_string(env, "GITHUB_ACTIONS")? != "true"
        || env_string(env, "GITHUB_REPOSITORY")? != REPOSITORY
        || env_string(env, "GITHUB_SERVER_URL")? != "https://github.com"
        || env_string(env, "GITHUB_SHA")? != commit
        || env_string(env, "GITHUB_JOB")? != "runtime32"
        || !hex(commit, 40)
        || !decimal(&env_string(env, "SOURCE_DATE_EPOCH")?)
    {
        return Err("Hosted32 original CI identity differs".into());
    }
    let workflow_ref = env_string(env, "GITHUB_WORKFLOW_REF")?;
    let suffix = workflow_ref
        .strip_prefix(&format!("{REPOSITORY}/{WORKFLOW32}@"))
        .ok_or("Wrong hosted32 workflow")?;
    if !["refs/heads/", "refs/tags/", "refs/pull/"]
        .iter()
        .any(|p| suffix.starts_with(p))
        || suffix.ends_with('/')
        || suffix.contains("..")
        || suffix.contains("//")
        || !suffix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
    {
        return Err("Invalid hosted32 workflow ref".into());
    }
    let workflow_sha = env_string(env, "GITHUB_WORKFLOW_SHA")?;
    let run = env_string(env, "GITHUB_RUN_ID")?;
    let attempt = env_string(env, "GITHUB_RUN_ATTEMPT")?;
    if !hex(&workflow_sha, 40) || !decimal(&run) || !decimal(&attempt) {
        return Err("Invalid hosted32 workflow run".into());
    }
    Ok(
        json!({"repository":REPOSITORY,"checkout_commit":commit,"workflow_ref":workflow_ref,"workflow_sha":workflow_sha,"workflow_run":format!("https://github.com/{REPOSITORY}/actions/runs/{run}/attempts/{attempt}"),"run_id":run,"run_attempt":attempt,"job":"runtime32","runner":"ubuntu-24.04"}),
    )
}
#[derive(Clone)]
struct Tool {
    invocation: PathBuf,
    canonical: PathBuf,
}
fn tool(env: &BTreeMap<String, String>, name: &str) -> Result<Tool, String> {
    for directory in std::env::split_paths(&env_string(env, "PATH")?) {
        let invocation = directory.join(name);
        match fs::metadata(&invocation) {
            Ok(m) if m.is_file() && m.mode() & 0o111 != 0 => {
                return Ok(Tool {
                    canonical: invocation
                        .canonicalize()
                        .map_err(|_| "Cannot resolve selected hosted32 tool")?,
                    invocation,
                });
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err("Cannot inspect hosted32 PATH tool".into()),
        }
    }
    Err("Required hosted32 tool is absent".into())
}
fn metadata_environment(
    env: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for key in [
        "PATH",
        "HOME",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "RUSTUP_TOOLCHAIN",
    ] {
        out.insert(key.to_owned(), env_string(env, key)?);
    }
    out.insert("LC_ALL".into(), "C".into());
    out.insert("GIT_CONFIG_NOSYSTEM".into(), "1".into());
    out.insert("GIT_CONFIG_GLOBAL".into(), "/dev/null".into());
    out.insert("GIT_NO_REPLACE_OBJECTS".into(), "1".into());
    Ok(out)
}
fn metadata(
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
        .envs(metadata_environment(env)?)
        .args(args);
    let bytes = crate::process::capture(&mut command, b"", Duration::from_secs(15))?;
    if bytes.len() > maximum {
        return Err("Hosted32 fixed metadata exceeds its bound".into());
    }
    String::from_utf8(bytes)
        .map(|s| s.trim().to_owned())
        .map_err(|_| "Invalid hosted32 metadata UTF-8".into())
}
// GCC may report an absolute supplier path containing symlinks and `..`.
// Resolve that compiler-owned spelling with filesystem semantics; shared input,
// environment and user path admission remains strict and unchanged.
fn compiler_reported_linker_file(reported: &str) -> Result<Value, String> {
    if reported.is_empty()
        || reported.len() > 8192
        || reported.chars().any(char::is_control)
        || !Path::new(reported).is_absolute()
    {
        return Err("Invalid Hosted32 compiler-reported linker supplier path".into());
    }
    let resolved = fs::canonicalize(reported)
        .map_err(|_| "Cannot resolve Hosted32 compiler-reported linker supplier")?;
    let mut row = resolved_linker_file(&resolved)?;
    row["compiler_reported_path"] = json!(reported);
    Ok(row)
}

fn resolved_linker_file(resolved: &Path) -> Result<Value, String> {
    if resolved
        .to_str()
        .is_none_or(|path| path.len() > 8192 || path.chars().any(char::is_control))
    {
        return Err("Invalid resolved Hosted32 linker supplier path".into());
    }
    tool_descriptor(resolved).map_err(|_| {
        "Hosted32 compiler-reported linker supplier is not an admitted regular file".into()
    })
}

fn tool_witness(value: &Tool) -> Result<Value, String> {
    let mut row = tool_descriptor(&value.canonical)?;
    row["invocation_path"] = json!(value.invocation);
    Ok(row)
}
fn rustup_invocation(env: &BTreeMap<String, String>) -> Result<Tool, String> {
    let value = tool(env, "rustup")?;
    if value.invocation != Path::new(&env_string(env, "CARGO_HOME")?).join("bin/rustup") {
        return Err("Rustup is not first at the exact Cargo-home invocation".into());
    }
    Ok(value)
}
fn compiler_metadata_valid(rustc: &str, cargo: &str, cfg: &str) -> Result<(), String> {
    if !rustc.lines().any(|s| s == "release: 1.99.0")
        || !rustc.lines().any(|s| s == format!("host: {HOST}"))
        || !cargo.starts_with("cargo 1.99.0 ")
        || ![
            "target_pointer_width=\"32\"",
            "target_feature=\"fxsr\"",
            "target_feature=\"sse\"",
            "target_feature=\"sse2\"",
        ]
        .iter()
        .all(|wanted| cfg.lines().any(|l| l == *wanted))
    {
        return Err("Actual hosted32 compiler host/version or Pentium4 cfg differs".into());
    }
    Ok(())
}
fn roots(expected: &str, installed: &str) -> Result<Value, String> {
    let mut wanted = BTreeMap::new();
    for line in expected.lines() {
        let pair: Vec<_> = line.split('\t').collect();
        if pair.len() != 2
            || !ROOT_PACKAGES.contains(&pair[0])
            || pair[1].is_empty()
            || pair[1].len() > 256
            || !pair[1]
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".+:~_-".contains(&c))
            || wanted.insert(pair[0], pair[1]).is_some()
        {
            return Err("Invalid selected hosted32 APT roots".into());
        }
    }
    if wanted.len() != ROOT_PACKAGES.len() || installed.lines().count() > 4096 {
        return Err("Hosted32 APT root count or inventory bound".into());
    }
    let mut found = BTreeMap::new();
    for line in installed.lines() {
        let parts: Vec<_> = line.split('\t').collect();
        if parts.len() != 3 || parts.iter().any(|s| s.is_empty() || s.len() > 512) {
            return Err("Invalid recorded package inventory".into());
        }
        if ROOT_PACKAGES.contains(&parts[0]) && found.insert(parts[0], parts[1]).is_some() {
            return Err("Duplicate installed APT root".into());
        }
    }
    if wanted != found {
        return Err("Selected APT roots differ from actual installed versions".into());
    }
    Ok(json!(wanted))
}
fn std_inventory(path: &Path) -> Result<Value, String> {
    prospective_parents(path)?;
    let mut files = BTreeMap::new();
    let mut bytes = 0u64;
    for item in fs::read_dir(path)
        .map_err(|_| "Cannot read installed i686 standard libraries")?
        .take(257)
    {
        let item = item.map_err(|_| "Cannot read target std entry")?;
        let name = item
            .file_name()
            .into_string()
            .map_err(|_| "Non-UTF8 target std name")?;
        if files.len() >= 256 || name.len() > 256 {
            return Err("Target std inventory count/name bound".into());
        }
        let before =
            fs::symlink_metadata(item.path()).map_err(|_| "Cannot inspect target std entry")?;
        if !before.is_file() || before.file_type().is_symlink() || before.len() > 64 * 1024 * 1024 {
            return Err("Target std type/size bound".into());
        }
        let row = tool_descriptor(&item.path())?;
        let size = row["bytes"].as_u64().ok_or("Invalid target std size")?;
        bytes = bytes
            .checked_add(size)
            .ok_or("Target std aggregate overflow")?;
        if size > 64 * 1024 * 1024 || bytes > HOST_TOOL_MAX {
            return Err("Target std byte bound".into());
        }
        files.insert(name, row);
    }
    if !["libstd-", "libcore-"].iter().all(|prefix| {
        files
            .keys()
            .any(|n| n.starts_with(prefix) && n.ends_with(".rlib"))
    }) {
        return Err("Missing installed i686 std/core".into());
    }
    Ok(
        json!({"files":files,"bytes":bytes,"qualification":"Installed target supplier byte inventory only; not ABI/CPU execution proof"}),
    )
}

fn identity(metadata: &fs::Metadata) -> [u64; 11] {
    [
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mode() as u64,
        metadata.nlink(),
        metadata.uid() as u64,
        metadata.gid() as u64,
        metadata.mtime() as u64,
        metadata.mtime_nsec() as u64,
        metadata.ctime() as u64,
        metadata.ctime_nsec() as u64,
    ]
}

// Host compiler drivers can exceed 256 MiB. Hash into a fixed buffer, never a tool-sized
// Vec; this separate supplier limit does not change product/archive admission.
fn tool_descriptor(path: &Path) -> Result<Value, String> {
    let path = canonical(path)?;
    let before = fs::symlink_metadata(&path).map_err(|_| "Cannot inspect Hosted32 tool")?;
    if !before.is_file() || before.nlink() == 0 || before.len() > HOST_TOOL_MAX {
        return Err("Invalid or oversized Hosted32 compiler tool".into());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(&path)
        .map_err(|_| "Cannot open Hosted32 compiler tool")?;
    if identity(&before)
        != identity(
            &file
                .metadata()
                .map_err(|_| "Cannot inspect opened Hosted32 tool")?,
        )
    {
        return Err("Hosted32 tool changed before hashing".into());
    }
    let mut digest = Sha256::new();
    let mut count = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|_| "Cannot read Hosted32 compiler tool")?;
        if n == 0 {
            break;
        }
        count = count
            .checked_add(n as u64)
            .ok_or("Hosted32 tool length overflow")?;
        if count > HOST_TOOL_MAX || count > before.len() {
            return Err("Hosted32 tool grew while hashing".into());
        }
        digest.update(&buffer[..n]);
    }
    if count != before.len()
        || identity(&before)
            != identity(
                &file
                    .metadata()
                    .map_err(|_| "Cannot inspect hashed Hosted32 tool")?,
            )
        || identity(&before)
            != identity(&fs::symlink_metadata(&path).map_err(|_| "Hosted32 tool vanished")?)
    {
        return Err("Hosted32 tool changed while hashing".into());
    }
    Ok(
        json!({"path":path,"bytes":count,"sha256":format!("{:x}",digest.finalize()),"identity":{
        "device":before.dev(),"inode":before.ino(),"mode":before.mode(),"links":before.nlink(),
        "mtime_seconds":before.mtime(),"mtime_nanoseconds":before.mtime_nsec(),
        "ctime_seconds":before.ctime(),"ctime_nanoseconds":before.ctime_nsec()}}),
    )
}

fn config_paths(root: &Path, env: &BTreeMap<String, String>) -> Result<BTreeSet<PathBuf>, String> {
    let mut paths = BTreeSet::new();
    for parent in root.join("rust").ancestors() {
        paths.insert(parent.join(".cargo/config"));
        paths.insert(parent.join(".cargo/config.toml"));
    }
    for home in [
        PathBuf::from(env_string(env, "CARGO_HOME")?),
        PathBuf::from(env_string(env, "HOME")?).join(".cargo"),
    ] {
        paths.insert(home.join("config"));
        paths.insert(home.join("config.toml"));
    }
    Ok(paths)
}

fn absent_configs(paths: &BTreeSet<PathBuf>, admitted: &Path) -> Result<Vec<PathBuf>, String> {
    let mut absent = Vec::new();
    for path in paths {
        if path.as_path() == admitted {
            continue;
        }
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => absent.push(path.clone()),
            _ => {
                return Err(
                    "Hosted32 Cargo configuration override is present or unreadable".into(),
                );
            }
        }
    }
    Ok(absent)
}

fn cargo_config(root: &Path, env: &BTreeMap<String, String>) -> Result<Value, String> {
    let admitted = root.join("rust/.cargo/config.toml");
    if digest(&stable(&admitted, 65536)?) != CARGO_CONFIG {
        return Err("Unreviewed Hosted32 workspace Cargo configuration".into());
    }
    let absent = absent_configs(&config_paths(root, env)?, &admitted)?;
    Ok(json!({"admitted":descriptor(&admitted,65536)?,"absent":absent}))
}

pub(super) fn snapshot(root: &Path, p: I686Profile) -> Result<Value, String> {
    let env = selected_env(std::env::vars_os())?;
    let claimed = env_string(&env, "GITHUB_SHA")?;
    let ci = validate_env(&env, root, p, &claimed)?;
    for key in ["HOME", "CARGO_HOME", "RUSTUP_HOME"] {
        if !canonical(Path::new(&env_string(&env, key)?))?.is_dir() {
            return Err("Hosted32 home is not a canonical directory".into());
        }
    }
    let git = tool(&env, "git")?;
    let commit = metadata(
        &env,
        root,
        &git.invocation,
        &["rev-parse", "--verify", "HEAD"],
        4096,
    )?;
    if commit != claimed
        || !metadata(
            &env,
            root,
            &git.invocation,
            &["status", "--porcelain", "--untracked-files=normal"],
            65536,
        )?
        .is_empty()
    {
        return Err("Hosted32 requires clean exact source".into());
    }
    let epoch = metadata(
        &env,
        root,
        &git.invocation,
        &["show", "-s", "--format=%ct", "HEAD"],
        4096,
    )?;
    if !decimal(&epoch) || env_string(&env, "SOURCE_DATE_EPOCH")? != epoch {
        return Err("Hosted32 source epoch differs".into());
    }
    let config = cargo_config(root, &env)?;
    let rustup = rustup_invocation(&env)?;
    let sysroot = canonical(
        &Path::new(&env_string(&env, "RUSTUP_HOME")?)
            .join("toolchains/1.99.0-x86_64-unknown-linux-gnu"),
    )?;
    let mut tools = serde_json::Map::new();
    tools.insert("rustup".into(), tool_witness(&rustup)?);
    let mut actual = BTreeMap::new();
    for name in ["rustc", "cargo"] {
        let shim = tool(&env, name)?;
        if shim.invocation
            != Path::new(&env_string(&env, "CARGO_HOME")?)
                .join("bin")
                .join(name)
            || shim.canonical != rustup.canonical
        {
            return Err("Hosted32 rustup shim selection differs".into());
        }
        tools.insert(format!("{name}_shim"), tool_witness(&shim)?);
        let selected = metadata(
            &env,
            root,
            &rustup.invocation,
            &[
                "which",
                "--toolchain",
                "1.99.0-x86_64-unknown-linux-gnu",
                name,
            ],
            8192,
        )?;
        let selected = canonical(Path::new(&selected))?;
        if selected != sysroot.join("bin").join(name) {
            return Err("Hosted32 selected compiler is outside exact x64 sysroot".into());
        }
        tools.insert(name.into(), tool_descriptor(&selected)?);
        actual.insert(name, selected);
    }
    let rustc = &actual["rustc"];
    let cargo = &actual["cargo"];
    let rustc_text = metadata(&env, root, rustc, &["-vV"], 16384)?;
    let cargo_text = metadata(&env, root, cargo, &["--version"], 4096)?;
    let cfg = metadata(
        &env,
        root,
        rustc,
        &[
            "--print",
            "cfg",
            "--target",
            TARGET,
            "-C",
            "target-cpu=pentium4",
        ],
        65536,
    )?;
    compiler_metadata_valid(&rustc_text, &cargo_text, &cfg)?;
    if canonical(Path::new(&metadata(
        &env,
        root,
        rustc,
        &["--print", "sysroot"],
        8192,
    )?))?
        != sysroot
    {
        return Err("Hosted32 actual compiler sysroot differs".into());
    }
    let mut drivers = Vec::new();
    for (index, entry) in fs::read_dir(sysroot.join("lib"))
        .map_err(|_| "Cannot read host compiler libraries")?
        .take(129)
        .enumerate()
    {
        if index >= 128 {
            return Err("Host compiler library inventory bound".into());
        }
        let entry = entry.map_err(|_| "Cannot read host compiler library entry")?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with("librustc_driver") && n.ends_with(".so"))
        {
            drivers.push(entry.path());
        }
    }
    if drivers.len() != 1 {
        return Err("Hosted32 compiler driver is ambiguous".into());
    }
    tools.insert("rustc_driver".into(), tool_descriptor(&drivers[0])?);
    let std = std_inventory(&sysroot.join("lib/rustlib").join(TARGET).join("lib"))?;
    for name in [
        "gcc",
        "ld",
        "ar",
        "ranlib",
        "make",
        "perl",
        "python3",
        "uname",
        "setarch",
        "dpkg-query",
    ] {
        tools.insert(name.into(), tool_witness(&tool(&env, name)?)?);
    }
    let gcc = tool(&env, "gcc")?;
    let uname = tool(&env, "uname")?;
    if metadata(&env, root, &uname.invocation, &["-m"], 4096)? != "x86_64"
        || metadata(&env, root, &gcc.invocation, &["-dumpmachine"], 4096)? != "x86_64-linux-gnu"
    {
        return Err("Hosted32 real compiler/runner host differs".into());
    }
    let out = root.join("artifacts/rust-rewrite/runtime32");
    let mut receipts = serde_json::Map::new();
    for name in [
        "apt-policy.txt",
        "requested-root-versions.tsv",
        "installed-packages.tsv",
        "apt-index-targets.txt",
        "gcc-version.txt",
        "perl-ipc-cmd.txt",
        "make-version.txt",
        "python-version.txt",
        "os-release.txt",
        "kernel.txt",
        "rustc-version.txt",
        "cargo-version.txt",
        "rustup-version.txt",
        "target-cfg.txt",
        "host-uname.txt",
        "personality-uname.txt",
        "personality-rustc-version.txt",
        "source-commit.txt",
        "source-status.txt",
    ] {
        receipts.insert(name.into(), descriptor(&out.join(name), INPUT_MAX)?);
    }
    let selected = String::from_utf8(stable(&out.join("requested-root-versions.tsv"), 65536)?)
        .map_err(|_| "Invalid APT selected-root UTF8")?;
    let installed = String::from_utf8(stable(&out.join("installed-packages.tsv"), INPUT_MAX)?)
        .map_err(|_| "Invalid installed inventory UTF8")?;
    let dpkg = tool(&env, "dpkg-query")?;
    let live = metadata(
        &env,
        root,
        &dpkg.invocation,
        &["-W", "-f=${Package}\t${Version}\t${Architecture}\n"],
        INPUT_MAX as usize,
    )?;
    if live != installed.trim() {
        return Err("Hosted32 installed inventory changed since preflight".into());
    }
    let roots = roots(&selected, &installed)?;
    let mut sysroot_files = serde_json::Map::new();
    for name in ["crt1.o", "crti.o", "crtn.o", "libc.so", "libgcc.a"] {
        let arg = format!("-print-file-name={name}");
        let path = metadata(&env, root, &gcc.invocation, &["-m32", &arg], 8192)
            .map_err(|_| format!("Hosted32 fixed GCC linker metadata failed for {name}"))?;
        let supplier = compiler_reported_linker_file(&path)
            .map_err(|reason| format!("Hosted32 fixed GCC linker supplier {name}: {reason}"))?;
        sysroot_files.insert(name.into(), supplier);
    }
    let mut files = serde_json::Map::new();
    for name in [
        "rust/Cargo.lock",
        "rust/Cargo.toml",
        "rust/rust-toolchain.toml",
        WORKFLOW32,
        "rust/.cargo/config.toml",
    ] {
        files.insert(name.into(), descriptor(&root.join(name), INPUT_MAX)?);
    }
    let runner_source = descriptor(
        &root.join("rust/distribution/ci/runtime32-proof.py"),
        INPUT_MAX,
    )?;
    Ok(
        json!({"schema_version":1,"kind":"native_binary_build_inputs","profile":"i686-hosted-v1","target":TARGET,"source_commit":commit,"source_dirty":false,"cargo_lock_sha256":files["rust/Cargo.lock"]["sha256"],"source_date_epoch":epoch,"ci":ci,"environment":env,"files":files,"cargo_config":config,"builder":null,
        "compiler":{"tools":tools,"sysroot":sysroot,"rustc_text":rustc_text,"cargo_text":cargo_text,"target_cfg":cfg,"target_std":std,"rustup_settings":descriptor(&Path::new(&env_string(&env,"RUSTUP_HOME")?).join("settings.toml"),INPUT_MAX)?},
        "supplier":{"kind":"hosted_ubuntu_multilib_observation","selected_roots":roots,"preflight_receipts":receipts,"installed_inventory_sha256":digest(live.as_bytes()),"gcc_text":metadata(&env,root,&gcc.invocation,&["--version"],16384)?,"gcc_m32_macros":metadata(&env,root,&gcc.invocation,&["-m32","-dM","-E","-"],65536)?,"linker_files":sysroot_files,"qualification":"Exact selected roots from rolling signed APT indexes; no immutable image/full package-byte or complete C/library closure claim"},
        "runner_source":runner_source,"cpu_policy":{"portable_cpu":false,"rust_cpu":"pentium4","native_c_assembly":"Observed defaults only; unqualified"},
        "command":["cargo","build","--release","--target",TARGET,"--package","claude-autorouter","--offline","--locked","--message-format=json"],"working_directory":root.join("rust")}),
    )
}
fn validate_inspection(value: &Value) -> Result<(), String> {
    if value["format"] != "ELF"
        || value["target"] != TARGET
        || value["inspection_schema_version"] != 2
        || value["interpreter"] != "/lib/ld-linux.so.2"
        || value["rpath"] != Value::Null
        || value["runpath"] != Value::Null
    {
        return Err("Hosted32 ELF/interpreter/search-path policy differs".into());
    }
    let libs = value["dynamic_libraries"]
        .as_array()
        .ok_or("Missing ELF32 dependencies")?;
    for lib in libs {
        let name = lib
            .as_str()
            .ok_or("Invalid ELF32 library")?
            .rsplit('/')
            .next()
            .unwrap_or("");
        if ["libssl", "libcrypto"].iter().any(|base| {
            name == *base
                || name.starts_with(&format!("{base}."))
                || name.starts_with(&format!("{base}-"))
        }) {
            return Err("Hosted32 artifact depends on host OpenSSL".into());
        }
    }
    Ok(())
}
pub(super) fn inspect(_: I686Profile, binary: &[u8]) -> Result<Value, String> {
    let value = crate::provenance_binary::inspect(TARGET, binary)?;
    validate_inspection(&value)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "autorouter-i686-draft-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&p).unwrap();
            Self(p.canonicalize().unwrap())
        }
        fn file(&self, name: &str, body: &[u8]) -> PathBuf {
            let p = self.0.join(name);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, body).unwrap();
            p
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn env(root: &Path) -> BTreeMap<String, String> {
        let mut e = BTreeMap::from(
            [
                ("CARGO_INCREMENTAL", "0"),
                ("CARGO_PROFILE_DEV_DEBUG", "0"),
                ("CARGO_PROFILE_TEST_DEBUG", "0"),
                ("CARGO_BUILD_JOBS", "4"),
                ("CARGO_BUILD_TARGET", TARGET),
                ("CARGO_NET_OFFLINE", "true"),
                ("CARGO_TERM_COLOR", "never"),
                ("CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_LINKER", "gcc"),
                ("RUSTUP_TOOLCHAIN", "1.99.0-x86_64-unknown-linux-gnu"),
                (
                    "AUTOROUTER_RUNTIME32_RUST_HOST",
                    "1.99.0-x86_64-unknown-linux-gnu",
                ),
                ("RUSTFLAGS", RUSTFLAGS32),
                ("OPENSSL_NO_VENDOR", "0"),
                ("OPENSSL_STATIC", "1"),
                ("OPENSSL_SRC_PERL", "/usr/bin/perl"),
                ("AUTOROUTER_BUILD_TARGET", TARGET),
                ("AUTOROUTER_BUILD_MACHINE", "x86_64"),
                ("AUTOROUTER_BUILD_RUNNER", "ubuntu-24.04"),
                ("RUNNER_OS", "Linux"),
                ("RUNNER_ARCH", "X64"),
                ("RUNNER_ENVIRONMENT", "github-hosted"),
                ("GITHUB_ACTIONS", "true"),
                ("GITHUB_REPOSITORY", REPOSITORY),
                ("GITHUB_SERVER_URL", "https://github.com"),
                ("GITHUB_JOB", "runtime32"),
                ("GITHUB_RUN_ID", "1"),
                ("GITHUB_RUN_ATTEMPT", "1"),
                ("SOURCE_DATE_EPOCH", "1"),
                ("HOME", "/synthetic/home"),
                ("CARGO_HOME", "/synthetic/home/.cargo"),
                ("RUSTUP_HOME", "/synthetic/home/.rustup"),
                ("PATH", "/usr/bin:/bin"),
            ]
            .map(|(k, v)| (k.to_owned(), v.to_owned())),
        );
        e.insert("GITHUB_SHA".into(), "a".repeat(40));
        e.insert("GITHUB_WORKFLOW_SHA".into(), "b".repeat(40));
        e.insert(
            "GITHUB_WORKFLOW_REF".into(),
            format!("{REPOSITORY}/{WORKFLOW32}@refs/pull/16/merge"),
        );
        e.insert(
            "CARGO_TARGET_DIR".into(),
            root.join("artifacts/rust-rewrite/runtime32/target")
                .to_str()
                .unwrap()
                .into(),
        );
        e.insert(
            "CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_RUNNER".into(),
            format!(
                "python3 {}/rust/distribution/ci/runtime32-proof.py run-test",
                root.display()
            ),
        );
        e
    }
    fn state(root: &Path) -> Value {
        json!({"environment":{"CARGO_TARGET_DIR":root.join("artifacts/rust-rewrite/runtime32/target")}})
    }
    #[test]
    fn only_i686_profile_is_admitted() {
        assert!(profile(TARGET).is_ok());
        for t in [HOST, "aarch64-unknown-linux-gnu", "i686-unknown-linux-musl"] {
            assert!(profile(t).is_err());
        }
    }
    #[test]
    fn initial_cpu_policy_is_false_existing_families_stay_true() {
        assert!(!BuildProfile::reviewed(TARGET).unwrap().portable_cpu());
        for t in [
            HOST,
            "aarch64-unknown-linux-gnu",
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "x86_64-unknown-linux-musl",
            "aarch64-unknown-linux-musl",
        ] {
            assert!(BuildProfile::reviewed(t).unwrap().portable_cpu());
        }
    }
    #[test]
    fn parent_profile_pair_and_builder_guards() {
        let v = json!({"target":TARGET,"profile":"i686-hosted-v1","builder":null});
        assert!(BuildProfile::from_inputs(&v).is_ok());
        for family in ["gnu-glibc228-v1", "macos-hosted-v1", "musl-alpine321-v1"] {
            let mut bad = v.clone();
            bad["profile"] = json!(family);
            assert!(BuildProfile::from_inputs(&bad).is_err());
        }
        let mut bad = v;
        bad["builder"] = json!({});
        assert!(BuildProfile::from_inputs(&bad).is_err());
    }
    #[test]
    fn valid_closed_environment_binds_target_and_runner() {
        let e = env(Path::new("/synthetic/repo"));
        assert!(
            validate_env(
                &e,
                Path::new("/synthetic/repo"),
                profile(TARGET).unwrap(),
                &"a".repeat(40)
            )
            .is_ok()
        );
    }
    #[test]
    fn unknown_compiler_flags_and_arbitrary_runner_refuse() {
        for (key, value) in [
            ("RUSTFLAGS", "-C target-cpu=native"),
            ("CC", "/other/gcc"),
            ("CFLAGS_i686_unknown_linux_gnu", "-march=native"),
            ("CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_RUNNER", "sh untrusted"),
            ("OPENSSL_DIR", "/other"),
        ] {
            let mut e = env(Path::new("/synthetic/repo"));
            e.insert(key.into(), value.into());
            assert!(
                validate_env(
                    &e,
                    Path::new("/synthetic/repo"),
                    profile(TARGET).unwrap(),
                    &"a".repeat(40)
                )
                .is_err()
            );
        }
    }
    #[test]
    fn workflow_host_and_attempt_mismatch_refuse() {
        for key in [
            "GITHUB_JOB",
            "RUNNER_ARCH",
            "AUTOROUTER_BUILD_MACHINE",
            "RUSTUP_TOOLCHAIN",
            "GITHUB_SHA",
            "GITHUB_RUN_ATTEMPT",
        ] {
            let mut e = env(Path::new("/synthetic/repo"));
            e.insert(key.into(), "wrong".into());
            assert!(
                validate_env(
                    &e,
                    Path::new("/synthetic/repo"),
                    profile(TARGET).unwrap(),
                    &"a".repeat(40)
                )
                .is_err()
            );
        }
    }
    #[test]
    fn metadata_retains_home_and_toolchain_without_extra_overrides() {
        let e = env(Path::new("/synthetic/repo"));
        let m = metadata_environment(&e).unwrap();
        for k in ["HOME", "CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN"] {
            assert_eq!(m[k], e[k]);
        }
        assert!(!m.contains_key("RUSTFLAGS"));
        assert_eq!(m["GIT_CONFIG_NOSYSTEM"], "1");
        assert_eq!(m["GIT_CONFIG_GLOBAL"], "/dev/null");
        assert_eq!(m["GIT_NO_REPLACE_OBJECTS"], "1");
        assert_eq!(m.len(), 9);
    }
    #[test]
    fn selector_limits_and_duplicate_names_refuse() {
        let pair = (OsString::from("HOME"), OsString::from("/synthetic"));
        assert!(selected_env([pair.clone(), pair]).is_err());
        assert!(
            selected_env([(OsString::from("HOME"), OsString::from("x".repeat(8193)))]).is_err()
        );
    }
    #[test]
    fn real_home_is_required_for_metadata() {
        let mut e = env(Path::new("/synthetic/repo"));
        e.remove("HOME");
        assert!(metadata_environment(&e).is_err());
    }
    #[test]
    fn selected_apt_roots_match_installed_versions() {
        let expected = "gcc-multilib\t1\nlibc6-dev-i386\t2\nmake\t3\nperl\t4\n";
        let installed =
            "gcc-multilib\t1\tamd64\nlibc6-dev-i386\t2\tamd64\nmake\t3\tamd64\nperl\t4\tamd64\n";
        assert!(roots(expected, installed).is_ok());
        assert!(roots(expected, &installed.replace("perl\t4", "perl\t5")).is_err());
        assert!(roots(&(expected.to_owned() + "perl\t4\n"), installed).is_err());
    }
    #[test]
    fn root_version_unknown_or_missing_refuses() {
        assert!(roots("gcc-multilib\t1\n", "gcc-multilib\t1\tamd64\n").is_err());
        assert!(roots("unknown\t1\n", "").is_err());
    }
    #[test]
    fn both_host_and_target_trees_must_be_cold() {
        let s = Scratch::new();
        let p = profile(TARGET).unwrap();
        let b = state(&s.0);
        assert!(fresh_release_tree(&s.0, &b, p).is_ok());
        for path in ["release", "i686-unknown-linux-gnu/release"] {
            let t =
                s.0.join("artifacts/rust-rewrite/runtime32/target")
                    .join(path);
            fs::create_dir_all(&t).unwrap();
            assert!(fresh_release_tree(&s.0, &b, p).is_err());
            fs::remove_dir(&t).unwrap();
        }
    }
    #[test]
    fn symlinked_target_parent_refuses() {
        let s = Scratch::new();
        let real = s.0.join("real");
        fs::create_dir(&real).unwrap();
        fs::create_dir_all(s.0.join("artifacts/rust-rewrite")).unwrap();
        std::os::unix::fs::symlink(&real, s.0.join("artifacts/rust-rewrite/runtime32")).unwrap();
        assert!(fresh_release_tree(&s.0, &state(&s.0), profile(TARGET).unwrap()).is_err());
    }
    #[test]
    fn parent_selects_only_target_release_binary() {
        let s = Scratch::new();
        let p = BuildProfile::reviewed(TARGET).unwrap();
        let f=s.file("artifacts/rust-rewrite/runtime32/target/i686-unknown-linux-gnu/release/claude-autorouter",b"target");
        assert_eq!(
            p.executable(&s.0, &state(&s.0), &json!({"binary":{"executable":f}}))
                .unwrap(),
            f
        );
        let host = s.file(
            "artifacts/rust-rewrite/runtime32/target/release/claude-autorouter",
            b"host",
        );
        assert!(
            p.executable(&s.0, &state(&s.0), &json!({"binary":{"executable":host}}))
                .is_err()
        );
    }
    #[test]
    fn actual_x64_compiler_and_i686_sse2_metadata_required() {
        let good = "target_pointer_width=\"32\"\ntarget_feature=\"fxsr\"\ntarget_feature=\"sse\"\ntarget_feature=\"sse2\"";
        assert!(
            compiler_metadata_valid(
                &format!("release: 1.99.0\nhost: {HOST}"),
                "cargo 1.99.0 (fixed)",
                good
            )
            .is_ok()
        );
        assert!(
            compiler_metadata_valid(
                "release: 1.99.0\nhost: i686-unknown-linux-gnu",
                "cargo 1.99.0 (fixed)",
                good
            )
            .is_err()
        );
        assert!(
            compiler_metadata_valid(
                &format!("release: 1.99.0\nhost: {HOST}"),
                "cargo 1.99.0 (fixed)",
                &good.replace("sse2", "avx2")
            )
            .is_err()
        );
    }
    #[test]
    fn target_std_has_real_core_and_std_inventory() {
        let s = Scratch::new();
        s.file("libcore-fake.rlib", b"core");
        s.file("libstd-fake.rlib", b"std");
        assert!(std_inventory(&s.0).is_ok());
        fs::remove_file(s.0.join("libcore-fake.rlib")).unwrap();
        assert!(std_inventory(&s.0).is_err());
    }
    #[test]
    fn tool_witness_preserves_alias_name_and_canonical_bytes() {
        let s = Scratch::new();
        let real = s.file("rustup-init", b"metadata");
        fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&real, s.0.join("rustup")).unwrap();
        let e = BTreeMap::from([("PATH".into(), s.0.to_str().unwrap().into())]);
        let t = tool(&e, "rustup").unwrap();
        assert_eq!(t.invocation, s.0.join("rustup"));
        assert_eq!(t.canonical, real);
        let w = tool_witness(&t).unwrap();
        assert_eq!(w["invocation_path"], json!(s.0.join("rustup")));
    }
    #[test]
    fn symlinked_or_oversized_std_entry_refuses() {
        let s = Scratch::new();
        let real = s.file("outside", b"std");
        std::os::unix::fs::symlink(&real, s.0.join("libstd-x.rlib")).unwrap();
        assert!(std_inventory(&s.0).is_err());
        fs::remove_file(s.0.join("libstd-x.rlib")).unwrap();
        let big = fs::File::create(s.0.join("libcore-big.rlib")).unwrap();
        big.set_len(64 * 1024 * 1024 + 1).unwrap();
        assert!(std_inventory(&s.0).is_err());
    }
    #[test]
    fn observed_elf_policy_does_not_invent_glibc228() {
        let good = json!({"format":"ELF","target":TARGET,"inspection_schema_version":2,"interpreter":"/lib/ld-linux.so.2","rpath":null,"runpath":null,"dynamic_libraries":["libc.so.6"],"minimum_glibc":"2.39","version_requirements":[{"library":"libc.so.6","versions":[{"name":"GLIBC_2.39","weak":false}]}]});
        assert!(validate_inspection(&good).is_ok());
        for (k, v) in [
            ("interpreter", json!("/other/loader")),
            ("rpath", json!("/build")),
            ("dynamic_libraries", json!(["libssl.so.3"])),
        ] {
            let mut bad = good.clone();
            bad[k] = v;
            assert!(validate_inspection(&bad).is_err());
        }
    }
    #[test]
    fn input_profile_tampering_does_not_acquire_true_cpu_claim() {
        let v = json!({"target":TARGET,"profile":"i686-hosted-v1","builder":null});
        assert!(BuildProfile::from_inputs(&v).is_ok());
        let mut bad = v;
        bad["target"] = json!(HOST);
        assert!(BuildProfile::from_inputs(&bad).is_err());
        assert!(!BuildProfile::reviewed(TARGET).unwrap().portable_cpu());
    }

    #[test]
    fn compiler_linker_report_retains_parent_spelling_and_canonical_file() {
        let s = Scratch::new();
        let real = s.file("lib/libc.so", b"supplier");
        fs::create_dir(s.0.join("lib/child")).unwrap();
        let reported = s.0.join("lib/child/../libc.so");
        assert!(
            canonical(&reported).is_err(),
            "shared lexical guard stays strict"
        );
        let row = compiler_reported_linker_file(reported.to_str().unwrap()).unwrap();
        assert_eq!(row["compiler_reported_path"], reported.to_str().unwrap());
        assert_eq!(row["path"], json!(real));
        assert_eq!(row["bytes"], 8);
        assert_eq!(row["sha256"], digest(b"supplier"));
    }

    #[test]
    fn compiler_linker_report_resolves_symlink_before_parent_not_lexically() {
        let s = Scratch::new();
        let real = s.file("real/libc.so", b"real----");
        let decoy = s.file("libc.so", b"decoy---");
        let leaf = s.0.join("real/leaf");
        fs::create_dir(&leaf).unwrap();
        std::os::unix::fs::symlink(&leaf, s.0.join("alias")).unwrap();
        let reported = s.0.join("alias/../libc.so");
        assert!(canonical(&reported).is_err());
        let row = compiler_reported_linker_file(reported.to_str().unwrap()).unwrap();
        assert_eq!(row["path"], json!(real));
        assert_ne!(row["path"], json!(decoy));
        assert_eq!(row["sha256"], digest(b"real----"));
        assert_ne!(row["sha256"], digest(b"decoy---"));
        assert_eq!(row["compiler_reported_path"], reported.to_str().unwrap());
    }

    #[test]
    fn compiler_linker_report_rejects_unresolved_relative_and_control_spellings() {
        let s = Scratch::new();
        let real = s.file("libc.so", b"supplier");
        let text = real.to_str().unwrap();
        for reported in ["", "libc.so", "./libc.so", "../libc.so"] {
            assert!(compiler_reported_linker_file(reported).is_err());
        }
        for control in ['\0', '\t', '\n', '\r', '\u{7f}', '\u{85}'] {
            let reported = format!("{text}{control}");
            let error = compiler_reported_linker_file(&reported).unwrap_err();
            assert_eq!(
                error,
                "Invalid Hosted32 compiler-reported linker supplier path"
            );
            assert!(!error.contains(text));
        }
        assert!(compiler_reported_linker_file(&format!("/{}", "x".repeat(8192))).is_err());
    }

    #[test]
    fn compiler_linker_report_rejects_missing_directories_and_fifo_before_read() {
        let s = Scratch::new();
        for path in [s.0.join("missing"), s.0.clone()] {
            let error = compiler_reported_linker_file(path.to_str().unwrap()).unwrap_err();
            assert!(!error.contains(s.0.to_str().unwrap()));
        }
        let fifo = s.0.join("fifo");
        nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR).unwrap();
        assert!(compiler_reported_linker_file(fifo.to_str().unwrap()).is_err());
        std::os::unix::fs::symlink(s.0.join("missing"), s.0.join("dangling")).unwrap();
        assert!(compiler_reported_linker_file(s.0.join("dangling").to_str().unwrap()).is_err());
    }

    #[test]
    fn compiler_linker_report_rejects_bad_resolved_names_and_sparse_oversize() {
        use std::os::unix::ffi::OsStringExt;
        let s = Scratch::new();
        let non_utf8 = s.0.join(OsString::from_vec(b"nonutf8-\xff".to_vec()));
        // APFS refuses non-UTF-8 names. All hosts test the production guard;
        // Linux also exercises filesystem resolution through a real symlink.
        assert_eq!(
            resolved_linker_file(&non_utf8).unwrap_err(),
            "Invalid resolved Hosted32 linker supplier path"
        );
        #[cfg(target_os = "linux")]
        {
            fs::write(&non_utf8, b"supplier").unwrap();
            let alias = s.0.join("utf8-alias");
            std::os::unix::fs::symlink(&non_utf8, &alias).unwrap();
            assert!(compiler_reported_linker_file(alias.to_str().unwrap()).is_err());
        }
        let control = s.file("control\nname", b"supplier");
        let clean = s.0.join("clean-alias");
        std::os::unix::fs::symlink(&control, &clean).unwrap();
        assert!(compiler_reported_linker_file(clean.to_str().unwrap()).is_err());
        let big = s.0.join("oversized");
        fs::File::create(&big)
            .unwrap()
            .set_len(HOST_TOOL_MAX + 1)
            .unwrap();
        assert!(compiler_reported_linker_file(big.to_str().unwrap()).is_err());
    }

    #[test]
    fn compiler_linker_report_binds_changed_supplier_bytes_at_same_spelling() {
        let s = Scratch::new();
        let real = s.file("libc.so", b"before!!");
        fs::create_dir(s.0.join("child")).unwrap();
        let reported = s.0.join("child/../libc.so");
        let first = compiler_reported_linker_file(reported.to_str().unwrap()).unwrap();
        fs::write(&real, b"after!!!").unwrap();
        let second = compiler_reported_linker_file(reported.to_str().unwrap()).unwrap();
        assert_eq!(
            first["compiler_reported_path"],
            second["compiler_reported_path"]
        );
        assert_eq!(first["path"], second["path"]);
        assert_eq!(first["bytes"], second["bytes"]);
        assert_ne!(first["sha256"], second["sha256"]);
        assert_ne!(
            first, second,
            "before/after provenance snapshots detect supplier drift"
        );
    }
}
