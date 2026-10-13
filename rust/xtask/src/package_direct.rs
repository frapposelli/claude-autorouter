//! Private direct feasibility artifacts from an already-verified native npm archive.
//! Production direct release admission remains separate and schema-2-only.
use super::{archive, binary, known_target, sha};
use crate::release;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const EXECUTABLE: &str = "bin/claude-autorouter";
const MANIFEST: &str = "build-manifest.json";
const PUBLIC: &[&str] = &[
    "README.md",
    "LICENSE",
    "THIRD-PARTY-LICENSES.txt",
    "platforms.json",
];

fn name(version: &str, target: &str) -> String {
    format!("claude-autorouter-{version}-{target}.tar.gz")
}

fn assemble(bytes: &[u8], target: &str, npm_name: &str) -> Result<(String, Vec<u8>), String> {
    let decoded = archive::decode(bytes, archive::NATIVE)?;
    super::verify_decoded(bytes, &decoded)?;
    let build: Value = serde_json::from_slice(&decoded.files[MANIFEST].bytes)
        .map_err(|_| "Invalid native feasibility manifest")?;
    let version = build["version"]
        .as_str()
        .ok_or("Missing native feasibility version")?;
    if npm_name != format!("claude-autorouter-{version}.tgz") {
        return Err("Native feasibility archive filename differs from its version".into());
    }
    let artifact = build["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["target"] == target)
        .ok_or("Verified npm archive lacks the requested direct target")?;
    let mut direct_artifact = artifact.clone();
    direct_artifact["path"] = json!(EXECUTABLE);
    let mut files = BTreeMap::new();
    for path in PUBLIC {
        files.insert((*path).to_owned(), decoded.files[*path].clone());
    }
    files.insert(
        EXECUTABLE.into(),
        decoded.files[artifact["path"].as_str().unwrap()].clone(),
    );
    let declarations=files.iter().map(|(path,entry)|json!({"path":path,"sha256":sha(&entry.bytes),"bytes":entry.bytes.len(),"mode":entry.mode})).collect::<Vec<_>>();
    let manifest = json!({"schema_version":1,"kind":"native_direct_feasibility","private":true,"release_approved":false,"version":version,"target":target,"source":build["source"],"npm":{"filename":npm_name,"sha256":sha(bytes)},"artifact":direct_artifact,"files":declarations,"dependencies":build["dependencies"],"license_files_missing":build["license_files_missing"],"license_review":"pending","platform_qualification":"pending","full_matrix_size_gate":"pending until every qualified target is present"});
    files.insert(
        MANIFEST.into(),
        archive::Entry {
            bytes: serde_json::to_vec_pretty(&manifest).unwrap(),
            mode: 0o644,
        },
    );
    let filename = name(version, target);
    let direct = archive::encode_root(release::PACKAGE, &files, archive::NATIVE)?;
    let decoded = archive::decode_root(&direct, release::PACKAGE, archive::NATIVE)?;
    verify_decoded(&filename, &direct, &decoded)?;
    Ok((filename, direct))
}

pub(crate) fn verify_decoded(
    filename: &str,
    bytes: &[u8],
    decoded: &archive::DecodedArchive,
) -> Result<Value, String> {
    decoded.require_policy(archive::NATIVE)?;
    let files = &decoded.files;
    let build: Value = serde_json::from_slice(
        &files
            .get(MANIFEST)
            .ok_or("Direct feasibility archive lacks manifest")?
            .bytes,
    )
    .map_err(|_| "Invalid direct feasibility manifest")?;
    let version = build["version"]
        .as_str()
        .ok_or("Invalid direct feasibility version")?;
    release::parse_version(version).map_err(|_| "Invalid direct feasibility version")?;
    let target = build["target"]
        .as_str()
        .ok_or("Invalid direct feasibility target")?;
    if build["schema_version"] != 1
        || build["kind"] != "native_direct_feasibility"
        || build["private"] != true
        || build["release_approved"] != false
        || !known_target(target)
        || filename != name(version, target)
        || build["npm"]["filename"] != format!("claude-autorouter-{version}.tgz")
        || !build["npm"]["sha256"].as_str().is_some_and(|value| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return Err("Invalid private direct feasibility identity".into());
    }
    let mut seen = BTreeSet::new();
    for row in build["files"]
        .as_array()
        .ok_or("Missing direct feasibility file declarations")?
    {
        let path = row["path"]
            .as_str()
            .ok_or("Invalid direct feasibility path")?;
        if !(PUBLIC.contains(&path) || path == EXECUTABLE) || !seen.insert(path) {
            return Err("Unexpected or duplicate direct feasibility file".into());
        }
        let actual = files.get(path).ok_or("Missing direct feasibility file")?;
        let mode = if path == EXECUTABLE { 0o755 } else { 0o644 };
        if row["sha256"] != sha(&actual.bytes)
            || row["bytes"].as_u64() != Some(actual.bytes.len() as u64)
            || row["mode"] != mode
            || actual.mode != mode
        {
            return Err("Direct feasibility file checksum, size or mode differs".into());
        }
    }
    if seen.len() + 1 != files.len()
        || files[MANIFEST].mode != 0o644
        || !PUBLIC
            .iter()
            .chain(std::iter::once(&EXECUTABLE))
            .all(|path| seen.contains(*path))
    {
        return Err("Incomplete or undeclared direct feasibility payload".into());
    }
    let binary = &files[EXECUTABLE].bytes;
    let artifact = &build["artifact"];
    if artifact["target"] != target
        || artifact["path"] != EXECUTABLE
        || artifact["sha256"] != sha(binary)
        || artifact["bytes"].as_u64() != Some(binary.len() as u64)
        || artifact["inspection"] != binary::inspect(target, binary)?
    {
        return Err("Direct feasibility executable inspection differs".into());
    }
    Ok(
        json!({"schema_version":1,"kind":"native_direct_feasibility","verified":true,"private":true,"release_approved":false,"filename":filename,"version":version,"target":target,"sha256":sha(bytes),"compressed_bytes":bytes.len(),"expanded_tar_bytes":decoded.expanded_bytes,"source":build["source"],"npm":build["npm"],"platform_qualification":"pending","license_review":"pending","source_to_binary_provenance":"unverified"}),
    )
}

pub(crate) fn run(args: &[String], root: &Path) -> Result<Value, String> {
    if !(args.len() == 7 || (args.len() == 8 && args[7] == "--smoke"))
        || args[1] != "--direct-from"
        || args[3] != "--target"
        || args[5] != "--output"
    {
        return Err("Usage: cargo xtask package --direct-from NPM_ARCHIVE --target TARGET --output NEW_DIRECTORY [--smoke]".into());
    }
    let input = root.join(&args[2]);
    let npm_name = input
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("Invalid input native npm archive name")?;
    let bytes = release::regular(&input, archive::MAX_COMPRESSED as u64)?;
    let sidecar = release::regular(&input.with_file_name(format!("{npm_name}.sha256")), 1024)?;
    if sidecar != format!("{}  {npm_name}\n", sha(&bytes)).as_bytes() {
        return Err("Input native npm archive checksum sidecar differs".into());
    }
    let (filename, direct) = assemble(&bytes, &args[4], npm_name)?;
    let output = root.join(&args[6]);
    // Exclusive directory creation rejects an existing directory or any link,
    // including a dangling one. Never replace an earlier immutable candidate.
    fs::create_dir(&output)
        .map_err(|_| "Direct destination must be new with an existing parent")?;
    let write = |name: &str, bytes: &[u8]| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o644)
            .open(output.join(name))
            .map_err(|_| "Cannot create direct feasibility artifact")?;
        file.write_all(bytes)
            .map_err(|_| "Cannot write direct feasibility artifact".into())
    };
    let result = (|| {
        write(&filename, &direct)?;
        write(
            &format!("{filename}.sha256"),
            format!("{}  {filename}\n", sha(&direct)).as_bytes(),
        )?;
        let decoded = archive::decode_root(&direct, release::PACKAGE, archive::NATIVE)?;
        let mut report = verify_decoded(&filename, &direct, &decoded)?;
        report["archive"] = json!(output.join(&filename));
        if args.len() == 8 {
            report["smoke"] =
                crate::release_pack::direct_smoke::run(&output.join(&filename), root)?;
        }
        write(
            "package-report.json",
            &serde_json::to_vec_pretty(&report).unwrap(),
        )?;
        Ok(report)
    })();
    if result.is_err() {
        // Keep assembled immutable bytes for a failed smoke's diagnosis. Only
        // incomplete writes are cleaned up; completed archives are reviewable.
        if !output.join(&filename).is_file() || !output.join(format!("{filename}.sha256")).is_file()
        {
            let _ = fs::remove_dir_all(&output);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_process::Scratch;

    fn npm_fixture() -> Vec<u8> {
        let target = "aarch64-apple-darwin";
        let mut executable = vec![0u8; 32];
        executable[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        executable[4..8].copy_from_slice(&0x0100000cu32.to_le_bytes());
        executable[12..16].copy_from_slice(&2u32.to_le_bytes());
        let native = format!("native/{target}/claude-autorouter");
        let mut files = BTreeMap::new();
        for path in PUBLIC {
            files.insert(
                (*path).into(),
                archive::Entry {
                    bytes: b"Synthetic fixture, no qualification claim".to_vec(),
                    mode: 0o644,
                },
            );
        }
        files.insert("package.json".into(),archive::Entry{bytes:serde_json::to_vec(&json!({"name":"claude-autorouter","version":"0.5.2","private":true,"bin":{"claude-autorouter":"bin/autorouter"}})).unwrap(),mode:0o644});
        files.insert(
            "bin/autorouter".into(),
            archive::Entry {
                bytes: super::super::DISPATCHER.as_bytes().to_vec(),
                mode: 0o755,
            },
        );
        files.insert(
            native.clone(),
            archive::Entry {
                bytes: executable.clone(),
                mode: 0o755,
            },
        );
        let declarations=files.iter().map(|(path,entry)|json!({"path":path,"sha256":sha(&entry.bytes),"bytes":entry.bytes.len(),"mode":entry.mode})).collect::<Vec<_>>();
        let manifest = json!({"schema_version":1,"kind":"native_npm_feasibility","release_approved":false,"version":"0.5.2","source":{"commit":"a".repeat(40),"dirty":true,"cargo_lock_sha256":"b".repeat(64),"provenance":"synthetic unverified source fixture"},"files":declarations,"artifacts":[{"target":target,"path":native,"sha256":sha(&executable),"bytes":executable.len(),"inspection":binary::inspect(target,&executable).unwrap()}],"dependencies":[],"license_files_missing":[]});
        files.insert(
            MANIFEST.into(),
            archive::Entry {
                bytes: serde_json::to_vec(&manifest).unwrap(),
                mode: 0o644,
            },
        );
        archive::encode_root("package", &files, archive::NATIVE).unwrap()
    }

    #[test]
    fn direct_feasibility_preserves_exact_npm_payload_and_cannot_become_a_release() {
        let npm = npm_fixture();
        let (filename, direct) =
            assemble(&npm, "aarch64-apple-darwin", "claude-autorouter-0.5.2.tgz").unwrap();
        let decoded = archive::decode_root(&direct, release::PACKAGE, archive::NATIVE).unwrap();
        let npm_decoded = archive::decode(&npm, archive::NATIVE).unwrap();
        for path in PUBLIC {
            assert_eq!(decoded.files[*path].bytes, npm_decoded.files[*path].bytes);
            assert_eq!(decoded.files[*path].mode, npm_decoded.files[*path].mode);
        }
        assert_eq!(
            decoded.files[EXECUTABLE].bytes,
            npm_decoded.files["native/aarch64-apple-darwin/claude-autorouter"].bytes
        );
        let verified = verify_decoded(&filename, &direct, &decoded).unwrap();
        assert_eq!(verified["release_approved"], false);
        assert_eq!(verified["source"]["dirty"], true);
        assert_eq!(verified["npm"]["sha256"], sha(&npm));
        assert_eq!(verified["source_to_binary_provenance"], "unverified");
        assert!(crate::release_pack::verify_direct_fixture(&filename, &direct).is_err());
        assert!(assemble(&npm, "x86_64-apple-darwin", "claude-autorouter-0.5.2.tgz").is_err());
        assert!(assemble(&npm, "aarch64-apple-darwin", "changed-filename.tgz").is_err());
        for mutation in ["payload", "mode", "extra", "approved"] {
            let mut changed = decoded.files.clone();
            match mutation {
                "payload" => changed.get_mut("README.md").unwrap().bytes.push(b'!'),
                "mode" => changed.get_mut(EXECUTABLE).unwrap().mode = 0o644,
                "extra" => {
                    changed.insert(
                        "src/private.mjs".into(),
                        archive::Entry {
                            bytes: b"synthetic forbidden runtime".to_vec(),
                            mode: 0o644,
                        },
                    );
                }
                "approved" => {
                    let mut manifest: Value =
                        serde_json::from_slice(&changed[MANIFEST].bytes).unwrap();
                    manifest["release_approved"] = json!(true);
                    changed.get_mut(MANIFEST).unwrap().bytes =
                        serde_json::to_vec(&manifest).unwrap();
                }
                _ => unreachable!(),
            }
            let bytes = archive::encode_root(release::PACKAGE, &changed, archive::NATIVE).unwrap();
            let decoded = archive::decode_root(&bytes, release::PACKAGE, archive::NATIVE).unwrap();
            assert!(
                verify_decoded(&filename, &bytes, &decoded).is_err(),
                "{mutation}"
            );
        }
    }

    #[test]
    fn direct_conversion_requires_original_checksum_and_never_overwrites_a_candidate() {
        let scratch = Scratch::new("direct-feasibility-assembly").unwrap();
        let npm = npm_fixture();
        let filename = "claude-autorouter-0.5.2.tgz";
        scratch.file(filename, &npm).unwrap();
        let sidecar = format!("{filename}.sha256");
        scratch
            .file(&sidecar, format!("{}  {filename}\n", sha(&npm)).as_bytes())
            .unwrap();
        let args = [
            "package",
            "--direct-from",
            filename,
            "--target",
            "aarch64-apple-darwin",
            "--output",
            "candidate",
        ]
        .map(str::to_owned);
        let report = run(&args, &scratch.0).unwrap();
        let artifact = scratch
            .0
            .join("candidate/claude-autorouter-0.5.2-aarch64-apple-darwin.tar.gz");
        let original = fs::read(&artifact).unwrap();
        assert_eq!(report["sha256"], sha(&original));
        assert!(run(&args, &scratch.0).is_err());
        assert_eq!(fs::read(&artifact).unwrap(), original);
        fs::write(
            scratch.0.join(sidecar),
            format!("{}  {filename}\n", "0".repeat(64)),
        )
        .unwrap();
        let mut args = args;
        args[6] = "changed-checksum".into();
        assert!(run(&args, &scratch.0).is_err());
        assert!(!scratch.0.join("changed-checksum").exists());
    }
}
