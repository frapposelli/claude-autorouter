//! Final immutable-archive authorization is external to avoid a hash cycle.
use super::{MAX_INPUT, descriptor, digest, exact_set, hex, json_bytes, json_read, text};
use crate::package::archive;
use crate::release::{self, PACKAGE};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
const NAME: &str = "release-authorization.json";
const INDEX: &str = "release-index.json";
pub(super) const SHARED: &[&str] = &[
    "byte_integrity",
    "help_version",
    "no_node_runtime",
    "launch_cleanup",
    "terminal_signals",
    "tls_custom_ca",
];
fn sidecar(path: &Path, cap: u64) -> Result<Vec<u8>, String> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("Invalid artifact path")?;
    let bytes = release::regular(path, cap)?;
    let checksum = release::regular(&path.with_file_name(format!("{name}.sha256")), 1024)?;
    if checksum != format!("{}  {name}\n", digest(&bytes)).as_bytes() {
        return Err("Final artifact sidecar mismatch".into());
    }
    Ok(bytes)
}
fn safe_name(value: &Value) -> Result<&str, String> {
    value
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && !s.contains('/')
                && !s.contains('\\')
                && *s != "."
                && *s != ".."
                && !s.chars().any(char::is_control)
        })
        .ok_or_else(|| "Invalid release archive name".into())
}
pub(super) struct Candidate {
    pub(super) index: Value,
    pub(super) index_hash: String,
    pub(super) expected: BTreeMap<(String, String), Value>,
}
pub(super) fn candidate(dir: &Path) -> Result<Candidate, String> {
    let index_bytes = sidecar(&dir.join(INDEX), MAX_INPUT)?;
    let index = json_read(&index_bytes)?;
    let version = index["version"].as_str().ok_or("Missing release version")?;
    release::parse_version(version).map_err(|_| "Invalid release version")?;
    if index["schema_version"] != 2
        || index["kind"] != "native_release_artifacts"
        || index["release_approved"] != false
        || index["final_authorization"] != "required"
    {
        return Err("Invalid candidate release index".into());
    }
    let npm_name = format!("{PACKAGE}-{version}.tgz");
    let mut archive_names = BTreeSet::new();
    let mut expanded_total = 0usize;
    let mut direct_targets = BTreeSet::new();
    let mut expected = BTreeMap::new();
    let mut npm_files = None;
    let mut direct_files = Vec::new();
    let mut npm_targets = None;
    let mut npm_hash = None;
    let mut direct_npm_hashes = BTreeSet::new();
    for declared in index["archives"]
        .as_array()
        .filter(|a| a.len() <= 65)
        .ok_or("Missing bounded release archives")?
    {
        let name = safe_name(&declared["filename"])?;
        if !archive_names.insert(name.to_owned()) {
            return Err("Duplicate candidate archive".into());
        }
        let bytes = sidecar(&dir.join(name), archive::MAX_ARCHIVE as u64)?;
        if declared["sha256"] != digest(&bytes)
            || declared["bytes"].as_u64() != Some(bytes.len() as u64)
            || declared["integrity"] != release::integrity(&bytes)
        {
            return Err("Candidate archive changed after qualification".into());
        }
        if name == npm_name {
            let (files, expanded) = archive::decode(&bytes)?;
            expanded_total = expanded_total
                .checked_add(expanded)
                .ok_or("Release set size overflow")?;
            let manifest = json_read(
                &files
                    .get("package.json")
                    .ok_or("Missing npm manifest")?
                    .bytes,
            )?;
            release::metadata(&manifest, &format!("v{version}"), &json!({}))?;
            let build = release::native_files(&files, &manifest)?;
            if build["source"] != index["source"]
                || build["qualification"] != index["qualification"]
                || build["approval"] != index["approval"]
            {
                return Err("Candidate npm provenance differs from index".into());
            }
            let matrix = json_read(&files["platforms.json"].bytes)?;
            let targets = super::matrix_targets(&matrix)?;
            for target in &targets {
                expected.insert((name.to_owned(),target.clone()),json!({"filename":name,"target":target,"distribution":"npm","sha256":digest(&bytes)}));
            }
            npm_files = Some(files);
            npm_targets = Some(targets);
            npm_hash = Some(digest(&bytes));
        } else {
            let report = super::direct::verify_bytes(name, &bytes)?;
            expanded_total = expanded_total
                .checked_add(report["expanded_tar_bytes"].as_u64().unwrap() as usize)
                .ok_or("Release set size overflow")?;
            direct_files.push((
                report["target"].as_str().unwrap().to_owned(),
                archive::decode_root(&bytes, PACKAGE)?.0,
            ));
            if report["version"] != version || report["source"] != index["source"] {
                return Err("Direct candidate identity differs from index".into());
            }
            let target = report["target"].as_str().unwrap();
            if !direct_targets.insert(target.to_owned()) {
                return Err("Duplicate direct target".into());
            }
            direct_npm_hashes.insert(report["npm"]["sha256"].as_str().unwrap().to_owned());
            expected.insert((name.to_owned(),target.to_owned()),json!({"filename":name,"target":target,"distribution":"direct","sha256":digest(&bytes)}));
        }
        if expanded_total > super::MAX_EVIDENCE_TOTAL {
            return Err("Expanded release set exceeds 512 MiB verification budget".into());
        }
    }
    if npm_targets.as_ref() != Some(&direct_targets)
        || npm_hash.is_none()
        || direct_npm_hashes != BTreeSet::from([npm_hash.unwrap()])
    {
        return Err(
            "Candidate must contain npm and matching direct archives for the full target matrix"
                .into(),
        );
    }
    let npm_files = npm_files.unwrap();
    for (target, files) in direct_files {
        for (path, actual) in &files {
            if path == "build-manifest.json" {
                continue;
            }
            let native = format!("native/{target}/{PACKAGE}");
            let npm_path = if path == "bin/claude-autorouter" {
                native.as_str()
            } else {
                path.as_str()
            };
            let expected = npm_files
                .get(npm_path)
                .ok_or("Direct payload has no npm counterpart")?;
            if actual.bytes != expected.bytes || actual.mode != expected.mode {
                return Err(
                    "Direct payload differs from the matching immutable npm archive".into(),
                );
            }
        }
    }
    let mut allowed = BTreeSet::from([INDEX.to_owned(), format!("{INDEX}.sha256")]);
    for name in archive_names {
        allowed.insert(format!("{name}.sha256"));
        allowed.insert(name);
    }
    let mut actual = std::fs::read_dir(dir)
        .map_err(|_| "Cannot inspect release directory")?
        .map(|entry| {
            entry
                .map_err(|_| "Cannot inspect release entry")
                .and_then(|entry| {
                    entry
                        .file_name()
                        .into_string()
                        .map_err(|_| "Non-UTF-8 release entry")
                })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    // Authorization is absent during assembly validation; if present it must be a complete pair.
    let authorization_present = actual.remove(NAME);
    let checksum_present = actual.remove(&format!("{NAME}.sha256"));
    if authorization_present != checksum_present || actual != allowed {
        return Err("Release directory must contain only the exact immutable archive set and authorization pair".into());
    }
    Ok(Candidate {
        index,
        index_hash: digest(&index_bytes),
        expected,
    })
}
fn instances(rows: &Value, candidate: &Candidate) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for row in rows.as_array().ok_or("Missing final installed instances")? {
        let name = safe_name(&row["filename"])?;
        let target = row["target"]
            .as_str()
            .ok_or("Missing final installed target")?;
        let key = (name.to_owned(), target.to_owned());
        let expected = candidate
            .expected
            .get(&key)
            .ok_or("Unexpected final installed archive or target")?;
        if !seen.insert(key)
            || row["sha256"] != expected["sha256"]
            || row["distribution"] != expected["distribution"]
            || row["passed"] != true
            || SHARED.iter().any(|key| row["checks"][key] != true)
            || (row["distribution"] == "npm"
                && ["offline_install_no_scripts", "upgrade_rollback"]
                    .iter()
                    .any(|key| row["checks"][key] != true))
        {
            return Err(
                "Final installed evidence must pass every check for these exact archive bytes"
                    .into(),
            );
        }
    }
    if seen != candidate.expected.keys().cloned().collect() {
        return Err("Final lifecycle evidence must cover every npm and direct target".into());
    }
    Ok(())
}
pub(super) fn authorize(
    base: &Path,
    input: &Value,
    candidate: &Candidate,
) -> Result<Value, String> {
    if input["schema_version"] != 1 || input["kind"] != "native_release_final_inputs" {
        return Err("Expected final release authorization inputs".into());
    }
    let report_bytes = descriptor(base, &input["report"], MAX_INPUT)?;
    let report = json_read(&report_bytes)?;
    if report["schema_version"] != 1
        || report["kind"] != "native_release_final_qualification"
        || report["passed"] != true
        || report["complete"] != true
        || report["source"] != candidate.index["source"]
        || report["release_index_sha256"] != candidate.index_hash
    {
        return Err(
            "Final installed report must bind the immutable candidate index and source".into(),
        );
    }
    instances(&report["instances"], candidate)?;
    let mut verified = Vec::new();
    let mut evidence_total = 0usize;
    for row in report["instances"].as_array().unwrap() {
        let mut hashes = BTreeSet::new();
        for evidence in row["evidence"]
            .as_array()
            .filter(|a| !a.is_empty() && a.len() <= 64)
            .ok_or("Missing final instance evidence")?
        {
            let bytes = descriptor(base, evidence, super::MAX_EVIDENCE)?;
            evidence_total = evidence_total
                .checked_add(bytes.len())
                .ok_or("Evidence size overflow")?;
            if evidence_total > super::MAX_EVIDENCE_TOTAL {
                return Err("Final evidence verification budget exceeded".into());
            }
            hashes.insert(digest(&bytes));
        }
        // Only explicit metadata crosses into the distributable authorization; paths and diagnostics stay local.
        verified.push(json!({"filename":row["filename"],"target":row["target"],"distribution":row["distribution"],"sha256":row["sha256"],"passed":true,"checks":SHARED.iter().chain(["offline_install_no_scripts","upgrade_rollback"].iter()).filter(|key|row["checks"][**key]==true).map(|key|((*key).to_owned(),json!(true))).collect::<serde_json::Map<_,_>>(),"evidence_sha256":hashes}));
    }
    let approval_bytes = descriptor(base, &input["approval"], MAX_INPUT)?;
    let approval = json_read(&approval_bytes)?;
    if approval["schema_version"] != 1
        || approval["kind"] != "native_release_final_approval"
        || approval["approved"] != true
        || approval["source"] != candidate.index["source"]
        || approval["release_index_sha256"] != candidate.index_hash
        || approval["report_sha256"] != digest(&report_bytes)
        || !text(&approval["review_reference"])
        || !text(&approval["reviewer"])
    {
        return Err(
            "Separate final review must approve exact archive index and installed evidence".into(),
        );
    }
    Ok(
        json!({"schema_version":2,"kind":"native_release_authorization","approved":true,"version":candidate.index["version"],"source":candidate.index["source"],"release_index_sha256":candidate.index_hash,"report_sha256":digest(&report_bytes),"approval_sha256":digest(&approval_bytes),"instances":verified}),
    )
}
pub fn verify(archive: &Path) -> Result<Value, String> {
    let dir = archive.parent().unwrap_or(Path::new("."));
    let candidate = candidate(dir)?;
    let name = archive
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("Invalid npm archive filename")?;
    if !candidate.expected.keys().any(|(f, _)| f == name) {
        return Err("Archive absent from authorized release set".into());
    }
    let authorization = json_read(&sidecar(&dir.join(NAME), MAX_INPUT)?)?;
    if authorization["schema_version"] != 2
        || authorization["kind"] != "native_release_authorization"
        || authorization["approved"] != true
        || authorization["version"] != candidate.index["version"]
        || authorization["source"] != candidate.index["source"]
        || authorization["release_index_sha256"] != candidate.index_hash
        || !hex(&authorization["report_sha256"], 64)
        || !hex(&authorization["approval_sha256"], 64)
    {
        return Err("Missing exact final archive authorization".into());
    }
    instances(&authorization["instances"], &candidate)?;
    for row in authorization["instances"].as_array().unwrap() {
        let hashes = exact_set(&row["evidence_sha256"])?;
        if hashes.is_empty() || hashes.iter().any(|s| !hex(&json!(s), 64)) {
            return Err("Missing retained final evidence hashes".into());
        }
    }
    Ok(authorization)
}
pub fn run(args: &[String]) -> Result<bool, String> {
    if args.len() != 3 || args[1] != "--inputs" {
        return Err("Expected release-pack authorize DIRECTORY --inputs FILE".into());
    }
    let directory = Path::new(&args[0]);
    let input_path = Path::new(&args[2]);
    let candidate = candidate(directory)?;
    let input = json_read(&release::regular(input_path, MAX_INPUT)?)?;
    let authorization = authorize(
        input_path.parent().unwrap_or(Path::new(".")),
        &input,
        &candidate,
    )?;
    let bytes = json_bytes(&authorization);
    let name = format!("{NAME}.sha256");
    // Exclusive writes; a second authorization cannot silently replace the reviewed record.
    use std::io::Write;
    let mut created = Vec::new();
    let result: Result<(), String> = (|| {
        for (name, bytes) in [
            (NAME, bytes.clone()),
            (
                name.as_str(),
                format!("{}  {NAME}\n", digest(&bytes)).into_bytes(),
            ),
        ] {
            let path = directory.join(name);
            let mut options = std::fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o644);
            }
            let mut file = options
                .open(&path)
                .map_err(|_| "Final authorization already exists or cannot be created")?;
            created.push(path);
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| "Cannot persist final authorization")?;
        }
        verify(&directory.join(format!(
            "{PACKAGE}-{}.tgz",
            candidate.index["version"].as_str().unwrap()
        )))?;
        Ok(())
    })();
    if result.is_err() {
        for path in created {
            let _ = std::fs::remove_file(path);
        }
    }
    result?;
    println!("{}", String::from_utf8_lossy(&bytes));
    Ok(true)
}
