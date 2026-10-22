//! Reviewed native Alpine SDK witnesses; no builds, downloads or product calls.
use super::*;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::io::Read;

const MUSL_WORKFLOW: &str = ".github/workflows/rust-musl.yml";
const PINS: &str = "rust/distribution/musl-builder-pins.json";
const PINS_SHA: &str = "f763189402b9feef6bec42a185e9623bf10bb4ab2c47bba11a038c8b15f65964";
const DOCKERFILE: &str = "rust/distribution/ci/musl.Dockerfile";
const SCRIPT: &str = "rust/distribution/ci/check-musl.sh";
const FETCHER: &str = "rust/distribution/ci/fetch-musl-apks.mjs";
const SDK_TOOL_MAX: u64 = 512 * 1024 * 1024;
const SDK: &str = "/opt/autorouter-build-inputs";
const SDK_PATH: &str =
    "/opt/autorouter-rust/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin";
const QUALIFICATION: &str = "Current Alpine3.21 container userland only; oldest Alpine/kernel/CPU/WSL/corporate deployment and full-matrix/release gates remain pending";
const EXTRA_ENV: &[&str] = &[
    "HOME",
    "AUTOROUTER_SOURCE_ROOT",
    "AUTOROUTER_SOURCE_COMMIT",
    "AUTOROUTER_NODE_IMAGE",
    "AUTOROUTER_WORKFLOW_RUN",
    "SOURCE_DATE_EPOCH",
];

#[derive(Clone, Copy)]
pub(super) struct MuslProfile {
    pub(super) target: &'static str,
    machine: &'static str,
    runner: &'static str,
    image: &'static str,
    rust_archive: &'static str,
    rust_cpu: &'static str,
    c_cpu: &'static str,
}

pub(super) fn profile(target: &str) -> Result<MuslProfile, String> {
    match target {
        "x86_64-unknown-linux-musl" => Ok(MuslProfile {
            target: "x86_64-unknown-linux-musl",
            machine: "x86_64",
            runner: "ubuntu-24.04",
            image: "docker.io/library/node@sha256:01393fe5a51489b63da0ab51aa8e0a7ff9990132917cf20cfc3d46f5e36c0e48",
            rust_archive: "0ed5ec213fbde7cd720054d4a7eabd592822e73ce406da0aea57e7b1b48e8b0f",
            rust_cpu: "x86-64",
            c_cpu: "-march=x86-64 -mtune=generic",
        }),
        "aarch64-unknown-linux-musl" => Ok(MuslProfile {
            target: "aarch64-unknown-linux-musl",
            machine: "aarch64",
            runner: "ubuntu-24.04-arm",
            image: "docker.io/library/node@sha256:4a78eedb5c49d58c0c0b610ebc48f4ac397358604daac64e8dec1baecde2a31b",
            rust_archive: "7559507ec98f044718ab6a786e26ecfd09764b5af29e0f0c075eb8e738c3214d",
            rust_cpu: "generic",
            c_cpu: "-march=armv8-a",
        }),
        _ => Err("Native provenance target has no reviewed musl build profile".into()),
    }
}

pub(super) fn rustflags(p: MuslProfile, root: &Path) -> Result<String, String> {
    let root = root.to_str().ok_or("Non-UTF8 musl source root")?;
    if !Path::new(root).is_absolute()
        || root.len() > 4096
        || root
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '=')
    {
        return Err("Musl source root cannot be represented by reviewed Rust flags".into());
    }
    Ok(format!(
        "-C target-cpu={} -C target-feature=+crt-static --remap-path-prefix={root}=/autorouter-source",
        p.rust_cpu
    ))
}

fn select_musl_env(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<BTreeMap<String, String>, String> {
    let mut selected = BTreeMap::new();
    let mut total = 0usize;
    for (name, value) in vars {
        let name_text = name
            .to_str()
            .ok_or("Non-UTF8 musl build environment name")?;
        let rows = if EXTRA_ENV.contains(&name_text) {
            BTreeMap::from([(
                name_text.to_owned(),
                value
                    .into_string()
                    .map_err(|_| "Non-UTF8 public musl build variable")?,
            )])
        } else {
            super::select_env(std::iter::once((name, value)))?
        };
        for (name, value) in rows {
            total = total
                .checked_add(name.len())
                .and_then(|n| n.checked_add(value.len()))
                .ok_or("Public musl build environment size overflow")?;
            if name.len() > 128
                || value.len() > 8192
                || value.contains(['\0', '\r', '\n'])
                || selected.len() >= 64
                || total > 65536
            {
                return Err("Invalid or oversized public musl build environment".into());
            }
            if selected.insert(name, value).is_some() {
                return Err("Duplicate public musl build variable".into());
            }
        }
    }
    Ok(selected)
}

fn output_base(root: &Path) -> PathBuf {
    root.join("artifacts/rust-rewrite/musl-portability")
}

fn validate_env(
    env: &BTreeMap<String, String>,
    p: MuslProfile,
    root: &Path,
    commit: &str,
) -> Result<Value, String> {
    let out = output_base(root);
    let public_path = |path: PathBuf| {
        path.to_str()
            .map(str::to_owned)
            .ok_or("Non-UTF8 musl output path")
    };
    let expected = BTreeMap::from([
        ("PATH", SDK_PATH.to_owned()),
        ("HOME", public_path(out.join("home"))?),
        ("CARGO_HOME", public_path(out.join("cargo-cache"))?),
        ("CARGO_TARGET_DIR", public_path(out.join("target-main"))?),
        ("CARGO_BUILD_TARGET", p.target.to_owned()),
        ("CARGO_INCREMENTAL", "0".into()),
        ("CARGO_PROFILE_DEV_DEBUG", "0".into()),
        ("CARGO_PROFILE_TEST_DEBUG", "0".into()),
        ("CC", "/tmp/autorouter-compilers/cc".into()),
        ("CXX", "/tmp/autorouter-compilers/cxx".into()),
        ("CFLAGS", "-O2".into()),
        ("CXXFLAGS", "-O2".into()),
        ("RUSTFLAGS", rustflags(p, root)?),
        ("OPENSSL_NO_VENDOR", "0".into()),
        ("OPENSSL_STATIC", "1".into()),
        ("AUTOROUTER_BUILD_TARGET", p.target.into()),
        ("AUTOROUTER_BUILD_MACHINE", p.machine.into()),
        ("AUTOROUTER_SOURCE_ROOT", public_path(root.to_path_buf())?),
        ("AUTOROUTER_SOURCE_COMMIT", commit.to_owned()),
        ("AUTOROUTER_NODE_IMAGE", p.image.into()),
    ]);
    for (name, value) in &expected {
        if env.get(*name) != Some(value) {
            return Err(format!("Unreviewed musl build variable: {name}"));
        }
    }
    let permitted = [
        "SOURCE_DATE_EPOCH",
        "AUTOROUTER_WORKFLOW_RUN",
        "CARGO_BUILD_JOBS",
        "CARGO_NET_OFFLINE",
    ];
    for name in env.keys() {
        if !PUBLIC_GITHUB.contains(&name.as_str())
            && !expected.contains_key(name.as_str())
            && !permitted.contains(&name.as_str())
        {
            return Err(format!("Unexpected musl compiler/build override: {name}"));
        }
    }
    if !hex(commit, 40) || !decimal(&env_string(env, "SOURCE_DATE_EPOCH")?) {
        return Err("Invalid musl source identity/epoch".into());
    }
    if let Some(value) = env.get("CARGO_BUILD_JOBS")
        && !decimal(value)
    {
        return Err("Invalid bounded musl build jobs".into());
    }
    if let Some(value) = env.get("CARGO_NET_OFFLINE")
        && value != "true"
    {
        return Err("Unexpected musl offline override".into());
    }
    if env_string(env, "GITHUB_ACTIONS")? != "true"
        || env_string(env, "GITHUB_REPOSITORY")? != REPOSITORY
        || env_string(env, "GITHUB_SERVER_URL")? != "https://github.com"
        || env_string(env, "GITHUB_SHA")? != commit
        || env_string(env, "GITHUB_JOB")? != "musl"
    {
        return Err("Musl provenance requires matching original CI workflow context".into());
    }
    let workflow_ref = env_string(env, "GITHUB_WORKFLOW_REF")?;
    let prefix = format!("{REPOSITORY}/{MUSL_WORKFLOW}@");
    let suffix = workflow_ref
        .strip_prefix(&prefix)
        .ok_or("Unreviewed musl workflow identity")?;
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
        return Err("Unreviewed musl workflow identity".into());
    }
    let workflow_sha = env_string(env, "GITHUB_WORKFLOW_SHA")?;
    let run = env_string(env, "GITHUB_RUN_ID")?;
    let attempt = env_string(env, "GITHUB_RUN_ATTEMPT")?;
    let run_url = format!("https://github.com/{REPOSITORY}/actions/runs/{run}");
    if !hex(&workflow_sha, 40)
        || !decimal(&run)
        || !decimal(&attempt)
        || env_string(env, "AUTOROUTER_WORKFLOW_RUN")? != run_url
    {
        return Err("Invalid musl workflow/run metadata".into());
    }
    Ok(
        json!({"repository":REPOSITORY,"checkout_commit":commit,"workflow_ref":workflow_ref,"workflow_sha":workflow_sha,
        "workflow_run":format!("{run_url}/attempts/{attempt}"),"run_id":run,"run_attempt":attempt,"job":"musl",
        "runner_profile":p.runner,"runner_profile_scope":"Reviewed workflow matrix label; not an independent runner observation"}),
    )
}

// No new parent policy: this child checks both Cargo target-mode output roots.
fn prospective_parents(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err("Musl output path must be absolute and lexical".into());
    }
    let mut prefix = PathBuf::new();
    for component in path.components() {
        prefix.push(component.as_os_str());
        match fs::symlink_metadata(&prefix) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err("Musl output parent is not an admitted real directory".into()),
        }
    }
    Ok(())
}

fn target_root(root: &Path, state: &Value, p: MuslProfile) -> Result<PathBuf, String> {
    let expected = output_base(root).join("target-main");
    if state["environment"]["CARGO_BUILD_TARGET"] != p.target
        || state["environment"]["CARGO_TARGET_DIR"].as_str() != expected.to_str()
    {
        return Err("Musl output environment is not the reviewed target tree".into());
    }
    prospective_parents(&expected)?;
    prospective_parents(&expected.join(p.target))?;
    Ok(expected)
}

pub(super) fn fresh_release_tree(root: &Path, state: &Value, p: MuslProfile) -> Result<(), String> {
    let target = target_root(root, state, p)?;
    for release in [
        target.join("release"),
        target.join(p.target).join("release"),
    ] {
        match fs::symlink_metadata(release) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err("Musl provenance requires fresh host and target release trees".into()),
        }
    }
    Ok(())
}

pub(super) fn admitted_executable(
    root: &Path,
    state: &Value,
    inspected: &Value,
    p: MuslProfile,
) -> Result<PathBuf, String> {
    let expected = target_root(root, state, p)?
        .join(p.target)
        .join("release/claude-autorouter");
    let executable = canonical(Path::new(text(&inspected["binary"], "executable")?))?;
    if executable != expected {
        return Err("Cargo executable is not the admitted musl production path".into());
    }
    Ok(executable)
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
        if path == admitted {
            continue;
        }
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => absent.push(path.clone()),
            _ => return Err("Musl Cargo configuration override is present or unreadable".into()),
        }
    }
    Ok(absent)
}

fn cargo_config(root: &Path, env: &BTreeMap<String, String>) -> Result<Value, String> {
    let admitted = root.join("rust/.cargo/config.toml");
    if digest(&stable(&admitted, 65536)?) != CARGO_CONFIG {
        return Err("Unreviewed musl workspace Cargo configuration".into());
    }
    Ok(
        json!({"admitted":descriptor(&admitted,65536)?,"absent":absent_configs(&config_paths(root,env)?,&admitted)?}),
    )
}

fn exact_keys(value: &Value, names: &[&str]) -> Result<(), String> {
    let object = value.as_object().ok_or("Invalid musl metadata object")?;
    if object.len() != names.len() || names.iter().any(|key| !object.contains_key(*key)) {
        return Err("Unreviewed musl metadata fields".into());
    }
    Ok(())
}

// Encode only an already validated complete SHA256; no general base64 parser.
fn sha_base64(value: &str) -> Result<String, String> {
    if !hex(value, 64) {
        return Err("Invalid closed APK SHA256".into());
    }
    let bytes = (0..32)
        .map(|i| {
            u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).map_err(|_| "Invalid APK digest byte")
        })
        .collect::<Result<Vec<_>, _>>()?;
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(44);
    for block in bytes.chunks(3) {
        let a = block[0];
        let b = block.get(1).copied().unwrap_or(0);
        let c = block.get(2).copied().unwrap_or(0);
        result.push(ALPHABET[(a >> 2) as usize] as char);
        result.push(ALPHABET[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        result.push(if block.len() > 1 {
            ALPHABET[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        result.push(if block.len() > 2 {
            ALPHABET[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    Ok(result)
}

fn canonical_sha_base64(value: &str) -> bool {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = value.as_bytes();
    bytes.len() == 44
        && bytes[43] == b'='
        && bytes[..43].iter().all(|byte| ALPHABET.contains(byte))
        && ALPHABET
            .iter()
            .position(|byte| *byte == bytes[42])
            .is_some_and(|value| value & 3 == 0)
}

fn apk_filename(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".apk") else {
        return false;
    };
    let Some((prefix, revision)) = stem.rsplit_once("-r") else {
        return false;
    };
    name.len() <= 128
        && !prefix.is_empty()
        && !revision.is_empty()
        && revision.bytes().all(|b| b.is_ascii_digit())
        && name
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && stem
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"+_.-".contains(&b))
}

struct SdkInputs {
    pins: Value,
    apks: Value,
    expected: String,
    actual: String,
    installed: String,
    rust_archive: String,
    tar: String,
    xz: String,
}

fn validate_sdk(sdk: &SdkInputs, pins_sha: &str, p: MuslProfile) -> Result<(), String> {
    if !hex(pins_sha, 64)
        || sdk.pins["schema_version"] != 2
        || sdk.pins["rust_version"] != "1.99.0"
        || sdk.pins["node_version"] != "22.14.0"
    {
        return Err("Unreviewed musl SDK pin schema/versions".into());
    }
    let targets = sdk.pins["targets"]
        .as_array()
        .filter(|rows| rows.len() == 2)
        .ok_or("Invalid musl pin target inventory")?;
    for target in ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"] {
        let expected = profile(target)?;
        let rows = targets
            .iter()
            .filter(|row| row["target"] == expected.target)
            .collect::<Vec<_>>();
        if rows.len() != 1
            || rows[0]["arch"] != expected.machine
            || rows[0]["runner"] != expected.runner
            || rows[0]["node_image"] != expected.image
            || rows[0]["rust_archive_sha256"] != expected.rust_archive
        {
            return Err("Musl SDK target pins do not match reviewed profiles".into());
        }
    }
    let set = &sdk.pins["apk_package_set"];
    if set["schema_version"] != 1
        || set["kind"] != "closed_alpine_apk_sha256_set"
        || set["repository"] != "https://dl-cdn.alpinelinux.org/alpine/v3.21/main"
    {
        return Err("Unreviewed closed APK pin set".into());
    }
    let names = set["filenames"]
        .as_array()
        .filter(|rows| rows.len() == 61)
        .ok_or("Invalid closed APK filename count")?;
    exact_keys(&set["sha256_base64"], &["x86_64", "aarch64"])?;
    for arch in ["x86_64", "aarch64"] {
        if !set["sha256_base64"][arch].as_array().is_some_and(|rows| {
            rows.len() == 61
                && rows
                    .iter()
                    .all(|row| row.as_str().is_some_and(canonical_sha_base64))
        }) {
            return Err("Invalid closed APK digest inventory".into());
        }
    }
    let files = sdk.apks["files"]
        .as_array()
        .filter(|rows| rows.len() == 61)
        .ok_or("Invalid verified APK file count")?;
    exact_keys(
        &sdk.apks,
        &[
            "schema_version",
            "kind",
            "arch",
            "pins_sha256",
            "package_count",
            "total_bytes",
            "files",
        ],
    )?;
    if sdk.apks["schema_version"] != 1
        || sdk.apks["kind"] != "verified_closed_alpine_apk_set"
        || sdk.apks["arch"] != p.machine
        || sdk.apks["pins_sha256"] != pins_sha
        || sdk.apks["package_count"] != 61
    {
        return Err("Closed APK proof does not match current pins/architecture".into());
    }
    let mut total = 0u64;
    let mut previous = "";
    let mut manifest = String::new();
    for (index, (name, file)) in names.iter().zip(files).enumerate() {
        let name = name.as_str().ok_or("Invalid pinned APK filename")?;
        exact_keys(file, &["filename", "bytes", "sha256"])?;
        let hash = text(file, "sha256")?;
        let size = file["bytes"]
            .as_u64()
            .filter(|size| *size <= 64 * 1024 * 1024)
            .ok_or("Invalid APK byte bound")?;
        total = total
            .checked_add(size)
            .filter(|n| *n <= 256 * 1024 * 1024)
            .ok_or("APK aggregate byte bound")?;
        let expected_hash = set["sha256_base64"][p.machine][index]
            .as_str()
            .ok_or("Invalid pinned APK digest type")?;
        if !apk_filename(name)
            || name <= previous
            || file["filename"] != name
            || sha_base64(hash)? != expected_hash
        {
            return Err("APK filename/order/digest does not match closed pin tuple".into());
        }
        previous = name;
        manifest.push_str(&format!("{hash}  /tmp/ci-packages/{name}\n"));
    }
    if sdk.apks["total_bytes"] != total || sdk.expected != manifest || sdk.actual != manifest {
        return Err("APK totals or retained byte manifests differ".into());
    }
    let packages = sdk
        .installed
        .strip_suffix('\n')
        .ok_or("Unterminated SDK package inventory")?
        .split('\n')
        .collect::<Vec<_>>();
    if packages.len() > 512
        || packages.iter().any(|row| {
            row.is_empty()
                || row.len() > 256
                || !row
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"+_.-".contains(&b))
        })
        || packages.windows(2).any(|pair| pair[0] >= pair[1])
        || names.iter().any(|name| {
            packages
                .binary_search(&name.as_str().unwrap_or("").trim_end_matches(".apk"))
                .is_err()
        })
    {
        return Err("Installed SDK packages do not contain the exact closed set".into());
    }
    let archive = format!(
        "{}  rust-1.99.0-{}-unknown-linux-musl.tar.xz\n",
        p.rust_archive, p.machine
    );
    if sdk.rust_archive != archive
        || sdk.tar.lines().next() != Some("tar (GNU tar) 1.35")
        || sdk.xz.lines().next() != Some("xz (XZ Utils) 5.8.4")
    {
        return Err("Pinned Rust archive or extraction tools do not match SDK".into());
    }
    Ok(())
}

fn installed_packages_match(sdk: &SdkInputs, actual: &str) -> Result<(), String> {
    let mut rows = actual.lines().collect::<Vec<_>>();
    if rows.len() > 512 {
        return Err("Actual SDK package inventory exceeds bound".into());
    }
    rows.sort_unstable();
    if format!("{}\n", rows.join("\n")) != sdk.installed {
        return Err("Actual SDK packages differ from retained closed-install witness".into());
    }
    Ok(())
}

fn build_image_identity(root: &Path) -> Result<Value, String> {
    let path = output_base(root).join("build-image-id.txt");
    let bytes = stable(&path, 4096)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "Invalid public SDK image identifier")?;
    let id = text
        .strip_suffix('\n')
        .ok_or("Unterminated SDK image identifier")?;
    if !id.strip_prefix("sha256:").is_some_and(|hash| hex(hash, 64)) {
        return Err("Invalid derived SDK image identifier".into());
    }
    let file = descriptor(&path, 4096)?;
    if file["sha256"] != digest(&bytes) {
        return Err("SDK image identifier changed while observing it".into());
    }
    Ok(
        json!({"id":id,"file":file,"scope":"Derived local SDK image ID reported by reviewed Docker workflow, distinct from pinned base manifest; not independent image attestation"}),
    )
}

fn sdk_inputs(root: &Path, p: MuslProfile) -> Result<(SdkInputs, Value), String> {
    let source_pins = stable(&root.join(PINS), 65536)?;
    if digest(&source_pins) != PINS_SHA {
        return Err("Unreviewed musl SDK pin bytes".into());
    }
    let mut files = serde_json::Map::new();
    let mut content = BTreeMap::new();
    for name in [
        "sdk-pins.json",
        "apk-closed-inputs.json",
        "apk-package-expected.sha256",
        "apk-package-bytes.sha256",
        "installed-packages.txt",
        "rust-archive.sha256",
        "tar-version.txt",
        "xz-version.txt",
    ] {
        let path = Path::new(SDK).join(name);
        let bytes = stable(&path, INPUT_MAX)?;
        let witness = descriptor(&path, INPUT_MAX)?;
        if witness["sha256"] != digest(&bytes) || witness["bytes"] != bytes.len() {
            return Err("SDK bytes differ from their observed descriptor".into());
        }
        files.insert(name.into(), witness);
        content.insert(
            name,
            String::from_utf8(bytes).map_err(|_| "Invalid public SDK metadata UTF-8")?,
        );
    }
    if digest(content["sdk-pins.json"].as_bytes()) != PINS_SHA {
        return Err("SDK pin copy differs from source pins".into());
    }
    let sdk = SdkInputs {
        pins: json_read(&source_pins)?,
        apks: json_read(content["apk-closed-inputs.json"].as_bytes())?,
        expected: content["apk-package-expected.sha256"].clone(),
        actual: content["apk-package-bytes.sha256"].clone(),
        installed: content["installed-packages.txt"].clone(),
        rust_archive: content["rust-archive.sha256"].clone(),
        tar: content["tar-version.txt"].trim().to_owned(),
        xz: content["xz-version.txt"].trim().to_owned(),
    };
    validate_sdk(&sdk, PINS_SHA, p)?;
    let witness = json!({"files":files,"closed_apks":sdk.apks,"installed_packages":sdk.installed.split('\n').collect::<Vec<_>>(),
        "rust_archive_sha256":sdk.rust_archive,"tar_text":sdk.tar,"xz_text":sdk.xz});
    Ok((sdk, witness))
}

struct Tool {
    invocation: PathBuf,
    canonical: PathBuf,
}
fn selected_tool(env: &BTreeMap<String, String>, name: &str) -> Result<Tool, String> {
    let path = env_string(env, "PATH")?;
    if std::env::split_paths(&path).any(|part| !part.is_absolute()) {
        return Err("Musl compiler PATH must contain only absolute directories".into());
    }
    let resolved = resolve_tool(env, name)?;
    for parent in std::env::split_paths(&path) {
        if !parent.is_absolute() {
            return Err("Musl compiler PATH must contain only absolute directories".into());
        }
        let invocation = parent.join(name);
        if invocation.is_file() {
            if invocation
                .canonicalize()
                .map_err(|_| "Cannot resolve musl invocation")?
                != resolved
            {
                return Err("Musl selected tool changed".into());
            }
            return Ok(Tool {
                invocation,
                canonical: resolved,
            });
        }
    }
    Err("Missing musl tool invocation".into())
}

fn standalone_paths(rustc: &Tool, cargo: &Tool) -> Result<(), String> {
    for (name, tool) in [("rustc", rustc), ("cargo", cargo)] {
        let expected = Path::new("/opt/autorouter-rust/bin").join(name);
        if tool.invocation != expected || tool.canonical != expected {
            return Err("Musl requires pinned standalone Rust executables".into());
        }
    }
    Ok(())
}
fn standalone_tools(
    p: MuslProfile,
    rustc: &Tool,
    cargo: &Tool,
    rustc_text: &str,
) -> Result<(), String> {
    standalone_paths(rustc, cargo)?;
    if !rustc_text.lines().any(|line| line == "release: 1.99.0")
        || !rustc_text
            .lines()
            .any(|line| line == format!("host: {}", p.target))
    {
        return Err("Musl compiler version/host differs from pinned toolchain".into());
    }
    Ok(())
}

fn tool_identity(metadata: &fs::Metadata) -> [u64; 11] {
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
// SDK Node/compiler tools can exceed the product bound. Stream into a fixed
// buffer under the existing Mac supplier-sized limit; product caps stay exact.
fn sdk_tool_descriptor(path: &Path) -> Result<Value, String> {
    let path = canonical(path)?;
    let before = fs::symlink_metadata(&path).map_err(|_| "Cannot inspect musl SDK tool")?;
    if !before.is_file() || before.nlink() == 0 || before.len() > SDK_TOOL_MAX {
        return Err("Invalid or oversized musl SDK tool".into());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(&path)
        .map_err(|_| "Cannot open musl SDK tool")?;
    if tool_identity(&before)
        != tool_identity(
            &file
                .metadata()
                .map_err(|_| "Cannot inspect opened SDK tool")?,
        )
    {
        return Err("Musl SDK tool changed before hashing".into());
    }
    let mut hash = Sha256::new();
    let mut count = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|_| "Cannot read musl SDK tool")?;
        if n == 0 {
            break;
        }
        count = count
            .checked_add(n as u64)
            .filter(|size| *size <= before.len() && *size <= SDK_TOOL_MAX)
            .ok_or("Musl SDK tool grew during hashing")?;
        hash.update(&buffer[..n]);
    }
    if count != before.len()
        || tool_identity(&before)
            != tool_identity(
                &file
                    .metadata()
                    .map_err(|_| "Cannot inspect hashed SDK tool")?,
            )
        || tool_identity(&before)
            != tool_identity(&fs::symlink_metadata(&path).map_err(|_| "Musl SDK tool disappeared")?)
    {
        return Err("Musl SDK tool changed during hashing".into());
    }
    Ok(
        json!({"path":path,"bytes":count,"sha256":format!("{:x}",hash.finalize()),"identity":{"device":before.dev(),"inode":before.ino(),"mode":before.mode(),"links":before.nlink(),
        "mtime_seconds":before.mtime(),"mtime_nanoseconds":before.mtime_nsec(),"ctime_seconds":before.ctime(),"ctime_nanoseconds":before.ctime_nsec()}}),
    )
}
fn tool_witness(tool: &Tool) -> Result<Value, String> {
    Ok(json!({"invocation_path":tool.invocation,"file":sdk_tool_descriptor(&tool.canonical)?}))
}

// Docker's numeric caller UID need not have a passwd entry. Keep metadata
// commands on the already admitted build homes without inheriting other values.
fn metadata_environment(
    env: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    Ok(BTreeMap::from([
        ("PATH".into(), env_string(env, "PATH")?),
        ("HOME".into(), env_string(env, "HOME")?),
        ("CARGO_HOME".into(), env_string(env, "CARGO_HOME")?),
        ("LC_ALL".into(), "C".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_NO_REPLACE_OBJECTS".into(), "1".into()),
    ]))
}

fn capture_metadata(
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
        return Err("Public compiler metadata exceeds bound".into());
    }
    String::from_utf8(bytes)
        .map(|value| value.trim().to_owned())
        .map_err(|_| "Invalid compiler metadata UTF-8".into())
}

fn metadata(
    env: &BTreeMap<String, String>,
    root: &Path,
    tool: &Tool,
    args: &[&str],
    maximum: usize,
) -> Result<String, String> {
    let name = tool
        .invocation
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| {
            [
                "rustc", "cargo", "gcc", "g++", "ld", "uname", "perl", "tar", "xz", "node", "apk",
            ]
            .contains(name)
        })
        .unwrap_or("metadata-tool");
    capture_metadata(env, root, &tool.invocation, args, maximum)
        .map_err(|error| format!("Musl metadata {name} failed: {error}"))
}
fn admitted_wrapper(
    p: MuslProfile,
    name: &str,
    compiler: &Tool,
    bytes: &[u8],
) -> Result<String, String> {
    if !["CC", "CXX"].contains(&name) {
        return Err("Unknown musl compiler wrapper role".into());
    }
    let expected = format!(
        "#!/bin/sh\nexec /usr/bin/{} {} \"$@\"\n",
        if name == "CC" { "gcc" } else { "g++" },
        p.c_cpu
    );
    if bytes != expected.as_bytes()
        || compiler.invocation
            != Path::new(if name == "CC" {
                "/usr/bin/gcc"
            } else {
                "/usr/bin/g++"
            })
    {
        return Err("Musl wrapper does not enforce the exact reviewed compiler baseline".into());
    }
    Ok(expected)
}
fn wrapper_witness(
    env: &BTreeMap<String, String>,
    root: &Path,
    p: MuslProfile,
    name: &str,
    compiler: &Tool,
) -> Result<Value, String> {
    let path = PathBuf::from(env_string(env, name)?);
    let bytes = stable(&path, 4096)?;
    let expected = admitted_wrapper(p, name, compiler, &bytes)?;
    Ok(
        json!({"path":path,"contents":expected,"sha256":digest(&bytes),"identity":descriptor(&path,4096)?,"compiler":tool_witness(compiler)?,
        "effective_target_options":capture_metadata(env,root,&path,&["-Q","--help=target"],65536)?,
        "baseline_macros":capture_metadata(env,root,&path,&["-dM","-E","-x",if name=="CC" {"c"} else {"c++"},"-"],65536)?}),
    )
}

fn validate_builder(builder: &Value, state: &Value, p: MuslProfile) -> Result<(), String> {
    exact_keys(
        builder,
        &[
            "schema_version",
            "kind",
            "source_commit",
            "source_dirty",
            "cargo_lock_sha256",
            "workflow_sha256",
            "sdk_dockerfile_sha256",
            "sdk_script_sha256",
            "pins_sha256",
            "node_image",
            "workflow_run",
            "target",
            "node",
            "rustc",
            "cargo",
            "kernel",
            "alpine",
            "package_index_sha256",
            "package_selection",
            "apk_package_set",
            "apk_fetcher_sha256",
            "package_bytes_sha256",
            "installed_packages",
            "rust_archive_sha256",
            "gcc",
            "musl",
            "perl",
            "tar",
            "xz",
            "cc_wrapper",
            "cxx_wrapper",
            "rustflags",
            "cflags",
            "source_date_epoch",
            "qualification",
        ],
    )?;
    if builder["schema_version"] != 3
        || builder["kind"] != "musl_ci_builder_inputs"
        || builder["source_dirty"] != false
        || builder["target"] != p.target
        || builder["node_image"] != p.image
        || builder["pins_sha256"] != PINS_SHA
        || builder["node"] != "v22.14.0"
        || builder["package_index_sha256"] != Value::Null
        || builder["package_selection"] != "closed_apk_sha256_manifest_v1"
        || builder["qualification"] != QUALIFICATION
        || builder["cflags"] != "-O2"
    {
        return Err("Unreviewed musl builder schema/profile/SDK claim".into());
    }
    for (name, path) in [
        ("workflow_sha256", MUSL_WORKFLOW),
        ("sdk_dockerfile_sha256", DOCKERFILE),
        ("sdk_script_sha256", SCRIPT),
        ("apk_fetcher_sha256", FETCHER),
        ("pins_sha256", PINS),
    ] {
        if builder[name] != state["files"][path]["sha256"] {
            return Err("Musl builder source file differs from observed source".into());
        }
    }
    for name in ["source_commit", "cargo_lock_sha256", "source_date_epoch"] {
        if builder[name] != state[name] {
            return Err("Musl builder source/epoch differs from checkout".into());
        }
    }
    for (name, field) in [
        ("rustc", "rustc_text"),
        ("cargo", "cargo_text"),
        ("gcc", "gcc_text"),
        ("kernel", "kernel_text"),
        ("alpine", "alpine"),
        ("musl", "musl_text"),
        ("perl", "perl_text"),
        ("tar", "tar_text"),
        ("xz", "xz_text"),
    ] {
        if builder[name] != state["compiler"][field] {
            return Err("Musl builder compiler/SDK metadata differs from observation".into());
        }
    }
    if builder["workflow_run"] != state["environment"]["AUTOROUTER_WORKFLOW_RUN"]
        || builder["rustflags"] != state["environment"]["RUSTFLAGS"]
        || builder["cc_wrapper"] != state["compiler"]["wrappers"]["CC"]["contents"]
        || builder["cxx_wrapper"] != state["compiler"]["wrappers"]["CXX"]["contents"]
        || builder["apk_package_set"] != state["sdk"]["closed_apks"]
        || builder["installed_packages"] != state["sdk"]["installed_packages"]
        || builder["rust_archive_sha256"] != state["sdk"]["rust_archive_sha256"]
        || builder["package_bytes_sha256"] != state["sdk"]["package_bytes_sha256"]
    {
        return Err("Musl builder closure/wrappers/report context differs from observation".into());
    }
    Ok(())
}

pub(super) fn snapshot(root: &Path, p: MuslProfile, builder_path: &Path) -> Result<Value, String> {
    canonical(root)?;
    let env = select_musl_env(std::env::vars_os())?;
    let claimed = env_string(&env, "GITHUB_SHA")?;
    let ci = validate_env(&env, p, root, &claimed)?;
    let commit = git(&env, root, &["rev-parse", "--verify", "HEAD"])?;
    if commit != claimed
        || !git(
            &env,
            root,
            &["status", "--porcelain", "--untracked-files=normal"],
        )?
        .is_empty()
    {
        return Err("Musl provenance requires a clean exact source checkout".into());
    }
    let config = cargo_config(root, &env)?;
    let epoch = git(&env, root, &["show", "-s", "--format=%ct", "HEAD"])?;
    if env_string(&env, "SOURCE_DATE_EPOCH")? != epoch {
        return Err("Musl source epoch differs from clean checkout".into());
    }
    for name in ["HOME", "CARGO_HOME"] {
        if !canonical(Path::new(&env_string(&env, name)?))?.is_dir() {
            return Err("Musl synthetic build home is not a directory".into());
        }
    }
    let (sdk, mut sdk_witness) = sdk_inputs(root, p)?;
    sdk_witness["package_bytes_sha256"] = json!(sdk.actual);
    sdk_witness["derived_image"] = build_image_identity(root)?;
    let mut tools = BTreeMap::new();
    let mut witnesses = serde_json::Map::new();
    for name in [
        "git", "rustc", "cargo", "gcc", "g++", "cc", "ld", "uname", "perl", "tar", "xz", "node",
        "apk",
    ] {
        let tool = selected_tool(&env, name)?;
        witnesses.insert(name.into(), tool_witness(&tool)?);
        tools.insert(name, tool);
    }
    standalone_paths(&tools["rustc"], &tools["cargo"])?;
    if tools["cc"].canonical != tools["gcc"].canonical {
        return Err("Musl default C linker driver differs from SDK GCC".into());
    }
    installed_packages_match(
        &sdk,
        &metadata(&env, root, &tools["apk"], &["info", "-v"], 65536)?,
    )?;
    let rustc_text = metadata(&env, root, &tools["rustc"], &["-vV"], 16384)?;
    standalone_tools(p, &tools["rustc"], &tools["cargo"], &rustc_text)?;
    if metadata(&env, root, &tools["uname"], &["-m"], 4096)? != p.machine
        || metadata(&env, root, &tools["node"], &["--version"], 4096)? != "v22.14.0"
    {
        return Err("Musl native machine/Node version differs from profile".into());
    }
    let alpine = String::from_utf8(stable(Path::new("/etc/alpine-release"), 4096)?)
        .map_err(|_| "Invalid Alpine release metadata")?
        .trim()
        .to_owned();
    if !alpine
        .strip_prefix("3.21.")
        .is_some_and(|patch| !patch.is_empty() && patch.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("Musl SDK is not the reviewed Alpine3.21 userland".into());
    }
    let compiler = json!({"tools":witnesses,"rustc_text":rustc_text,"cargo_text":metadata(&env,root,&tools["cargo"],&["--version"],4096)?,
        "gcc_text":metadata(&env,root,&tools["gcc"],&["--version"],16384)?,"linker_text":metadata(&env,root,&tools["ld"],&["--version"],16384)?,
        "kernel_text":metadata(&env,root,&tools["uname"],&["-srmo"],4096)?,"alpine":alpine,"musl_text":metadata(&env,root,&tools["apk"],&["info","-v","musl"],4096)?,
        "perl_text":metadata(&env,root,&tools["perl"],&["-V"],65536)?,"tar_text":metadata(&env,root,&tools["tar"],&["--version"],16384)?,"xz_text":metadata(&env,root,&tools["xz"],&["--version"],16384)?,
        "wrappers":{"CC":wrapper_witness(&env,root,p,"CC",&tools["gcc"] )?,"CXX":wrapper_witness(&env,root,p,"CXX",&tools["g++"] )?}});
    if compiler["tar_text"] != sdk.tar || compiler["xz_text"] != sdk.xz {
        return Err("SDK extraction tool metadata differs from retained witness".into());
    }
    let mut files = serde_json::Map::new();
    for name in [
        "rust/Cargo.lock",
        "rust/Cargo.toml",
        "rust/rust-toolchain.toml",
        "rust/.cargo/config.toml",
        MUSL_WORKFLOW,
        DOCKERFILE,
        SCRIPT,
        FETCHER,
        PINS,
    ] {
        files.insert(name.into(), descriptor(&root.join(name), INPUT_MAX)?);
    }
    let builder_path = canonical(builder_path)?;
    let builder = json_read(&single_report(&builder_path, INPUT_MAX)?)?;
    let state = json!({"schema_version":1,"kind":"native_binary_build_inputs","profile":"musl-alpine321-v1","target":p.target,"source_commit":commit,"source_dirty":false,
        "cargo_lock_sha256":files["rust/Cargo.lock"]["sha256"],"source_date_epoch":epoch,"ci":ci,"environment":env,"files":files,"cargo_config":config,
        "compiler":compiler,"sdk":sdk_witness,"builder":descriptor(&builder_path,INPUT_MAX)?,
        "command":["cargo","build","--release","--offline","--locked","--package","claude-autorouter","--message-format=json"],"working_directory":root.join("rust")});
    validate_builder(&builder, &state, p)?;
    Ok(state)
}

pub(super) fn inspect(p: MuslProfile, binary: &[u8]) -> Result<Value, String> {
    let value = crate::provenance_binary::inspect(p.target, binary)?;
    if value["format"] != "ELF"
        || value["target"] != p.target
        || value["interpreter"] != Value::Null
        || value["dynamic_libraries"] != json!([])
        || value["rpath"] != Value::Null
        || value["runpath"] != Value::Null
        || value["version_requirements"] != json!([])
        || value["glibc_version_requirements"] != json!([])
        || value["minimum_glibc"] != Value::Null
    {
        return Err(
            "Musl artifact is not a static executable without loader/library/version paths".into(),
        );
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    // Inline metadata and private Scratch files only. No real SDK, compiler,
    // downloads, checkout fixtures, subprocesses or process-environment changes.
    use super::*;
    use crate::audit_support::Scratch;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn profiles() -> [MuslProfile; 2] {
        [
            profile("x86_64-unknown-linux-musl").unwrap(),
            profile("aarch64-unknown-linux-musl").unwrap(),
        ]
    }
    fn env_fixture(p: MuslProfile, root: &Path) -> BTreeMap<String, String> {
        let out = output_base(root);
        let mut env = BTreeMap::new();
        for (name, value) in [
            ("PATH", SDK_PATH),
            ("CARGO_INCREMENTAL", "0"),
            ("CARGO_PROFILE_DEV_DEBUG", "0"),
            ("CARGO_PROFILE_TEST_DEBUG", "0"),
            ("CARGO_BUILD_TARGET", p.target),
            ("CC", "/tmp/autorouter-compilers/cc"),
            ("CXX", "/tmp/autorouter-compilers/cxx"),
            ("CFLAGS", "-O2"),
            ("CXXFLAGS", "-O2"),
            ("OPENSSL_NO_VENDOR", "0"),
            ("OPENSSL_STATIC", "1"),
            ("AUTOROUTER_BUILD_TARGET", p.target),
            ("AUTOROUTER_BUILD_MACHINE", p.machine),
            ("AUTOROUTER_NODE_IMAGE", p.image),
            ("SOURCE_DATE_EPOCH", "1700000000"),
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_REPOSITORY", REPOSITORY),
            ("GITHUB_SERVER_URL", "https://github.com"),
            ("GITHUB_JOB", "musl"),
            ("GITHUB_RUN_ID", "41"),
            ("GITHUB_RUN_ATTEMPT", "2"),
        ] {
            env.insert(name.into(), value.into());
        }
        for (name, path) in [
            ("HOME", out.join("home")),
            ("CARGO_HOME", out.join("cargo-cache")),
            ("CARGO_TARGET_DIR", out.join("target-main")),
            ("AUTOROUTER_SOURCE_ROOT", root.to_path_buf()),
        ] {
            env.insert(name.into(), path.to_str().unwrap().into());
        }
        env.insert("GITHUB_SHA".into(), "a".repeat(40));
        env.insert("AUTOROUTER_SOURCE_COMMIT".into(), "a".repeat(40));
        env.insert("GITHUB_WORKFLOW_SHA".into(), "b".repeat(40));
        env.insert(
            "GITHUB_WORKFLOW_REF".into(),
            format!("{REPOSITORY}/{MUSL_WORKFLOW}@refs/pull/17/merge"),
        );
        env.insert(
            "AUTOROUTER_WORKFLOW_RUN".into(),
            format!("https://github.com/{REPOSITORY}/actions/runs/41"),
        );
        env.insert("RUSTFLAGS".into(), rustflags(p, root).unwrap());
        env
    }
    fn state(p: MuslProfile, root: &Path) -> Value {
        json!({"environment":env_fixture(p,root)})
    }
    fn sdk_fixture(p: MuslProfile) -> SdkInputs {
        let names = (0..61)
            .map(|i| format!("package-{i:02}-1.0-r0.apk"))
            .collect::<Vec<_>>();
        let digests = |arch: &str| {
            (0..61)
                .map(|i| digest(format!("synthetic-{arch}-{i}").as_bytes()))
                .collect::<Vec<_>>()
        };
        let hashes = digests(p.machine);
        let files = names
            .iter()
            .zip(&hashes)
            .enumerate()
            .map(|(i, (name, hash))| json!({"filename":name,"bytes":i+1,"sha256":hash}))
            .collect::<Vec<_>>();
        let manifest = names
            .iter()
            .zip(&hashes)
            .map(|(name, hash)| format!("{hash}  /tmp/ci-packages/{name}\n"))
            .collect::<String>();
        let targets = profiles().iter().map(|p|json!({"target":p.target,"arch":p.machine,"runner":p.runner,"node_image":p.image,"rust_archive_sha256":p.rust_archive})).collect::<Vec<_>>();
        // The admission digest is a supplied pure-validator boundary here.
        // Production separately requires actual source/SDK byte hash PINS_SHA.
        SdkInputs {
            pins: json!({"schema_version":2,"rust_version":"1.99.0","node_version":"22.14.0","targets":targets,
            "apk_package_set":{"schema_version":1,"kind":"closed_alpine_apk_sha256_set","repository":"https://dl-cdn.alpinelinux.org/alpine/v3.21/main",
            "filenames":names,"sha256_base64":{"x86_64":digests("x86_64").iter().map(|h|sha_base64(h).unwrap()).collect::<Vec<_>>(),"aarch64":digests("aarch64").iter().map(|h|sha_base64(h).unwrap()).collect::<Vec<_>>()}}}),
            apks: json!({"schema_version":1,"kind":"verified_closed_alpine_apk_set","arch":p.machine,"pins_sha256":PINS_SHA,"package_count":61,"total_bytes":1891,"files":files}),
            expected: manifest.clone(),
            actual: manifest,
            installed: names
                .iter()
                .map(|name| format!("{}\n", name.trim_end_matches(".apk")))
                .collect(),
            rust_archive: format!(
                "{}  rust-1.99.0-{}-unknown-linux-musl.tar.xz\n",
                p.rust_archive, p.machine
            ),
            tar: "tar (GNU tar) 1.35\nsynthetic metadata".into(),
            xz: "xz (XZ Utils) 5.8.4\nsynthetic metadata".into(),
        }
    }

    #[test]
    fn parent_dispatch_requires_builder_and_exact_musl_profile_target_pairs() {
        for p in profiles() {
            let args = [
                "--target",
                p.target,
                "--builder-report",
                "/synthetic/builder.json",
                "--build-report",
                "/synthetic/build.jsonl",
                "--feature-report",
                "/synthetic/features.json",
                "--out",
                "/synthetic/inputs.json",
            ]
            .map(str::to_owned);
            assert!(matches!(
                begin_options(&args).unwrap().0,
                BuildProfile::Musl(_)
            ));
            let without_builder = args
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != 2 && *i != 3)
                .map(|(_, v)| v.clone())
                .collect::<Vec<_>>();
            assert!(begin_options(&without_builder).is_err());
            let good = json!({"target":p.target,"profile":"musl-alpine321-v1","builder":{"path":"/synthetic/builder.json"}});
            assert!(matches!(
                BuildProfile::from_inputs(&good).unwrap(),
                BuildProfile::Musl(_)
            ));
            for wrong in [
                "gnu-glibc228-v1",
                "macos-hosted-v1",
                "musl-alpine321-v2",
                "",
            ] {
                let mut changed = good.clone();
                changed["profile"] = json!(wrong);
                assert!(BuildProfile::from_inputs(&changed).is_err());
            }
            for wrong in [
                "x86_64-unknown-linux-gnu",
                "aarch64-apple-darwin",
                "i686-unknown-linux-musl",
            ] {
                let mut changed = good.clone();
                changed["target"] = json!(wrong);
                assert!(BuildProfile::from_inputs(&changed).is_err());
            }
        }
        assert!(profile("x86_64-unknown-linux-gnu").is_err());
    }

    #[test]
    fn parent_flags_and_ci_metadata_bind_both_native_profiles() {
        let root = Path::new("/synthetic/musl");
        for p in profiles() {
            let expected = format!(
                "-C target-cpu={} -C target-feature=+crt-static --remap-path-prefix=/synthetic/musl=/autorouter-source",
                p.rust_cpu
            );
            assert_eq!(
                BuildProfile::reviewed(p.target)
                    .unwrap()
                    .rustflags(root)
                    .unwrap(),
                expected
            );
            let ci = validate_env(&env_fixture(p, root), p, root, &"a".repeat(40)).unwrap();
            assert_eq!(
                ci["workflow_run"],
                format!("https://github.com/{REPOSITORY}/actions/runs/41/attempts/2")
            );
            assert_eq!(ci["job"], "musl");
            assert_eq!(ci["runner_profile"], p.runner);
        }
    }

    #[test]
    fn musl_environment_selection_is_scoped_private_and_bounded() {
        let pairs = [
            (OsString::from("HOME"), OsString::from("/synthetic/home")),
            (
                OsString::from("AUTOROUTER_NODE_IMAGE"),
                OsString::from("public-image"),
            ),
            (
                OsString::from("GITHUB_TOKEN"),
                OsString::from("fake-secret"),
            ),
        ];
        assert!(super::super::select_env(pairs.clone()).unwrap().is_empty());
        let selected = select_musl_env(pairs).unwrap();
        assert_eq!(selected.len(), 2);
        assert!(!selected.contains_key("GITHUB_TOKEN"));
        let duplicate = [
            (OsString::from("HOME"), OsString::from("/one")),
            (OsString::from("HOME"), OsString::from("/two")),
        ];
        assert!(select_musl_env(duplicate).is_err());
        for count in [65, 66] {
            assert!(
                select_musl_env((0..count).map(|i| (
                    OsString::from(format!("RUST_TEST_{i}")),
                    OsString::from("x")
                )))
                .is_err()
            );
        }
        assert!(
            select_musl_env((0..8).map(|i| (
                OsString::from(format!("RUST_TEST_{i}")),
                OsString::from("x".repeat(8192))
            )))
            .is_err()
        );
        assert!(
            select_musl_env([(OsString::from("HOME"), OsString::from("x".repeat(8193)))]).is_err()
        );
        assert!(
            select_musl_env([(OsString::from("HOME"), OsString::from_vec(vec![0xff]))]).is_err()
        );
    }

    #[test]
    fn every_build_override_and_ci_domain_fails_shut_when_changed() {
        let root = Path::new("/synthetic/musl");
        for p in profiles() {
            let good = env_fixture(p, root);
            for key in good.keys() {
                let mut bad = good.clone();
                bad.insert(key.clone(), "unreviewed".into());
                assert!(
                    validate_env(&bad, p, root, &"a".repeat(40)).is_err(),
                    "{key}"
                );
            }
            for key in [
                "RUSTC_WRAPPER",
                "RUSTFLAGS_ENCODED",
                "CARGO_ENCODED_RUSTFLAGS",
                "CARGO_PROFILE_RELEASE_OPT_LEVEL",
                "CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER",
                "HOST_CC",
                "TARGET_CFLAGS",
                "CC_aarch64_unknown_linux_musl",
                "PKG_CONFIG_PATH",
                "LD_LIBRARY_PATH",
                "RUSTUP_HOME",
                "BASH_ENV",
                "AUTOROUTER_BUILD_UNKNOWN",
            ] {
                let mut bad = good.clone();
                bad.insert(key.into(), "fake-private-override".into());
                assert!(
                    validate_env(&bad, p, root, &"a".repeat(40)).is_err(),
                    "{key}"
                );
            }
            for path in [
                format!("{SDK_PATH}:relative"),
                format!("{SDK_PATH}:"),
                "/unreviewed/bin".into(),
            ] {
                let mut bad = good.clone();
                bad.insert("PATH".into(), path);
                assert!(validate_env(&bad, p, root, &"a".repeat(40)).is_err());
            }
        }
    }

    #[test]
    fn real_owned_source_paths_cannot_inject_remap_flags() {
        let scratch = Scratch::new("musl-remap-source").unwrap();
        let p = profiles()[0];
        for name in [
            "with space",
            "with=equals",
            "with\ttab",
            "with\nline",
            "with\u{a0}unicode",
        ] {
            let path = scratch.0.join(name);
            fs::create_dir(&path).unwrap();
            assert!(rustflags(p, &path).is_err());
        }
        assert!(rustflags(p, Path::new("relative")).is_err());
        assert!(
            rustflags(
                p,
                &PathBuf::from(OsString::from_vec(b"/non-utf8-\xff".to_vec()))
            )
            .is_err()
        );
        assert!(rustflags(p, Path::new(&format!("/{}", "x".repeat(4097)))).is_err());
    }

    #[test]
    fn canonical_sha256_base64_has_known_padding_and_rejects_non_digests() {
        assert_eq!(
            sha_base64(&"00".repeat(32)).unwrap(),
            format!("{}=", "A".repeat(43))
        );
        assert_eq!(
            sha_base64(&"ff".repeat(32)).unwrap(),
            format!("{}8=", "/".repeat(42))
        );
        assert!(canonical_sha_base64(&sha_base64(&"00".repeat(32)).unwrap()));
        assert!(!canonical_sha_base64(&format!("{}9=", "/".repeat(42))));
        assert!(!canonical_sha_base64(&"A".repeat(44)));
        for bad in [
            "a".repeat(63),
            "A".repeat(64),
            "x".repeat(64),
            format!("{}é", "0".repeat(62)),
        ] {
            assert!(sha_base64(&bad).is_err());
        }
    }

    #[test]
    fn complete_inline_61_tuple_proofs_bind_every_digest_for_both_architectures() {
        for p in profiles() {
            let sdk = sdk_fixture(p);
            validate_sdk(&sdk, PINS_SHA, p).unwrap();
            for index in 0..61 {
                let mut bad = sdk_fixture(p);
                bad.apks["files"][index]["sha256"] = json!("0".repeat(64));
                assert!(
                    validate_sdk(&bad, PINS_SHA, p).is_err(),
                    "{} tuple{index}",
                    p.target
                );
                let mut bad = sdk_fixture(p);
                bad.pins["apk_package_set"]["sha256_base64"][p.machine][index] =
                    json!("A".repeat(44));
                assert!(
                    validate_sdk(&bad, PINS_SHA, p).is_err(),
                    "{} pin{index}",
                    p.target
                );
            }
        }
    }

    #[test]
    fn apk_schema_inventory_order_and_checked_byte_limits_cannot_be_substituted() {
        let p = profiles()[0];
        for (key, value) in [
            ("schema_version", json!(2)),
            ("kind", json!("other")),
            ("arch", json!("aarch64")),
            ("pins_sha256", json!("0".repeat(64))),
            ("package_count", json!(60)),
            ("total_bytes", json!(0)),
            ("total_bytes", json!("1891")),
        ] {
            let mut bad = sdk_fixture(p);
            bad.apks[key] = value;
            assert!(validate_sdk(&bad, PINS_SHA, p).is_err(), "{key}");
        }
        let mut bad = sdk_fixture(p);
        bad.apks["files"].as_array_mut().unwrap().pop();
        assert!(validate_sdk(&bad, PINS_SHA, p).is_err());
        let mut bad = sdk_fixture(p);
        bad.apks["files"].as_array_mut().unwrap().swap(0, 1);
        assert!(validate_sdk(&bad, PINS_SHA, p).is_err());
        let mut bad = sdk_fixture(p);
        bad.pins["apk_package_set"]["filenames"][1] =
            bad.pins["apk_package_set"]["filenames"][0].clone();
        assert!(validate_sdk(&bad, PINS_SHA, p).is_err());
        let mut bad = sdk_fixture(p);
        bad.pins["apk_package_set"]["filenames"]
            .as_array_mut()
            .unwrap()
            .swap(0, 1);
        assert!(validate_sdk(&bad, PINS_SHA, p).is_err());
        let mut bad = sdk_fixture(p);
        bad.pins["apk_package_set"]["filenames"][0] = json!("../package-00-1.0-r0.apk");
        assert!(validate_sdk(&bad, PINS_SHA, p).is_err());
        let mut bad = sdk_fixture(p);
        bad.pins["apk_package_set"]["sha256_base64"]["other"] = json!([]);
        assert!(validate_sdk(&bad, PINS_SHA, p).is_err());
        let mut bad = sdk_fixture(p);
        bad.apks["files"][0]["extra"] = json!(true);
        assert!(validate_sdk(&bad, PINS_SHA, p).is_err());
        for value in [
            json!(-1),
            json!("1"),
            json!(64 * 1024 * 1024u64 + 1),
            json!(u64::MAX),
        ] {
            let mut bad = sdk_fixture(p);
            bad.apks["files"][0]["bytes"] = value;
            assert!(validate_sdk(&bad, PINS_SHA, p).is_err());
        }
        let mut bad = sdk_fixture(p);
        for file in bad.apks["files"].as_array_mut().unwrap() {
            file["bytes"] = json!(64 * 1024 * 1024u64);
        }
        bad.apks["total_bytes"] = json!(61 * 64 * 1024 * 1024u64);
        assert!(validate_sdk(&bad, PINS_SHA, p).is_err());
    }

    #[test]
    fn retained_manifests_installed_packages_and_extractors_are_required() {
        let p = profiles()[0];
        for field in [
            "expected",
            "actual",
            "installed",
            "rust_archive",
            "tar",
            "xz",
        ] {
            let mut bad = sdk_fixture(p);
            match field {
                "expected" => bad.expected.clear(),
                "actual" => bad.actual.clear(),
                "installed" => bad.installed.clear(),
                "rust_archive" => bad.rust_archive.clear(),
                "tar" => bad.tar = "tar 1.35".into(),
                _ => bad.xz = "xz 5.8.4".into(),
            }
            assert!(validate_sdk(&bad, PINS_SHA, p).is_err(), "{field}");
        }
        let mut bad = sdk_fixture(p);
        bad.installed = bad
            .installed
            .lines()
            .skip(1)
            .map(|line| format!("{line}\n"))
            .collect();
        assert!(validate_sdk(&bad, PINS_SHA, p).is_err());
        let mut bad = sdk_fixture(p);
        bad.installed.push_str("package-00-1.0-r0\n");
        assert!(validate_sdk(&bad, PINS_SHA, p).is_err());
        let mut bad = sdk_fixture(p);
        bad.installed.pop();
        assert!(validate_sdk(&bad, PINS_SHA, p).is_err());
        let mut bad = sdk_fixture(p);
        bad.rust_archive = bad
            .rust_archive
            .replace("  rust-", "  /tmp/rust-bootstrap/rust-");
        assert!(validate_sdk(&bad, PINS_SHA, p).is_err());
    }

    #[test]
    fn standalone_and_alias_selection_use_distinct_invocation_and_file_identities() {
        for p in profiles() {
            let rustc = Tool {
                invocation: "/opt/autorouter-rust/bin/rustc".into(),
                canonical: "/opt/autorouter-rust/bin/rustc".into(),
            };
            let cargo = Tool {
                invocation: "/opt/autorouter-rust/bin/cargo".into(),
                canonical: "/opt/autorouter-rust/bin/cargo".into(),
            };
            let text = format!("release: 1.99.0\nhost: {}", p.target);
            standalone_tools(p, &rustc, &cargo, &text).unwrap();
            let bad = Tool {
                invocation: "/unreviewed/rustc".into(),
                canonical: rustc.canonical.clone(),
            };
            assert!(standalone_tools(p, &bad, &cargo, &text).is_err());
            let bad = Tool {
                invocation: rustc.invocation.clone(),
                canonical: "/unreviewed/rustup".into(),
            };
            assert!(standalone_tools(p, &bad, &cargo, &text).is_err());
            for wrong in [
                text.replace("1.99.0", "1.98.0"),
                text.replace(p.target, "x86_64-unknown-linux-gnu"),
            ] {
                assert!(standalone_tools(p, &rustc, &cargo, &wrong).is_err());
            }
        }
        let scratch = Scratch::new("musl-tool-alias").unwrap();
        let bin = scratch.0.join("bin");
        fs::create_dir(&bin).unwrap();
        let real = scratch.file("synthetic-driver", b"not executed").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
        let alias = bin.join("gcc");
        symlink(&real, &alias).unwrap();
        let mut env = BTreeMap::from([("PATH".into(), bin.to_str().unwrap().into())]);
        let tool = selected_tool(&env, "gcc").unwrap();
        assert_eq!(tool.invocation, alias);
        assert_eq!(tool.canonical, real);
        assert_eq!(tool_witness(&tool).unwrap()["file"]["path"], json!(real));
        env.insert("PATH".into(), format!("{}:relative", bin.display()));
        assert!(selected_tool(&env, "gcc").is_err());
    }

    #[test]
    fn parent_cold_dispatch_checks_both_trees_and_preserves_foreign_sentinels() {
        for p in profiles() {
            let scratch = Scratch::new("musl-cold-trees").unwrap();
            let before = state(p, &scratch.0);
            let parent = BuildProfile::reviewed(p.target).unwrap();
            parent.require_cold_release(&scratch.0, &before).unwrap();
            let target = output_base(&scratch.0).join("target-main");
            for release in [
                target.join("release"),
                target.join(p.target).join("release"),
            ] {
                fs::create_dir_all(&release).unwrap();
                fs::write(release.join("sentinel"), b"preserved cache").unwrap();
                assert!(parent.require_cold_release(&scratch.0, &before).is_err());
                assert_eq!(
                    fs::read(release.join("sentinel")).unwrap(),
                    b"preserved cache"
                );
                fs::remove_file(release.join("sentinel")).unwrap();
                fs::remove_dir(&release).unwrap();
                fs::write(&release, b"preserved file").unwrap();
                assert!(parent.require_cold_release(&scratch.0, &before).is_err());
                assert_eq!(fs::read(&release).unwrap(), b"preserved file");
                fs::remove_file(&release).unwrap();
            }
            parent.require_cold_release(&scratch.0, &before).unwrap();
            let mut wrong = before.clone();
            wrong["environment"]["CARGO_BUILD_TARGET"] = json!("x86_64-unknown-linux-gnu");
            assert!(parent.require_cold_release(&scratch.0, &wrong).is_err());
        }
    }

    #[test]
    fn target_parents_and_release_symlinks_refuse_without_removing_replacements() {
        let p = profiles()[0];
        let scratch = Scratch::new("musl-parent-alias").unwrap();
        let before = state(p, &scratch.0);
        let out = output_base(&scratch.0);
        fs::create_dir_all(&out).unwrap();
        let real = scratch.0.join("real");
        fs::create_dir(&real).unwrap();
        let target = out.join("target-main");
        symlink(&real, &target).unwrap();
        assert!(fresh_release_tree(&scratch.0, &before, p).is_err());
        assert!(
            fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_file(&target).unwrap();
        fs::create_dir(&target).unwrap();
        symlink(&real, target.join(p.target)).unwrap();
        assert!(fresh_release_tree(&scratch.0, &before, p).is_err());
        fs::remove_file(target.join(p.target)).unwrap();
        symlink(&real, target.join("release")).unwrap();
        assert!(fresh_release_tree(&scratch.0, &before, p).is_err());
        fs::remove_file(target.join("release")).unwrap();
        fs::write(target.join(p.target), b"foreign target file").unwrap();
        assert!(fresh_release_tree(&scratch.0, &before, p).is_err());
        assert_eq!(
            fs::read(target.join(p.target)).unwrap(),
            b"foreign target file"
        );
    }

    #[test]
    fn parent_executable_dispatch_requires_actual_target_release_and_no_alias() {
        for p in profiles() {
            let scratch = Scratch::new("musl-production-path").unwrap();
            let before = state(p, &scratch.0);
            let parent = BuildProfile::reviewed(p.target).unwrap();
            let target = output_base(&scratch.0).join("target-main");
            let binary = target.join(p.target).join("release/claude-autorouter");
            fs::create_dir_all(binary.parent().unwrap()).unwrap();
            fs::write(&binary, b"owned synthetic executable; not executed").unwrap();
            let inspected = json!({"binary":{"executable":binary}});
            assert_eq!(
                parent.executable(&scratch.0, &before, &inspected).unwrap(),
                binary
            );
            let wrong = target.join("release/claude-autorouter");
            fs::create_dir_all(wrong.parent().unwrap()).unwrap();
            fs::write(&wrong, b"wrong host artifact").unwrap();
            assert!(
                parent
                    .executable(&scratch.0, &before, &json!({"binary":{"executable":wrong}}))
                    .is_err()
            );
            let alias = target.join(p.target).join("release/alias");
            symlink(&binary, &alias).unwrap();
            assert!(
                parent
                    .executable(&scratch.0, &before, &json!({"binary":{"executable":alias}}))
                    .is_err()
            );
        }
    }

    #[test]
    fn config_discovery_is_tested_with_only_owned_paths_and_each_override_refuses() {
        let p = profiles()[0];
        let scratch = Scratch::new("musl-cargo-config").unwrap();
        let env = env_fixture(p, &scratch.0);
        let paths = config_paths(&scratch.0, &env).unwrap();
        let admitted = scratch.0.join("rust/.cargo/config.toml");
        assert!(paths.contains(&PathBuf::from("/.cargo/config.toml")));
        assert!(paths.contains(&PathBuf::from(&env["CARGO_HOME"]).join("config")));
        let owned = paths
            .into_iter()
            .filter(|path| path.starts_with(&scratch.0))
            .collect::<BTreeSet<_>>();
        assert!(!owned.is_empty());
        absent_configs(&owned, &admitted).unwrap();
        for path in &owned {
            if path == &admitted {
                continue;
            }
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"unreviewed config").unwrap();
            assert!(absent_configs(&owned, &admitted).is_err());
            assert_eq!(fs::read(path).unwrap(), b"unreviewed config");
            fs::remove_file(path).unwrap();
            symlink(&admitted, path).unwrap();
            assert!(absent_configs(&owned, &admitted).is_err());
            fs::remove_file(path).unwrap();
        }
    }

    fn builder_fixture(p: MuslProfile) -> (Value, Value) {
        let root = Path::new("/synthetic/musl");
        let sdk = sdk_fixture(p);
        let env = env_fixture(p, root);
        let mut files = serde_json::Map::new();
        for name in [MUSL_WORKFLOW, DOCKERFILE, SCRIPT, FETCHER, PINS] {
            files.insert(name.into(),json!({"sha256":if name==PINS {PINS_SHA.to_owned()} else {digest(name.as_bytes())}}));
        }
        let cc = format!("#!/bin/sh\nexec /usr/bin/gcc {} \"$@\"\n", p.c_cpu);
        let cxx = format!("#!/bin/sh\nexec /usr/bin/g++ {} \"$@\"\n", p.c_cpu);
        let compiler = json!({"rustc_text":format!("release: 1.99.0\nhost: {}",p.target),"cargo_text":"cargo1.99.0 synthetic","gcc_text":"synthetic GCC14.2.0","kernel_text":"synthetic kernel metadata",
            "alpine":"3.21.3","musl_text":"musl-1.2.5-r11","perl_text":"synthetic Perl metadata","tar_text":sdk.tar,"xz_text":sdk.xz,"wrappers":{"CC":{"contents":cc},"CXX":{"contents":cxx}}});
        let state = json!({"source_commit":"a".repeat(40),"cargo_lock_sha256":"c".repeat(64),"source_date_epoch":"1700000000","environment":env,"files":files,"compiler":compiler,
            "sdk":{"closed_apks":sdk.apks,"installed_packages":sdk.installed.split('\n').collect::<Vec<_>>(),"rust_archive_sha256":sdk.rust_archive,"package_bytes_sha256":sdk.actual}});
        let mut builder = json!({"schema_version":3,"kind":"musl_ci_builder_inputs","source_dirty":false,"target":p.target,"node_image":p.image,"pins_sha256":PINS_SHA,"node":"v22.14.0",
            "package_index_sha256":null,"package_selection":"closed_apk_sha256_manifest_v1","qualification":QUALIFICATION,"cflags":"-O2"});
        for name in ["source_commit", "cargo_lock_sha256", "source_date_epoch"] {
            builder[name] = state[name].clone();
        }
        for (key, path) in [
            ("workflow_sha256", MUSL_WORKFLOW),
            ("sdk_dockerfile_sha256", DOCKERFILE),
            ("sdk_script_sha256", SCRIPT),
            ("apk_fetcher_sha256", FETCHER),
        ] {
            builder[key] = state["files"][path]["sha256"].clone();
        }
        for (key, field) in [
            ("rustc", "rustc_text"),
            ("cargo", "cargo_text"),
            ("gcc", "gcc_text"),
            ("kernel", "kernel_text"),
            ("alpine", "alpine"),
            ("musl", "musl_text"),
            ("perl", "perl_text"),
            ("tar", "tar_text"),
            ("xz", "xz_text"),
        ] {
            builder[key] = state["compiler"][field].clone();
        }
        builder["workflow_run"] = state["environment"]["AUTOROUTER_WORKFLOW_RUN"].clone();
        builder["rustflags"] = state["environment"]["RUSTFLAGS"].clone();
        builder["cc_wrapper"] = state["compiler"]["wrappers"]["CC"]["contents"].clone();
        builder["cxx_wrapper"] = state["compiler"]["wrappers"]["CXX"]["contents"].clone();
        for (key, field) in [
            ("apk_package_set", "closed_apks"),
            ("installed_packages", "installed_packages"),
            ("rust_archive_sha256", "rust_archive_sha256"),
            ("package_bytes_sha256", "package_bytes_sha256"),
        ] {
            builder[key] = state["sdk"][field].clone();
        }
        (builder, state)
    }

    #[test]
    fn schema3_builder_fields_cannot_substitute_source_sdk_tools_or_run_context() {
        for p in profiles() {
            let (builder, state) = builder_fixture(p);
            validate_builder(&builder, &state, p).unwrap();
            for key in builder.as_object().unwrap().keys() {
                let mut bad = builder.clone();
                bad[key] = json!("unreviewed substitution");
                assert!(validate_builder(&bad, &state, p).is_err(), "{key}");
                let mut bad = builder.clone();
                bad.as_object_mut().unwrap().remove(key);
                assert!(validate_builder(&bad, &state, p).is_err(), "missing {key}");
            }
            let mut old = builder.clone();
            old["schema_version"] = json!(2);
            assert!(validate_builder(&old, &state, p).is_err());
            let mut extra = builder.clone();
            extra["unreviewed"] = json!(true);
            assert!(validate_builder(&extra, &state, p).is_err());
            for path in [MUSL_WORKFLOW, DOCKERFILE, SCRIPT, FETCHER, PINS] {
                let mut bad = state.clone();
                bad["files"][path]["sha256"] = json!("0".repeat(64));
                assert!(validate_builder(&builder, &bad, p).is_err(), "{path}");
            }
            for field in [
                "closed_apks",
                "installed_packages",
                "rust_archive_sha256",
                "package_bytes_sha256",
            ] {
                let mut bad = state.clone();
                bad["sdk"][field] = Value::Null;
                assert!(validate_builder(&builder, &bad, p).is_err(), "{field}");
            }
        }
    }

    fn put(bytes: &mut [u8], offset: usize, width: usize, value: u64) {
        for index in 0..width {
            bytes[offset + index] = (value >> (index * 8)) as u8;
        }
    }
    // Data-only mapped static-PIE header and optional dynamic metadata. No
    // fixture is executable or used as runtime/platform qualification evidence.
    fn elf_fixture(p: MuslProfile, kind: &str) -> Vec<u8> {
        let mut bytes = vec![0; 0x800];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[6] = 1;
        put(&mut bytes, 16, 2, 3);
        put(
            &mut bytes,
            18,
            2,
            if p.machine == "x86_64" { 62 } else { 183 },
        );
        put(&mut bytes, 32, 8, 64);
        put(&mut bytes, 54, 2, 56);
        let mut strings = vec![0];
        let mut string_offset = |value: &str| {
            let at = strings.len() as u64;
            strings.extend_from_slice(value.as_bytes());
            strings.push(0);
            at
        };
        let libc = string_offset("libc.so.6");
        let search = string_offset("/synthetic/builder/lib");
        let version = string_offset("GLIBC_2.28");
        bytes[0x480..0x480 + strings.len()].copy_from_slice(&strings);
        let mut tags = vec![(5, 0x8280), (10, strings.len() as u64)];
        match kind {
            "needed" => tags.push((1, libc)),
            "rpath" => tags.push((15, search)),
            "runpath" => tags.push((29, search)),
            "version" => {
                tags.push((0x6ffffffe, 0x8400));
                tags.push((0x6fffffff, 1));
            }
            _ => {}
        }
        tags.push((0, 0));
        let mut segments = vec![(1, 0x200, 0x600), (2, 0x280, tags.len() as u64 * 16)];
        if kind == "interpreter" {
            segments.push((3, 0x210, 12));
            bytes[0x210..0x21c].copy_from_slice(b"/ld-test.so\0");
        }
        put(&mut bytes, 56, 2, segments.len() as u64);
        for (index, (kind, start, size)) in segments.into_iter().enumerate() {
            let at = 64 + index * 56;
            put(&mut bytes, at, 4, kind);
            put(&mut bytes, at + 8, 8, start);
            put(&mut bytes, at + 16, 8, 0x8000 + start - 0x200);
            put(&mut bytes, at + 32, 8, size);
            put(&mut bytes, at + 40, 8, size);
        }
        for (index, (tag, value)) in tags.into_iter().enumerate() {
            put(&mut bytes, 0x280 + index * 16, 8, tag);
            put(&mut bytes, 0x288 + index * 16, 8, value);
        }
        if kind == "version" {
            put(&mut bytes, 0x600, 2, 1);
            put(&mut bytes, 0x602, 2, 1);
            put(&mut bytes, 0x604, 4, libc);
            put(&mut bytes, 0x608, 4, 16);
            put(&mut bytes, 0x618, 4, version);
        }
        bytes
    }

    #[test]
    fn both_cpu_static_pie_headers_pass_and_loader_dependencies_cannot_be_hidden() {
        for p in profiles() {
            let clean = elf_fixture(p, "static");
            let raw = crate::provenance_binary::inspect(p.target, &clean).unwrap();
            assert_eq!(raw["interpreter"], Value::Null);
            assert_eq!(raw["dynamic_libraries"], json!([]));
            assert_eq!(raw["minimum_glibc"], Value::Null);
            inspect(p, &clean).unwrap();
            let gnu = if p.machine == "x86_64" {
                "x86_64-unknown-linux-gnu"
            } else {
                "aarch64-unknown-linux-gnu"
            };
            for kind in ["interpreter", "needed"] {
                let bytes = elf_fixture(p, kind);
                let decoded = crate::provenance_binary::inspect(gnu, &bytes).unwrap();
                if kind == "interpreter" {
                    assert_eq!(decoded["interpreter"], "/ld-test.so");
                } else {
                    assert_eq!(decoded["dynamic_libraries"], json!(["libc.so.6"]));
                }
                assert!(inspect(p, &bytes).unwrap_err().contains("must be static"));
            }
            let mut wrong = clean.clone();
            put(
                &mut wrong,
                18,
                2,
                if p.machine == "x86_64" { 183 } else { 62 },
            );
            assert!(inspect(p, &wrong).is_err());
            let mut bad = clean.clone();
            put(&mut bad, 64 + 56 + 32, 8, u64::MAX);
            assert!(inspect(p, &bad).is_err());
            let mut bad = clean.clone();
            bad[5] = 2;
            assert!(inspect(p, &bad).is_err());
        }
    }

    #[test]
    fn additional_static_musl_policy_rejects_decodable_search_and_version_metadata() {
        for p in profiles() {
            for kind in ["rpath", "runpath", "version"] {
                let bytes = elf_fixture(p, kind);
                let raw = crate::provenance_binary::inspect(p.target, &bytes).unwrap();
                if kind == "version" {
                    assert_eq!(raw["minimum_glibc"], "2.28");
                    assert_ne!(raw["version_requirements"], json!([]));
                } else {
                    assert_eq!(raw[kind], "/synthetic/builder/lib");
                }
                assert!(
                    inspect(p, &bytes)
                        .unwrap_err()
                        .contains("without loader/library/version paths")
                );
            }
        }
    }

    #[test]
    fn exact_wrapper_baselines_and_actual_installed_inventory_are_independent_gates() {
        for p in profiles() {
            for (role, name) in [("CC", "gcc"), ("CXX", "g++")] {
                let tool = Tool {
                    invocation: Path::new("/usr/bin").join(name),
                    canonical: Path::new("/synthetic/sdk").join(name),
                };
                let good = format!("#!/bin/sh\nexec /usr/bin/{name} {} \"$@\"\n", p.c_cpu);
                assert_eq!(
                    admitted_wrapper(p, role, &tool, good.as_bytes()).unwrap(),
                    good
                );
                for bad in [
                    good.replace(p.c_cpu, "-march=native"),
                    good.replace("$@", "$*"),
                    good.replace("/usr/bin/", "/unreviewed/"),
                ] {
                    assert!(admitted_wrapper(p, role, &tool, bad.as_bytes()).is_err());
                }
                let wrong = Tool {
                    invocation: Path::new("/unreviewed").join(name),
                    canonical: tool.canonical.clone(),
                };
                assert!(admitted_wrapper(p, role, &wrong, good.as_bytes()).is_err());
            }
            let sdk = sdk_fixture(p);
            let reversed = sdk.installed.lines().rev().collect::<Vec<_>>().join("\n");
            installed_packages_match(&sdk, &reversed).unwrap();
            assert!(installed_packages_match(&sdk, "").is_err());
            assert!(
                installed_packages_match(
                    &sdk,
                    &format!("{}unreviewed-package-1-r0\n", sdk.installed)
                )
                .is_err()
            );
            assert!(
                installed_packages_match(
                    &sdk,
                    &sdk.installed.lines().skip(1).collect::<Vec<_>>().join("\n")
                )
                .is_err()
            );
        }
    }

    #[test]
    fn streamed_sdk_tool_witnesses_reject_oversize_before_read_and_preserve_identity() {
        let scratch = Scratch::new("musl-sdk-tool-bound").unwrap();
        let path = scratch.file("tool", b"synthetic SDK bytes").unwrap();
        let first = sdk_tool_descriptor(&path).unwrap();
        assert_eq!(first["sha256"], digest(b"synthetic SDK bytes"));
        let link = scratch.0.join("alias");
        symlink(&path, &link).unwrap();
        assert!(sdk_tool_descriptor(&link).is_err());
        let hard = scratch.0.join("hard");
        fs::hard_link(&path, &hard).unwrap();
        assert_eq!(sdk_tool_descriptor(&hard).unwrap()["identity"]["links"], 2);
        fs::write(&path, b"changed SDK bytes").unwrap();
        assert_ne!(
            sdk_tool_descriptor(&path).unwrap()["sha256"],
            first["sha256"]
        );
        let oversized = scratch.file("sparse-oversized", b"").unwrap();
        OpenOptions::new()
            .write(true)
            .open(&oversized)
            .unwrap()
            .set_len(SDK_TOOL_MAX + 1)
            .unwrap();
        assert!(
            sdk_tool_descriptor(&oversized)
                .unwrap_err()
                .contains("oversized")
        );
    }

    #[test]
    fn metadata_environment_admits_exact_build_homes_and_excludes_other_values() {
        let source = BTreeMap::from([
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HOME".into(), "/synthetic/build-home".into()),
            ("CARGO_HOME".into(), "/synthetic/cargo-cache".into()),
            ("RUSTFLAGS".into(), "synthetic-override".into()),
            ("SYNTHETIC_PRIVATE_VALUE".into(), "synthetic-private".into()),
        ]);
        assert_eq!(
            metadata_environment(&source).unwrap(),
            BTreeMap::from([
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("HOME".into(), "/synthetic/build-home".into()),
                ("CARGO_HOME".into(), "/synthetic/cargo-cache".into()),
                ("LC_ALL".into(), "C".into()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
                ("GIT_NO_REPLACE_OBJECTS".into(), "1".into()),
            ])
        );
        for name in ["PATH", "HOME", "CARGO_HOME"] {
            let mut missing = source.clone();
            missing.remove(name);
            assert!(metadata_environment(&missing).is_err());
        }
    }

    #[test]
    fn owned_metadata_child_receives_build_homes_without_inheriting_overrides() {
        let scratch = Scratch::new("musl-metadata-homes").unwrap();
        let home = scratch.0.join("home");
        let cargo_home = scratch.0.join("cargo-cache");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&cargo_home).unwrap();
        let script = scratch
            .file(
                "metadata-fixture",
                br##"#!/bin/sh
set -eu
[ "${HOME-unset}" = "$1" ] || exit 41
[ "${CARGO_HOME-unset}" = "$2" ] || exit 42
[ "$LC_ALL" = C ] || exit 43
[ "$GIT_CONFIG_GLOBAL" = /dev/null ] || exit 44
[ "${RUSTFLAGS+present}" != present ] || exit 45
[ "${SYNTHETIC_PRIVATE_VALUE+present}" != present ] || exit 46
printf 'home metadata ok\n'
"##,
            )
            .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let home = home.to_str().unwrap();
        let cargo_home = cargo_home.to_str().unwrap();
        let env = BTreeMap::from([
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HOME".into(), home.into()),
            ("CARGO_HOME".into(), cargo_home.into()),
            ("RUSTFLAGS".into(), "synthetic-override".into()),
            ("SYNTHETIC_PRIVATE_VALUE".into(), "synthetic-private".into()),
        ]);
        let args = [home, cargo_home];
        assert!(capture(&env, &scratch.0, &script, &args, 64).is_err());
        let tool = Tool {
            invocation: script.clone(),
            canonical: script,
        };
        assert_eq!(
            metadata(&env, &scratch.0, &tool, &args, 64).unwrap(),
            "home metadata ok"
        );
        assert!(
            metadata(&env, &scratch.0, &tool, &args, 4)
                .unwrap_err()
                .contains("exceeds bound")
        );
    }

    #[test]
    fn derived_sdk_image_identity_is_bounded_and_distinct_from_base_manifest() {
        let scratch = Scratch::new("musl-derived-sdk-id").unwrap();
        let out = output_base(&scratch.0);
        fs::create_dir_all(&out).unwrap();
        let path = out.join("build-image-id.txt");
        let id = format!("sha256:{}", "d".repeat(64));
        fs::write(&path, format!("{id}\n")).unwrap();
        assert_eq!(build_image_identity(&scratch.0).unwrap()["id"], id);
        for bad in [
            id.clone(),
            format!("{id}\nextra\n"),
            format!("sha256:{}\n", "D".repeat(64)),
            format!("{}\n", "x".repeat(4096)),
        ] {
            fs::write(&path, bad).unwrap();
            assert!(build_image_identity(&scratch.0).is_err());
        }
    }
}
