//! Target-specific evaluator handoff. Receiver execution needs no source checkout,
//! Node, Cargo, download hook or user configuration.
use super::{Entry, ROOT, SOURCE, sha};
use crate::package::archive::{decode_root, encode_root};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path};

const EXECUTABLE: &str = "bin/autorouter-benchmark";
const MANIFEST: &str = "executable-manifest.json";
const FIXTURE: &str = super::pair::OLLAMA_ROUTING_FIXTURE.0;
const MANIFEST_LIMIT: usize = 1024 * 1024;
const FILES: &[(&str, u32, usize)] = &[
    (EXECUTABLE, 0o755, SOURCE.file),
    (FIXTURE, 0o644, 1024 * 1024),
    ("LICENSE", 0o644, 1024 * 1024),
    ("THIRD-PARTY-LICENSES.txt", 0o644, 4 * 1024 * 1024),
    ("README.native.md", 0o644, 64 * 1024),
];
const KIND: &str = "native_benchmark_executable_bundle";
const README: &str = "# AutoRouter executable evaluator bundle\n\nThis target-specific artifact runs with no Node, Cargo, Git, source checkout or dependency installation. Verify its authenticated archive checksum before extracting with tar -xzpf into a new 0700 directory; preserve the declared file modes even under a restrictive umask. It contains only the native evaluator helper, public license material and frozen synthetic fixture.\n\n    ./bin/autorouter-benchmark evaluate-ollama --help\n\nThe only supported command is evaluate-ollama. Reading help makes no network calls. Explicit evaluation requires an already-running local Ollama and an explicitly installed decision model; no models are downloaded. From any working directory:\n\n    /absolute/path/to/bundle/bin/autorouter-benchmark evaluate-ollama --models YOUR_INSTALLED_TAG --split heldout --rounds 3 --min-agreement 1 --max-under-route-rate 0 --output /absolute/path/to/new-report.json\n\nChoose thresholds before calling a model. Keep cold, warm, stress and quality evidence separate; --timeout-ms 0 disables only the warm timer. The evaluator refuses unrelated resident models and unloads only its admitted candidate afterward. Reports may fail strict quality acceptance. This artifact cannot run Node/native paired comparisons or other source tools.\n\nThe manifest binds every archive input, source identity, fixture and executable target/hash. Source-to-binary provenance is unverified for supplied prebuilt executables. Platform qualification, release approval and downstream task quality remain pending. The separate source and matched-reference transfer formats retain their own unchanged bounds and prerequisites. No private prompts, credentials, configuration, caches or reports are included.\n";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Declaration {
    path: String,
    bytes: usize,
    sha256: String,
    mode: u32,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u8,
    kind: String,
    target: String,
    inspection: Value,
    files: Vec<Declaration>,
    fixture_sha256: String,
    cargo_lock_sha256: String,
    source_identity: Vec<Declaration>,
    source_identity_sha256: String,
    source_to_binary_provenance: String,
    dependencies: Vec<Value>,
    license_files_missing: Vec<String>,
    supported_commands: Vec<String>,
    requires_node: bool,
    requires_cargo: bool,
    downloads_performed: u8,
    live_calls_performed: bool,
    private_material_included: bool,
    release_approved: bool,
    platform_qualification: String,
}
fn declarations(files: &BTreeMap<String, Entry>) -> Vec<Declaration> {
    files
        .iter()
        .map(|(path, entry)| Declaration {
            path: path.clone(),
            bytes: entry.bytes.len(),
            sha256: sha(&entry.bytes),
            mode: entry.mode,
        })
        .collect()
}
fn hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}
fn read_file(path: &Path, limit: usize) -> Result<(Vec<u8>, u32), String> {
    read_file_with_after_read(path, limit, || {})
}
fn read_file_with_after_read(
    path: &Path,
    limit: usize,
    after_read: impl FnOnce(),
) -> Result<(Vec<u8>, u32), String> {
    read_input(path, limit, true, after_read)
}
fn read_supplied_binary(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    // Cargo can hardlink release/xtask to its deps executable on Linux. This
    // explicitly named input may have stable aliases; sources/receivers may not.
    Ok(read_input(path, limit, false, || {})?.0)
}
fn read_input(
    path: &Path,
    limit: usize,
    single_link: bool,
    after_read: impl FnOnce(),
) -> Result<(Vec<u8>, u32), String> {
    // Inspect ancestors before opening, without claiming to lock the ancestor
    // namespace. Never block on a substituted FIFO; verify the open handle and
    // final leaf identity/metadata again after reading.
    let mut current = if path.is_absolute() {
        Path::new("/").to_path_buf()
    } else {
        std::env::current_dir().map_err(|_| "Cannot resolve executable input directory")?
    };
    for part in path.components() {
        match part {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(part) => current.push(part),
            _ => return Err("Unsafe executable bundle input path".into()),
        }
        if fs::symlink_metadata(&current)
            .map_err(|_| "Missing executable bundle input")?
            .file_type()
            .is_symlink()
        {
            return Err("Executable bundle inputs must not contain symbolic links".into());
        }
    }
    let before =
        fs::symlink_metadata(&current).map_err(|_| "Cannot inspect executable bundle input")?;
    if !before.is_file() || (single_link && before.nlink() != 1) || before.len() > limit as u64 {
        return Err("Executable bundle input must be a bounded regular file".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((nix::fcntl::OFlag::O_NOFOLLOW | nix::fcntl::OFlag::O_NONBLOCK).bits())
        .open(&current)
        .map_err(|_| "Cannot open executable bundle input")?;
    let opened = file
        .metadata()
        .map_err(|_| "Cannot inspect opened executable input")?;
    if !opened.is_file() || before.dev() != opened.dev() || before.ino() != opened.ino() {
        return Err("Executable bundle input changed during inspection".into());
    }
    let mut bytes = Vec::new();
    (&file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read executable bundle input")?;
    after_read();
    let after = file
        .metadata()
        .map_err(|_| "Cannot recheck opened executable input")?;
    let leaf =
        fs::symlink_metadata(&current).map_err(|_| "Cannot recheck executable input path")?;
    if bytes.len() > limit
        || bytes.len() as u64 != opened.len()
        || !same_file(&before, &opened)
        || !same_file(&opened, &after)
        || !same_file(&after, &leaf)
    {
        return Err("Executable bundle input exceeds bounds or changed during reading".into());
    }
    Ok((bytes, opened.mode() & 0o7777))
}
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.is_file()
        && b.is_file()
        && a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.nlink() == b.nlink()
        && a.mode() == b.mode()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
fn validate(files: &BTreeMap<String, Entry>) -> Result<Manifest, String> {
    if files.len() != FILES.len() + 1
        || files
            .keys()
            .any(|p| p != MANIFEST && !FILES.iter().any(|(name, _, _)| p == name))
    {
        return Err("Executable bundle contains missing or undeclared files".into());
    }
    let record = files
        .get(MANIFEST)
        .ok_or("Missing executable bundle manifest")?;
    if record.bytes.len() > MANIFEST_LIMIT || record.mode != 0o644 {
        return Err("Invalid executable bundle manifest bounds or mode".into());
    }
    let manifest: Manifest =
        serde_json::from_slice(&record.bytes).map_err(|_| "Invalid executable bundle manifest")?;
    let inputs = files
        .iter()
        .filter(|(path, _)| path.as_str() != MANIFEST)
        .map(|(path, entry)| (path.clone(), entry.clone()))
        .collect();
    if manifest.schema_version != 1
        || manifest.kind != KIND
        || manifest.files != declarations(&inputs)
        || manifest.supported_commands != ["evaluate-ollama"]
        || manifest.source_to_binary_provenance != "unverified_prebuilt"
        || manifest.platform_qualification != "pending"
        || manifest.requires_node
        || manifest.requires_cargo
        || manifest.downloads_performed != 0
        || manifest.live_calls_performed
        || manifest.private_material_included
        || manifest.release_approved
        || !manifest.license_files_missing.is_empty()
        || manifest.dependencies.is_empty()
        || !crate::package::known_target(&manifest.target)
    {
        return Err("Executable bundle declarations or scope differ".into());
    }
    for (path, mode, limit) in FILES {
        let entry = &files[*path];
        if entry.mode != *mode || entry.bytes.is_empty() || entry.bytes.len() > *limit {
            return Err("Executable bundle file bounds or mode differ".into());
        }
    }
    if files[FIXTURE].bytes.len() != super::pair::OLLAMA_ROUTING_FIXTURE.1
        || manifest.fixture_sha256 != super::pair::OLLAMA_ROUTING_FIXTURE.2
        || manifest.fixture_sha256 != sha(&files[FIXTURE].bytes)
        || manifest.inspection
            != crate::package::binary::inspect(&manifest.target, &files[EXECUTABLE].bytes)?
        || !hex(&manifest.cargo_lock_sha256)
        || manifest.source_identity.is_empty()
        || manifest
            .source_identity
            .windows(2)
            .any(|pair| pair[0].path >= pair[1].path)
        || manifest.source_identity.iter().any(|entry| {
            (!super::allowed(&entry.path)
                && !["README.native.md", "source-manifest.json"].contains(&entry.path.as_str()))
                || !hex(&entry.sha256)
                || entry.mode != 0o644
                || entry.bytes > 4 * 1024 * 1024
        })
        || manifest.source_identity_sha256
            != sha(&serde_json::to_vec(&manifest.source_identity)
                .map_err(|_| "Invalid source identity")?)
        || !manifest
            .source_identity
            .iter()
            .any(|entry| entry.path == FIXTURE && entry.sha256 == manifest.fixture_sha256)
        || !manifest.source_identity.iter().any(|entry| {
            entry.path == "rust/Cargo.lock" && entry.sha256 == manifest.cargo_lock_sha256
        })
    {
        return Err("Executable bundle executable, fixture or source identity differs".into());
    }
    crate::package::archive::expanded_size(files, SOURCE)?;
    Ok(manifest)
}
fn assemble(
    root: &Path,
    target: &str,
    binary: Vec<u8>,
    licenses: Vec<u8>,
    dependencies: Vec<Value>,
) -> Result<BTreeMap<String, Entry>, String> {
    let source_identity: Vec<Declaration> = serde_json::from_value(super::source_identity(root)?)
        .map_err(|_| "Invalid public source identity")?;
    let mut files = BTreeMap::new();
    for (path, bytes, mode) in [
        (EXECUTABLE, binary, 0o755),
        (FIXTURE, super::regular(root, FIXTURE)?, 0o644),
        ("LICENSE", super::regular(root, "LICENSE")?, 0o644),
        ("THIRD-PARTY-LICENSES.txt", licenses, 0o644),
        ("README.native.md", README.as_bytes().to_vec(), 0o644),
    ] {
        files.insert(path.to_owned(), Entry { bytes, mode });
    }
    let manifest = Manifest {
        schema_version: 1,
        kind: KIND.into(),
        target: target.into(),
        inspection: crate::package::binary::inspect(target, &files[EXECUTABLE].bytes)?,
        files: declarations(&files),
        fixture_sha256: sha(&files[FIXTURE].bytes),
        cargo_lock_sha256: sha(&super::regular(root, "rust/Cargo.lock")?),
        source_identity_sha256: sha(&serde_json::to_vec(&source_identity)
            .map_err(|_| "Cannot serialize source identity")?),
        source_identity,
        source_to_binary_provenance: "unverified_prebuilt".into(),
        dependencies,
        license_files_missing: Vec::new(),
        supported_commands: vec!["evaluate-ollama".into()],
        requires_node: false,
        requires_cargo: false,
        downloads_performed: 0,
        live_calls_performed: false,
        private_material_included: false,
        release_approved: false,
        platform_qualification: "pending".into(),
    };
    files.insert(
        MANIFEST.into(),
        Entry {
            bytes: serde_json::to_vec_pretty(&manifest)
                .map_err(|_| "Cannot serialize executable manifest")?,
            mode: 0o644,
        },
    );
    validate(&files)?;
    Ok(files)
}
pub fn run(args: &[String], root: &Path) -> Result<bool, String> {
    if args.len() != 3 || args[2].starts_with("--") {
        return Err("Usage: cargo xtask benchmark-bundle --executable TARGET=RELEASE_XTASK NEW_ARCHIVE.tar.gz".into());
    }
    let (target, binary) = args[1]
        .split_once('=')
        .ok_or("--executable requires TARGET=RELEASE_XTASK")?;
    let destination = root.join(&args[2]);
    let sidecar = std::path::PathBuf::from(format!("{}.sha256", destination.display()));
    if fs::symlink_metadata(&destination).is_ok() || fs::symlink_metadata(&sidecar).is_ok() {
        return Err("Refusing to overwrite an existing bundle or checksum".into());
    }
    if !crate::package::known_target(target) || binary.is_empty() {
        return Err("Unsupported executable bundle target or missing binary".into());
    }
    let binary = read_supplied_binary(&root.join(binary), SOURCE.file)?;
    crate::package::binary::inspect(target, &binary)?;
    let (licenses, dependencies) = crate::package::complete_licenses(root)?;
    let files = assemble(root, target, binary, licenses, dependencies)?;
    // Verify the exact emitted representation with the unchanged source policy.
    let encoded = encode_root(ROOT, &files, SOURCE)?;
    let decoded = decode_root(&encoded, ROOT, SOURCE)?;
    validate(&decoded.files)?;
    let mut result = super::write_archive(ROOT, &files, &destination, SOURCE)?;
    result["source_only"] = json!(false);
    result["kind"] = json!(KIND);
    result["target"] = json!(target);
    result["requires_cargo"] = json!(false);
    result["dependencies_bundled"] = json!(true);
    result["source_to_binary_provenance"] = json!("unverified_prebuilt");
    println!(
        "{}",
        serde_json::to_string_pretty(&result)
            .map_err(|_| "Cannot serialize executable bundle result")?
    );
    Ok(true)
}
fn receiver(root: &Path, args: &[String]) -> Result<bool, String> {
    let mut files = BTreeMap::new();
    for (path, _, limit) in FILES
        .iter()
        .copied()
        .chain([(MANIFEST, 0o644, MANIFEST_LIMIT)])
    {
        let (bytes, mode) = read_file(&root.join(path), limit)?;
        files.insert(path.into(), Entry { bytes, mode });
    }
    validate(&files)?;
    match args.first().map(String::as_str) {
        None | Some("--help" | "help") => {
            println!(
                "AutoRouter executable evaluator bundle\nUsage: autorouter-benchmark evaluate-ollama [OPTIONS]\nOnly evaluate-ollama is available; use evaluate-ollama --help. No Node, Cargo, source checkout or dependency installation is required. Help makes no network calls."
            );
            Ok(true)
        }
        Some("evaluate-ollama") => crate::ollama_evaluation::run_with_verified_fixture(
            &args[1..],
            files
                .remove(FIXTURE)
                .ok_or("Missing admitted evaluator fixture")?
                .bytes,
        ),
        _ => Err(
            "Executable benchmark bundle supports only evaluate-ollama; use evaluate-ollama --help"
                .into(),
        ),
    }
}
/// A named packaged helper must fail closed before any build-checkout fallback.
pub fn run_packaged() -> Option<Result<bool, String>> {
    let invoked = std::env::args_os().next().is_some_and(|path| {
        Path::new(&path)
            .file_name()
            .is_some_and(|name| name == "autorouter-benchmark")
    });
    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(_) if invoked => {
            return Some(Err("Cannot resolve packaged evaluator executable".into()));
        }
        Err(_) => return None,
    };
    let named = executable
        .file_name()
        .is_some_and(|name| name == "autorouter-benchmark");
    let root = match executable.parent().and_then(Path::parent) {
        Some(root) => root,
        None if invoked || named => {
            return Some(Err("Packaged evaluator has no extracted root".into()));
        }
        None => return None,
    };
    if !invoked && !named && fs::symlink_metadata(root.join(MANIFEST)).is_err() {
        return None;
    }
    if executable
        .parent()
        .and_then(Path::file_name)
        .is_none_or(|name| name != "bin")
    {
        return Some(Err(
            "Packaged evaluator must remain in its extracted bin directory".into(),
        ));
    }
    Some(
        std::env::args_os()
            .skip(1)
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| "Invalid executable evaluator arguments".into())
            })
            .collect::<Result<Vec<_>, String>>()
            .and_then(|args| receiver(root, &args)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_process::Scratch;
    use std::collections::BTreeSet;
    #[test]
    fn bounded_input_rejects_symlinks_hardlinks_and_debug_sized_files() {
        let scratch = Scratch::new("executable-bundle-input").unwrap();
        let input = scratch.file("binary", b"synthetic").unwrap();
        fs::hard_link(&input, scratch.0.join("hardlink")).unwrap();
        assert!(read_file(&input, SOURCE.file).is_err());
        assert_eq!(
            read_supplied_binary(&input, SOURCE.file).unwrap(),
            b"synthetic"
        );
        fs::remove_file(scratch.0.join("hardlink")).unwrap();
        std::os::unix::fs::symlink(&input, scratch.0.join("symlink")).unwrap();
        assert!(read_file(&scratch.0.join("symlink"), SOURCE.file).is_err());
        fs::OpenOptions::new()
            .write(true)
            .open(&input)
            .unwrap()
            .set_len(SOURCE.file as u64 + 1)
            .unwrap();
        assert!(read_file(&input, SOURCE.file).is_err());
        assert_eq!(SOURCE.expanded, 32 * 1024 * 1024);
        assert_eq!(SOURCE.file, 32 * 1024 * 1024);
    }
    #[test]
    fn input_changed_with_the_same_length_or_replaced_after_reading_is_rejected() {
        let scratch = Scratch::new("executable-bundle-changed").unwrap();
        let input = scratch.file("binary", b"original").unwrap();
        assert_eq!(read_file(&input, SOURCE.file).unwrap().0, b"original");
        assert!(
            read_file_with_after_read(&input, SOURCE.file, || {
                fs::write(&input, b"modified").unwrap();
            })
            .is_err()
        );
        assert!(
            read_file_with_after_read(&input, SOURCE.file, || {
                let replacement = scratch.file("replacement", b"modified").unwrap();
                fs::rename(replacement, &input).unwrap();
            })
            .is_err()
        );
    }
    #[test]
    fn executable_archive_allowlist_is_exact_and_separate_from_sources() {
        let names: BTreeSet<_> = FILES
            .iter()
            .map(|(path, _, _)| *path)
            .chain([MANIFEST])
            .collect();
        assert_eq!(names.len(), 6);
        for private in [
            ".env",
            ".git/config",
            "node_modules/pkg/index.js",
            "artifacts/session.json",
            "config.json",
            "rust/target/release/xtask",
        ] {
            assert!(!names.contains(private));
        }
        assert!(FILES.iter().all(|(_, _, limit)| *limit <= SOURCE.file));
        assert!(validate(&BTreeMap::new()).is_err());
    }
    #[test]
    fn exact_manifest_rejects_tampering_unknown_files_targets_and_schema_fields() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let mut binary = vec![0_u8; 32];
        binary[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        binary[4..8].copy_from_slice(&0x0100000c_u32.to_le_bytes());
        binary[12..16].copy_from_slice(&2_u32.to_le_bytes());
        let files = assemble(
            &root,
            "aarch64-apple-darwin",
            binary,
            b"Synthetic complete license text".to_vec(),
            vec![json!({"name":"synthetic-dependency","license":"Apache-2.0"})],
        )
        .unwrap();
        assert!(validate(&files).is_ok());
        let mut changed = files.clone();
        changed.get_mut(FIXTURE).unwrap().bytes.push(b' ');
        assert!(validate(&changed).is_err());
        let mut changed = files.clone();
        changed.get_mut("THIRD-PARTY-LICENSES.txt").unwrap().mode = 0o755;
        assert!(validate(&changed).is_err());
        let mut changed = files.clone();
        changed.insert(
            ".env".into(),
            Entry {
                bytes: b"synthetic-private-canary".to_vec(),
                mode: 0o600,
            },
        );
        assert!(validate(&changed).is_err());
        for (field, value) in [
            ("target", json!("x86_64-apple-darwin")),
            ("source_identity_sha256", json!("0".repeat(64))),
            ("requires_cargo", json!(true)),
            ("license_files_missing", json!(["missing-license"])),
            ("unexpected_field", json!(true)),
        ] {
            let mut changed = files.clone();
            let mut manifest: Value = serde_json::from_slice(&changed[MANIFEST].bytes).unwrap();
            manifest[field] = value;
            changed.get_mut(MANIFEST).unwrap().bytes = serde_json::to_vec(&manifest).unwrap();
            assert!(validate(&changed).is_err(), "{field}");
        }
        // A regenerated self-consistent manifest is still not permission to
        // relabel the immutable held-out corpus. Preserve size to test the
        // independent frozen hash, rather than only a length mismatch.
        let mut changed = files.clone();
        let bytes = &mut changed.get_mut(FIXTURE).unwrap().bytes;
        let heldout = bytes
            .windows(b"held-h-arithmetic".len())
            .position(|value| value == b"held-h-arithmetic")
            .unwrap();
        let at = heldout
            + bytes[heldout..]
                .windows(7)
                .position(|value| value == b"\"haiku\"")
                .unwrap();
        bytes[at..at + 7].copy_from_slice(b"\"opus\" ");
        let relabeled: Value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(
            relabeled
                .as_array()
                .unwrap()
                .iter()
                .find(|case| case["id"] == "held-h-arithmetic")
                .unwrap()["expected"],
            "opus"
        );
        let digest = sha(bytes);
        let mut manifest: Manifest = serde_json::from_slice(&changed[MANIFEST].bytes).unwrap();
        manifest.fixture_sha256 = digest.clone();
        manifest
            .files
            .iter_mut()
            .find(|entry| entry.path == FIXTURE)
            .unwrap()
            .sha256 = digest.clone();
        manifest
            .source_identity
            .iter_mut()
            .find(|entry| entry.path == FIXTURE)
            .unwrap()
            .sha256 = digest;
        manifest.source_identity_sha256 =
            sha(&serde_json::to_vec(&manifest.source_identity).unwrap());
        changed.get_mut(MANIFEST).unwrap().bytes = serde_json::to_vec(&manifest).unwrap();
        assert!(validate(&changed).is_err());
    }
}
