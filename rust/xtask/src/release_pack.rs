//! Production assembly consumes retained qualifications; it never creates them.
use crate::evaluation::digest;
use crate::package::archive::{self, Entry};
use crate::release::{self, PACKAGE};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
pub const GATES: &[&str] = &[
    "compatibility",
    "performance",
    "platforms",
    "licenses",
    "installed_lifecycle",
];
const MAX_INPUT: u64 = 2 * 1024 * 1024;
const MAX_EVIDENCE: u64 = 128 * 1024 * 1024;
const MAX_EVIDENCE_TOTAL: usize = 512 * 1024 * 1024;
#[path = "release_authorization.rs"]
mod authorization;
#[path = "release_direct.rs"]
mod direct;
#[cfg(test)]
pub use authorization::verify as verify_authorization;
pub use authorization::verify_decoded as verify_authorization_decoded;

fn json_bytes(value: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).expect("serializable release metadata");
    bytes.push(b'\n');
    bytes
}
fn json_read(bytes: &[u8]) -> Result<Value, String> {
    serde_json::from_slice(bytes).map_err(|_| "Invalid release input JSON".into())
}
fn hex(value: &Value, len: usize) -> bool {
    value.as_str().is_some_and(|v| {
        v.len() == len
            && v.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn text(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|v| !v.is_empty() && v.len() <= 1024 && !v.chars().any(char::is_control))
}
fn descriptor(base: &Path, value: &Value, cap: u64) -> Result<Vec<u8>, String> {
    if !hex(&value["sha256"], 64) {
        return Err("Every release material requires an explicit SHA256".into());
    }
    let name = value["path"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or("Release material requires an explicit path")?;
    let bytes = release::regular(&base.join(name), cap)?;
    if digest(&bytes) != value["sha256"] {
        return Err("Release material checksum differs from reviewed input".into());
    }
    Ok(bytes)
}
fn exact_set(value: &Value) -> Result<BTreeSet<String>, String> {
    let rows = value
        .as_array()
        .ok_or("Expected explicit unique string list")?;
    let mut set = BTreeSet::new();
    for row in rows {
        let item = row
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("Invalid string list entry")?;
        if !set.insert(item.to_owned()) {
            return Err("Duplicate string list entry".into());
        }
    }
    Ok(set)
}
fn matrix_targets(matrix: &Value) -> Result<BTreeSet<String>, String> {
    if matrix["schema_version"] != 1
        || matrix["status"] != "release_matrix_approved"
        || matrix["unresolved_baseline_architectures"]
            .as_array()
            .is_none_or(|v| !v.is_empty())
    {
        return Err("Full native platform matrix remains unapproved or unresolved".into());
    }
    let mut targets = BTreeSet::new();
    for row in matrix["targets"]
        .as_array()
        .ok_or("Missing release platform targets")?
    {
        let target = row["target"]
            .as_str()
            .ok_or("Invalid release platform target")?;
        if row["qualification"] != "passed" || !targets.insert(target.to_owned()) {
            return Err("Every platform target must be unique and qualified".into());
        }
    }
    if targets.is_empty() {
        return Err("Empty release platform matrix".into());
    }
    Ok(targets)
}
pub(crate) fn validate_matrix(
    matrix: &Value,
    baseline: &Value,
) -> Result<BTreeSet<String>, String> {
    let targets = matrix_targets(matrix)?;
    for row in baseline["targets"]
        .as_array()
        .ok_or("Invalid checked-in platform baseline")?
    {
        if !row["target"].as_str().is_some_and(|t| targets.contains(t)) {
            return Err("Release matrix silently omits a checked-in baseline target".into());
        }
    }
    if matrix["baseline"] != baseline["baseline"]
        || matrix["artifact_caps"] != baseline["artifact_caps"]
    {
        return Err("Release platform baseline identity or archive caps changed".into());
    }
    let required: BTreeSet<_> = baseline["unresolved_baseline_architectures"]
        .as_array()
        .ok_or("Invalid platform baseline")?
        .iter()
        .map(|v| v.as_str().ok_or("Invalid baseline architecture"))
        .collect::<Result<_, _>>()?;
    let mut seen = BTreeSet::new();
    for resolution in matrix["baseline_resolutions"]
        .as_array()
        .ok_or("Explicit baseline architecture resolutions required")?
    {
        let name = resolution["architecture"]
            .as_str()
            .ok_or("Invalid baseline resolution")?;
        if !required.contains(name)
            || !seen.insert(name)
            || !hex(&resolution["evidence_sha256"], 64)
            || !text(&resolution["review_reference"])
        {
            return Err("Invalid or duplicate reviewed baseline resolution".into());
        }
        match resolution["result"].as_str() {
            Some("qualified")
                if resolution["target"]
                    .as_str()
                    .is_some_and(|t| targets.contains(t)) => {}
            Some("baseline_unavailable") if text(&resolution["reason"]) => {}
            _ => {
                return Err(
                    "Baseline architecture must be qualified or proven unavailable in the baseline"
                        .into(),
                );
            }
        }
    }
    if seen != required {
        return Err("Every unresolved baseline architecture needs reviewed evidence".into());
    }
    Ok(targets)
}
fn checks(gate: &str) -> &'static [&'static str] {
    match gate {
        "compatibility" => &[
            "assertion_mapping_complete",
            "differential_parity",
            "protocol_bytes",
            "privacy",
            "ordinary_checks",
        ],
        "performance" => &[
            "predeclared_thresholds",
            "representative_hardware",
            "matched_baseline",
            "required_latency_memory_gates",
            "paired_live_task_quality",
        ],
        "platforms" => &[
            "oldest_os_libc",
            "dynamic_dependencies_cpu",
            "tls_custom_ca",
            "claude_available",
            "terminal_signals",
            "wsl",
            "aggregate_archive_caps",
        ],
        "licenses" => &[
            "all_dependency_texts",
            "vendored_materials",
            "license_review",
        ],
        "installed_lifecycle" => &[
            "offline_npm_no_scripts",
            "no_node_runtime",
            "direct_archive",
            "launch_cleanup",
            "config_history_migration",
            "upgrade_rollback",
        ],
        _ => &[],
    }
}
struct Qualified {
    platforms: Vec<u8>,
    licenses: Vec<u8>,
    qualification: Value,
    approval: Value,
    artifacts: BTreeMap<String, (Vec<u8>, Value)>,
}
fn qualify(
    base: &Path,
    inputs: &Value,
    source: &Value,
    version: &str,
    baseline: &Value,
    public_files_sha: &str,
) -> Result<Qualified, String> {
    if inputs["schema_version"] != 1 || inputs["kind"] != "native_release_inputs" {
        return Err("Production release requires native_release_inputs schema 1".into());
    }
    let platforms = descriptor(base, &inputs["platforms"], MAX_INPUT)?;
    let licenses = descriptor(base, &inputs["licenses"], archive::MAX_FILE as u64)?;
    if licenses.is_empty() || std::str::from_utf8(&licenses).is_err() {
        return Err("Reviewed license material must be nonempty UTF-8".into());
    }
    let matrix = json_read(&platforms)?;
    let targets = validate_matrix(&matrix, baseline)?;
    let mut qualification = json!({});
    let mut report_hashes = json!({});
    let mut report_artifacts = Vec::new();
    let mut evidence_hashes = BTreeSet::new();
    let mut evidence_total = 0usize;
    for gate in GATES {
        let report_bytes = descriptor(base, &inputs["qualification"][gate], MAX_INPUT)?;
        let report = json_read(&report_bytes)?;
        if report["schema_version"] != 1
            || report["kind"] != "native_release_qualification"
            || report["gate"] != *gate
            || report["source_commit"] != source["commit"]
            || report["cargo_lock_sha256"] != source["cargo_lock_sha256"]
            || report["passed"] != true
            || report["complete"] != true
            || checks(gate)
                .iter()
                .any(|check| report["checks"][check] != true)
        {
            return Err(format!(
                "Missing or incomplete {gate} qualification for this exact source"
            ));
        }
        let evidence = report["evidence"]
            .as_array()
            .filter(|v| !v.is_empty() && v.len() <= 1024)
            .ok_or("Qualification requires retained bounded evidence")?;
        for item in evidence {
            let bytes = descriptor(base, item, MAX_EVIDENCE)?;
            evidence_total = evidence_total
                .checked_add(bytes.len())
                .ok_or("Evidence size overflow")?;
            if evidence_total > MAX_EVIDENCE_TOTAL {
                return Err("Retained evidence exceeds 512 MiB verification budget".into());
            }
            evidence_hashes.insert(digest(&bytes));
        }
        if *gate == "platforms"
            && (report["platforms_sha256"] != digest(&platforms)
                || exact_set(&report["targets"])? != targets)
        {
            return Err("Platform qualification differs from the reviewed full matrix".into());
        }
        if *gate == "licenses" && report["licenses_sha256"] != digest(&licenses) {
            return Err("License qualification differs from supplied material".into());
        }
        if *gate == "installed_lifecycle" && exact_set(&report["targets"])? != targets {
            return Err("Installed lifecycle evidence does not cover every target".into());
        }
        report_artifacts.push(report["artifacts"].clone());
        let hash = digest(&report_bytes);
        report_hashes[gate] = json!(hash);
        qualification[gate] =
            json!({"passed":true,"report_sha256":hash,"source_commit":source["commit"]});
    }
    for resolution in matrix["baseline_resolutions"].as_array().unwrap() {
        if !evidence_hashes.contains(resolution["evidence_sha256"].as_str().unwrap()) {
            return Err(
                "Baseline resolution evidence is not retained in qualification reports".into(),
            );
        }
    }
    let mut artifact_bytes = 0usize;
    let mut artifacts = BTreeMap::new();
    let mut artifact_hashes = json!({});
    for row in inputs["artifacts"]
        .as_array()
        .filter(|v| v.len() <= 64)
        .ok_or("Missing bounded release artifacts")?
    {
        let target = row["target"].as_str().ok_or("Invalid artifact target")?;
        if !targets.contains(target) || artifacts.contains_key(target) {
            return Err("Duplicate or undeclared native artifact".into());
        }
        let binary = descriptor(base, &row["binary"], archive::MAX_FILE as u64)?;
        artifact_bytes = artifact_bytes
            .checked_add(binary.len())
            .ok_or("Binary size overflow")?;
        if artifact_bytes > archive::MAX_NATIVE_EXPANDED {
            return Err("Aggregate native binaries exceed the expanded npm archive cap".into());
        }
        let provenance_bytes = descriptor(base, &row["provenance"], MAX_INPUT)?;
        let provenance = json_read(&provenance_bytes)?;
        let hash = digest(&binary);
        if provenance["schema_version"] != 1
            || provenance["kind"] != "native_binary_provenance"
            || provenance["provenance"] != "ci-source-build"
            || provenance["source_commit"] != source["commit"]
            || provenance["cargo_lock_sha256"] != source["cargo_lock_sha256"]
            || provenance["target"] != target
            || provenance["binary_sha256"] != hash
            || provenance["repository"] != release::REPOSITORY
            || provenance["profile"] != "release"
            || provenance["portable_cpu"] != true
            || !text(&provenance["rustc"])
            || !text(&provenance["workflow_run"])
            || !provenance["rustflags"].as_array().is_some_and(|v| {
                v.iter()
                    .all(|f| f.as_str().is_some_and(|s| !s.contains("native")))
            })
        {
            return Err("Native binary lacks matching reviewed CI build provenance".into());
        }
        let artifact = json!({"target":target,"path":format!("native/{target}/{PACKAGE}"),"sha256":hash,"source_commit":source["commit"],"cargo_lock_sha256":source["cargo_lock_sha256"],"provenance_sha256":digest(&provenance_bytes),"inspection":crate::package::binary::inspect(target,&binary)?});
        artifact_hashes[target] =
            json!({"binary_sha256":hash,"provenance_sha256":digest(&provenance_bytes)});
        artifacts.insert(target.to_owned(), (binary, artifact));
    }
    if artifacts.keys().cloned().collect::<BTreeSet<_>>() != targets {
        return Err("Missing executable for a qualified native target".into());
    }
    if report_artifacts
        .iter()
        .any(|binding| *binding != artifact_hashes)
    {
        return Err(
            "Each qualification report must bind the exact CI binary and provenance hashes".into(),
        );
    }
    let approval_bytes = descriptor(base, &inputs["approval"], MAX_INPUT)?;
    let approval = json_read(&approval_bytes)?;
    if approval["schema_version"] != 1
        || approval["kind"] != "native_release_approval"
        || approval["approved"] != true
        || approval["source_commit"] != source["commit"]
        || approval["cargo_lock_sha256"] != source["cargo_lock_sha256"]
        || approval["version"] != version
        || approval["platforms_sha256"] != digest(&platforms)
        || approval["licenses_sha256"] != digest(&licenses)
        || approval["public_files_sha256"] != public_files_sha
        || approval["qualification"] != report_hashes
        || approval["artifacts"] != artifact_hashes
        || !text(&approval["review_reference"])
        || !text(&approval["reviewer"])
        || [
            "baseline_platform_scope",
            "runtime_documentation",
            "live_canaries",
            "license_review",
        ]
        .iter()
        .any(|key| approval["reviewed"][key] != true)
    {
        return Err("Separate release approval must bind exact source, reports, materials, docs and binaries".into());
    }
    Ok(Qualified {
        platforms,
        licenses,
        qualification,
        approval: json!({"approved":true,"record_sha256":digest(&approval_bytes),"source_commit":source["commit"]}),
        artifacts,
    })
}
fn declarations(files: &BTreeMap<String, Entry>) -> Value {
    Value::Array(files.iter().map(|(path,entry)| json!({"path":path,"bytes":entry.bytes.len(),"mode":entry.mode,"sha256":digest(&entry.bytes)})).collect())
}
fn insert(files: &mut BTreeMap<String, Entry>, path: &str, bytes: Vec<u8>, mode: u32) {
    files.insert(path.into(), Entry { bytes, mode });
}
fn public_files(root: &Path) -> Result<BTreeMap<String, Entry>, String> {
    let mut files = BTreeMap::new();
    for path in release::ROOT_FILES.iter().chain(release::DOCS) {
        // Exact named source files only, without traversing symlink directories.
        let mut current = root.to_path_buf();
        for component in Path::new(path).components() {
            current.push(component.as_os_str());
            if std::fs::symlink_metadata(&current)
                .map_err(|_| "Cannot inspect public source material")?
                .file_type()
                .is_symlink()
            {
                return Err("Public source material cannot traverse symlinks".into());
            }
        }
        insert(
            &mut files,
            path,
            release::regular(&root.join(path), MAX_INPUT)?,
            0o644,
        );
    }
    Ok(files)
}
pub(crate) fn native_manifest(source: &Value) -> Result<Value, String> {
    let mut manifest = source.clone();
    let object = manifest
        .as_object_mut()
        .ok_or("Invalid source package manifest")?;
    for key in [
        "scripts",
        "devDependencies",
        "engines",
        "dependencies",
        "optionalDependencies",
        "peerDependencies",
        "bundledDependencies",
        "bundleDependencies",
    ] {
        object.remove(key);
    }
    object.insert("bin".into(), json!({PACKAGE:"bin/autorouter"}));
    object.insert(
        "files".into(),
        json!(
            release::ROOT_FILES
                .iter()
                .chain(release::DOCS)
                .copied()
                .chain([
                    "bin/autorouter",
                    "native",
                    "build-manifest.json",
                    "platforms.json",
                    "THIRD-PARTY-LICENSES.txt"
                ])
                .collect::<Vec<_>>()
        ),
    );
    Ok(manifest)
}
fn assemble(
    mut files: BTreeMap<String, Entry>,
    source: &Value,
    version: &str,
    qualified: Qualified,
) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let manifest = native_manifest(&json_read(&files["package.json"].bytes)?)?;
    release::metadata(&manifest, &format!("v{version}"), &json!({}))?;
    insert(&mut files, "package.json", json_bytes(&manifest), 0o644);
    insert(&mut files, "platforms.json", qualified.platforms, 0o644);
    insert(
        &mut files,
        "THIRD-PARTY-LICENSES.txt",
        qualified.licenses,
        0o644,
    );
    insert(
        &mut files,
        "bin/autorouter",
        include_bytes!("../../distribution/autorouter.sh").to_vec(),
        0o755,
    );
    let mut artifacts = Vec::new();
    for (target, (binary, artifact)) in qualified.artifacts {
        insert(
            &mut files,
            &format!("native/{target}/{PACKAGE}"),
            binary,
            0o755,
        );
        artifacts.push(artifact);
    }
    let build = json!({"schema_version":2,"kind":"native_npm_release","release_approved":false,"qualification_approved":true,"version":version,"source":source,"qualification":qualified.qualification,"approval":qualified.approval,"files":declarations(&files),"artifacts":artifacts});
    insert(&mut files, "build-manifest.json", json_bytes(&build), 0o644);
    release::native_files(&files, &manifest)?;
    let npm = archive::encode_root("package", &files, archive::NATIVE)?;
    drop(files);
    let (files, _) = archive::decode(&npm, archive::NATIVE)?.into_parts();
    release::native_files(&files, &manifest)?;
    let npm_name = format!("{PACKAGE}-{version}.tgz");
    let mut output = BTreeMap::new();
    let npm_sha = digest(&npm);
    output.insert(npm_name.clone(), npm);
    let mut total_output = output.values().map(Vec::len).sum::<usize>();
    for artifact in build["artifacts"].as_array().unwrap() {
        let (name, bytes) = direct::assemble(&files, &build, artifact, &npm_name, &npm_sha)?;
        total_output = total_output
            .checked_add(bytes.len())
            .ok_or("Release archive set overflow")?;
        if total_output > MAX_EVIDENCE_TOTAL {
            return Err("Release archive set exceeds 512 MiB assembly budget".into());
        }
        output.insert(name, bytes);
    }
    let index = json!({"schema_version":2,"kind":"native_release_artifacts","release_approved":false,"final_authorization":"required","version":version,"source":source,"qualification":build["qualification"],"approval":build["approval"],"archives":output.iter().map(|(name,bytes)|json!({"filename":name,"sha256":digest(bytes),"bytes":bytes.len(),"integrity":release::integrity(bytes)})).collect::<Vec<_>>()});
    output.insert("release-index.json".into(), json_bytes(&index));
    let sidecars: Vec<_> = output
        .iter()
        .map(|(name, bytes)| {
            (
                format!("{name}.sha256"),
                format!("{}  {name}\n", digest(bytes)).into_bytes(),
            )
        })
        .collect();
    output.extend(sidecars);
    Ok(output)
}
fn write_output(path: &Path, files: &BTreeMap<String, Vec<u8>>) -> Result<(), String> {
    // create_dir is exclusive, including dangling symlinks. Failed assembly never replaces output.
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|_| "Release output must be a fresh directory with an existing parent")?;
    let result = (|| {
        for (name, bytes) in files {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o644);
            }
            let mut file = options
                .open(path.join(name))
                .map_err(|_| "Cannot create release artifact")?;
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| "Cannot persist release artifact")?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(path);
    }
    result
}
pub fn run(args: &[String], root: &Path) -> Result<bool, String> {
    if args.is_empty() || args == ["--help"] {
        println!(
            "Usage: cargo xtask release-pack vVERSION --inputs FILE --output FRESH_DIRECTORY\n       cargo xtask release-pack verify-direct ARCHIVE\n       cargo xtask release-pack materials\n       cargo xtask release-pack authorize DIRECTORY --inputs FILE\n\nOffline production candidate assembly only: clean exact source tag, reviewed full platform matrix, hash-bound CI binaries, five retained qualification reports and separate approval required. No publishing, builds, downloads, version edits or qualification measurements. See rust/distribution/release-schema.md."
        );
        return Ok(true);
    }
    if args == ["materials"] {
        let files = public_files(root)?;
        let declarations = declarations(&files);
        println!(
            "{}",
            String::from_utf8_lossy(&json_bytes(
                &json!({"kind":"native_release_materials","release_approved":false,"public_files_sha256":digest(&json_bytes(&declarations)),"files":declarations,"source_commit":release::git(root,&["rev-parse","HEAD"])? ,"cargo_lock_sha256":digest(&release::regular(&root.join("rust/Cargo.lock"),MAX_INPUT)?)})
            ))
        );
        return Ok(true);
    }
    if args.first().map(String::as_str) == Some("authorize") {
        return authorization::run(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("verify-direct") && args.len() == 2 {
        println!(
            "{}",
            serde_json::to_string_pretty(&direct::verify_path(Path::new(&args[1]))?).unwrap()
        );
        return Ok(true);
    }
    if args.len() != 5 || args[1] != "--inputs" || args[3] != "--output" {
        return Err("Expected release-pack vVERSION --inputs FILE --output FRESH_DIRECTORY".into());
    }
    let tag = &args[0];
    let inputs_path = PathBuf::from(&args[2]);
    let output_path = PathBuf::from(&args[4]);
    let inputs = json_read(&release::regular(&inputs_path, MAX_INPUT)?)?;
    let public = public_files(root)?;
    let manifest = json_read(&public["package.json"].bytes)?;
    let metadata = release::metadata(&manifest, tag, &crate::env_file::effective())?;
    release::check_source(root, tag)?;
    let source = json!({"commit":release::git(root,&["rev-parse","HEAD"])? ,"dirty":false,"cargo_lock_sha256":digest(&release::regular(&root.join("rust/Cargo.lock"),MAX_INPUT)?),"provenance":"ci-source-build"});
    let baseline = json_read(&release::regular(
        &root.join("rust/distribution/platforms.json"),
        MAX_INPUT,
    )?)?;
    let version = metadata["version"].as_str().unwrap();
    let files_sha = digest(&json_bytes(&declarations(&public)));
    let qualified = qualify(
        inputs_path.parent().unwrap_or(Path::new(".")),
        &inputs,
        &source,
        version,
        &baseline,
        &files_sha,
    )?;
    let artifacts = assemble(public, &source, version, qualified)?;
    // Recheck source immediately before writing; approval binds the already-read bytes.
    release::check_source(root, tag)?;
    write_output(&output_path, &artifacts)?;
    println!(
        "{}",
        String::from_utf8_lossy(&artifacts["release-index.json"])
    );
    Ok(true)
}

#[cfg(test)]
#[path = "release_pack_tests.rs"]
mod tests;
