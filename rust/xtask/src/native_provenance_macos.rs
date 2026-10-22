//! Reviewed hosted-Mac build witnesses; no builds, downloads or product calls.
use super::*;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::io::Read;

const MAC_WORKFLOW: &str = ".github/workflows/ci.yml";
const MAC_TOOL_MAX: u64 = 512 * 1024 * 1024;
const MAC_EXTRA: &[&str] = &[
    "HOME",
    "SDKROOT",
    "DEVELOPER_DIR",
    "RUNNER_OS",
    "RUNNER_ARCH",
    "RUNNER_ENVIRONMENT",
    "ImageOS",
    "ImageVersion",
];

#[derive(Clone, Copy)]
pub(super) struct MacProfile {
    pub(super) target: &'static str,
    pub(super) rustflags: &'static str,
    machine: &'static str,
    runner: &'static str,
    runner_arch: &'static str,
}

pub(super) fn profile(target: &str) -> Result<MacProfile, String> {
    match target {
        "aarch64-apple-darwin" => Ok(MacProfile {
            target: "aarch64-apple-darwin",
            machine: "arm64",
            runner: "macos-15",
            runner_arch: "ARM64",
            rustflags: "-C target-cpu=apple-m1 -C linker=/usr/bin/clang",
        }),
        "x86_64-apple-darwin" => Ok(MacProfile {
            target: "x86_64-apple-darwin",
            machine: "x86_64",
            runner: "macos-15-intel",
            runner_arch: "X64",
            rustflags: "-C target-cpu=x86-64 -C linker=/usr/bin/clang",
        }),
        _ => Err("Native provenance target has no reviewed Mac build profile".into()),
    }
}

// The GNU selector is unchanged. Its ignored HOME/SDK/runner variables become
// public witnesses only on this branch; total bounds still cover the full map.
fn select_macos_env(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<BTreeMap<String, String>, String> {
    let mut selected = BTreeMap::new();
    let mut total = 0usize;
    for (name, value) in vars {
        let name_text = name.to_str().ok_or("Non-UTF8 Mac build environment name")?;
        let rows = if MAC_EXTRA.contains(&name_text) {
            BTreeMap::from([(
                name_text.to_owned(),
                value
                    .into_string()
                    .map_err(|_| "Non-UTF8 public Mac build variable")?,
            )])
        } else {
            super::select_env(std::iter::once((name, value)))?
        };
        for (name, value) in rows {
            total = total
                .checked_add(name.len())
                .and_then(|n| n.checked_add(value.len()))
                .ok_or("Public Mac build environment size overflow")?;
            if name.len() > 128
                || value.len() > 8192
                || value.contains(['\0', '\r', '\n'])
                || selected.len() >= 64
                || total > 65536
            {
                return Err("Invalid or oversized public Mac build environment".into());
            }
            if selected.insert(name, value).is_some() {
                return Err("Duplicate public Mac build variable".into());
            }
        }
    }
    Ok(selected)
}

fn validate_env(
    env: &BTreeMap<String, String>,
    p: MacProfile,
    commit: &str,
) -> Result<Value, String> {
    let expected = BTreeMap::from([
        ("CARGO_INCREMENTAL", "0"),
        ("RUSTUP_TOOLCHAIN", "1.99.0"),
        ("CFLAGS", "-O2"),
        ("CXXFLAGS", "-O2"),
        ("RUSTFLAGS", p.rustflags),
        ("OPENSSL_NO_VENDOR", "0"),
        ("OPENSSL_STATIC", "1"),
        ("OPENSSL_SRC_PERL", "/usr/bin/perl"),
        ("MACOSX_DEPLOYMENT_TARGET", "11.0"),
        ("AUTOROUTER_BUILD_TARGET", p.target),
        ("AUTOROUTER_BUILD_MACHINE", p.machine),
        ("AUTOROUTER_BUILD_RUNNER", p.runner),
        ("RUNNER_OS", "macOS"),
        ("RUNNER_ARCH", p.runner_arch),
        ("RUNNER_ENVIRONMENT", "github-hosted"),
    ]);
    for (name, value) in &expected {
        if env.get(*name).map(String::as_str) != Some(*value) {
            return Err(format!("Unreviewed Mac build variable: {name}"));
        }
    }
    let permitted = [
        "CC",
        "CXX",
        "PATH",
        "HOME",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "DEVELOPER_DIR",
        "SDKROOT",
        "CARGO_BUILD_JOBS",
        "CARGO_NET_OFFLINE",
        "CARGO_TARGET_DIR",
        "ImageOS",
        "ImageVersion",
    ];
    for name in env.keys() {
        if !PUBLIC_GITHUB.contains(&name.as_str())
            && !expected.contains_key(name.as_str())
            && !permitted.contains(&name.as_str())
        {
            return Err(format!("Unexpected Mac compiler/build override: {name}"));
        }
    }
    let path = env_string(env, "PATH")?;
    if std::env::split_paths(&path).any(|part| !part.is_absolute()) {
        return Err("Mac compiler PATH must contain only absolute directories".into());
    }
    for name in [
        "HOME",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "DEVELOPER_DIR",
        "SDKROOT",
    ] {
        let path = canonical(Path::new(&env_string(env, name)?))?;
        if !path.is_dir() {
            return Err("Mac build home/SDK path is not a directory".into());
        }
    }
    if let Some(jobs) = env.get("CARGO_BUILD_JOBS")
        && !decimal(jobs)
    {
        return Err("Invalid bounded Mac build jobs".into());
    }
    if let Some(value) = env.get("CARGO_NET_OFFLINE")
        && value != "true"
    {
        return Err("Unexpected Mac offline override".into());
    }
    if let Some(value) = env.get("CARGO_TARGET_DIR")
        && !Path::new(value).is_absolute()
    {
        return Err("Mac target directory must be absolute".into());
    }
    if env_string(env, "GITHUB_ACTIONS")? != "true"
        || env_string(env, "GITHUB_REPOSITORY")? != REPOSITORY
        || env_string(env, "GITHUB_SERVER_URL")? != "https://github.com"
        || env_string(env, "GITHUB_SHA")? != commit
        || env_string(env, "GITHUB_JOB")? != "rust"
    {
        return Err("Mac provenance requires matching original CI workflow context".into());
    }
    let workflow_ref = env_string(env, "GITHUB_WORKFLOW_REF")?;
    let suffix = workflow_ref
        .strip_prefix(&format!("{REPOSITORY}/{MAC_WORKFLOW}@"))
        .ok_or("Unreviewed Mac provenance workflow identity")?;
    if !["refs/heads/", "refs/tags/", "refs/pull/"]
        .iter()
        .any(|&p| suffix.starts_with(p))
        || suffix.ends_with('/')
        || suffix.contains("..")
        || suffix.contains("//")
        || !suffix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
    {
        return Err("Unreviewed Mac provenance workflow identity".into());
    }
    let workflow_sha = env_string(env, "GITHUB_WORKFLOW_SHA")?;
    let run_id = env_string(env, "GITHUB_RUN_ID")?;
    let attempt = env_string(env, "GITHUB_RUN_ATTEMPT")?;
    if !hex(&workflow_sha, 40) || !decimal(&run_id) || !decimal(&attempt) {
        return Err("Invalid Mac workflow/run metadata".into());
    }
    Ok(
        json!({"repository":REPOSITORY,"checkout_commit":commit,"workflow_ref":workflow_ref,
        "workflow_sha":workflow_sha,"workflow_run":format!("https://github.com/{REPOSITORY}/actions/runs/{run_id}/attempts/{attempt}"),
        "run_id":run_id,"run_attempt":attempt,"job":"rust","runner":p.runner}),
    )
}

// Diagnostics expose only reviewed tool names and fixed metadata arguments.
// Never include an executable path, environment value or subprocess payload.
fn metadata_failure(program: &Path, args: &[&str], detail: &str) -> String {
    const TOOLS: &[&str] = &[
        "rustup",
        "rustc",
        "cargo",
        "git",
        "uname",
        "xcrun",
        "xcode-select",
        "xcodebuild",
        "clang",
        "clang++",
        "perl",
    ];
    const ARGUMENTS: &[&[&str]] = &[
        &["-vV"],
        &["--version"],
        &["--print", "sysroot"],
        &["which", "--toolchain", "1.99.0", "rustc"],
        &["which", "--toolchain", "1.99.0", "cargo"],
        &["-p"],
        &["--sdk", "macosx", "--show-sdk-path"],
        &["--sdk", "macosx", "--show-sdk-version"],
        &["--sdk", "macosx", "--find", "clang"],
        &["--sdk", "macosx", "--find", "clang++"],
        &["--sdk", "macosx", "--find", "ld"],
        &["--sdk", "macosx", "--find", "ar"],
        &["-version"],
        &["-V"],
        &["-m"],
        &["-srvm"],
        &["rev-parse", "--verify", "HEAD"],
        &["status", "--porcelain", "--untracked-files=normal"],
        &["show", "-s", "--format=%ct", "HEAD"],
    ];
    let basename = program
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let tool = if TOOLS.contains(&basename) {
        basename
    } else {
        "metadata-tool"
    };
    let arguments = if ARGUMENTS.contains(&args) {
        args.join(" ")
    } else {
        "<unlisted metadata arguments>".into()
    };
    // process::capture returns sanitized status/deadline/IO descriptions, never
    // its captured stderr/stdout or Command environment/argument Debug output.
    format!("Mac provenance metadata {tool} {arguments}: {detail}")
}

fn mac_git(env: &BTreeMap<String, String>, root: &Path, args: &[&str]) -> Result<String, String> {
    super::git(env, root, args).map_err(|error| metadata_failure(Path::new("git"), args, &error))
}

fn mac_system_capture(
    env: &BTreeMap<String, String>,
    root: &Path,
    program: &Path,
    args: &[&str],
    maximum: usize,
) -> Result<String, String> {
    super::capture(env, root, program, args, maximum)
        .map_err(|error| metadata_failure(program, args, &error))
}

fn mac_capture(
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
        .env("LC_ALL", "C");
    for name in [
        "HOME",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "RUSTUP_TOOLCHAIN",
        "DEVELOPER_DIR",
        "SDKROOT",
    ] {
        command.env(name, env_string(env, name)?);
    }
    command.args(args);
    let bytes = crate::process::capture(&mut command, b"", Duration::from_secs(15))
        .map_err(|error| metadata_failure(program, args, &error))?;
    if bytes.len() > maximum {
        return Err(metadata_failure(
            program,
            args,
            "Mac compiler metadata exceeds bound",
        ));
    }
    String::from_utf8(bytes)
        .map(|v| v.trim().to_owned())
        .map_err(|_| metadata_failure(program, args, "Invalid Mac compiler metadata UTF-8"))
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

// Xcode clang can exceed 256 MiB. Hash into a fixed buffer, never a tool-sized
// Vec; this separate supplier limit does not change product/archive admission.
fn tool_descriptor(path: &Path) -> Result<Value, String> {
    let path = canonical(path)?;
    let before = fs::symlink_metadata(&path).map_err(|_| "Cannot inspect Mac tool")?;
    if !before.is_file() || before.nlink() == 0 || before.len() > MAC_TOOL_MAX {
        return Err("Invalid or oversized Mac compiler tool".into());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(&path)
        .map_err(|_| "Cannot open Mac compiler tool")?;
    if identity(&before)
        != identity(
            &file
                .metadata()
                .map_err(|_| "Cannot inspect opened Mac tool")?,
        )
    {
        return Err("Mac tool changed before hashing".into());
    }
    let mut digest = Sha256::new();
    let mut count = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|_| "Cannot read Mac compiler tool")?;
        if n == 0 {
            break;
        }
        count = count
            .checked_add(n as u64)
            .ok_or("Mac tool length overflow")?;
        if count > MAC_TOOL_MAX || count > before.len() {
            return Err("Mac tool grew while hashing".into());
        }
        digest.update(&buffer[..n]);
    }
    if count != before.len()
        || identity(&before)
            != identity(
                &file
                    .metadata()
                    .map_err(|_| "Cannot inspect hashed Mac tool")?,
            )
        || identity(&before)
            != identity(&fs::symlink_metadata(&path).map_err(|_| "Mac tool vanished")?)
    {
        return Err("Mac tool changed while hashing".into());
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
            _ => return Err("Mac Cargo configuration override is present or unreadable".into()),
        }
    }
    Ok(absent)
}

fn no_mac_config_overrides(root: &Path, env: &BTreeMap<String, String>) -> Result<Value, String> {
    let admitted = root.join("rust/.cargo/config.toml");
    if digest(&stable(&admitted, 65536)?) != CARGO_CONFIG {
        return Err("Unreviewed Mac workspace Cargo configuration".into());
    }
    let absent = absent_configs(&config_paths(root, env)?, &admitted)?;
    Ok(json!({"admitted":descriptor(&admitted,65536)?,"absent":absent}))
}

fn selected_path(
    env: &BTreeMap<String, String>,
    root: &Path,
    tool: &str,
    args: &[&str],
) -> Result<PathBuf, String> {
    let value = mac_capture(env, root, Path::new(tool), args, 8192)?;
    let path = Path::new(&value);
    if !path.is_absolute() {
        return Err("Selected Mac tool path is not absolute".into());
    }
    path.canonicalize()
        .map_err(|_| "Cannot resolve selected Mac tool path".into())
}

// Rustup dispatches by argv[0]. Keep its admitted invocation name while
// independently binding the canonical file selected first on compiler PATH.
fn admitted_rustup_invocation(
    env: &BTreeMap<String, String>,
    cargo_home: &Path,
) -> Result<(PathBuf, PathBuf), String> {
    let invocation = cargo_home.join("bin/rustup");
    let canonical = resolve_tool(env, "rustup")?;
    if canonical
        != invocation
            .canonicalize()
            .map_err(|_| "Cannot resolve admitted rustup")?
    {
        return Err("Rustup is not the admitted Cargo-home executable".into());
    }
    Ok((invocation, canonical))
}

pub(super) fn snapshot(root: &Path, p: MacProfile) -> Result<Value, String> {
    let env = select_macos_env(std::env::vars_os())?;
    let claimed = env_string(&env, "GITHUB_SHA")?;
    if !hex(&claimed, 40) {
        return Err("Invalid Mac checkout metadata".into());
    }
    let ci = validate_env(&env, p, &claimed)?;
    let commit = mac_git(&env, root, &["rev-parse", "--verify", "HEAD"])?;
    if commit != claimed
        || !mac_git(
            &env,
            root,
            &["status", "--porcelain", "--untracked-files=normal"],
        )?
        .is_empty()
    {
        return Err("Mac provenance requires a clean exact source checkout".into());
    }
    let config = no_mac_config_overrides(root, &env)?;
    let cargo_home = PathBuf::from(env_string(&env, "CARGO_HOME")?);
    let rustup_home = PathBuf::from(env_string(&env, "RUSTUP_HOME")?);
    // Refuse absent installations before invoking rustup, not an automatic
    // install request. Do not invent a GNU dist-archive checksum for rustup.
    let sysroot = canonical(
        &rustup_home
            .join("toolchains")
            .join(format!("1.99.0-{}", p.target)),
    )?;
    let (rustup, rustup_canonical) = admitted_rustup_invocation(&env, &cargo_home)?;
    let mut rustup_witness = tool_descriptor(&rustup_canonical)?;
    rustup_witness["invocation_path"] = json!(rustup);
    let mut tools = serde_json::Map::new();
    tools.insert("rustup".into(), rustup_witness.clone());
    let mut actual = BTreeMap::new();
    for name in ["rustc", "cargo"] {
        let shim = resolve_tool(&env, name)?;
        if shim
            != cargo_home
                .join("bin")
                .join(name)
                .canonicalize()
                .map_err(|_| "Cannot resolve rustup shim")?
        {
            return Err("Rustup shim is not first on compiler PATH".into());
        }
        let witness = tool_descriptor(&shim)?;
        if witness["sha256"] != rustup_witness["sha256"] {
            return Err("Compiler shim is not the witnessed rustup binary".into());
        }
        tools.insert(format!("{name}_shim"), witness);
        let path = selected_path(
            &env,
            root,
            rustup.to_str().ok_or("Non-UTF8 rustup path")?,
            &["which", "--toolchain", "1.99.0", name],
        )?;
        if path != sysroot.join("bin").join(name) {
            return Err("Rustup selected an unexpected actual toolchain executable".into());
        }
        tools.insert(name.into(), tool_descriptor(&path)?);
        actual.insert(name, path);
    }
    let rustc = &actual["rustc"];
    let cargo = &actual["cargo"];
    let rustc_text = mac_capture(&env, root, rustc, &["-vV"], 16384)?;
    if !rustc_text.lines().any(|line| line == "release: 1.99.0")
        || !rustc_text
            .lines()
            .any(|line| line == format!("host: {}", p.target))
        || selected_path(
            &env,
            root,
            rustc.to_str().ok_or("Non-UTF8 compiler path")?,
            &["--print", "sysroot"],
        )? != sysroot
    {
        return Err("Mac actual compiler version/host/sysroot mismatch".into());
    }
    let drivers = fs::read_dir(sysroot.join("lib"))
        .map_err(|_| "Cannot inspect Rust compiler libraries")?
        .take(129)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Cannot inspect Rust compiler library entry")?;
    if drivers.len() > 128 {
        return Err("Rust compiler library inventory exceeds bound".into());
    }
    let drivers: Vec<_> = drivers
        .into_iter()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("librustc_driver") && name.ends_with(".dylib"))
        })
        .collect();
    if drivers.len() != 1 {
        return Err("Mac compiler driver identity is ambiguous".into());
    }
    tools.insert("rustc_driver".into(), tool_descriptor(&drivers[0].path())?);
    for tool in ["git", "uname"] {
        tools.insert(tool.into(), tool_descriptor(&resolve_tool(&env, tool)?)?);
    }
    for tool in [
        "/usr/bin/xcrun",
        "/usr/bin/xcode-select",
        "/usr/bin/xcodebuild",
        "/usr/bin/clang",
        "/usr/bin/clang++",
        "/usr/bin/ld",
        "/usr/bin/ar",
        "/usr/bin/perl",
    ] {
        tools.insert(
            tool.into(),
            tool_descriptor(
                &Path::new(tool)
                    .canonicalize()
                    .map_err(|_| "Cannot resolve Mac system tool")?,
            )?,
        );
    }
    let developer = selected_path(&env, root, "/usr/bin/xcode-select", &["-p"])?;
    let sdk = selected_path(
        &env,
        root,
        "/usr/bin/xcrun",
        &["--sdk", "macosx", "--show-sdk-path"],
    )?;
    if developer != env_string(&env, "DEVELOPER_DIR")?
        || sdk != env_string(&env, "SDKROOT")?
        || !sdk.starts_with(&developer)
    {
        return Err("Selected developer/SDK path does not match admitted build environment".into());
    }
    let sdk_settings = descriptor(&sdk.join("SDKSettings.json"), INPUT_MAX)?;
    let sdk_version = mac_capture(
        &env,
        root,
        Path::new("/usr/bin/xcrun"),
        &["--sdk", "macosx", "--show-sdk-version"],
        4096,
    )?;
    let settings = json_read(&stable(&sdk.join("SDKSettings.json"), INPUT_MAX)?)?;
    if settings["Version"] != sdk_version {
        return Err("SDK settings/version mismatch".into());
    }
    let mut selected_tools = serde_json::Map::new();
    for tool in ["clang", "clang++", "ld", "ar"] {
        let invocation = mac_capture(
            &env,
            root,
            Path::new("/usr/bin/xcrun"),
            &["--sdk", "macosx", "--find", tool],
            8192,
        )?;
        if !Path::new(&invocation).is_absolute() {
            return Err("Xcrun invocation path is not absolute".into());
        }
        let path = Path::new(&invocation)
            .canonicalize()
            .map_err(|_| "Cannot resolve selected SDK tool")?;
        if !path.starts_with(&developer) {
            return Err("Xcrun selected a tool outside admitted developer directory".into());
        }
        let mut witness = tool_descriptor(&path)?;
        witness["invocation_path"] = json!(invocation);
        selected_tools.insert(tool.into(), witness);
    }
    for (variable, tool) in [("CC", "clang"), ("CXX", "clang++")] {
        // Preserve clang++ argv[0] while witnessing its resolved clang bytes.
        if selected_tools[tool]["invocation_path"] != env_string(&env, variable)? {
            return Err("Mac C/C++ invocation is not the selected SDK compiler".into());
        }
    }
    let machine = mac_system_capture(&env, root, &resolve_tool(&env, "uname")?, &["-m"], 4096)?;
    if machine != p.machine {
        return Err("Mac hosted machine does not match reviewed profile".into());
    }
    let mut files = serde_json::Map::new();
    for name in [
        "rust/Cargo.lock",
        "rust/Cargo.toml",
        "rust/rust-toolchain.toml",
        MAC_WORKFLOW,
        "rust/.cargo/config.toml",
    ] {
        files.insert(name.into(), descriptor(&root.join(name), INPUT_MAX)?);
    }
    let epoch = mac_git(&env, root, &["show", "-s", "--format=%ct", "HEAD"])?;
    if !decimal(&epoch)
        || std::env::var("SOURCE_DATE_EPOCH").ok().as_deref() != Some(epoch.as_str())
    {
        return Err("Mac source epoch does not match clean source commit".into());
    }
    Ok(
        json!({"schema_version":1,"kind":"native_binary_build_inputs","profile":"macos-hosted-v1",
        "target":p.target,"source_commit":commit,"source_dirty":false,"cargo_lock_sha256":files["rust/Cargo.lock"]["sha256"],
        "source_date_epoch":epoch,"ci":ci,"environment":env,"files":files,"cargo_config":config,"builder":null,
        "compiler":{"tools":tools,"rustc_text":rustc_text,"cargo_text":mac_capture(&env,root,cargo,&["--version"],4096)?,
            "rustup_settings":descriptor(&rustup_home.join("settings.toml"),INPUT_MAX)?,"sysroot":sysroot,
            "developer_dir":developer,"sdk":sdk,"sdk_settings":sdk_settings,"sdk_version":sdk_version,"selected_tools":selected_tools,
            "xcode_text":mac_capture(&env,root,Path::new("/usr/bin/xcodebuild"),&["-version"],16384)?,
            "clang_text":mac_capture(&env,root,Path::new("/usr/bin/clang"),&["--version"],16384)?,
            "perl_text":mac_capture(&env,root,Path::new("/usr/bin/perl"),&["-V"],65536)?,
            "kernel_text":mac_system_capture(&env,root,&resolve_tool(&env,"uname")?, &["-srvm"],4096)?},
        "command":["cargo","build","--release","--offline","--locked","--package","claude-autorouter","--message-format=json"],
        "working_directory":root.join("rust")}),
    )
}

pub(super) fn inspect(p: MacProfile, binary: &[u8]) -> Result<Value, String> {
    let inspection = crate::provenance_binary::inspect(p.target, binary)?;
    if inspection["format"] != "Mach-O"
        || inspection["target"] != p.target
        || inspection["minimum_macos"] != "11.0.0"
    {
        return Err("Mac executable does not match reviewed deployment profile".into());
    }
    // The shared parser also rejects RPATH and ambiguous/non-macOS version
    // commands; header inspection still does not establish oldest-OS execution.
    Ok(inspection)
}

#[cfg(test)]
mod tests {
    // Synthetic metadata and owned local files only. Never mutate the process
    // environment, invoke actual compiler/rustup/xcrun, or inspect the real SDK.
    use super::*;
    use crate::audit_support::Scratch;

    fn fixture(p: MacProfile) -> (Scratch, BTreeMap<String, String>) {
        let scratch = Scratch::new("mac-provenance-metadata").unwrap();
        let mut env = BTreeMap::new();
        for (name, value) in [
            ("PATH", "/usr/bin:/bin"),
            ("CARGO_INCREMENTAL", "0"),
            ("RUSTUP_TOOLCHAIN", "1.99.0"),
            ("CC", "/synthetic/developer/clang"),
            ("CXX", "/synthetic/developer/clang++"),
            ("CFLAGS", "-O2"),
            ("CXXFLAGS", "-O2"),
            ("RUSTFLAGS", p.rustflags),
            ("OPENSSL_NO_VENDOR", "0"),
            ("OPENSSL_STATIC", "1"),
            ("OPENSSL_SRC_PERL", "/usr/bin/perl"),
            ("MACOSX_DEPLOYMENT_TARGET", "11.0"),
            ("AUTOROUTER_BUILD_TARGET", p.target),
            ("AUTOROUTER_BUILD_MACHINE", p.machine),
            ("AUTOROUTER_BUILD_RUNNER", p.runner),
            ("RUNNER_OS", "macOS"),
            ("RUNNER_ARCH", p.runner_arch),
            ("RUNNER_ENVIRONMENT", "github-hosted"),
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_REPOSITORY", REPOSITORY),
            ("GITHUB_SERVER_URL", "https://github.com"),
            ("GITHUB_JOB", "rust"),
            ("GITHUB_RUN_ID", "741"),
            ("GITHUB_RUN_ATTEMPT", "2"),
        ] {
            env.insert(name.into(), value.into());
        }
        env.insert("GITHUB_SHA".into(), "a".repeat(40));
        env.insert("GITHUB_WORKFLOW_SHA".into(), "b".repeat(40));
        env.insert(
            "GITHUB_WORKFLOW_REF".into(),
            format!("{REPOSITORY}/{MAC_WORKFLOW}@refs/pull/17/merge"),
        );
        for (name, suffix) in [
            ("HOME", "home"),
            ("CARGO_HOME", "home/cargo"),
            ("RUSTUP_HOME", "home/rustup"),
            ("DEVELOPER_DIR", "developer"),
            ("SDKROOT", "developer/SDK"),
        ] {
            let path = scratch.0.join(suffix);
            fs::create_dir_all(&path).unwrap();
            env.insert(name.into(), path.to_str().unwrap().into());
        }
        (scratch, env)
    }

    #[test]
    fn rustup_metadata_preserves_multicall_invocation_and_admitted_identity() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let p = profile("aarch64-apple-darwin").unwrap();
        let (scratch, mut env) = fixture(p);
        let cargo_home = PathBuf::from(&env["CARGO_HOME"]);
        let bin = cargo_home.join("bin");
        fs::create_dir(&bin).unwrap();
        let actual = PathBuf::from(&env["RUSTUP_HOME"])
            .join("toolchains/1.99.0-aarch64-apple-darwin/bin/rustc");
        fs::create_dir_all(actual.parent().unwrap()).unwrap();
        fs::write(&actual, b"synthetic compiler identity; not executed").unwrap();
        let script = b"#!/bin/sh\n[ \"${0##*/}\" = rustup ] || exit 1\n[ \"$#\" = 4 ] && [ \"$1\" = which ] && [ \"$2\" = --toolchain ] && [ \"$3\" = 1.99.0 ] && [ \"$4\" = rustc ] || exit 2\nprintf '%s/toolchains/1.99.0-aarch64-apple-darwin/bin/rustc\\n' \"$RUSTUP_HOME\"\n";
        let implementation = scratch.file("rustup-init", script).unwrap();
        fs::set_permissions(&implementation, fs::Permissions::from_mode(0o700)).unwrap();
        let alias = bin.join("rustup");
        symlink(&implementation, &alias).unwrap();
        env.insert("PATH".into(), format!("{}:/usr/bin:/bin", bin.display()));
        let args = &["which", "--toolchain", "1.99.0", "rustc"];
        let canonical = resolve_tool(&env, "rustup").unwrap();
        assert_eq!(canonical, implementation);
        let error = mac_capture(&env, &scratch.0, &canonical, args, 4096).unwrap_err();
        assert!(error.contains("exit status: 1"), "{error}");
        let (invocation, admitted_canonical) =
            admitted_rustup_invocation(&env, &cargo_home).unwrap();
        assert_eq!(invocation, alias);
        assert_eq!(admitted_canonical, canonical);
        assert_eq!(
            selected_path(&env, &scratch.0, invocation.to_str().unwrap(), args).unwrap(),
            actual
        );
        assert_eq!(
            tool_descriptor(&admitted_canonical).unwrap(),
            tool_descriptor(&canonical).unwrap()
        );
        assert!(tool_descriptor(&invocation).is_err());
        let other_home = scratch.0.join("other-cargo-home");
        fs::create_dir_all(other_home.join("bin")).unwrap();
        fs::write(other_home.join("bin/rustup"), b"different Cargo-home tool").unwrap();
        assert!(admitted_rustup_invocation(&env, &other_home).is_err());
        let shadow_bin = scratch.0.join("shadow-bin");
        fs::create_dir(&shadow_bin).unwrap();
        let shadow = shadow_bin.join("rustup");
        fs::write(&shadow, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&shadow, fs::Permissions::from_mode(0o700)).unwrap();
        env.insert(
            "PATH".into(),
            format!("{}:{}:/usr/bin:/bin", shadow_bin.display(), bin.display()),
        );
        assert!(admitted_rustup_invocation(&env, &cargo_home).is_err());
    }

    #[test]
    fn failed_owned_metadata_tools_identify_public_stage_without_private_payloads() {
        use std::os::unix::fs::PermissionsExt;
        let p = profile("aarch64-apple-darwin").unwrap();
        let (scratch, mut env) = fixture(p);
        let private_home = scratch.0.join("fake-private-environment-sentinel");
        fs::create_dir(&private_home).unwrap();
        env.insert("HOME".into(), private_home.to_str().unwrap().into());
        env.insert(
            "PATH".into(),
            format!("{}:/usr/bin:/bin", scratch.0.display()),
        );
        let script = b"#!/bin/sh\nprintf 'fake-private-stdout-sentinel\\n'\nprintf 'fake-private-stderr-sentinel\\n' >&2\nprintf '%s\\n' \"$HOME\" >&2\nexit 7\n";
        for (tool, args) in [
            ("rustc", &["-vV"][..]),
            ("git", &["rev-parse", "--verify", "HEAD"][..]),
        ] {
            let program = scratch.file(tool, script).unwrap();
            fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
            let error = if tool == "git" {
                mac_git(&env, &scratch.0, args).unwrap_err()
            } else {
                mac_capture(&env, &scratch.0, &program, args, 4096).unwrap_err()
            };
            assert!(
                error.contains(&format!("{tool} {}", args.join(" "))),
                "{error}"
            );
            assert!(error.contains("exit status: 7"), "{error}");
            for private in [
                "fake-private-stdout-sentinel",
                "fake-private-stderr-sentinel",
                "fake-private-environment-sentinel",
                scratch.0.to_str().unwrap(),
            ] {
                assert!(
                    !error.contains(private),
                    "Private metadata appeared in error"
                );
            }
        }
    }

    #[test]
    fn metadata_error_context_redacts_unlisted_names_and_args_but_preserves_timeout_detail() {
        let error = metadata_failure(
            Path::new("/fake-private-environment-sentinel/fake-private-tool-name"),
            &["--private", "fake-private-argument"],
            "Fixture executable timed out",
        );
        assert!(error.contains("metadata-tool <unlisted metadata arguments>"));
        assert!(error.contains("Fixture executable timed out"));
        for private in [
            "fake-private-environment-sentinel",
            "fake-private-tool-name",
            "fake-private-argument",
        ] {
            assert!(!error.contains(private));
        }
    }

    #[test]
    fn mac_extra_environment_is_scoped_without_changing_gnu_selection() {
        let pairs = [
            (OsString::from("HOME"), OsString::from("/synthetic/home")),
            (OsString::from("SDKROOT"), OsString::from("/synthetic/sdk")),
            (
                OsString::from("GITHUB_TOKEN"),
                OsString::from("fake-secret"),
            ),
        ];
        assert!(super::super::select_env(pairs.clone()).unwrap().is_empty());
        let selected = select_macos_env(pairs).unwrap();
        assert_eq!(selected.len(), 2);
        assert_eq!(selected["HOME"], "/synthetic/home");
        assert!(!selected.contains_key("GITHUB_TOKEN"));
    }

    #[test]
    fn mac_environment_bounds_cover_aggregate_keys_bytes_and_path_tail() {
        let pairs = |n| {
            (0..n).map(|i| {
                (
                    OsString::from(format!("RUST_TEST_{i}")),
                    OsString::from("v"),
                )
            })
        };
        assert_eq!(select_macos_env(pairs(64)).unwrap().len(), 64);
        assert!(select_macos_env(pairs(65)).is_err());
        assert!(
            select_macos_env((0..8).map(|i| (
                OsString::from(format!("RUST_TEST_{i}")),
                OsString::from("x".repeat(8192))
            )))
            .is_err()
        );
        let p = profile("aarch64-apple-darwin").unwrap();
        let (_scratch, mut env) = fixture(p);
        env.insert("PATH".into(), "/usr/bin:/bin:".into());
        assert!(validate_env(&env, p, &"a".repeat(40)).is_err());
    }

    #[test]
    fn apple_arm_cpu_admission_requires_the_reviewed_m1_baseline() {
        let p = profile("aarch64-apple-darwin").unwrap();
        let (_scratch, mut env) = fixture(p);
        // This independently declared admitted policy must succeed; deriving the
        // positive value from p.rustflags would hide an accidental baseline change.
        env.insert(
            "RUSTFLAGS".into(),
            "-C target-cpu=apple-m1 -C linker=/usr/bin/clang".into(),
        );
        assert!(validate_env(&env, p, &"a".repeat(40)).is_ok());
        for flags in [
            "-C target-cpu=generic -C linker=/usr/bin/clang",
            "-C target-cpu=native -C linker=/usr/bin/clang",
            "-C target-cpu=apple-m2 -C linker=/usr/bin/clang",
            "-C target-cpu=synthetic-unknown-apple-cpu -C linker=/usr/bin/clang",
            "-C target-cpu=apple-m1 -C target-feature=+aes,+sha2 -C linker=/usr/bin/clang",
        ] {
            let mut changed = env.clone();
            changed.insert("RUSTFLAGS".into(), flags.into());
            assert!(validate_env(&changed, p, &"a".repeat(40)).is_err());
        }
    }

    #[test]
    fn two_mac_profiles_bind_distinct_host_runner_workflow_and_portable_flags() {
        for target in ["aarch64-apple-darwin", "x86_64-apple-darwin"] {
            let p = profile(target).unwrap();
            let (_scratch, env) = fixture(p);
            let ci = validate_env(&env, p, &"a".repeat(40)).unwrap();
            assert_eq!(ci["runner"], p.runner);
            assert_eq!(ci["job"], "rust");
            assert!(super::super::profile(target).is_err());
            for (name, value) in [
                ("GITHUB_JOB", "glibc228"),
                ("AUTOROUTER_BUILD_RUNNER", "macos-latest"),
                ("RUNNER_ARCH", "wrong"),
                ("MACOSX_DEPLOYMENT_TARGET", "15.0"),
                ("RUSTUP_TOOLCHAIN", "stable"),
                ("RUSTFLAGS", "-C target-cpu=native"),
                ("DYLD_LIBRARY_PATH", "/synthetic/lib"),
                (
                    "CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER",
                    "/synthetic/linker",
                ),
            ] {
                let mut changed = env.clone();
                changed.insert(name.into(), value.into());
                assert!(
                    validate_env(&changed, p, &"a".repeat(40)).is_err(),
                    "{name}"
                );
            }
            let mut changed = env.clone();
            changed.insert(
                "GITHUB_WORKFLOW_REF".into(),
                format!("{REPOSITORY}/{WORKFLOW}@refs/heads/main"),
            );
            assert!(validate_env(&changed, p, &"a".repeat(40)).is_err());
        }
    }

    #[test]
    fn begin_builder_option_is_mandatory_only_for_gnu_and_unknown_options_refuse() {
        let args = |target: &str, builder: bool| {
            let mut values = vec![
                "--target",
                target,
                "--build-report",
                "/synthetic/build",
                "--feature-report",
                "/synthetic/features",
                "--out",
                "/synthetic/inputs",
            ];
            if builder {
                values.extend(["--builder-report", "/synthetic/builder"]);
            }
            values.into_iter().map(str::to_owned).collect::<Vec<_>>()
        };
        for (target, builder) in [
            ("x86_64-unknown-linux-gnu", true),
            ("aarch64-unknown-linux-gnu", true),
            ("x86_64-apple-darwin", false),
            ("aarch64-apple-darwin", false),
        ] {
            assert!(super::super::begin_options(&args(target, builder)).is_ok());
            assert!(super::super::begin_options(&args(target, !builder)).is_err());
            let mut changed = args(target, builder);
            changed[2] = "--unknown".into();
            assert!(super::super::begin_options(&changed).is_err());
        }
    }

    #[test]
    fn build_input_profile_target_pairs_cannot_cross_admission_families() {
        for (target, name) in [
            ("x86_64-unknown-linux-gnu", "gnu-glibc228-v1"),
            ("aarch64-apple-darwin", "macos-hosted-v1"),
        ] {
            let mut record = json!({"target":target,"profile":name,"builder":null});
            assert!(super::super::BuildProfile::from_inputs(&record).is_ok());
            record["profile"] = json!(if name == "gnu-glibc228-v1" {
                "macos-hosted-v1"
            } else {
                "gnu-glibc228-v1"
            });
            assert!(super::super::BuildProfile::from_inputs(&record).is_err());
        }
        assert!(super::super::BuildProfile::from_inputs(&json!({"target":"aarch64-apple-darwin","profile":"macos-hosted-v1","builder":{"path":"/synthetic/builder"}})).is_err());
    }

    #[test]
    fn streaming_tool_witness_accepts_library_bytes_and_hardlinks_without_following_links() {
        let scratch = Scratch::new("mac-provenance-tools").unwrap();
        let path = scratch
            .file("driver.dylib", b"synthetic compiler library")
            .unwrap();
        let first = tool_descriptor(&path).unwrap();
        assert_eq!(first["sha256"], digest(b"synthetic compiler library"));
        let alias = scratch.0.join("hardlink");
        fs::hard_link(&path, &alias).unwrap();
        assert_eq!(tool_descriptor(&alias).unwrap()["identity"]["links"], 2);
        let link = scratch.0.join("symlink");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(tool_descriptor(&link).is_err());
        fs::write(&path, b"changed synthetic compiler").unwrap();
        assert_ne!(tool_descriptor(&path).unwrap()["sha256"], first["sha256"]);
    }

    #[test]
    fn streaming_tool_size_refuses_sparse_oversize_before_reading_payload() {
        let scratch = Scratch::new("mac-provenance-tool-cap").unwrap();
        let path = scratch.file("oversize", b"").unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAC_TOOL_MAX + 1)
            .unwrap();
        assert!(tool_descriptor(&path).is_err());
    }
    fn macho(cpu: u32, commands: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes = vec![0u8; 32];
        bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        for (offset, value) in [
            (4, cpu),
            (8, if cpu == 0x1000007 { 3 } else { 0 }),
            (12, 2),
            (16, commands.len() as u32),
            (20, commands.iter().map(Vec::len).sum::<usize>() as u32),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        for command in commands {
            bytes.extend_from_slice(command);
        }
        bytes
    }
    fn build_version(platform: u32, minimum: u32) -> Vec<u8> {
        [0x32u32, 24, platform, minimum, 0x1a0000, 0]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect()
    }

    #[test]
    fn mac_inspector_enforces_profile_minimum_and_shared_macho_refusals_for_both_cpus() {
        for (target, cpu, other) in [
            ("aarch64-apple-darwin", 0x100000cu32, 0x1000007u32),
            ("x86_64-apple-darwin", 0x1000007, 0x100000c),
        ] {
            let p = profile(target).unwrap();
            let good = build_version(1, 0x0b0000);
            let decoded =
                crate::provenance_binary::inspect(target, &macho(cpu, std::slice::from_ref(&good)))
                    .unwrap();
            assert_eq!(decoded["minimum_macos"], "11.0.0");
            assert_eq!(
                inspect(p, &macho(cpu, std::slice::from_ref(&good))).unwrap(),
                decoded
            );
            assert_eq!(decoded["execution_qualification"], "pending");
            let missing = macho(cpu, &[]);
            assert_eq!(
                crate::provenance_binary::inspect(target, &missing).unwrap()["minimum_macos"],
                Value::Null
            );
            assert!(inspect(p, &missing).is_err());
            for (version, expected) in [(0x0a0f00, "10.15.0"), (0x0f0000, "15.0.0")] {
                let bytes = macho(cpu, &[build_version(1, version)]);
                assert_eq!(
                    crate::provenance_binary::inspect(target, &bytes).unwrap()["minimum_macos"],
                    expected
                );
                assert!(inspect(p, &bytes).is_err());
            }
            assert!(inspect(p, &macho(other, std::slice::from_ref(&good))).is_err());
            let legacy: Vec<u8> = [0x24u32, 16, 0x0b0000, 0x1a0000]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect();
            assert!(inspect(p, &macho(cpu, std::slice::from_ref(&legacy))).is_ok());
            let duplicate = macho(cpu, &[good.clone(), legacy]);
            assert_eq!(
                crate::provenance_binary::inspect(target, &duplicate).unwrap_err(),
                "Duplicate Mach-O minimum platform command"
            );
            assert!(inspect(p, &duplicate).is_err());
            let foreign = macho(cpu, &[build_version(2, 0x0b0000)]);
            assert_eq!(
                crate::provenance_binary::inspect(target, &foreign).unwrap_err(),
                "Mach-O artifact does not declare the macOS platform"
            );
            assert!(inspect(p, &foreign).is_err());
            let mut rpath: Vec<u8> = [0x8000001cu32, 24, 12, 0, 0, 0]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect();
            rpath[12..21].copy_from_slice(b"/usr/lib\0");
            let bytes = macho(cpu, &[good, rpath]);
            assert_eq!(
                crate::provenance_binary::inspect(target, &bytes).unwrap_err(),
                "Mach-O artifact declares a library search path"
            );
            assert!(inspect(p, &bytes).is_err());
        }
    }

    #[test]
    fn discovered_effective_cargo_configs_refuse_owned_home_ancestor_and_symlink_overrides() {
        let p = profile("aarch64-apple-darwin").unwrap();
        let (scratch, env) = fixture(p);
        let project = scratch.0.join("project");
        let admitted = project.join("rust/.cargo/config.toml");
        fs::create_dir_all(admitted.parent().unwrap()).unwrap();
        let config = b"[alias]\nxtask = \"run --package xtask --\"\n";
        assert_eq!(digest(config), CARGO_CONFIG);
        fs::write(&admitted, config).unwrap();
        let discovered = config_paths(&project, &env).unwrap();
        let cargo = PathBuf::from(env["CARGO_HOME"].as_str()).join("config.toml");
        let ancestor = project.join(".cargo/config");
        assert!(discovered.contains(&cargo));
        assert!(discovered.contains(&ancestor));
        // Exercise the production-used absence guard on exactly the discovered
        // Scratch-owned paths; never read an actual contributor home/ancestor.
        let owned: BTreeSet<_> = discovered
            .into_iter()
            .filter(|path| path.starts_with(&scratch.0))
            .collect();
        assert_eq!(
            absent_configs(&owned, &admitted).unwrap().len(),
            owned.len() - 1
        );
        fs::write(&cargo, b"[build]\nrustflags=['-Ctarget-cpu=native']\n").unwrap();
        assert!(absent_configs(&owned, &admitted).is_err());
        fs::remove_file(&cargo).unwrap();
        fs::create_dir_all(ancestor.parent().unwrap()).unwrap();
        fs::write(&ancestor, b"synthetic override").unwrap();
        assert!(absent_configs(&owned, &admitted).is_err());
        fs::remove_file(&ancestor).unwrap();
        std::os::unix::fs::symlink(scratch.0.join("missing"), &cargo).unwrap();
        assert!(absent_configs(&owned, &admitted).is_err());
    }
}
