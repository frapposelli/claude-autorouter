//! Immutable release identity/source/archive checks. This module never publishes.
use crate::evaluation::digest;
use crate::package::archive::{self, Entry};
use crate::process::{capture, read_bounded};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha512};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
pub const PACKAGE: &str = "claude-autorouter";
pub const REPOSITORY: &str = "frapposelli/claude-autorouter";
pub const REGISTRY: &str = "https://registry.npmjs.org";
#[derive(Clone, Debug)]
pub struct ReleaseError {
    pub code: &'static str,
    pub detail: Value,
}
impl std::fmt::Display for ReleaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Release check failed: {}", self.code)
    }
}
impl std::error::Error for ReleaseError {}
pub fn error(code: &'static str, reason: &str) -> ReleaseError {
    ReleaseError {
        code,
        detail: json!({"reason":reason}),
    }
}
pub fn unavailable(reason: &str) -> ReleaseError {
    error("registry_unavailable", reason)
}
pub fn mismatch(reason: &str) -> ReleaseError {
    error("release_mismatch", reason)
}
#[derive(Debug, PartialEq, Eq)]
pub struct Version {
    pub numbers: [u64; 3],
    pub prerelease: Vec<String>,
}
pub fn parse_version(value: &str) -> Result<Version, ReleaseError> {
    let fail = || error("invalid_version", "strict_semver_required");
    if value.is_empty()
        || value.len() > 199
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
    {
        return Err(fail());
    }
    let (core, pre) = value
        .split_once('-')
        .map(|(a, b)| (a, Some(b)))
        .unwrap_or((value, None));
    let parts: Vec<_> = core.split('.').collect();
    if parts.len() != 3 {
        return Err(fail());
    }
    let mut numbers = [0; 3];
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty()
            || !part.bytes().all(|b| b.is_ascii_digit())
            || (part.len() > 1 && part.starts_with('0'))
        {
            return Err(fail());
        }
        numbers[i] = part.parse().map_err(|_| fail())?;
        if numbers[i] > 9_007_199_254_740_991 {
            return Err(fail());
        }
    }
    let prerelease = pre
        .map(|s| s.split('.').map(str::to_owned).collect::<Vec<_>>())
        .unwrap_or_default();
    if prerelease.iter().any(|s| {
        s.is_empty()
            || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || (s.len() > 1 && s.starts_with('0') && s.bytes().all(|b| b.is_ascii_digit()))
    }) {
        return Err(fail());
    }
    Ok(Version {
        numbers,
        prerelease,
    })
}
pub fn compare_versions(a: &str, b: &str) -> Result<Ordering, ReleaseError> {
    let (a, b) = (parse_version(a)?, parse_version(b)?);
    let order = a.numbers.cmp(&b.numbers);
    if order != Ordering::Equal {
        return Ok(order);
    }
    if a.prerelease.is_empty() || b.prerelease.is_empty() {
        return Ok(match (a.prerelease.is_empty(), b.prerelease.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            _ => Ordering::Less,
        });
    }
    for (x, y) in a.prerelease.iter().zip(&b.prerelease) {
        if x == y {
            continue;
        }
        let (xnum, ynum) = (
            x.bytes().all(|b| b.is_ascii_digit()),
            y.bytes().all(|b| b.is_ascii_digit()),
        );
        return Ok(match (xnum, ynum) {
            (true, true) => x.len().cmp(&y.len()).then_with(|| x.cmp(y)),
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => x.cmp(y),
        });
    }
    Ok(a.prerelease.len().cmp(&b.prerelease.len()))
}
pub fn metadata(manifest: &Value, tag: &str, env: &Value) -> Result<Value, String> {
    if !tag.starts_with('v') || tag.len() > 200 {
        return Err("Release tag must be v<SemVer>".into());
    }
    let version = &tag[1..];
    let parsed = parse_version(version)
        .map_err(|_| "Release tag must contain strict SemVer without build metadata")?;
    if !manifest.is_object() || manifest["name"] != PACKAGE {
        return Err(format!("Package name must be {PACKAGE}"));
    }
    if manifest["version"] != version {
        return Err("Release tag does not match package.json version".into());
    }
    if manifest["repository"]["url"] != format!("git+https://github.com/{REPOSITORY}.git") {
        return Err("Package repository.url does not match the release repository".into());
    }
    if manifest["publishConfig"]["access"] != "public"
        || manifest["publishConfig"]["registry"] != format!("{REGISTRY}/")
    {
        return Err(
            "Package publishConfig must select the public npm registry and public access".into(),
        );
    }
    if manifest.get("private").is_some_and(|v| *v != false) {
        return Err("Release package must not be private".into());
    }
    for key in ["dependencies", "optionalDependencies"] {
        if manifest
            .get(key)
            .is_some_and(|v| !v.as_object().is_some_and(|v| v.is_empty()))
        {
            return Err(format!("Release package must have no {key}"));
        }
    }
    if env
        .get("GITHUB_REPOSITORY")
        .is_some_and(|v| *v != REPOSITORY)
    {
        return Err("GITHUB_REPOSITORY does not match the release repository".into());
    }
    let identity = env["GITHUB_ACTIONS"] == "true"
        || [
            "GITHUB_REPOSITORY",
            "GITHUB_REF",
            "GITHUB_REF_TYPE",
            "GITHUB_SHA",
        ]
        .iter()
        .any(|key| env.get(key).is_some());
    if identity
        && (env["GITHUB_REF_TYPE"] != "tag" || env["GITHUB_REF"] != format!("refs/tags/{tag}"))
    {
        return Err("GitHub release must run on the exact version tag".into());
    }
    Ok(
        json!({"version":version,"archive":format!("dist/{PACKAGE}-{version}.tgz"),"dist_tag":if parsed.prerelease.is_empty(){"latest"}else{"next"}}),
    )
}
pub fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = capture(
        Command::new("git")
            .arg("--no-pager")
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .current_dir(root),
        b"",
        Duration::from_secs(10),
    )?;
    String::from_utf8(output)
        .map(|s| s.trim().to_owned())
        .map_err(|_| "Invalid git output".into())
}
pub fn check_source(root: &Path, tag: &str) -> Result<(), String> {
    let head = git(root, &["rev-parse", "--verify", "HEAD^{commit}"]).map_err(
        |_| "Release tag and HEAD must resolve to commits; fetch the full repository and tags",
    )?;
    let tagged = git(
        root,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/tags/{tag}^{{commit}}"),
        ],
    )
    .map_err(
        |_| "Release tag and HEAD must resolve to commits; fetch the full repository and tags",
    )?;
    if head != tagged {
        return Err("Release tag does not resolve to the checked-out HEAD commit".into());
    }
    git(
        root,
        &[
            "merge-base",
            "--is-ancestor",
            &head,
            "refs/remotes/origin/main",
        ],
    )
    .map_err(|_| "Release commit must be reachable from origin/main; fetch the full main branch")?;
    if !git(root, &["status", "--porcelain=v1", "--untracked-files=all"])?.is_empty() {
        return Err("Release checkout must have no modified or untracked source files".into());
    }
    Ok(())
}
pub fn regular(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| "Cannot inspect release artifact")?;
    if !meta.is_file() || meta.len() > max {
        return Err("Release artifact must be a bounded regular file".into());
    }
    read_bounded(path, max)
}
pub fn base64(bytes: &[u8]) -> String {
    const C: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for group in bytes.chunks(3) {
        let n = ((group[0] as u32) << 16)
            | ((group.get(1).copied().unwrap_or(0) as u32) << 8)
            | group.get(2).copied().unwrap_or(0) as u32;
        out.push(C[((n >> 18) & 63) as usize] as char);
        out.push(C[((n >> 12) & 63) as usize] as char);
        out.push(if group.len() > 1 {
            C[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if group.len() > 2 {
            C[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
pub fn integrity(bytes: &[u8]) -> String {
    format!("sha512-{}", base64(&Sha512::digest(bytes)))
}
pub(crate) const DOCS: &[&str] = &[
    "docs/reference.md",
    "docs/development.md",
    "docs/releasing.md",
    "docs/ollama-evaluation.md",
    "docs/subscription-integration.md",
    "docs/router-performance.md",
    "docs/router-performance.json",
    "docs/status-performance.md",
    "docs/status-performance.json",
    "docs/hardware-benchmark.md",
    "docs/hardware-results-16gb.md",
    "docs/hardware-results-16gb.json",
    "docs/hardware-comparison.md",
    "docs/hardware-results-64gb.json",
];
pub(crate) const ROOT_FILES: &[&str] = &[
    "package.json",
    "README.md",
    "CONTRIBUTING.md",
    "LICENSE",
    ".env.example",
    "SECURITY.md",
    "SUPPORT.md",
    "CODE_OF_CONDUCT.md",
];
pub(crate) const REQUIRED_DOCS: &[&str] = &[
    "docs/reference.md",
    "docs/development.md",
    "docs/releasing.md",
    "docs/ollama-evaluation.md",
];
fn legacy_path(path: &str) -> bool {
    let mut parts = path.split('/');
    if !matches!(parts.next(), Some("bin" | "src")) {
        return false;
    }
    let parts: Vec<_> = parts.collect();
    !parts.is_empty()
        && parts.iter().enumerate().all(|(i, p)| {
            let name = if i + 1 == parts.len() {
                p.strip_suffix(".mjs").unwrap_or("")
            } else {
                p
            };
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        })
}
pub fn legacy_files(files: &BTreeMap<String, Entry>) -> Result<(), String> {
    for path in files.keys() {
        if !ROOT_FILES.contains(&path.as_str())
            && !DOCS.contains(&path.as_str())
            && !legacy_path(path)
        {
            return Err("Unexpected file in historical public package".into());
        }
        if path.to_ascii_lowercase().contains("node_modules") {
            return Err("Private path in historical public package".into());
        }
        let tokens: Vec<_> = path
            .split(|c: char| "/._-".contains(c))
            .map(str::to_ascii_lowercase)
            .collect();
        if tokens.iter().any(|s| {
            [
                "artifact",
                "artifacts",
                "test",
                "tests",
                "spec",
                "specs",
                "fixture",
                "fixtures",
                "probe",
                "probes",
                "secret",
                "secrets",
                "node_modules",
            ]
            .contains(&s.as_str())
        }) {
            return Err("Private path in historical public package".into());
        }
    }
    for path in [
        "package.json",
        "README.md",
        "bin/autorouter.mjs",
        "bin/statusline.mjs",
        "src/config.mjs",
        "src/router.mjs",
        "src/server.mjs",
    ]
    .into_iter()
    .chain(REQUIRED_DOCS.iter().copied())
    {
        if !files.contains_key(path) {
            return Err(format!("Historical public package is missing {path}"));
        }
    }
    Ok(())
}
fn hex(value: &Value, len: usize) -> bool {
    value.as_str().is_some_and(|s| {
        s.len() == len
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
pub fn native_files(files: &BTreeMap<String, Entry>, manifest: &Value) -> Result<Value, String> {
    let build: Value = serde_json::from_slice(
        &files
            .get("build-manifest.json")
            .ok_or("Native release requires a production build manifest")?
            .bytes,
    )
    .map_err(|_| "Invalid production build manifest")?;
    if build["schema_version"] != 2
        || build["kind"] != "native_npm_release"
        || build["release_approved"] != false
        || build["qualification_approved"] != true
        || build["version"] != manifest["version"]
    {
        return Err(
            "Native release candidate requires qualified schema 2 and separate final authorization"
                .into(),
        );
    }
    if build["source"]["dirty"] != false
        || !hex(&build["source"]["commit"], 40)
        || !hex(&build["source"]["cargo_lock_sha256"], 64)
        || build["source"]["provenance"] != "ci-source-build"
    {
        return Err("Native release lacks clean source-to-binary provenance".into());
    }
    for gate in [
        "compatibility",
        "performance",
        "platforms",
        "licenses",
        "installed_lifecycle",
    ] {
        let evidence = &build["qualification"][gate];
        if evidence["passed"] != true
            || !hex(&evidence["report_sha256"], 64)
            || evidence["source_commit"] != build["source"]["commit"]
        {
            return Err(format!("Native release lacks qualified {gate} evidence"));
        }
    }
    if manifest["bin"][PACKAGE] != "bin/autorouter"
        || manifest
            .get("scripts")
            .is_some_and(|v| !v.as_object().is_some_and(|m| m.is_empty()))
    {
        return Err("Native release requires the shell dispatcher and no lifecycle scripts".into());
    }
    let platforms: Value = serde_json::from_slice(
        &files
            .get("platforms.json")
            .ok_or("Missing qualified platform matrix")?
            .bytes,
    )
    .map_err(|_| "Invalid platform matrix")?;
    if platforms["status"] != "release_matrix_approved"
        || platforms["unresolved_baseline_architectures"]
            .as_array()
            .is_none_or(|v| !v.is_empty())
    {
        return Err("Native platform baseline is not fully qualified".into());
    }
    let targets: BTreeSet<_> = platforms["targets"]
        .as_array()
        .ok_or("Missing platform targets")?
        .iter()
        .filter_map(|v| v["target"].as_str())
        .collect();
    if targets.is_empty()
        || targets.len() != platforms["targets"].as_array().unwrap().len()
        || platforms["targets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["qualification"] != "passed")
    {
        return Err("Every release target requires qualification".into());
    }
    let native_path = |path: &str| {
        path.strip_prefix("native/")
            .and_then(|p| p.strip_suffix("/claude-autorouter"))
            .is_some_and(|t| targets.contains(t))
    };
    for path in files.keys() {
        if !ROOT_FILES.contains(&path.as_str())
            && !DOCS.contains(&path.as_str())
            && ![
                "build-manifest.json",
                "THIRD-PARTY-LICENSES.txt",
                "platforms.json",
                "bin/autorouter",
            ]
            .contains(&path.as_str())
            && !native_path(path)
        {
            return Err("Unexpected file in native production package".into());
        }
    }
    for (path, entry) in files {
        let expected = if path == "bin/autorouter" || path.starts_with("native/") {
            0o755
        } else {
            0o644
        };
        if entry.mode != expected {
            return Err("Unexpected native package file mode".into());
        }
    }
    let mut declarations = BTreeSet::new();
    for declared in build["files"]
        .as_array()
        .ok_or("Missing release file declarations")?
    {
        let path = declared["path"]
            .as_str()
            .ok_or("Invalid release file path")?;
        let actual = files.get(path).ok_or("Declared release file missing")?;
        if !declarations.insert(path)
            || declared["sha256"] != digest(&actual.bytes)
            || declared["bytes"].as_u64() != Some(actual.bytes.len() as u64)
            || declared["mode"].as_u64() != Some((actual.mode & 0o777) as u64)
        {
            return Err("Native release file checksum, mode or size mismatch".into());
        }
    }
    if declarations.len() + 1 != files.len() || declarations.contains("build-manifest.json") {
        return Err("Native release contains undeclared files".into());
    }
    for path in [
        "package.json",
        "README.md",
        "LICENSE",
        "THIRD-PARTY-LICENSES.txt",
        "platforms.json",
        "bin/autorouter",
    ]
    .into_iter()
    .chain(REQUIRED_DOCS.iter().copied())
    {
        if !declarations.contains(path) {
            return Err(format!(
                "Native runtime documentation or file missing: {path}"
            ));
        }
    }
    let dispatcher = &files["bin/autorouter"];
    if dispatcher.mode & 0o777 != 0o755
        || dispatcher.bytes != include_bytes!("../../distribution/autorouter.sh")
    {
        return Err("Native dispatcher differs from reviewed shell-only implementation".into());
    }
    let mut artifacts = BTreeSet::new();
    for artifact in build["artifacts"]
        .as_array()
        .ok_or("Missing release artifact declarations")?
    {
        let target = artifact["target"]
            .as_str()
            .ok_or("Invalid release target")?;
        let path = format!("native/{target}/claude-autorouter");
        let actual = files.get(&path).ok_or("Missing native executable")?;
        if !targets.contains(target)
            || !artifacts.insert(target)
            || artifact["path"] != path
            || artifact["sha256"] != digest(&actual.bytes)
            || actual.mode & 0o777 != 0o755
            || artifact["source_commit"] != build["source"]["commit"]
            || artifact["cargo_lock_sha256"] != build["source"]["cargo_lock_sha256"]
            || artifact["inspection"] != crate::package::binary::inspect(target, &actual.bytes)?
        {
            return Err("Native artifact bytes, target or provenance differ".into());
        }
    }
    if artifacts != targets {
        return Err("Native release does not cover the qualified platform matrix".into());
    }
    Ok(build)
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub version: String,
    pub tag: String,
    pub dist_tag: String,
    pub archive: PathBuf,
    pub filename: String,
    pub sha256: String,
    pub integrity: String,
    pub bytes: usize,
    pub native: bool,
}
impl Artifact {
    pub fn report_base(&self) -> Value {
        json!({"schema_version":1,"package":PACKAGE,"version":self.version,"tag":self.tag,"dist_tag":self.dist_tag,"archive":self.filename,"sha256":self.sha256,"integrity":self.integrity})
    }
}
pub fn inspect_artifact(
    path: &Path,
    tag: &str,
    allow_historical: bool,
) -> Result<Artifact, ReleaseError> {
    inspect_artifact_decoded(path, tag, allow_historical).map(|(artifact, _)| artifact)
}
fn inspect_artifact_decoded(
    path: &Path,
    tag: &str,
    allow_historical: bool,
) -> Result<(Artifact, archive::DecodedArchive), ReleaseError> {
    let version = tag
        .strip_prefix('v')
        .ok_or_else(|| error("invalid_version", "release_tag_required"))?;
    parse_version(version)?;
    let filename = format!("{PACKAGE}-{version}.tgz");
    if path.file_name().and_then(|s| s.to_str()) != Some(&filename) {
        return Err(error("invalid_archive", "filename_mismatch"));
    }
    let bytes = regular(path, archive::MAX_COMPRESSED as u64)
        .map_err(|_| error("invalid_archive", "archive_not_regular_or_bounded"))?;
    let checksum = regular(&path.with_extension("tgz.sha256"), 1024)
        .map_err(|_| error("invalid_archive", "checksum_not_regular_or_bounded"))?;
    let hash = digest(&bytes);
    if String::from_utf8_lossy(&checksum).trim_end() != format!("{hash}  {filename}") {
        return Err(error("invalid_archive", "checksum_mismatch"));
    }
    let envelope = if allow_historical {
        archive::MIXED
    } else {
        archive::NATIVE
    };
    let decoded = archive::decode(&bytes, envelope)
        .map_err(|_| error("invalid_archive", "archive_format"))?;
    let files = &decoded.files;
    let manifest: Value = serde_json::from_slice(
        &files
            .get("package.json")
            .ok_or_else(|| error("invalid_archive", "missing_manifest"))?
            .bytes,
    )
    .map_err(|_| error("invalid_archive", "invalid_manifest"))?;
    let metadata = metadata(&manifest, tag, &json!({}))
        .map_err(|_| error("invalid_archive", "manifest_identity"))?;
    let native =
        files.contains_key("build-manifest.json") || manifest["bin"][PACKAGE] == "bin/autorouter";
    // A native marker selects native validation irrevocably; malformed native
    // manifests cannot fall back to historical compatibility.
    decoded
        .require_policy(if native {
            archive::NATIVE
        } else {
            archive::HISTORICAL
        })
        .map_err(|_| error("invalid_archive", "archive_format_limits"))?;
    if native {
        native_files(files, &manifest)
            .map_err(|_| error("invalid_archive", "native_release_not_qualified"))?;
        crate::release_pack::verify_authorization_decoded(path, &bytes, &decoded)
            .map_err(|_| error("invalid_archive", "native_final_authorization_required"))?;
    } else if allow_historical {
        legacy_files(files).map_err(|_| error("invalid_archive", "legacy_archive_files"))?;
    } else {
        return Err(error("invalid_archive", "native_release_required"));
    }
    Ok((
        Artifact {
            version: version.into(),
            tag: tag.into(),
            dist_tag: metadata["dist_tag"].as_str().unwrap().into(),
            archive: path
                .canonicalize()
                .map_err(|_| error("invalid_archive", "archive_path"))?,
            filename,
            sha256: hash,
            integrity: integrity(&bytes),
            bytes: bytes.len(),
            native,
        },
        decoded,
    ))
}
pub fn github_output(values: &Value) -> Result<(), String> {
    let env = crate::env_file::effective();
    let Some(path) = env["GITHUB_OUTPUT"].as_str() else {
        return Ok(());
    };
    let mut text = String::new();
    for (key, value) in values.as_object().ok_or("Invalid GitHub outputs")? {
        let value = value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string());
        if key.contains(['\r', '\n', '=']) || value.contains(['\r', '\n']) {
            return Err("Unsafe GitHub output".into());
        }
        text.push_str(&format!("{key}={value}\n"));
    }
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(|_| "Cannot persist GitHub output".into())
}
fn check_archive(root: &Path, tag: &str, manifest: &Value, metadata: &Value) -> Result<(), String> {
    let dist = root.join("dist");
    if !std::fs::symlink_metadata(&dist).is_ok_and(|m| m.is_dir()) {
        return Err("dist must be a real directory".into());
    }
    let archive = root.join(metadata["archive"].as_str().unwrap());
    let (artifact, decoded) =
        inspect_artifact_decoded(&archive, tag, false).map_err(|e| e.to_string())?;
    let files = decoded.files;
    let embedded = JsManifest::parse(&files["package.json"].bytes)?;
    if embedded
        != JsManifest::parse(
            &crate::release_pack::native_manifest(manifest)?
                .to_string()
                .into_bytes(),
        )?
    {
        return Err("Archive package.json differs from the checkout".into());
    }
    let build = native_files(&files, &crate::release_pack::native_manifest(manifest)?)?;
    let platforms: Value = serde_json::from_slice(&files["platforms.json"].bytes)
        .map_err(|_| "Invalid archive platform matrix")?;
    let baseline: Value = serde_json::from_slice(&regular(
        &root.join("rust/distribution/platforms.json"),
        4 * 1024 * 1024,
    )?)
    .map_err(|_| "Invalid source platform matrix")?;
    crate::release_pack::validate_matrix(&platforms, &baseline)?;
    if build["source"]["commit"] != git(root, &["rev-parse", "--verify", "HEAD^{commit}"])?
        || build["source"]["cargo_lock_sha256"]
            != digest(&regular(&root.join("rust/Cargo.lock"), 4 * 1024 * 1024)?)
    {
        return Err("Release archive source identity differs from checkout".into());
    }
    for (path, entry) in &files {
        if path == "package.json"
            || path == "platforms.json"
            || path == "build-manifest.json"
            || path == "THIRD-PARTY-LICENSES.txt"
            || path.starts_with("native/")
        {
            continue;
        }
        let source = if path == "bin/autorouter" {
            root.join("rust/distribution/autorouter.sh")
        } else if path == "platforms.json" {
            root.join("rust/distribution/platforms.json")
        } else {
            root.join(path)
        };
        if regular(&source, archive::MAX_FILE as u64)? != entry.bytes {
            return Err(format!("Archive content differs from checkout: {path}"));
        }
    }
    if artifact.sha256.is_empty() {
        return Err("Missing archive identity".into());
    }
    Ok(())
}
struct JsManifest;
impl JsManifest {
    fn parse(bytes: &[u8]) -> Result<String, String> {
        autorouter_core::js_json::JsDocument::parse(bytes)
            .map(|d| d.stringify())
            .map_err(|_| "Invalid manifest JSON".into())
    }
}
pub fn run(args: &[String], root: &Path) -> Result<bool, String> {
    if args == ["--help"] {
        println!(
            "Usage: cargo xtask release-check source|archive v<version>\nChecks exact tag/HEAD/main ancestry or the immutable qualified native production archive. Private feasibility artifacts cannot pass release approval."
        );
        return Ok(true);
    }
    if args.len() != 2 || !["source", "archive"].contains(&args[0].as_str()) {
        return Err("Usage: cargo xtask release-check source|archive v<version>".into());
    }
    let manifest: Value =
        serde_json::from_slice(&regular(&root.join("package.json"), 1024 * 1024)?)
            .map_err(|_| "Invalid package manifest")?;
    let metadata = metadata(&manifest, &args[1], &crate::env_file::effective())?;
    if args[0] == "source" {
        check_source(root, &args[1])?;
    } else {
        check_archive(root, &args[1], &manifest, &metadata)?;
    }
    github_output(&metadata)?;
    println!("Verified {} release: {metadata}", args[0]);
    Ok(true)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn manifest() -> Value {
        json!({"name":PACKAGE,"version":"0.4.0","repository":{"url":format!("git+https://github.com/{REPOSITORY}.git")},"publishConfig":{"access":"public","registry":format!("{REGISTRY}/")}})
    }
    #[test]
    fn strict_semver_and_arbitrary_numeric_prereleases_preserve_order() {
        for bad in [
            "0.4.0+build",
            "01.4.0",
            "0.4.0-01",
            "0.4.0\n",
            "--tag",
            "latest",
            "9007199254740992.0.0",
        ] {
            assert!(parse_version(bad).is_err());
        }
        for (a, b) in [
            ("0.10.0", "0.9.9"),
            ("1.0.0", "1.0.0-rc.2"),
            ("1.0.0-rc.10", "1.0.0-rc.2"),
            (
                "1.0.0-99999999999999999999999",
                "1.0.0-9999999999999999999999",
            ),
        ] {
            assert_eq!(compare_versions(a, b).unwrap(), Ordering::Greater);
        }
    }
    #[test]
    fn release_identity_and_github_tag_are_exact() {
        let m = manifest();
        assert_eq!(
            metadata(&m, "v0.4.0", &json!({})).unwrap()["dist_tag"],
            "latest"
        );
        assert!(metadata(&m, "v0.4.0", &json!({"GITHUB_ACTIONS":"true"})).is_err());
        assert!(metadata(&m,"v0.4.0",&json!({"GITHUB_REPOSITORY":REPOSITORY,"GITHUB_REF_TYPE":"tag","GITHUB_REF":"refs/tags/v0.4.0"})).is_ok());
        let mut private = m;
        private["private"] = json!(true);
        assert!(metadata(&private, "v0.4.0", &json!({})).is_err());
    }
    #[test]
    fn feasibility_build_cannot_be_marked_public_by_manifest_only() {
        let mut files = BTreeMap::new();
        files.insert("build-manifest.json".into(),Entry{mode:0o644,bytes:json!({"schema_version":1,"kind":"native_npm_feasibility","release_approved":false,"version":"0.4.0"}).to_string().into_bytes()});
        assert_eq!(
            native_files(&files, &manifest()).unwrap_err(),
            "Native release candidate requires qualified schema 2 and separate final authorization"
        );
    }
    #[test]
    fn archive_identity_and_historical_opt_in_are_explicit() {
        let dir = crate::tool_process::Scratch::new("release-archive").unwrap();
        let mut m = manifest();
        m["bin"] = json!({PACKAGE:"bin/autorouter.mjs"});
        let mut files = BTreeMap::new();
        for path in [
            "README.md",
            "LICENSE",
            "bin/autorouter.mjs",
            "bin/statusline.mjs",
            "src/config.mjs",
            "src/router.mjs",
            "src/server.mjs",
        ]
        .into_iter()
        .chain(REQUIRED_DOCS.iter().copied())
        {
            files.insert(
                path.into(),
                Entry {
                    bytes: b"synthetic historical source".to_vec(),
                    mode: 0o644,
                },
            );
        }
        files.insert(
            "package.json".into(),
            Entry {
                bytes: m.to_string().into_bytes(),
                mode: 0o644,
            },
        );
        let bytes = archive::encode_root("package", &files, archive::HISTORICAL).unwrap();
        let filename = "claude-autorouter-0.4.0.tgz";
        let path = dir.file(filename, &bytes).unwrap();
        let checksum = format!("{}  {filename}\n", digest(&bytes));
        dir.file(&format!("{filename}.sha256"), checksum.as_bytes())
            .unwrap();
        assert!(!inspect_artifact(&path, "v0.4.0", true).unwrap().native);
        assert_eq!(
            inspect_artifact(&path, "v0.4.0", false)
                .err()
                .unwrap()
                .detail["reason"],
            "native_release_required"
        );
        dir.file(&format!("{filename}.sha256"), b"changed").unwrap();
        assert_eq!(
            inspect_artifact(&path, "v0.4.0", true)
                .err()
                .unwrap()
                .detail["reason"],
            "checksum_mismatch"
        );
    }
    #[test]
    fn mixed_inspection_preserves_historical_count_but_never_falls_back_from_native() {
        let dir = crate::tool_process::Scratch::new("release-mixed").unwrap();
        let filename = "claude-autorouter-0.4.0.tgz";
        let mut m = manifest();
        m["bin"] = json!({PACKAGE:"bin/autorouter.mjs"});
        let mut files = BTreeMap::new();
        for path in [
            "README.md",
            "LICENSE",
            "bin/autorouter.mjs",
            "bin/statusline.mjs",
            "src/config.mjs",
            "src/router.mjs",
            "src/server.mjs",
        ]
        .into_iter()
        .chain(REQUIRED_DOCS.iter().copied())
        {
            files.insert(
                path.into(),
                Entry {
                    bytes: b"synthetic".to_vec(),
                    mode: 0o644,
                },
            );
        }
        files.insert(
            "package.json".into(),
            Entry {
                bytes: m.to_string().into_bytes(),
                mode: 0o644,
            },
        );
        let inspect = |files: &BTreeMap<String, Entry>| {
            // MIXED is used only to construct otherwise forbidden test inputs.
            let bytes = archive::encode_root("package", files, archive::MIXED).unwrap();
            let path = dir.file(filename, &bytes).unwrap();
            dir.file(
                &format!("{filename}.sha256"),
                format!("{}  {filename}\n", digest(&bytes)).as_bytes(),
            )
            .unwrap();
            inspect_artifact(&path, "v0.4.0", true)
        };
        files.get_mut("README.md").unwrap().bytes = vec![0; 16 * 1024 * 1024];
        files.get_mut("LICENSE").unwrap().bytes = vec![0; 16 * 1024 * 1024];
        assert_eq!(
            inspect(&files).err().unwrap().detail["reason"],
            "archive_format_limits"
        );
        files.insert(
            "build-manifest.json".into(),
            Entry {
                bytes: b"invalid native JSON".to_vec(),
                mode: 0o644,
            },
        );
        // The same >32 MiB input is admitted as native, then rejected by its
        // native manifest. It cannot retry historical validation.
        assert_eq!(
            inspect(&files).err().unwrap().detail["reason"],
            "native_release_not_qualified"
        );
        files.get_mut("README.md").unwrap().bytes = vec![];
        files.get_mut("LICENSE").unwrap().bytes = vec![];
        files.remove("build-manifest.json");
        for n in files.len()..257 {
            files.insert(
                format!("src/extra{n}.mjs"),
                Entry {
                    bytes: vec![],
                    mode: 0o644,
                },
            );
        }
        assert!(!inspect(&files).unwrap().native);
        files.insert(
            "build-manifest.json".into(),
            Entry {
                bytes: b"{}".to_vec(),
                mode: 0o644,
            },
        );
        assert_eq!(
            inspect(&files).err().unwrap().detail["reason"],
            "archive_format_limits"
        );
    }
    #[test]
    fn source_tag_must_match_clean_head_on_main() {
        let dir = crate::tool_process::Scratch::new("release-source").unwrap();
        git(&dir.0, &["-c", "init.templateDir=", "init", "--quiet"]).unwrap();
        dir.file("tracked", b"synthetic").unwrap();
        git(&dir.0, &["add", "tracked"]).unwrap();
        git(
            &dir.0,
            &[
                "-c",
                "user.name=Synthetic Test",
                "-c",
                "user.email=synthetic@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "-m",
                "Synthetic release",
            ],
        )
        .unwrap();
        let head = git(&dir.0, &["rev-parse", "HEAD"]).unwrap();
        git(&dir.0, &["update-ref", "refs/remotes/origin/main", &head]).unwrap();
        git(&dir.0, &["tag", "v0.4.0"]).unwrap();
        check_source(&dir.0, "v0.4.0").unwrap();
        dir.file("untracked", b"synthetic").unwrap();
        assert!(
            check_source(&dir.0, "v0.4.0")
                .unwrap_err()
                .contains("untracked")
        );
        std::fs::remove_file(dir.0.join("untracked")).unwrap();
        dir.file("tracked", b"changed").unwrap();
        git(&dir.0, &["add", "tracked"]).unwrap();
        git(
            &dir.0,
            &[
                "-c",
                "user.name=Synthetic Test",
                "-c",
                "user.email=synthetic@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "-m",
                "Different commit",
            ],
        )
        .unwrap();
        assert!(
            check_source(&dir.0, "v0.4.0")
                .unwrap_err()
                .contains("checked-out HEAD")
        );
    }
    fn native_fixture() -> (BTreeMap<String, Entry>, Value) {
        let mut m = manifest();
        m["bin"] = json!({PACKAGE:"bin/autorouter"});
        let commit = "a".repeat(40);
        let lock = "b".repeat(64);
        let target = "aarch64-apple-darwin";
        let mut files = BTreeMap::new();
        for path in ["README.md", "LICENSE", "THIRD-PARTY-LICENSES.txt"]
            .into_iter()
            .chain(REQUIRED_DOCS.iter().copied())
        {
            files.insert(
                path.into(),
                Entry {
                    bytes: b"synthetic release documentation".to_vec(),
                    mode: 0o644,
                },
            );
        }
        files.insert(
            "package.json".into(),
            Entry {
                bytes: m.to_string().into_bytes(),
                mode: 0o644,
            },
        );
        files.insert(
            "bin/autorouter".into(),
            Entry {
                bytes: include_bytes!("../../distribution/autorouter.sh").to_vec(),
                mode: 0o755,
            },
        );
        files.insert("platforms.json".into(),Entry{bytes:json!({"status":"release_matrix_approved","unresolved_baseline_architectures":[],"targets":[{"target":target,"qualification":"passed"}]}).to_string().into_bytes(),mode:0o644});
        // Synthetic header only; this does not claim executable/OS qualification.
        let mut binary = vec![0u8; 32];
        binary[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        binary[4..8].copy_from_slice(&0x0100000cu32.to_le_bytes());
        binary[12..16].copy_from_slice(&2u32.to_le_bytes());
        let path = format!("native/{target}/claude-autorouter");
        let artifact = json!({"target":target,"path":path,"sha256":digest(&binary),"source_commit":commit,"cargo_lock_sha256":lock,"inspection":crate::package::binary::inspect(target,&binary).unwrap()});
        files.insert(
            path,
            Entry {
                bytes: binary,
                mode: 0o755,
            },
        );
        let mut qualification = json!({});
        for gate in [
            "compatibility",
            "performance",
            "platforms",
            "licenses",
            "installed_lifecycle",
        ] {
            qualification[gate] =
                json!({"passed":true,"report_sha256":"c".repeat(64),"source_commit":commit});
        }
        let declarations:Vec<_>=files.iter().map(|(path,entry)|json!({"path":path,"sha256":digest(&entry.bytes),"bytes":entry.bytes.len(),"mode":entry.mode})).collect();
        let build = json!({"schema_version":2,"kind":"native_npm_release","release_approved":false,"qualification_approved":true,"version":"0.4.0","source":{"dirty":false,"commit":commit,"cargo_lock_sha256":lock,"provenance":"ci-source-build"},"qualification":qualification,"files":declarations,"artifacts":[artifact]});
        files.insert(
            "build-manifest.json".into(),
            Entry {
                bytes: build.to_string().into_bytes(),
                mode: 0o644,
            },
        );
        (files, m)
    }
    #[test]
    fn production_reader_binds_all_files_modes_docs_and_qualification_to_source() {
        let (files, manifest) = native_fixture();
        native_files(&files, &manifest).unwrap();
        for mutation in [
            "unknown-runtime",
            "changed-bytes",
            "missing-doc",
            "qualification",
            "dirty-source",
            "private-build",
        ] {
            let mut changed = files.clone();
            match mutation {
                "unknown-runtime" => {
                    changed.insert(
                        "src/hidden.mjs".into(),
                        Entry {
                            bytes: b"hidden runtime".to_vec(),
                            mode: 0o644,
                        },
                    );
                }
                "changed-bytes" => changed.get_mut("bin/autorouter").unwrap().bytes.push(b' '),
                "missing-doc" => {
                    changed.remove("docs/reference.md");
                }
                _ => {
                    let mut build: Value =
                        serde_json::from_slice(&changed["build-manifest.json"].bytes).unwrap();
                    match mutation {
                        "qualification" => {
                            build["qualification"]["performance"]["passed"] = json!(false)
                        }
                        "dirty-source" => build["source"]["dirty"] = json!(true),
                        _ => build["release_approved"] = json!(true),
                    }
                    changed.get_mut("build-manifest.json").unwrap().bytes =
                        build.to_string().into_bytes();
                }
            }
            assert!(
                native_files(&changed, &manifest).is_err(),
                "mutation {mutation} must not qualify"
            );
        }
    }
}
