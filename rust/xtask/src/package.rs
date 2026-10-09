//! Experimental native/npm artifact assembly. Shipping JS manifests stay
//! untouched. No publication, network downloads, or lifecycle hooks occur.
#[path = "package_archive.rs"]
pub(crate) mod archive;
#[path = "package_binary.rs"]
pub(crate) mod binary;
use crate::process::{capture, capture_result, read_bounded};
use serde_json::{Value, json};
use sha2::{Digest, Sha256, Sha512};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
const DISPATCHER: &str = include_str!("../../distribution/autorouter.sh");
const PLATFORMS: &str = include_str!("../../distribution/platforms.json");
const VERSION: &str = env!("CARGO_PKG_VERSION");
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn put(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| "Cannot create native package directory")?;
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(mode)
        .open(path)
        .map_err(|_| "Refusing to overwrite an existing package file")?;
    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|_| "Cannot set package file permissions")?;
    file.write_all(bytes)
        .map_err(|_| "Cannot write package file".into())
}
fn fresh(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Err("Package destination already exists; choose a new empty destination".into());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| "Cannot create package parent directory")?;
    }
    fs::create_dir(path).map_err(|_| "Cannot create package destination".into())
}
fn known_target(target: &str) -> bool {
    serde_json::from_str::<Value>(PLATFORMS).unwrap()["targets"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["target"] == target)
}
fn allowed(path: &str) -> bool {
    matches!(
        path,
        "package.json"
            | "README.md"
            | "LICENSE"
            | "THIRD-PARTY-LICENSES.txt"
            | "build-manifest.json"
            | "platforms.json"
            | "bin/autorouter"
    ) || path
        .strip_prefix("native/")
        .and_then(|v| v.strip_suffix("/claude-autorouter"))
        .is_some_and(known_target)
}
fn npm_command(environment: &Path) -> Result<Command, String> {
    fs::create_dir_all(environment).map_err(|_| "Cannot create isolated npm environment")?;
    for name in ["user.npmrc", "global.npmrc"] {
        let path = environment.join(name);
        if !path.exists() {
            put(&path, b"", 0o600)?;
        }
    }
    let mut command = Command::new("npm");
    command.env_clear();
    for key in [
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
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .env("npm_config_cache", environment.join("cache"))
        .env("npm_config_userconfig", environment.join("user.npmrc"))
        .env("npm_config_globalconfig", environment.join("global.npmrc"))
        .env("npm_config_update_notifier", "false")
        .env("npm_config_audit", "false")
        .env("npm_config_fund", "false")
        .env("npm_config_ignore_scripts", "true");
    Ok(command)
}
struct LicenseMaterial {
    bytes: Vec<u8>,
    components: Vec<Value>,
    missing: Vec<String>,
}
fn licenses(root: &Path) -> Result<LicenseMaterial, String> {
    let bytes = capture(
        Command::new("cargo")
            .args(["metadata", "--format-version", "1", "--locked", "--offline"])
            .current_dir(root.join("rust")),
        b"",
        Duration::from_secs(60),
    )
    .map_err(|_| {
        "Cannot read locked offline Cargo dependency metadata for license inventory; run `cargo fetch --locked` from the rust directory to cache dependencies for all targets, then retry"
            .to_owned()
    })?;
    let metadata: Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid Cargo metadata")?;
    let mut output = Vec::new();
    let mut components = Vec::new();
    let mut missing = Vec::new();
    for package in metadata["packages"]
        .as_array()
        .ok_or("Cargo metadata lacks packages")?
    {
        let vendored = package["source"].is_null()
            && package["manifest_path"]
                .as_str()
                .is_some_and(|path| Path::new(path).starts_with(root.join("rust/vendor")));
        if package["source"].is_null() && !vendored {
            continue;
        }
        let name = package["name"].as_str().ok_or("Invalid dependency name")?;
        let version = package["version"]
            .as_str()
            .ok_or("Invalid dependency version")?;
        let manifest = Path::new(
            package["manifest_path"]
                .as_str()
                .ok_or("Invalid dependency manifest")?,
        );
        let directory = manifest.parent().ok_or("Invalid dependency directory")?;
        let mut paths = Vec::new();
        if let Some(path) = package["license_file"].as_str() {
            paths.push(directory.join(path));
        }
        for entry in
            fs::read_dir(directory).map_err(|_| "Cannot read dependency license directory")?
        {
            let entry = entry.map_err(|_| "Cannot inspect dependency license")?;
            let name = entry.file_name().to_string_lossy().to_ascii_uppercase();
            if (name.starts_with("LICENSE")
                || name.starts_with("COPYING")
                || name.starts_with("NOTICE")
                || name == "AUTHORS"
                || name.starts_with("COPYRIGHT"))
                && entry
                    .file_type()
                    .map_err(|_| "Cannot inspect dependency license")?
                    .is_file()
            {
                paths.push(entry.path());
            }
        }
        if name == "openssl-src" {
            if version != "300.6.1+3.6.3" {
                return Err(
                    "Review bundled OpenSSL license paths and hashes for the new source version"
                        .into(),
                );
            }
            for (relative, expected, component, component_version, license, kind) in [
                (
                    "openssl/LICENSE.txt",
                    "7d5450cb2d142651b8afa315b5f238efc805dad827d91ba367d8516bc9d49e7a",
                    "openssl-native",
                    "3.6.3",
                    "Apache-2.0",
                    "bundled native source",
                ),
                (
                    "openssl/external/perl/Text-Template-1.56/LICENSE",
                    "9837f05336ef3cbacb6a96e1672a0426d81ad01191f214b8d48e22ca62338181",
                    "openssl-text-template",
                    "1.56",
                    "Perl 5 terms: Artistic License or GPL version 1 or later; see bundled text",
                    "bundled build tool; not an application runtime dependency",
                ),
            ] {
                let path = directory.join(relative);
                let content = read_bounded(&path, 1024 * 1024)?;
                if sha(&content) != expected {
                    return Err("Bundled OpenSSL license checksum mismatch".into());
                }
                paths.push(path);
                components.push(json!({
                    "name":component,"version":component_version,"license":license,
                    "source":package["source"],"bundled_by":format!("{name}@{version}"),
                    "kind":kind,"license_path":relative,"license_sha256":expected
                }));
            }
        }
        paths.sort();
        paths.dedup();
        if paths.is_empty() && name == "alloc-stdlib" && version == "0.3.0" {
            let path = root.join("rust/distribution/licenses/alloc-stdlib-0.3.0-LICENSE");
            let bytes = read_bounded(&path, 1024 * 1024)?;
            if sha(&bytes) != "c0c56f26d9c051cac4d200c34c84e7ae9aaa853e01a982a1df08b09931e518ae" {
                return Err("Pinned alloc-stdlib license checksum mismatch".into());
            }
            paths.push(path);
        }
        if paths.is_empty() {
            missing.push(format!("{name}@{version}"));
        }
        for path in paths {
            let content = read_bounded(&path, 1024 * 1024)?;
            output.extend(
                format!(
                    "\n===== {name}@{version}: {} =====\n",
                    path.strip_prefix(directory)
                        .unwrap_or_else(|_| Path::new(path.file_name().unwrap()))
                        .to_string_lossy()
                )
                .as_bytes(),
            );
            output.extend(content);
            if output.len() > 4 * 1024 * 1024 {
                return Err("Dependency license bundle exceeds limit".into());
            }
        }
        components.push(json!({"name":name,"version":version,"license":package["license"],"source":package["source"],"vendored":vendored}));
    }
    let certificate_provenance: Value = serde_json::from_slice(&read_bounded(
        &root.join("rust/vendor/node-ca/provenance.json"),
        1024 * 1024,
    )?)
    .map_err(|_| "Invalid certificate provenance")?;
    let certificate_license = read_bounded(&root.join("rust/vendor/node-ca/LICENSE"), 1024 * 1024)?;
    if certificate_provenance["license_sha256"] != sha(&certificate_license) {
        return Err("Certificate license checksum mismatch".into());
    }
    output.extend(b"\n===== Bundled Node.js root certificates: Node.js LICENSE =====\n");
    output.extend(certificate_license);
    output.extend(b"\nCertificate extraction provenance: ");
    output.extend(
        serde_json::to_vec(&certificate_provenance)
            .map_err(|_| "Invalid certificate provenance")?,
    );
    if output.len() > 4 * 1024 * 1024 {
        return Err("Dependency license bundle exceeds limit".into());
    }
    components.push(json!({"name":"node-root-certificates","version":certificate_provenance["node_version"],"license":"See bundled Node.js LICENSE; review pending","source":certificate_provenance["source"],"pem_sha256":certificate_provenance["pem_sha256"],"license_sha256":certificate_provenance["license_sha256"]}));
    Ok(LicenseMaterial {
        bytes: output,
        components,
        missing,
    })
}
fn source_state(root: &Path) -> Result<Value, String> {
    let revision = capture(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root),
        b"",
        Duration::from_secs(10),
    )?;
    let dirty = capture(
        Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=normal"])
            .current_dir(root),
        b"",
        Duration::from_secs(10),
    )?;
    Ok(
        json!({"commit":String::from_utf8_lossy(&revision).trim(),"dirty":!dirty.is_empty(),"cargo_lock_sha256":sha(&read_bounded(&root.join("rust/Cargo.lock"),4*1024*1024)?),"provenance":"prebuilt local artifacts; source-to-binary reproducibility unverified"}),
    )
}
fn assemble(
    root: &Path,
    destination: &Path,
    artifacts: &[(String, PathBuf)],
    smoke_requested: bool,
) -> Result<Value, String> {
    if artifacts.is_empty() {
        return Err("Supply at least one --binary TARGET=PATH".into());
    }
    let mut targets = HashSet::new();
    let mut inspected = Vec::new();
    let mut inputs = Vec::new();
    for (target, path) in artifacts {
        if !known_target(target) || !targets.insert(target) {
            return Err("Unsupported or duplicate native target".into());
        }
        if !fs::symlink_metadata(path)
            .map_err(|_| "Cannot inspect native artifact")?
            .is_file()
        {
            return Err("Native artifacts must be regular files, not symbolic links".into());
        }
        let bytes = read_bounded(path, archive::MAX_ARCHIVE as u64)?;
        let inspection = binary::inspect(target, &bytes)?;
        let relative = format!("native/{target}/claude-autorouter");
        inspected.push(json!({"target":target,"path":relative,"sha256":sha(&bytes),"bytes":bytes.len(),"inspection":inspection}));
        inputs.push((relative, bytes));
    }
    let source = source_state(root)?;
    let LicenseMaterial {
        bytes: license_bytes,
        components,
        missing,
    } = licenses(root)?;
    fresh(destination)?;
    let stage = destination.join("stage");
    fs::create_dir(&stage).map_err(|_| "Cannot create native package staging directory")?;
    let manifest = json!({"name":"claude-autorouter","version":VERSION,"private":true,"description":"AutoRouter native Rust feasibility artifact; not a release","license":"Apache-2.0","bin":{"claude-autorouter":"bin/autorouter"},"os":["darwin","linux"],"files":["bin/autorouter","native/","README.md","LICENSE","THIRD-PARTY-LICENSES.txt","build-manifest.json","platforms.json"]});
    let readme = format!(
        "# AutoRouter native feasibility artifact\n\nVersion {VERSION}. This private candidate is not an approved release. Only the targets listed in build-manifest.json are bundled. It requires no Node runtime, downloads, or install hooks. Full platform, lifecycle, performance and release gates remain pending.\n\nUse claude-autorouter --help. The shipping JavaScript package is unchanged.\n"
    );
    let mut files = BTreeMap::<String, archive::Entry>::new();
    for (path, bytes, mode) in [
        (
            "package.json",
            serde_json::to_vec_pretty(&manifest).unwrap(),
            0o644,
        ),
        ("README.md", readme.into_bytes(), 0o644),
        (
            "LICENSE",
            read_bounded(&root.join("LICENSE"), 1024 * 1024)?,
            0o644,
        ),
        ("THIRD-PARTY-LICENSES.txt", license_bytes, 0o644),
        ("platforms.json", PLATFORMS.as_bytes().to_vec(), 0o644),
        ("bin/autorouter", DISPATCHER.as_bytes().to_vec(), 0o755),
    ] {
        files.insert(path.into(), archive::Entry { bytes, mode });
    }
    for (path, bytes) in inputs {
        files.insert(path, archive::Entry { bytes, mode: 0o755 });
    }
    let declarations=files.iter().map(|(path,entry)|json!({"path":path,"sha256":sha(&entry.bytes),"bytes":entry.bytes.len(),"mode":entry.mode})).collect::<Vec<_>>();
    let build = json!({"schema_version":1,"kind":"native_npm_feasibility","release_approved":false,"version":VERSION,"source":source,"artifacts":inspected,"files":declarations,"dependencies":components,"license_files_missing":missing,"license_review":"pending","platform_qualification":"pending","full_matrix_size_gate":"pending until every qualified target is present"});
    files.insert(
        "build-manifest.json".into(),
        archive::Entry {
            bytes: serde_json::to_vec_pretty(&build).unwrap(),
            mode: 0o644,
        },
    );
    let minimum_expanded: usize = files
        .values()
        .map(|entry| 512 + entry.bytes.len().div_ceil(512) * 512)
        .sum::<usize>()
        + 1024;
    if minimum_expanded > archive::MAX_ARCHIVE {
        return Err(format!(
            "Native bundle needs at least {minimum_expanded} expanded bytes, exceeding the existing 32 MiB cap; no cap was changed"
        ));
    }
    for (path, entry) in &files {
        put(&stage.join(path), &entry.bytes, entry.mode)?;
    }
    let mut npm = npm_command(&destination.join("npm"))?;
    npm.args([
        "pack",
        "--json",
        "--ignore-scripts",
        "--offline",
        "--pack-destination",
    ])
    .arg(destination)
    .current_dir(&stage);
    let output = capture(&mut npm, b"", Duration::from_secs(120))?;
    let reports: Value = serde_json::from_slice(&output).map_err(|_| "Invalid npm pack report")?;
    let reports = reports
        .as_array()
        .filter(|r| r.len() == 1)
        .ok_or("Unexpected npm pack report count")?;
    let filename = reports[0]["filename"]
        .as_str()
        .ok_or("Missing npm archive filename")?;
    if Path::new(filename).file_name().and_then(|v| v.to_str()) != Some(filename)
        || !filename.ends_with(".tgz")
    {
        return Err("Unsafe npm archive filename".into());
    }
    let archive_path = destination.join(filename);
    let bytes = read_bounded(&archive_path, archive::MAX_ARCHIVE as u64)?;
    let (actual, _) = archive::decode(&bytes)?;
    if actual.len() != files.len()
        || files.iter().any(|(path, expected)| {
            actual.get(path).is_none_or(|actual| {
                actual.bytes != expected.bytes || actual.mode & 0o777 != expected.mode
            })
        })
    {
        return Err("Packed bytes or modes differ from the staged native package".into());
    }
    let checksum = sha(&bytes);
    put(
        &archive_path.with_extension("tgz.sha256"),
        format!("{checksum}  {filename}\n").as_bytes(),
        0o644,
    )?;
    let mut report = verify(&archive_path, Some(&checksum))?;
    report["archive"] = json!(archive_path);
    if smoke_requested {
        report["smoke"] = smoke(&archive_path, root)?;
    }
    put(
        &destination.join("package-report.json"),
        serde_json::to_vec_pretty(&report).unwrap().as_slice(),
        0o644,
    )?;
    Ok(report)
}
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | chunk.get(2).copied().unwrap_or(0) as u32;
        text.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        text.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        text.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        text.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    text
}
pub(crate) fn verify(path: &Path, expected: Option<&str>) -> Result<Value, String> {
    let bytes = read_bounded(path, archive::MAX_ARCHIVE as u64)?;
    let checksum = sha(&bytes);
    if expected.is_some_and(|value| value != checksum) {
        return Err("Archive SHA256 differs from expected immutable artifact".into());
    }
    let (files, expanded) = archive::decode(&bytes)?;
    if files.keys().any(|p| !allowed(p)) {
        return Err("Native archive contains an unexpected file".into());
    }
    let package: Value = serde_json::from_slice(
        &files
            .get("package.json")
            .ok_or("Missing package manifest")?
            .bytes,
    )
    .map_err(|_| "Invalid package manifest")?;
    if package["name"] != "claude-autorouter"
        || package["private"] != true
        || package["bin"]["claude-autorouter"] != "bin/autorouter"
        || ["dependencies", "optionalDependencies", "scripts"]
            .iter()
            .any(|key| {
                package
                    .get(*key)
                    .is_some_and(|v| !v.as_object().is_some_and(|v| v.is_empty()))
            })
    {
        return Err("Candidate manifest must be private, dependency-free, and hook-free".into());
    }
    let build: Value = serde_json::from_slice(
        &files
            .get("build-manifest.json")
            .ok_or("Missing build manifest")?
            .bytes,
    )
    .map_err(|_| "Invalid build manifest")?;
    if build["schema_version"] != 1
        || build["kind"] != "native_npm_feasibility"
        || build["release_approved"] != false
        || build["version"] != package["version"]
    {
        return Err("Invalid native build manifest identity".into());
    }
    let mut declared = HashSet::new();
    for entry in build["files"]
        .as_array()
        .ok_or("Missing build file declarations")?
    {
        let path = entry["path"].as_str().ok_or("Invalid build file path")?;
        if !allowed(path) || !declared.insert(path) {
            return Err("Invalid or duplicate build file declaration".into());
        }
        let actual = files
            .get(path)
            .ok_or("Declared file missing from archive")?;
        if entry["sha256"] != sha(&actual.bytes)
            || entry["bytes"].as_u64() != Some(actual.bytes.len() as u64)
            || entry["mode"].as_u64() != Some((actual.mode & 0o777) as u64)
        {
            return Err("Native archive file failed checksum, size or mode verification".into());
        }
    }
    if declared.len() + 1 != files.len() || declared.contains("build-manifest.json") {
        return Err("Archive contains undeclared files".into());
    }
    for required in [
        "package.json",
        "README.md",
        "LICENSE",
        "THIRD-PARTY-LICENSES.txt",
        "platforms.json",
        "bin/autorouter",
    ] {
        if !declared.contains(required) {
            return Err("Native archive is missing a required file".into());
        }
    }
    let dispatcher = &files["bin/autorouter"];
    if dispatcher.bytes != DISPATCHER.as_bytes() || dispatcher.mode & 0o777 != 0o755 {
        return Err("Native dispatcher or executable mode changed".into());
    }
    let artifacts = build["artifacts"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or("Native package has no artifacts")?;
    let mut targets = HashSet::new();
    for artifact in artifacts {
        let target = artifact["target"].as_str().ok_or("Invalid native target")?;
        if !known_target(target) || !targets.insert(target) {
            return Err("Unsupported or duplicate artifact target".into());
        }
        let path = format!("native/{target}/claude-autorouter");
        if artifact["path"] != path {
            return Err("Native artifact path and target differ".into());
        }
        let entry = files.get(&path).ok_or("Missing native artifact")?;
        if entry.mode & 0o777 != 0o755
            || artifact["sha256"] != sha(&entry.bytes)
            || artifact["bytes"].as_u64() != Some(entry.bytes.len() as u64)
        {
            return Err("Native executable integrity or permissions changed".into());
        }
        if artifact["inspection"] != binary::inspect(target, &entry.bytes)? {
            return Err("Native artifact inspection differs from its actual header".into());
        }
    }
    if files.keys().filter(|p| p.starts_with("native/")).count() != targets.len() {
        return Err("Native archive has undeclared executable targets".into());
    }
    Ok(
        json!({"schema_version":1,"kind":"native_npm_feasibility","verified":true,"release_approved":false,"sha256":checksum,"integrity":format!("sha512-{}",base64(&Sha512::digest(&bytes))),"compressed_bytes":bytes.len(),"expanded_tar_bytes":expanded,"files":files.len(),"artifacts":artifacts,"archive_caps_bytes":archive::MAX_ARCHIVE,"platform_qualification":"pending","full_supported_matrix":"pending","source_to_binary_provenance":"unverified","license_files_missing":build["license_files_missing"],"license_review":"pending"}),
    )
}
struct Temporary(PathBuf);
impl Temporary {
    fn new() -> Result<Self, String> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "Invalid clock")?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "autorouter-native-package-{}-{nonce}",
            std::process::id()
        ));
        fresh(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn utility_path(directory: &Path) -> Result<(), String> {
    fs::create_dir(directory).map_err(|_| "Cannot create utility-only PATH")?;
    for utility in ["dirname", "readlink", "uname"] {
        let source = [Path::new("/usr/bin"), Path::new("/bin")]
            .into_iter()
            .map(|dir| dir.join(utility))
            .find(|path| path.is_file())
            .ok_or("Missing standard POSIX dispatcher utility")?;
        symlink(source, directory.join(utility)).map_err(|_| "Cannot stage dispatcher utility")?;
    }
    Ok(())
}
fn smoke(archive: &Path, root: &Path) -> Result<Value, String> {
    let verified = verify(archive, None)?;
    let archive = archive
        .canonicalize()
        .map_err(|_| "Cannot resolve archive")?;
    let temporary = Temporary::new()?;
    let prefix = temporary.0.join("install path ' $ with spaces");
    let npm_env = temporary.0.join("npm");
    let mut install = npm_command(&npm_env)?;
    install
        .args([
            "install",
            "--global",
            "--offline",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            "--prefix",
        ])
        .arg(&prefix)
        .arg(&archive)
        .current_dir(&temporary.0);
    capture(&mut install, b"", Duration::from_secs(120))?;
    let launcher = prefix.join("bin/claude-autorouter");
    let package_root = prefix.join("lib/node_modules/claude-autorouter");
    let host_arch = std::env::consts::ARCH;
    let host_os = std::env::consts::OS;
    let base_target = match (host_os, host_arch) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "arm") => "armv7-unknown-linux-gnueabihf",
        ("linux", "powerpc64") if cfg!(target_endian = "little") => "powerpc64le-unknown-linux-gnu",
        ("linux", "s390x") => "s390x-unknown-linux-gnu",
        ("linux", "loongarch64") => "loongarch64-unknown-linux-gnu",
        ("linux", "riscv64") => "riscv64gc-unknown-linux-gnu",
        _ => return Err("Installed smoke has no qualified host architecture mapping".into()),
    };
    let artifacts = verified["artifacts"]
        .as_array()
        .ok_or("Missing verified artifact list")?;
    // The dispatcher prefers an included static musl executable on these hosts.
    let musl_target = format!("{host_arch}-unknown-linux-musl");
    let target = if host_os == "linux"
        && matches!(host_arch, "aarch64" | "x86_64")
        && artifacts
            .iter()
            .any(|artifact| artifact["target"] == musl_target)
    {
        musl_target.as_str()
    } else {
        base_target
    };
    let native_relative = artifacts
        .iter()
        .find(|artifact| artifact["target"] == target)
        .and_then(|artifact| artifact["path"].as_str())
        .ok_or("Verified archive lacks the expected host executable")?;
    let native_executable = package_root
        .join(native_relative)
        .canonicalize()
        .map_err(|_| "Cannot resolve verified installed native executable")?;
    if !fs::symlink_metadata(&launcher)
        .map_err(|_| "npm did not install the command")?
        .file_type()
        .is_symlink()
    {
        return Err("npm command is not the expected symbolic link".into());
    }
    let utilities = temporary.0.join("without-node");
    utility_path(&utilities)?;
    let cwd = temporary.0.join("unrelated");
    fs::create_dir(&cwd).map_err(|_| "Cannot create isolated working directory")?;
    let run = |argument: &str| -> Result<Vec<u8>, String> {
        capture(
            Command::new(&launcher)
                .arg(argument)
                .env_clear()
                .env("PATH", &utilities)
                .env("AUTOROUTER_CONFIG", temporary.0.join("absent-config.json"))
                .current_dir(&cwd),
            b"",
            Duration::from_secs(20),
        )
    };
    let version = run("--version")?;
    let help = run("--help")?;
    if String::from_utf8_lossy(&version).trim() != VERSION
        || !String::from_utf8_lossy(&help).contains("Usage:")
    {
        return Err("Installed native CLI help/version failed".into());
    }
    let mut lifecycle = Command::new("cargo");
    lifecycle
        .args([
            "test",
            "--offline",
            "--locked",
            "-p",
            "claude-autorouter",
            "--test",
            "commands",
            "--test",
            "edge_cases",
            "--test",
            "launcher_logging",
            "--test",
            "onboarding_local",
            "--test",
            "launcher_contracts",
            "--test",
            "startup_options",
            "--test",
            "config_contracts",
        ])
        .current_dir(root.join("rust"))
        .env("AUTOROUTER_TEST_EXECUTABLE", &launcher)
        .env("AUTOROUTER_TEST_NATIVE_EXECUTABLE", &native_executable);
    let result = capture_result(&mut lifecycle, b"", Duration::from_secs(180))
        .map_err(|error| format!("Installed archive CLI lifecycle check failed: {error}"))?;
    if !result.status.success() {
        // Only completed, bounded captures are retained. Timeout/overflow
        // failures above keep their sanitized error and discard partial output.
        let persist = || -> Result<PathBuf, String> {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "Invalid fixture report clock")?
                .as_nanos();
            let path = archive
                .parent()
                .ok_or("Missing archive parent")?
                .join(format!("installed-lifecycle-failure-{nonce}.json"));
            let report = json!({"kind":"synthetic_installed_lifecycle_failure","archive_sha256":verified["sha256"],"exit":result.status.to_string(),"stdout":String::from_utf8_lossy(&result.stdout),"stderr":String::from_utf8_lossy(&result.stderr)});
            put(
                &path,
                &serde_json::to_vec_pretty(&report)
                    .map_err(|_| "Cannot serialize synthetic lifecycle diagnostics")?,
                0o600,
            )?;
            Ok(path)
        };
        let diagnostic = match persist() {
            Ok(path) => format!("synthetic diagnostics retained at {}", path.display()),
            Err(error) => format!("cannot retain synthetic diagnostics: {error}"),
        };
        return Err(format!(
            "Installed archive CLI lifecycle failed ({}); {diagnostic}",
            result.status
        ));
    }
    let output = String::from_utf8(result.stdout).map_err(|_| "Invalid lifecycle test output")?;
    let summaries = output
        .lines()
        .filter(|line| line.starts_with("test result: ok."))
        .collect::<Vec<_>>();
    if summaries.len() != 7
        || summaries.iter().any(|line| {
            line.strip_prefix("test result: ok. ")
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse::<usize>().ok())
                .is_none_or(|count| count == 0)
        })
    {
        return Err(
            "Installed archive lifecycle did not run all seven nonempty test suites".into(),
        );
    }
    Ok(
        json!({"passed":true,"offline_install":true,"ignore_scripts":true,"npm_symlink":true,"paths_with_spaces_quotes_and_dollar":true,"unrelated_cwd":true,"node_absent_from_execution_path":true,"help_version":true,"complete_launcher_lifecycle":{"passed":true,"test_suites":["crates/claude-autorouter/tests/commands.rs","crates/claude-autorouter/tests/edge_cases.rs","crates/claude-autorouter/tests/launcher_logging.rs","crates/claude-autorouter/tests/onboarding_local.rs","crates/claude-autorouter/tests/launcher_contracts.rs","crates/claude-autorouter/tests/startup_options.rs","crates/claude-autorouter/tests/config_contracts.rs"],"summaries":summaries}}),
    )
}
pub fn run(args: &[String], root: &Path) -> Result<bool, String> {
    let command = args.first().map(String::as_str).unwrap_or("");
    let report = match command {
        "package-inspect" => {
            if args.len() != 3 && !(args.len() == 5 && args[3] == "--max-glibc") {
                return Err(
                    "Usage: cargo xtask package-inspect TARGET BINARY [--max-glibc VERSION]".into(),
                );
            }
            let bytes = read_bounded(&root.join(&args[2]), archive::MAX_ARCHIVE as u64)?;
            let inspection = binary::inspect(&args[1], &bytes)?;
            if let Some(baseline) = args.get(4) {
                binary::enforce_glibc_baseline(&inspection, baseline)?;
            }
            json!({"sha256":sha(&bytes),"bytes":bytes.len(),"inspection":inspection,"glibc_symbol_baseline":args.get(4).map(|maximum|json!({"maximum":maximum,"passed":true,"runtime_qualification":"pending"}))})
        }
        "package" => {
            let mut destination = None;
            let mut artifacts = Vec::new();
            let mut smoke_requested = false;
            let mut index = 1;
            while index < args.len() {
                match args[index].as_str(){"--output"=>{index+=1;destination=args.get(index).map(|v|root.join(v));},"--binary"=>{index+=1;let(target,path)=args.get(index).and_then(|v|v.split_once('=')).ok_or("--binary requires TARGET=PATH")?;artifacts.push((target.to_owned(),root.join(path)));},"--smoke"=>smoke_requested=true,_=>return Err("Usage: cargo xtask package --binary TARGET=PATH [--binary ...] --output NEW_DIRECTORY [--smoke]".into())}
                index += 1;
            }
            assemble(
                root,
                &destination.ok_or("--output requires a new directory")?,
                &artifacts,
                smoke_requested,
            )?
        }
        "package-verify" => {
            if args.len() != 2 && args.len() != 4 {
                return Err("Usage: cargo xtask package-verify ARCHIVE [--sha256 HEX]".into());
            }
            if args.len() == 4 && args[2] != "--sha256" {
                return Err("Expected --sha256 HEX".into());
            }
            verify(&root.join(&args[1]), args.get(3).map(String::as_str))?
        }
        "package-smoke" if args.len() == 2 => smoke(&root.join(&args[1]), root)?,
        _ => return Err("Unknown native package command".into()),
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|_| "Cannot serialize package report")?
    );
    Ok(true)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn license_inventory_includes_subcrate_authors_and_bundled_sources() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let material = licenses(&root).unwrap();
        assert!(
            material.missing.is_empty(),
            "missing license texts: {:?}",
            material.missing
        );
        let text = String::from_utf8(material.bytes).unwrap();
        for marker in [
            "alloc-stdlib@0.3.0",
            "r-efi@5.3.0: AUTHORS",
            "AUTHORS-MIT:",
            "hyper@1.12.0: LICENSE",
            "Bundled Node.js root certificates",
            "Certificate extraction provenance",
            "openssl@0.10.81: LICENSE",
            "openssl@0.10.81: LICENSE-APACHE",
            "openssl-src@300.6.1+3.6.3: openssl/LICENSE.txt",
            "openssl-src@300.6.1+3.6.3: openssl/external/perl/Text-Template-1.56/LICENSE",
        ] {
            assert!(text.contains(marker), "missing {marker}");
        }
        assert!(
            material
                .components
                .iter()
                .any(|value| value["name"] == "hyper" && value["vendored"] == true)
        );
        assert!(material.components.iter().any(|value| {
            value["name"] == "openssl" && value["version"] == "0.10.81" && value["vendored"] == true
        }));
        assert!(
            material
                .components
                .iter()
                .any(|value| value["name"] == "node-root-certificates")
        );
        assert!(material.components.iter().any(|value| {
            value["name"] == "openssl-native"
                && value["version"] == "3.6.3"
                && value["license_sha256"]
                    == "7d5450cb2d142651b8afa315b5f238efc805dad827d91ba367d8516bc9d49e7a"
        }));
    }

    #[test]
    fn dispatcher_forwards_literal_arguments_and_exit_status_without_node() {
        let temporary = Temporary::new().unwrap();
        let root = temporary.0.join("package path ' $ x");
        put(&root.join("bin/autorouter"), DISPATCHER.as_bytes(), 0o755).unwrap();
        let native = format!(
            "native/{}/claude-autorouter",
            if cfg!(target_arch = "aarch64") {
                if cfg!(target_os = "macos") {
                    "aarch64-apple-darwin"
                } else {
                    "aarch64-unknown-linux-gnu"
                }
            } else if cfg!(target_os = "macos") {
                "x86_64-apple-darwin"
            } else {
                "x86_64-unknown-linux-gnu"
            }
        );
        put(
            &root.join(&native),
            b"#!/bin/sh\nprintf '%s\\n' \"$@\"\nexit 23\n",
            0o755,
        )
        .unwrap();
        let utilities = temporary.0.join("utilities");
        utility_path(&utilities).unwrap();
        let link = temporary.0.join("command");
        symlink(root.join("bin/autorouter"), &link).unwrap();
        let values = [
            "space argument",
            "'quoted'",
            "$HOME",
            "$(touch forbidden)",
            "`echo forbidden`",
            "--",
            "--settings=literal",
        ];
        let output = Command::new(link)
            .args(values)
            .env_clear()
            .env("PATH", &utilities)
            .current_dir(&temporary.0)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(23));
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{}\n", values.join("\n"))
        );
        assert!(!temporary.0.join("forbidden").exists());
    }
    #[test]
    fn missing_native_executable_fails_without_a_download_or_fallback() {
        let temporary = Temporary::new().unwrap();
        put(
            &temporary.0.join("bin/autorouter"),
            DISPATCHER.as_bytes(),
            0o755,
        )
        .unwrap();
        let output = Command::new(temporary.0.join("bin/autorouter"))
            .arg("--version")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("missing or not executable"));
    }
    #[test]
    fn integrity_encoding_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
    }
}
