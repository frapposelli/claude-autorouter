//! Direct archives share exactly the qualified npm payload for one target.
use super::{declarations, insert, json_bytes, json_read, matrix_targets};
use crate::evaluation::digest;
use crate::package::archive::{self, Entry};
use crate::release::{self, PACKAGE};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
const MANIFEST: &str = "build-manifest.json";
const EXECUTABLE: &str = "bin/claude-autorouter";
fn filename(version: &str, target: &str) -> String {
    format!("{PACKAGE}-{version}-{target}.tar.gz")
}
pub fn assemble(
    npm: &BTreeMap<String, Entry>,
    build: &Value,
    artifact: &Value,
    npm_name: &str,
    npm_sha: &str,
) -> Result<(String, Vec<u8>), String> {
    let target = artifact["target"]
        .as_str()
        .ok_or("Invalid direct archive target")?;
    let version = build["version"]
        .as_str()
        .ok_or("Invalid direct archive version")?;
    let mut files = BTreeMap::new();
    for (path, entry) in npm {
        if (release::ROOT_FILES.contains(&path.as_str()) && path != "package.json")
            || release::DOCS.contains(&path.as_str())
            || ["platforms.json", "THIRD-PARTY-LICENSES.txt"].contains(&path.as_str())
        {
            files.insert(path.clone(), entry.clone());
        }
    }
    files.insert(
        EXECUTABLE.into(),
        npm[artifact["path"].as_str().unwrap()].clone(),
    );
    let mut direct_artifact = artifact.clone();
    direct_artifact["path"] = json!(EXECUTABLE);
    let manifest = json!({"schema_version":2,"kind":"native_direct_release","release_approved":false,"qualification_approved":true,"version":version,"target":target,"source":build["source"],"qualification":build["qualification"],"approval":build["approval"],"npm":{"filename":npm_name,"sha256":npm_sha},"files":declarations(&files),"artifact":direct_artifact});
    insert(&mut files, MANIFEST, json_bytes(&manifest), 0o644);
    let bytes = archive::encode_root(PACKAGE, &files)?;
    verify_bytes(&filename(version, target), &bytes)?;
    Ok((filename(version, target), bytes))
}
pub fn verify_bytes(name: &str, bytes: &[u8]) -> Result<Value, String> {
    let (files, expanded) = archive::decode_root(bytes, PACKAGE)?;
    let build = json_read(
        &files
            .get(MANIFEST)
            .ok_or("Direct archive lacks manifest")?
            .bytes,
    )?;
    let version = build["version"].as_str().ok_or("Invalid direct version")?;
    release::parse_version(version).map_err(|_| "Invalid direct archive version")?;
    let target = build["target"].as_str().ok_or("Invalid direct target")?;
    if build["schema_version"] != 2
        || build["kind"] != "native_direct_release"
        || build["release_approved"] != false
        || build["qualification_approved"] != true
        || name != filename(version, target)
        || build["source"]["dirty"] != false
        || build["source"]["provenance"] != "ci-source-build"
        || !super::hex(&build["source"]["commit"], 40)
        || !super::hex(&build["source"]["cargo_lock_sha256"], 64)
        || build["approval"]["approved"] != true
        || !super::hex(&build["approval"]["record_sha256"], 64)
        || build["approval"]["source_commit"] != build["source"]["commit"]
        || build["npm"]["filename"] != format!("{PACKAGE}-{version}.tgz")
        || !super::hex(&build["npm"]["sha256"], 64)
    {
        return Err("Invalid direct archive identity or source approval".into());
    }
    for gate in super::GATES {
        let evidence = &build["qualification"][gate];
        if evidence["passed"] != true
            || !super::hex(&evidence["report_sha256"], 64)
            || evidence["source_commit"] != build["source"]["commit"]
        {
            return Err("Direct archive lacks qualification for this source".into());
        }
    }
    let matrix = json_read(
        &files
            .get("platforms.json")
            .ok_or("Missing direct platform matrix")?
            .bytes,
    )?;
    if !matrix_targets(&matrix)?.contains(target) {
        return Err("Direct target absent from qualified matrix".into());
    }
    for path in files.keys() {
        if !((release::ROOT_FILES.contains(&path.as_str()) && path != "package.json")
            || release::DOCS.contains(&path.as_str())
            || [
                MANIFEST,
                EXECUTABLE,
                "platforms.json",
                "THIRD-PARTY-LICENSES.txt",
            ]
            .contains(&path.as_str()))
        {
            return Err("Unexpected file in direct archive".into());
        }
        let expected_mode = if path == EXECUTABLE { 0o755 } else { 0o644 };
        if files[path].mode != expected_mode {
            return Err("Unexpected direct archive file mode".into());
        }
    }
    let mut seen = BTreeSet::new();
    for declaration in build["files"]
        .as_array()
        .ok_or("Missing direct file declarations")?
    {
        let path = declaration["path"]
            .as_str()
            .ok_or("Invalid direct file path")?;
        let actual = files.get(path).ok_or("Missing direct file")?;
        if !seen.insert(path)
            || declaration["sha256"] != digest(&actual.bytes)
            || declaration["bytes"].as_u64() != Some(actual.bytes.len() as u64)
            || declaration["mode"] != actual.mode
        {
            return Err("Direct file identity mismatch".into());
        }
    }
    if seen.len() + 1 != files.len() || seen.contains(MANIFEST) {
        return Err("Undeclared direct archive files".into());
    }
    for path in [
        "README.md",
        "LICENSE",
        "THIRD-PARTY-LICENSES.txt",
        "platforms.json",
        EXECUTABLE,
    ]
    .into_iter()
    .chain(release::REQUIRED_DOCS.iter().copied())
    {
        if !seen.contains(path) {
            return Err("Direct runtime documentation baseline missing".into());
        }
    }
    let binary = &files[EXECUTABLE].bytes;
    let artifact = &build["artifact"];
    if artifact["target"] != target
        || artifact["path"] != EXECUTABLE
        || artifact["sha256"] != digest(binary)
        || artifact["source_commit"] != build["source"]["commit"]
        || artifact["cargo_lock_sha256"] != build["source"]["cargo_lock_sha256"]
        || !super::hex(&artifact["provenance_sha256"], 64)
        || artifact["inspection"] != crate::package::binary::inspect(target, binary)?
    {
        return Err("Direct executable or provenance mismatch".into());
    }
    Ok(
        json!({"schema_version":2,"kind":"native_direct_release","verified":true,"filename":name,"version":version,"target":target,"sha256":digest(bytes),"bytes":bytes.len(),"expanded_tar_bytes":expanded,"source":build["source"],"npm":build["npm"],"final_authorization":"separate required evidence"}),
    )
}
pub fn verify_path(path: &Path) -> Result<Value, String> {
    let name = path
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or("Invalid direct archive filename")?;
    let bytes = release::regular(path, archive::MAX_ARCHIVE as u64)?;
    let checksum = release::regular(&path.with_file_name(format!("{name}.sha256")), 1024)?;
    if checksum != format!("{}  {name}\n", digest(&bytes)).as_bytes() {
        return Err("Direct archive checksum sidecar differs".into());
    }
    verify_bytes(name, &bytes)
}
